use std::sync::Arc;

use mc_rcon::RconClient;
use poise::serenity_prelude as serenity;
use rust_mc_status::McClient;
use tokio::sync::{Mutex, RwLock, mpsc::Sender};

use crate::log_parser::{self, MinecraftEvent};

/// Event forwarded from Minecraft to Discord.
#[derive(Debug, Clone)]
pub struct FromMinecraftEvent {
    pub username: String,
    pub content: String,
}

/// Event forwarded from Discord to Minecraft.
#[derive(Debug, Clone)]
pub struct FromDiscordEvent {
    pub username: String,
    pub content: String,
}

/// Shared state injected into every bot command and event handler.
#[derive(Clone)]
pub struct Data {
    pub dc_event_tx: Sender<FromDiscordEvent>,
    pub mc_status_client: McClient,
    pub target_channel_id_list: Arc<RwLock<Vec<serenity::ChannelId>>>,
    pub rcon_client: Arc<Mutex<RconClient>>,
    pub mc_server_address: String,
}

impl From<MinecraftEvent> for FromMinecraftEvent {
    fn from(event: MinecraftEvent) -> Self {
        match event {
            MinecraftEvent::Chat { username, message } => Self {
                username,
                content: message,
            },
            MinecraftEvent::Death { system_message } => {
                let bold_msg = log_parser::bold_first_word(&system_message);
                Self {
                    username: "⚰️".to_string(),
                    content: bold_msg,
                }
            }
            MinecraftEvent::Advancement { system_message } => {
                let bold_msg = log_parser::bold_first_word(&system_message);
                Self {
                    username: "🏆".to_string(),
                    content: bold_msg,
                }
            }
            MinecraftEvent::PlayerJoinLeave {
                system_message,
                is_join,
            } => {
                let icon = if is_join { "🟢 " } else { "🔴 " };
                let bold_msg = log_parser::bold_first_word(&system_message);
                Self {
                    username: icon.to_string(),
                    content: bold_msg,
                }
            }
        }
    }
}
