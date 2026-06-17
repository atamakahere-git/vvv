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

#[derive(Clone)]
pub struct Data {
    pub dc_event_tx: Sender<FromDiscordEvent>,
    pub mc_status_client: McClient,
    pub target_channel_id_list: Arc<RwLock<Vec<serenity::ChannelId>>>,
    pub rcon_client: Arc<Mutex<RconClient>>,
    pub mc_server_address: String,
}

impl MinecraftEvent {
    #[must_use]
    pub fn into_discord(self) -> Option<FromMinecraftEvent> {
        match self {
            Self::Chat { username, message } => Some(FromMinecraftEvent {
                username,
                content: message,
            }),
            Self::Join { username } => Some(pad_to_discord(
                "🟢",
                &format!("**{username}** joined the game"),
            )),
            Self::Leave { username } => Some(pad_to_discord(
                "🔴",
                &format!("**{username}** left the game"),
            )),
            Self::Disconnect { username, reason } => Some(pad_to_discord(
                "🔴",
                &format!("**{username}** lost connection: {reason}"),
            )),
            Self::Death { username, message } => {
                Some(pad_to_discord("⚰️", &format!("**{username}** {message}")))
            }
            Self::Advancement {
                username,
                advancement,
            } => Some(pad_to_discord(
                "🏆",
                &format!("**{username}** has made the advancement [{advancement}]"),
            )),
            Self::Command { username, command } => Some(pad_to_discord(
                "⌨️",
                &format!("**{username}** used command: `{command}`"),
            )),
            Self::ServerSay { message } => {
                Some(pad_to_discord("📢", &format!("[Server] {message}")))
            }
            Self::ServerStart => Some(pad_to_discord("🚀", "Server started!")),
            Self::ServerStop => Some(pad_to_discord("🛑", "Server stopped!")),
            Self::SaveComplete => Some(pad_to_discord("💾", "World saved!")),
            Self::PlayerList { .. } => None,
        }
    }
}

fn pad_to_discord(username: &str, content: &str) -> FromMinecraftEvent {
    FromMinecraftEvent {
        username: username.to_owned(),
        content: content.to_owned(),
    }
}
