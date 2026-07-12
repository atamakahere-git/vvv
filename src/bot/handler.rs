use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use poise::serenity_prelude as serenity;
use poise::serenity_prelude::Mentionable;
use rust_mc_status::McClient;
use tokio::sync::{
    Mutex, RwLock,
    mpsc::{Receiver, Sender},
};

use super::types::PendingVerification;
use super::BotError;
use super::commands;
use super::types::{BotParams, Data, FromDiscordEvent, FromMinecraftEvent};
use crate::log_parser::is_silent_message_prefix;
use crate::rcon::ReconnectingRcon;
use crate::storage::Storage;

async fn process_dc_mentions(content: &str, storage: &Storage) -> String {
    if !content.contains("<@") {
        return content.to_string();
    }

    let mut replacements: Vec<(usize, usize, String)> = Vec::new();
    let bytes = content.as_bytes();
    let mut i = 0;

    while i < bytes.len().saturating_sub(2) {
        if bytes[i] != b'<' || bytes[i + 1] != b'@' {
            i += 1;
            continue;
        }
        let start = i;
        i += 2;

        if i < bytes.len() && bytes[i] == b'!' {
            i += 1;
        }

        let num_start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }

        if i > num_start && i < bytes.len() && bytes[i] == b'>' {
            i += 1;
            let id_str = std::str::from_utf8(&bytes[num_start..i - 1]).unwrap_or("");
            if let Ok(user_id) = id_str.parse::<u64>()
                && let Some(mc_name) = storage.get_mc_from_dc(user_id).await
            {
                replacements.push((start, i, format!("@{mc_name}")));
            }
        }
    }

    let mut result = content.to_string();
    for (s, e, replacement) in replacements.into_iter().rev() {
        result.replace_range(s..e, &replacement);
    }

    result
}

async fn process_mc_mentions(content: &str, sender_mc: &str, storage: &Storage) -> String {
    let mut replacements: Vec<(usize, usize, u64)> = Vec::new();
    let bytes = content.as_bytes();
    let mut word_start: Option<usize> = None;

    for (i, &b) in bytes.iter().enumerate() {
        let is_word_char = b.is_ascii_alphanumeric() || b == b'_';
        if is_word_char && word_start.is_none() {
            word_start = Some(i);
        } else if !is_word_char
            && let Some(s) = word_start
        {
            let len = i - s;
            if (3..=16).contains(&len) {
                let word = &content[s..i];
                if word != sender_mc
                    && let Some(dc_id) = storage.get_dc_from_mc(word).await
                    && !storage.is_mention_muted(dc_id).await
                {
                    replacements.push((s, i, dc_id));
                }
            }
            word_start = None;
        }
    }

    if let Some(s) = word_start {
        let len = content.len() - s;
        if (3..=16).contains(&len) {
            let word = &content[s..];
            if word != sender_mc
                && let Some(dc_id) = storage.get_dc_from_mc(word).await
                && !storage.is_mention_muted(dc_id).await
            {
                replacements.push((s, content.len(), dc_id));
            }
        }
    }

    let mut result = content.to_string();
    for (s, e, dc_id) in replacements.into_iter().rev() {
        result.replace_range(s..e, &format!("<@{dc_id}>"));
    }

    result
}

