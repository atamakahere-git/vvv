use std::path::PathBuf;

use serde::Deserialize;

fn default_rcon_address() -> String {
    "localhost:25575".to_string()
}

fn default_mc_server_address() -> String {
    "localhost:25565".to_string()
}

/// Central application configuration.
///
/// Values are resolved from (lowest to highest priority):
/// 1. `/etc/ruze.toml` — system-wide defaults
/// 2. `$XDG_CONFIG_HOME/ruze.toml` — per-user config (falls back to `~/.config/ruze.toml`)
/// 3. `$HOME/.ruze.toml` — per-user home-directory override
/// 4. Environment variables (`RUZE_*` prefix, with deprecated old-name fallback)
#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub struct Config {
    #[serde(rename = "discord")]
    pub discord: DiscordConfig,
    #[serde(rename = "rcon")]
    pub rcon: RconConfig,
    #[serde(rename = "minecraft")]
    pub minecraft: MinecraftConfig,
    #[serde(rename = "bot")]
    pub bot: BotConfig,
    #[serde(rename = "log")]
    pub log: LogConfig,
    #[serde(default, rename = "storage")]
    pub storage: StorageConfig,
}

#[derive(Debug, Deserialize)]
pub struct DiscordConfig {
    pub token: String,
}

#[derive(Debug, Deserialize)]
pub struct RconConfig {
    #[serde(default = "default_rcon_address")]
    pub address: String,
    pub password: String,
}

#[derive(Debug, Deserialize)]
pub struct MinecraftConfig {
    #[serde(default = "default_mc_server_address")]
    pub server_address: String,
}

#[derive(Debug, Deserialize)]
pub struct BotConfig {
    pub owner_id: u64,
    /// If set, slash commands are registered in this guild immediately
    /// (instant sync) in addition to global registration.
    #[serde(default)]
    pub guild_id: Option<u64>,
}

#[derive(Debug, Deserialize, Default)]
pub struct LogConfig {
    pub path: String,
}

/// Optional storage configuration for the redb persistence backend.
#[derive(Debug, Deserialize, Default)]
pub struct StorageConfig {
    /// Optional override for the redb database file path.
    ///
    /// When unset, the database is created at `$XDG_STATE_HOME/ruze/ruze.redb`
    /// (falling back to `~/.local/state/ruze/ruze.redb`).
    #[serde(default)]
    pub database_path: Option<String>,
}

/// Error returned when configuration loading or validation fails.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("required field missing from all config sources: {0}")]
    MissingField(&'static str),
    #[error("failed to read config file `{path}`: {source}")]
    ReadFile {
        path: String,
        source: std::io::Error,
    },
    #[error("invalid TOML in `{path}`: {source}")]
    ParseToml {
        path: String,
        source: toml::de::Error,
    },
}

impl Config {
    /// Load configuration from all sources in priority order.
    ///
    /// # Errors
    ///
    /// Returns `ConfigError` if a file that exists cannot be read or parsed,
    /// or if required fields are missing after merging all sources.
    pub fn load() -> Result<Self, ConfigError> {
        tracing::info!("loading configuration...");

        let mut config = Self::empty();

        for (path, desc) in [
            ("/etc/ruze.toml", "global system config"),
            (&xdg_config_path(), "XDG config"),
            (&home_ruze_path(), "home config"),
        ] {
            match Self::merge_file(&mut config, path) {
                Ok(Some(())) => tracing::debug!(%path, "merged {desc}"),
                Ok(None) => tracing::debug!(%path, "{desc} not found, skipped"),
                Err(e) => return Err(e),
            }
        }

        Self::overlay_env(&mut config);

        config.validate()?;
        tracing::info!("configuration validated");
        Ok(config)
    }

    fn empty() -> Self {
        Self {
            discord: DiscordConfig {
                token: String::new(),
            },
            rcon: RconConfig {
                address: default_rcon_address(),
                password: String::new(),
            },
            minecraft: MinecraftConfig {
                server_address: default_mc_server_address(),
            },
            bot: BotConfig {
                owner_id: 0,
                guild_id: None,
            },
            log: LogConfig {
                path: String::new(),
            },
            storage: StorageConfig::default(),
        }
    }

