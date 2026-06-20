use std::{collections::HashSet, sync::Arc, time::Duration};

use mc_rcon::RconClient;
use poise::serenity_prelude as serenity;
use poise::serenity_prelude::Mentionable;
use rust_mc_status::McClient;
use tokio::sync::{
    Mutex, RwLock,
    mpsc::{Receiver, Sender},
};

use super::BotError;
use super::commands;
use super::types::{BotParams, Data, FromDiscordEvent, FromMinecraftEvent};
use crate::log_parser::is_silent_message_prefix;

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
    rcon_client: Arc<Mutex<RconClient>>,
) -> Result<(), BotError> {
    let intents =
        serenity::GatewayIntents::non_privileged() | serenity::GatewayIntents::MESSAGE_CONTENT;

    let initial_channels = crate::storage::load_channels()
        .await
        .inspect_err(|e| tracing::warn!(%e, "failed to load bridge state, starting fresh"))
        .unwrap_or_default();
    let bridge_channel_list = Arc::new(RwLock::new(initial_channels));
    let bridge_channel_list_clone = Arc::clone(&bridge_channel_list);

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
                    target_channel_id_list: bridge_channel_list.clone(),
                    rcon_client,
                    mc_server_address: addr,
                })
            })
        })
        .build();

    let client_builder = serenity::ClientBuilder::new(params.token, intents).framework(framework);
    let mut client = client_builder.await?;
    let cache_http = Arc::clone(&client.http);

    tokio::spawn(async move {
        while let Some(event) = mc_event_rx.recv().await {
            let formatted_message = format!("**{}**: {}", event.username, event.content);

            let targets = {
                let guard = bridge_channel_list_clone.read().await;
                guard.clone()
            };

            if targets.is_empty() {
                continue;
            }

            for target_channel in targets {
                let http_clone = Arc::clone(&cache_http);
                let msg = formatted_message.clone();
                let remove_list = Arc::clone(&bridge_channel_list_clone);

                tokio::spawn(async move {
                    if let Err(why) = target_channel.say(http_clone, msg).await {
                        tracing::warn!(
                            channel = %target_channel,
                            error = %why,
                            "failed to forward Minecraft event to Discord"
                        );

                        let error_msg = why.to_string();
                        if error_msg.contains("Unknown Channel")
                            || error_msg.contains("Missing Access")
                        {
                            let mut guard = remove_list.write().await;
                            guard.retain(|c| *c != target_channel);
                            tracing::info!(
                                channel = %target_channel,
                                "removed unreachable channel from bridge list"
                            );
                        }
                    }
                });
            }
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
                let targets = data.target_channel_id_list.read().await;
                targets.contains(&new_message.channel_id)
            };

            if should_relay && new_message.author.id != ctx.cache.current_user().id {
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
                    data.dc_event_tx
                        .send(FromDiscordEvent {
                            username: new_message.author.name.clone(),
                            content: new_message.content.clone(),
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