/// Start the Discord bot, register commands, and begin dispatching events.
///
/// # Errors
///
/// Returns `BotError` if the client fails to build or the bot fails to start.
#[allow(clippy::too_many_lines)]
pub async fn start_bot(
    params: BotParams,
    mc_server_address: url::Url,
    mut mc_event_rx: Receiver<FromMinecraftEvent>,
    dc_event_tx: Sender<FromDiscordEvent>,
    rcon_client: Arc<ReconnectingRcon>,
    storage: Arc<Storage>,
) -> Result<(), BotError> {
    let intents =
        serenity::GatewayIntents::non_privileged() | serenity::GatewayIntents::MESSAGE_CONTENT;

    let initial_channel = storage
        .get_bridge_channel()
        .await
        .inspect_err(|e| tracing::warn!(%e, "failed to load bridge binding, starting fresh"))
        .unwrap_or(None);
    let bridge_channel = Arc::new(RwLock::new(initial_channel));
    let bridge_channel_clone = Arc::clone(&bridge_channel);
    let storage_for_forward = Arc::clone(&storage);

    let pending_verifications: Arc<Mutex<HashMap<String, PendingVerification>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let pv_for_data = Arc::clone(&pending_verifications);
    let pv_for_forward = Arc::clone(&pending_verifications);
    let pv_for_cleanup = Arc::clone(&pending_verifications);

    let mc_status_client = McClient::new()
        .with_timeout(Duration::from_secs(5))
        .with_max_parallel(10);

    let mut owners = HashSet::new();
    owners.insert(serenity::UserId::new(params.owner_id));

    let guild_id = params.guild_id;

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            event_handler: |ctx, event, _, data| Box::pin(event_handler(ctx, event, data)),
            commands: vec![
                commands::ping(),
                commands::start_bridge(),
                commands::stop_bridge(),
                commands::info(),
                commands::stats(),
                commands::playtime(),
                commands::leaderboard(),
                commands::connect(),
                commands::disconnect(),
                commands::unsub(),
                commands::sub(),
                commands::mutemention(),
                commands::unmutemention(),
                commands::privacy(),
                commands::help(),
            ],
            prefix_options: poise::PrefixFrameworkOptions {
                prefix: Some("~".into()),
                edit_tracker: Some(Arc::new(poise::EditTracker::for_timespan(
                    Duration::from_hours(1),
                ))),
                additional_prefixes: vec![
                    poise::Prefix::Literal("hey reze,"),
                    poise::Prefix::Literal("hey reze"),
                ],
                ..Default::default()
            },
            owners,
            ..Default::default()
        })
        .setup(move |ctx, _ready, framework| {
            let addr = mc_server_address.clone();
            Box::pin(async move {
                let cmds = &framework.options().commands;
                poise::builtins::register_globally(ctx, cmds).await?;

                if let Some(gid) = guild_id {
                    let guild = serenity::GuildId::new(gid);
                    poise::builtins::register_in_guild(ctx, cmds, guild).await?;
                    tracing::info!(guild_id = %gid, "slash commands registered in guild (instant sync)");
                }

                Ok(Data {
                    dc_event_tx,
                    mc_status_client,
                    bridge_channel: bridge_channel.clone(),
                    storage: storage.clone(),
                    rcon_client,
                    mc_server_address: addr,
                    pending_verifications: pv_for_data.clone(),
                })
            })
        })
        .build();

    let client_builder = serenity::ClientBuilder::new(params.token, intents).framework(framework);
    let mut client = client_builder.await?;
    let cache_http = Arc::clone(&client.http);

    let cache_http_for_forward = Arc::clone(&cache_http);

    tokio::spawn(async move {
        while let Some(event) = mc_event_rx.recv().await {
            let target_channel = {
                let guard = bridge_channel_clone.read().await;
                *guard
            };

            let Some(target_channel) = target_channel else {
                continue;
            };

            let lower = event.content.to_ascii_lowercase();
            if let Some(code_raw) = lower.strip_prefix("@s confirm-") {
                let code = code_raw.trim().to_ascii_uppercase();
                let mut guard = pv_for_forward.lock().await;

                if let Some(pending) = guard.remove(&code) {
                    if pending.mc_username != event.username {
                        guard.insert(code, pending);
                        continue;
                    }
                    if pending.expires_at <= Instant::now() {
                        drop(guard);
                        let http = Arc::clone(&cache_http_for_forward);
                        let _ = target_channel
                            .say(
                                http,
                                format!(
                                    "⏰ <@{}> Verification for `{}` expired (took more than 30 seconds). Use `/connect` again.",
                                    pending.discord_user_id, pending.mc_username
                                ),
                            )
                            .await;
                        continue;
                    }
                    drop(guard);
                    let http = Arc::clone(&cache_http_for_forward);
                    let storage = Arc::clone(&storage_for_forward);
                    let mc = pending.mc_username.clone();
                    let dc = pending.discord_user_id;
                    tokio::spawn(async move {
                        if let Err(e) = storage.set_connection(dc, mc.clone()).await {
                            tracing::error!(%e, "failed to persist account connection");
                            let _ = target_channel
                                .say(
                                    http,
                                    format!("❌ <@{dc}> Failed to save connection. Please try again."),
                                )
                                .await;
                        } else {
                            let _ = target_channel
                                .say(
                                    http,
                                    format!(
                                        "✅ <@{dc}> Connected! Your Minecraft account **{mc}** is now linked."
                                    ),
                                )
                                .await;
                        }
                    });
                    continue;
                }

                let mut lockout_key: Option<String> = None;
                for (key, pending) in guard.iter_mut() {
                    if pending.mc_username == event.username {
                        pending.attempts += 1;
                        if pending.attempts >= 3 {
                            lockout_key = Some(key.clone());
                        }
                        break;
                    }
                }
                if let Some(key) = &lockout_key {
                    let pending = guard.remove(key);
                    drop(guard);
                    if let Some(pending) = pending {
                        let http = Arc::clone(&cache_http_for_forward);
                        let _ = target_channel
                            .say(
                                http,
                                format!(
                                    "🚫 <@{}> Verification for `{}` locked out after 3 failed attempts. Use `/connect` again.",
                                    pending.discord_user_id, pending.mc_username
                                ),
                            )
                            .await;
                    }
                } else {
                    drop(guard);
                }
                continue;
            }

            if storage_for_forward.is_privacy_enabled().await
                && !event.mc_username.is_empty()
                && let Some(dc_id) = storage_for_forward
                    .get_dc_from_mc(&event.mc_username)
                    .await
                && storage_for_forward.is_join_leave_opted_out(dc_id).await
            {
                continue;
            }

            let is_chat = event.username == event.mc_username && !event.mc_username.is_empty();

            let formatted_message = if is_chat
                && storage_for_forward.is_privacy_enabled().await
            {
                let mention_content = process_mc_mentions(
                    &event.content,
                    &event.mc_username,
                    &storage_for_forward,
                )
                .await;
                format!("**{}**: {}", event.username, mention_content)
            } else {
                format!("**{}**: {}", event.username, event.content)
            };

            let http_clone = Arc::clone(&cache_http_for_forward);
            let msg = formatted_message.clone();
            let bridge_ref = Arc::clone(&bridge_channel_clone);
            let storage_ref = Arc::clone(&storage_for_forward);

            tokio::spawn(async move {
                if let Err(why) = target_channel.say(http_clone, msg).await {
                    tracing::warn!(
                        channel = %target_channel,
                        error = %why,
                        "failed to forward Minecraft event to Discord"
                    );

                    let error_msg = why.to_string();
                    if error_msg.contains("Unknown Channel") || error_msg.contains("Missing Access")
                    {
                        let mut guard = bridge_ref.write().await;
                        *guard = None;
                        tracing::info!(
                            channel = %target_channel,
                            "cleared unreachable bridge channel"
                        );
                        if let Err(e) = storage_ref.clear_bridge_channel().await {
                            tracing::error!(%e, "failed to persist bridge clear");
                        }
                    }
                }
            });
        }
    });

    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(10)).await;
            let mut guard = pv_for_cleanup.lock().await;
            guard.retain(|_, v| v.expires_at > Instant::now());
        }
    });

    client.start().await?;
    Ok(())
}