    /// Returns `Ok(Some(()))` if the file existed and was merged,
    /// `Ok(None)` if the file doesn't exist (skipped),
    /// `Err(...)` on read or parse errors.
    fn merge_file(config: &mut Self, path: &str) -> Result<Option<()>, ConfigError> {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(ConfigError::ReadFile {
                    path: path.to_string(),
                    source: e,
                });
            }
        };

        let partial: Self = toml::from_str(&content).map_err(|e| ConfigError::ParseToml {
            path: path.to_string(),
            source: e,
        })?;

        Self::merge_into(config, &partial);
        Ok(Some(()))
    }

    fn merge_into(target: &mut Self, source: &Self) {
        if !source.discord.token.is_empty() {
            target.discord.token.clone_from(&source.discord.token);
        }
        if source.log.path != target.log.path || !source.log.path.is_empty() {
            target.log.path.clone_from(&source.log.path);
        }
        if source.rcon.address != default_rcon_address() || !source.rcon.address.is_empty() {
            target.rcon.address.clone_from(&source.rcon.address);
        }
        if !source.rcon.password.is_empty() {
            target.rcon.password.clone_from(&source.rcon.password);
        }
        if source.minecraft.server_address != default_mc_server_address()
            || !source.minecraft.server_address.is_empty()
        {
            target
                .minecraft
                .server_address
                .clone_from(&source.minecraft.server_address);
        }
        if source.bot.owner_id != 0 {
            target.bot.owner_id = source.bot.owner_id;
        }
        if source.bot.guild_id.is_some() {
            target.bot.guild_id = source.bot.guild_id;
        }
        if source.storage.database_path.is_some() {
            target.storage.database_path.clone_from(&source.storage.database_path);
        }
    }

    fn overlay_env(config: &mut Self) {
        // New env vars (RUZE_ prefix) — highest priority
        if let Ok(v) = std::env::var("RUZE_DISCORD_TOKEN") {
            config.discord.token = v;
        } else if let Ok(v) = std::env::var("DISCORD_TOKEN") {
            tracing::warn!("DISCORD_TOKEN is deprecated; use RUZE_DISCORD_TOKEN");
            config.discord.token = v;
        }

        if let Ok(v) = std::env::var("RUZE_LOG_PATH") {
            config.log.path = v;
        } else if let Ok(v) = std::env::var("LOG_PATH") {
            tracing::warn!("LOG_PATH is deprecated; use RUZE_LOG_PATH");
            config.log.path = v;
        }

        if let Ok(v) = std::env::var("RUZE_RCON_ADDRESS") {
            config.rcon.address = v;
        } else if let Ok(v) = std::env::var("RCON_SERVER_ADDRESS") {
            tracing::warn!("RCON_SERVER_ADDRESS is deprecated; use RUZE_RCON_ADDRESS");
            config.rcon.address = v;
        }

        if let Ok(v) = std::env::var("RUZE_RCON_PASSWORD") {
            config.rcon.password = v;
        } else if let Ok(v) = std::env::var("RCON_PASSWORD") {
            tracing::warn!("RCON_PASSWORD is deprecated; use RUZE_RCON_PASSWORD");
            config.rcon.password = v;
        }

        if let Ok(v) = std::env::var("RUZE_MC_SERVER_ADDRESS") {
            config.minecraft.server_address = v;
        } else if let Ok(v) = std::env::var("MC_SERVER_QUERY_ADDRESS") {
            tracing::warn!("MC_SERVER_QUERY_ADDRESS is deprecated; use RUZE_MC_SERVER_ADDRESS");
            config.minecraft.server_address = v;
        }

        if let Ok(v) = std::env::var("RUZE_OWNER_ID") {
            if let Ok(id) = v.parse::<u64>() {
                config.bot.owner_id = id;
            } else {
                tracing::error!("RUZE_OWNER_ID is not a valid u64: {v}");
            }
        }

        if let Ok(v) = std::env::var("RUZE_GUILD_ID") {
            if let Ok(id) = v.parse::<u64>() {
                config.bot.guild_id = Some(id);
            } else {
                tracing::error!("RUZE_GUILD_ID is not a valid u64: {v}");
            }
        }

        if let Ok(v) = std::env::var("RUZE_DATABASE_PATH") {
            config.storage.database_path = Some(v);
        }
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.discord.token.is_empty() {
            return Err(ConfigError::MissingField(
                "discord.token / RUZE_DISCORD_TOKEN",
            ));
        }
        if self.log.path.is_empty() {
            return Err(ConfigError::MissingField("log.path / RUZE_LOG_PATH"));
        }
        if self.rcon.password.is_empty() {
            return Err(ConfigError::MissingField(
                "rcon.password / RUZE_RCON_PASSWORD",
            ));
        }
        if self.bot.owner_id == 0 {
            return Err(ConfigError::MissingField("bot.owner_id / RUZE_OWNER_ID"));
        }
        Ok(())
    }
}

fn home_dir() -> PathBuf {
    std::env::var("HOME").map_or_else(|_| PathBuf::from("/home"), PathBuf::from)
}

fn xdg_config_home() -> PathBuf {
    std::env::var("XDG_CONFIG_HOME").map_or_else(|_| home_dir().join(".config"), PathBuf::from)
}

fn xdg_state_home() -> PathBuf {
    std::env::var("XDG_STATE_HOME")
        .map_or_else(|_| home_dir().join(".local").join("state"), PathBuf::from)
}

fn xdg_config_path() -> String {
    xdg_config_home()
        .join("ruze.toml")
        .to_string_lossy()
        .to_string()
}

fn home_ruze_path() -> String {
    home_dir().join(".ruze.toml").to_string_lossy().to_string()
}

/// Default redb database location: `$XDG_STATE_HOME/ruze/ruze.redb`.
pub fn default_db_path() -> PathBuf {
    xdg_state_home().join("ruze").join("ruze.redb")
}

/// Resolve the redb database path from (highest → lowest priority):
/// 1. `RUZE_DATABASE_PATH` env var (already overlaid onto `config.storage.database_path`)
/// 2. `[storage] database_path` from config
/// 3. `$XDG_STATE_HOME/ruze/ruze.redb` default
pub fn resolve_db_path(config: &Config) -> PathBuf {
    config
        .storage
        .database_path
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(default_db_path)
}
