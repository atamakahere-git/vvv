use std::{collections::HashSet, env, sync::Arc, time::Duration};

use mc_rcon::RconClient;
use poise::serenity_prelude as serenity;
use rust_mc_status::McClient;
use poise::serenity_prelude::Mentionable;
use tokio::sync::{
    Mutex, RwLock,
    mpsc::{Receiver, Sender},
};

use super::commands;
use super::types::{Data, FromDiscordEvent, FromMinecraftEvent};
use super::BotError;

const OWNER_ID: u64 = 1_314_616_785_156_444_175;

/// Start the Discord bot, register commands, and begin dispatching events.
///
/// # Errors
///
/// Returns `BotError` if the `DISCORD_TOKEN` env var is missing, the client
/// fails to build, or the bot fails to start.
pub async fn start_bot(
    mut mc_event_rx: Receiver<FromMinecraftEvent>,
    dc_event_tx: Sender<FromDiscordEvent>,
    rcon_client: Arc<Mutex<RconClient>>,
) -> Result<(), BotError> {
    let token =
        env::var("DISCORD_TOKEN").map_err(|_| BotError::EnvVar("DISCORD_TOKEN".into()))?;

    let intents =
        serenity::GatewayIntents::non_privileged() | serenity::GatewayIntents::MESSAGE_CONTENT;

    let bridge_channel_list = Arc::new(RwLock::new(Vec::new()));
    let bridge_channel_list_clone = Arc::clone(&bridge_channel_list);

    let mc_status_client = McClient::new()
        .with_timeout(Duration::from_secs(5))
        .with_max_parallel(10);

    let mut owners = HashSet::new();
    owners.insert(serenity::UserId::new(OWNER_ID));

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
        .setup(|ctx, _ready, framework| {
            Box::pin(async move {
                poise::builtins::register_globally(ctx, &framework.options().commands).await?;

                Ok(Data {
                    dc_event_tx,
                    mc_status_client,
                    target_channel_id_list: bridge_channel_list.clone(),
                    rcon_client,
                })
            })
        })
        .build();

    let client_builder = serenity::ClientBuilder::new(token, intents).framework(framework);
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

                tokio::spawn(async move {
                    if let Err(why) = target_channel.say(http_clone, msg).await {
                        tracing::warn!("failed to send to channel {target_channel}: {why:?}");
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
            tracing::info!("Logged in as {}", data_about_bot.user.name);
        }
        serenity::FullEvent::GuildMemberAddition { new_member } => {
            let Some(system_channel) = new_member
                .guild_id
                .to_guild_cached(ctx)
                .and_then(|g| g.system_channel_id)
            else {
                return Ok(());
            };

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
                    "Member Count: #{}",
                    new_member
                        .guild_id
                        .to_guild_cached(ctx)
                        .map_or(0, |g| g.member_count)
                )));

            let message = serenity::CreateMessage::new().embed(welcome_embed);
            let _ = system_channel.send_message(&ctx.http, message).await;
        }
        serenity::FullEvent::Message { new_message } => {
            let targets = data.target_channel_id_list.read().await;

            if targets.contains(&new_message.channel_id)
                && new_message.author.id != ctx.cache.current_user().id
            {
                let _ = data
                    .dc_event_tx
                    .send(FromDiscordEvent {
                        username: new_message.author.name.clone(),
                        content: new_message.content.clone(),
                    })
                    .await;
            }
        }
        _ => {}
    }
    Ok(())
}