async fn event_handler(
    ctx: &serenity::Context,
    event: &serenity::FullEvent,
    data: &Data,
) -> Result<(), BotError> {
    match event {
        serenity::FullEvent::Ready { data_about_bot, .. } => {
            tracing::info!(name = %data_about_bot.user.name, "bot logged in");
        }
        serenity::FullEvent::GuildMemberAddition { new_member } => {
            let (system_channel_id, member_count) = {
                let Some(guild) = new_member.guild_id.to_guild_cached(ctx) else {
                    return Ok(());
                };
                (guild.system_channel_id, guild.member_count)
            };

            let Some(system_channel) = system_channel_id else {
                return Ok(());
            };

            tracing::info!(
                user = %new_member.user.name,
                guild = %new_member.guild_id,
                "guild member joined"
            );

            let welcome_embed = serenity::CreateEmbed::new()
                .title("💥 A New Target Approaches! 💥")
                .description(format!(
                    "Welcome to the server, {}! Let's hope things don't get too... explosive. 🤫",
                    new_member.mention()
                ))
                .color(0x9b5_9b6)
                .thumbnail(
                    "https://i.pinimg.com/originals/5d/15/4b/5d154b68de57a87600fe9b98d692802c.gif",
                )
                .footer(serenity::CreateEmbedFooter::new(format!(
                    "Member Count: #{member_count}"
                )));

            let message = serenity::CreateMessage::new().embed(welcome_embed);
            system_channel
                .send_message(&ctx.http, message)
                .await
                .inspect_err(|e| {
                    tracing::warn!(
                        channel = %system_channel,
                        error = %e,
                        "failed to send welcome message"
                    );
                })
                .ok();
        }
        serenity::FullEvent::Message { new_message } => {
            let should_relay = {
                let guard = data.bridge_channel.read().await;
                *guard == Some(new_message.channel_id)
            };

            if should_relay && new_message.author.id != ctx.cache.current_user().id {
                let author_id = new_message.author.id.get();

                if data.storage.is_privacy_enabled().await {
                    if !data.storage.is_connected_dc(author_id).await {
                        tracing::debug!(
                            user = %new_message.author.name,
                            "not connected, skipping discord→mc"
                        );
                        return Ok(());
                    }
                    if data.storage.is_join_leave_opted_out(author_id).await {
                        tracing::debug!(
                            user = %new_message.author.name,
                            "opted out, skipping discord→mc"
                        );
                        return Ok(());
                    }
                }

                let is_silent = is_silent_message_prefix(&new_message.content)
                    || new_message.flags.is_some_and(|f| {
                        f.contains(serenity::MessageFlags::SUPPRESS_NOTIFICATIONS)
                    });

                if is_silent {
                    tracing::debug!(
                        user = %new_message.author.name,
                        content = %new_message.content,
                        "ignored silent discord→mc message"
                    );
                } else {
                    let content = if data.storage.is_privacy_enabled().await {
                        process_dc_mentions(&new_message.content, &data.storage).await
                    } else {
                        new_message.content.clone()
                    };
                    data.dc_event_tx
                        .send(FromDiscordEvent {
                            username: new_message.author.name.clone(),
                            content,
                        })
                        .await
                        .inspect_err(|e| {
                            tracing::warn!(
                                user = %new_message.author.name,
                                error = %e,
                                "discord→mc event dropped"
                            );
                        })
                        .ok();
                }
            }
        }
        _ => {}
    }
    Ok(())
}
