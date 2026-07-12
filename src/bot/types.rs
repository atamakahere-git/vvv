use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use poise::serenity_prelude as serenity;
use rust_mc_status::McClient;
use tokio::sync::{Mutex, RwLock, mpsc::Sender};

use crate::log_parser::MinecraftEvent;
use crate::rcon::ReconnectingRcon;
use crate::storage::Storage;

#[derive(Debug, Clone)]
pub struct FromMinecraftEvent {
    pub username: String,
    pub content: String,
    pub mc_username: String,
}

#[derive(Debug, Clone)]
pub struct FromDiscordEvent {
    pub username: String,
    pub content: String,
}

#[derive(Debug)]
pub struct PendingVerification {
    pub discord_user_id: u64,
    pub mc_username: String,
    pub expires_at: Instant,
    pub attempts: u32,
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
    pub bridge_channel: Arc<RwLock<Option<serenity::ChannelId>>>,
    pub storage: Arc<Storage>,
    pub rcon_client: Arc<ReconnectingRcon>,
    pub mc_server_address: url::Url,
    pub pending_verifications: Arc<Mutex<HashMap<String, PendingVerification>>>,
}

impl MinecraftEvent {
    #[must_use]
    pub fn into_discord(self) -> Option<FromMinecraftEvent> {
        match self {
            Self::Chat { username, message } => Some(FromMinecraftEvent {
                mc_username: username.clone(),
                username,
                content: message,
            }),
            Self::Join { username } => Some(into_bridge_event(
                "🟢",
                &format!("**{username}** joined the game"),
                &username,
            )),
            Self::Leave { username } => Some(into_bridge_event(
                "🔴",
                &format!("**{username}** left the game"),
                &username,
            )),
            Self::Disconnect { username, reason } => Some(into_bridge_event(
                "🔴",
                &format!("**{username}** left the game ({reason})"),
                &username,
            )),
            Self::Death { username, message } => Some(into_bridge_event(
                "⚰️",
                &format!("**{username}** {message}"),
                &username,
            )),
            Self::Advancement {
                username,
                advancement,
            } => Some(into_bridge_event(
                "🏆",
                &format!("**{username}** has made the advancement [{advancement}]"),
                &username,
            )),
            Self::Command { username, command } => Some(into_bridge_event(
                "⌨️",
                &format!("**{username}** used command: `{command}`"),
                &username,
            )),
            Self::ServerSay { message } => {
                Some(into_bridge_event("📢", &format!("[Server] {message}"), ""))
            }
            Self::ServerStart => Some(into_bridge_event("🚀", "Server started!", "")),
            Self::ServerStop => Some(into_bridge_event("🛑", "Server stopped!", "")),
            Self::SaveComplete => Some(into_bridge_event("💾", "World saved!", "")),
            Self::PlayerList { .. } | Self::UuidResolved { .. } => None,
        }
    }
}

fn into_bridge_event(username: &str, content: &str, mc_username: &str) -> FromMinecraftEvent {
    FromMinecraftEvent {
        username: username.to_owned(),
        content: content.to_owned(),
        mc_username: mc_username.to_owned(),
    }
}
