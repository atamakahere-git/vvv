use std::sync::Arc;

use mc_rcon::RconClient;
use poise::serenity_prelude as serenity;
use rust_mc_status::McClient;
use tokio::sync::{Mutex, RwLock, mpsc::Sender};

use crate::log_parser::MinecraftEvent;

#[derive(Debug, Clone)]
pub struct FromMinecraftEvent {
    pub username: String,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct FromDiscordEvent {
    pub username: String,
    pub content: String,
}

/// Bot configuration fields that are consumed once during startup.
pub struct BotParams {
    pub token: String,
    pub owner_id: u64,
    pub guild_id: Option<u64>,
}

#[derive(Clone)]
pub struct Data {
    pub dc_event_tx: Sender<FromDiscordEvent>,
    pub mc_status_client: McClient,
    pub target_channel_id_list: Arc<RwLock<Vec<serenity::ChannelId>>>,
    pub rcon_client: Arc<Mutex<RconClient>>,
    pub mc_server_address: url::Url,
}

impl MinecraftEvent {
    #[must_use]
    pub fn into_discord(self) -> Option<FromMinecraftEvent> {
        match self {
            Self::Chat { username, message } => Some(FromMinecraftEvent {
                username,
                content: message,
            }),
            Self::Join { username } => Some(into_bridge_event(
                "🟢",
                &format!("**{username}** joined the game"),
            )),
            Self::Leave { username } => Some(into_bridge_event(
                "🔴",
                &format!("**{username}** left the game"),
            )),
            Self::Disconnect { username, reason } => Some(into_bridge_event(
                "🔴",
                &format!("**{username}** left the game ({reason})"),
            )),
            Self::Death { username, message } => Some(into_bridge_event(
                "⚰️",
                &format!("**{username}** {message}"),
            )),
            Self::Advancement {
                username,
                advancement,
            } => Some(into_bridge_event(
                "🏆",
                &format!("**{username}** has made the advancement [{advancement}]"),
            )),
            Self::Command { username, command } => Some(into_bridge_event(
                "⌨️",
                &format!("**{username}** used command: `{command}`"),
            )),
            Self::ServerSay { message } => {
                Some(into_bridge_event("📢", &format!("[Server] {message}")))
            }
            Self::ServerStart => Some(into_bridge_event("🚀", "Server started!")),
            Self::ServerStop => Some(into_bridge_event("🛑", "Server stopped!")),
            Self::SaveComplete => Some(into_bridge_event("💾", "World saved!")),
            Self::PlayerList { .. } => None,
        }
    }
}

fn into_bridge_event(username: &str, content: &str) -> FromMinecraftEvent {
    FromMinecraftEvent {
        username: username.to_owned(),
        content: content.to_owned(),
    }
}
