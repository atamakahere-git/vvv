use std::sync::Arc;

use linemux::MuxedLines;
use tokio::sync::Mutex;
use tokio::sync::mpsc;

use bot::types::{BotParams, FromDiscordEvent, FromMinecraftEvent};

mod bot;
mod consts;
mod log_parser;
mod rcon;
mod storage;
#[tokio::main]
async fn main() -> Result<(), bot::BotError> {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_thread_ids(false)
        .with_line_number(true)
        .with_timer(tracing_subscriber::fmt::time::LocalTime::rfc_3339())
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    tracing::info!("starting Ruze bridge...");

    let config =
        consts::Config::load().inspect_err(|e| tracing::error!("configuration error: {e}"))?;

    let (mc_event_tx, mc_event_rx) = mpsc::channel::<FromMinecraftEvent>(32);
    let (dc_event_tx, mut dc_event_rx) = mpsc::channel::<FromDiscordEvent>(32);

    let log_path = config.log.path.clone();

    tokio::spawn(async move {
        let mut lines_ok = match MuxedLines::new() {
            Ok(lines) => lines,
            Err(e) => {
                tracing::error!("failed to initialize log watcher: {e:?}");
                return;
            }
        };

        tracing::info!(path = %log_path, "log watcher started");

        if let Err(why) = lines_ok.add_file(log_path).await {
            tracing::warn!("failed to add log file: {why:?}");
        }

        while let Ok(Some(line)) = lines_ok.next_line().await {
            if let Some(event) = log_parser::parse_log_line(line.line()) {
                let Some(discord_payload) = event.into_discord() else {
                    continue;
                };
                tracing::info!(
                    username = %discord_payload.username,
                    "mc→dc"
                );
                if let Err(why) = mc_event_tx.send(discord_payload).await {
                    tracing::warn!("mc→dc channel send failed: {why:?}");
                }
            }
        }
    });

    let rcon_client = rcon::connect(&config.rcon.address, &config.rcon.password)?;
    let shared_rcon = Arc::new(Mutex::new(rcon_client));
    let rcon_clone = Arc::clone(&shared_rcon);

    tokio::spawn(async move {
        tracing::info!("Discord → Minecraft relay started");

        while let Some(event) = dc_event_rx.recv().await {
            let safe_username: String = event
                .username
                .chars()
                .map(|c| match c {
                    '"' | '\\' => ' ',
                    _ => c,
                })
                .collect();
            let safe_content: String = event
                .content
                .chars()
                .map(|c| match c {
                    '"' | '\\' => ' ',
                    _ => c,
                })
                .collect();
            let formatted_command = format!(
                r#"tellraw @a {{"text":"[Discord] <{safe_username}>: {safe_content}", "color":"gold"}}"#
            );
            let guard = rcon_clone.lock().await;
            if let Err(why) = guard.send_command(&formatted_command) {
                tracing::warn!(
                    username = %event.username,
                    error = %why,
                    "dc→mc send failed"
                );
            } else {
                tracing::info!(
                    username = %event.username,
                    "dc→mc"
                );
            }
        }
    });

    tracing::info!("bridge is now running");

    let db_path = consts::resolve_db_path(&config);
    let storage = Arc::new(
        storage::Storage::open(db_path, config.minecraft.server_address.clone())
            .inspect_err(|e| tracing::error!("failed to open storage: {e}"))?,
    );

    let bot_params = BotParams {
        token: config.discord.token,
        owner_id: config.bot.owner_id,
        guild_id: config.bot.guild_id,
    };

    bot::handler::start_bot(
        bot_params,
        parse_mc_address(&config.minecraft.server_address),
        mc_event_rx,
        dc_event_tx,
        shared_rcon,
        storage,
    )
    .await?;

    Ok(())
}

fn parse_mc_address(raw: &str) -> url::Url {
    if raw.contains("://") {
        raw.parse().unwrap_or_else(|e| {
            tracing::error!(%raw, %e, "invalid minecraft server URL, using default");
            "mc://localhost:25565".parse().expect("hardcoded URL")
        })
    } else {
        format!("mc://{raw}").parse().unwrap_or_else(|e| {
            tracing::error!(%raw, %e, "invalid minecraft server address, using default");
            "mc://localhost:25565".parse().expect("hardcoded URL")
        })
    }
}
