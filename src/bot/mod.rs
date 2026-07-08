//! Discord bot module — bridges Minecraft server chat with a Discord guild.
//!
//! Handles bot startup, event routing, slash/prefix commands, and shared
//! state management.

pub mod commands;
pub mod handler;
pub mod types;

/// Bot-specific error covering all failure modes of the Discord bridge.
#[derive(Debug, thiserror::Error)]
pub enum BotError {
    #[error("discord error: {0}")]
    Serenity(Box<poise::serenity_prelude::Error>),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("configuration error: {0}")]
    Config(#[from] crate::consts::ConfigError),
    #[error("rcon error: {0}")]
    Rcon(#[from] crate::rcon::RconError),
    #[error("storage error: {0}")]
    Storage(#[from] crate::storage::StorageError),
}

impl From<poise::serenity_prelude::Error> for BotError {
    fn from(e: poise::serenity_prelude::Error) -> Self {
        Self::Serenity(Box::new(e))
    }
}

pub(crate) type Context<'a> = poise::Context<'a, types::Data, BotError>;
