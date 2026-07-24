use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use poise::serenity_prelude as serenity;
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use tokio::sync::RwLock;

/// Single-row bridge binding table.
///
/// Key is the constant `"current"`; value is the tuple
/// `(channel_id, guild_id, mc_server_address)`.
const BRIDGE: TableDefinition<&str, (u64, u64, String)> = TableDefinition::new("bridge");

/// Sentinel key for the single bridge row.
const CURRENT: &str = "current";

/// username → UUID mapping (populated from "UUID of player" log lines).
pub const USERNAME_UUID: TableDefinition<String, String> = TableDefinition::new("username_uuid");

/// UUID → cumulative player stats.
///
/// Value tuple: `(total_play_time_secs, first_login_ts, last_login_ts,
/// last_logout_ts, total_logins, total_deaths, total_advancements,
/// total_messages, total_commands)`.
pub const PLAYERS: TableDefinition<String, PlayerStatsTuple> = TableDefinition::new("players");

/// Tuple type for the `PLAYERS` table value, factored out for readability.
type PlayerStatsTuple = (u64, i64, i64, i64, u64, u64, u64, u64, u64);

/// Discord user ID → Minecraft username mapping for account connections.
const DC_TO_MC: TableDefinition<u64, String> = TableDefinition::new("dc_to_mc");

/// Minecraft username → Discord user ID reverse mapping.
const MC_TO_DC: TableDefinition<String, u64> = TableDefinition::new("mc_to_dc");

/// Set of Discord user IDs who opted out of join/leave announcements.
const JOIN_LEAVE_OPTOUT: TableDefinition<u64, bool> = TableDefinition::new("join_leave_optout");

/// Set of Discord user IDs who muted cross-chat mentions.
const MUTE_MENTION: TableDefinition<u64, bool> = TableDefinition::new("mute_mention");

/// Discord user IDs who are muted from sending Discord→MC bridge messages.
///
/// Key: `discord_id`. Value: expiry timestamp in seconds (0 = permanent).
const MUTED_USERS: TableDefinition<u64, u64> = TableDefinition::new("muted_users");

/// Key-value settings table (e.g. `privacy_enabled`).
const SETTINGS: TableDefinition<&str, bool> = TableDefinition::new("settings");
const PRIVACY_ENABLED_KEY: &str = "privacy_enabled";
const PLAYER_PROFILE_ENABLED_KEY: &str = "player_profile_enabled";

/// (UUID, "YYYY-MM-DD") → seconds played that day (in configured timezone).
pub const DAILY_PLAY_TIME: TableDefinition<(String, String), u64> =
    TableDefinition::new("daily_play_time");

/// Cumulative player stats read from the `PLAYERS` table.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlayerStats {
    pub total_play_time_secs: u64,
    pub first_login_ts: i64,
    pub last_login_ts: i64,
    pub last_logout_ts: i64,
    pub total_logins: u64,
    pub total_deaths: u64,
    pub total_advancements: u64,
    pub total_messages: u64,
    pub total_commands: u64,
}

impl PlayerStats {
    fn from_tuple(v: PlayerStatsTuple) -> Self {
        Self {
            total_play_time_secs: v.0,
            first_login_ts: v.1,
            last_login_ts: v.2,
            last_logout_ts: v.3,
            total_logins: v.4,
            total_deaths: v.5,
            total_advancements: v.6,
            total_messages: v.7,
            total_commands: v.8,
        }
    }

    fn to_tuple(&self) -> PlayerStatsTuple {
        (
            self.total_play_time_secs,
            self.first_login_ts,
            self.last_login_ts,
            self.last_logout_ts,
            self.total_logins,
            self.total_deaths,
            self.total_advancements,
            self.total_messages,
            self.total_commands,
        )
    }
}

/// Per-player delta accumulated in memory between flushes.
#[derive(Debug, Clone, Default)]
pub struct PlayerDelta {
    pub messages: u64,
    pub commands: u64,
    pub deaths: u64,
    pub advancements: u64,
    pub play_time_secs: u64,
    pub last_login_ts: Option<i64>,
    pub last_logout_ts: Option<i64>,
    pub is_new_player: bool,
    pub total_logins_delta: u64,
}

/// Daily play-time split for a flush operation.
#[derive(Debug, Clone)]
pub struct DailyTimeSplit {
    pub date: String,
    pub secs: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("redb database error: {0}")]
    RedbDatabase(#[from] redb::DatabaseError),
    #[error("redb transaction error: {0}")]
    RedbTransaction(#[from] redb::TransactionError),
    #[error("redb table error: {0}")]
    RedbTable(#[from] redb::TableError),
    #[error("redb storage error: {0}")]
    RedbStorage(#[from] redb::StorageError),
    #[error("redb commit error: {0}")]
    RedbCommit(#[from] redb::CommitError),
    #[error("redb durability error: {0}")]
    RedbDurability(#[from] redb::SetDurabilityError),
    #[error("I/O error at `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("storage worker panicked: {0}")]
    BlockingPanic(String),
    #[error("minecraft username already claimed by another discord user")]
    AlreadyClaimed,
}

/// redb-backed persistence for the bridge binding and player stats.
///
/// Holds a long-lived `Arc<Database>` (`Send + Sync`) opened once at startup.
/// Each transaction runs inside `spawn_blocking` because redb transactions are
/// not `Send`.
///
/// Small lookup tables (account mappings, opt-out preferences, mute preferences)
/// are mirrored in-memory behind `RwLock`s for zero-overhead hot-path reads.
/// Writes flush to both in-memory and redb atomically.
#[derive(Debug, Clone)]
pub struct Storage {
    db: Arc<Database>,
    mc_server_address: String,
    /// Parallel sorted vectors: `mc_usernames` (sorted) and `discord_ids` (same index).
    /// MC→DC lookup via binary search on `mc_usernames`.
    account_cache: Arc<RwLock<(Vec<String>, Vec<u64>)>>,
    /// Discord user IDs that opted out of join/leave announcements.
    join_leave_optout: Arc<RwLock<HashSet<u64>>>,
    /// Discord user IDs that muted cross-chat mentions.
    mute_mention: Arc<RwLock<HashSet<u64>>>,
    /// Discord user IDs muted from sending to bridge (`discord_id` → `expiry_ts`).
    /// 0 = permanent mute.
    muted_users: Arc<RwLock<HashMap<u64, u64>>>,
    /// Global privacy feature toggle (admin-controlled).
    privacy_enabled: Arc<RwLock<bool>>,
    /// Player profile dashboard toggle (admin-controlled).
    player_profile_enabled: Arc<RwLock<bool>>,
}

impl Storage {
    /// Open (or create) the redb database at `path`, storing the MC server
    /// address for future row values.
    ///
    /// # Errors
    ///
    /// Returns `StorageError` if the parent directory cannot be created or the
    /// database cannot be opened.
    pub fn open(path: &Path, mc_server_address: String) -> Result<Self, StorageError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| StorageError::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }

        tracing::debug!(path = %path.display(), "opening redb database");
        let db = Database::create(path)?;

        let (mc_usernames, discord_ids) = load_account_mappings(&db);
        let join_leave = load_optout_set(&db, JOIN_LEAVE_OPTOUT);
        let mute = load_optout_set(&db, MUTE_MENTION);
        let muted = load_muted_users(&db);
        let privacy = load_privacy_enabled(&db);
        let profile_toggle = load_player_profile_enabled(&db);

        Ok(Self {
            db: Arc::new(db),
            mc_server_address,
            account_cache: Arc::new(RwLock::new((mc_usernames, discord_ids))),
            join_leave_optout: Arc::new(RwLock::new(join_leave)),
            mute_mention: Arc::new(RwLock::new(mute)),
            muted_users: Arc::new(RwLock::new(muted)),
            privacy_enabled: Arc::new(RwLock::new(privacy)),
            player_profile_enabled: Arc::new(RwLock::new(profile_toggle)),
        })
    }

    /// Read the currently bridged Discord channel, if any.
    ///
    /// # Errors
    ///
    /// Returns `StorageError` on any redb or I/O failure. An absent row (fresh
    /// database) returns `Ok(None)` without error.
    pub async fn get_bridge_channel(&self) -> Result<Option<serenity::ChannelId>, StorageError> {
        let db = Arc::clone(&self.db);
        let result = tokio::task::spawn_blocking(move || read_bridge(&db)).await;
        match result {
            Ok(Ok(channel_id)) => {
                tracing::debug!(channel = ?channel_id, "loaded bridge binding from redb");
                Ok(channel_id.map(serenity::ChannelId::new))
            }
            Ok(Err(e)) => {
                tracing::warn!(%e, "failed to read bridge binding, starting fresh");
                Ok(None)
            }
            Err(join_err) => Err(StorageError::BlockingPanic(join_err.to_string())),
        }
    }

    /// Persist the bridge binding to `channel_id` (overwrites any prior row).
    ///
    /// # Errors
    ///
    /// Returns `StorageError` on any redb or I/O failure.
    pub async fn set_bridge_channel(
        &self,
        channel_id: u64,
        guild_id: u64,
    ) -> Result<(), StorageError> {
        let mc_server_address = self.mc_server_address.clone();
        let db = Arc::clone(&self.db);
        let result = tokio::task::spawn_blocking(move || {
            write_bridge(&db, channel_id, guild_id, &mc_server_address)
        })
        .await;
        match result {
            Ok(Ok(())) => {
                tracing::debug!(
                    channel = channel_id,
                    guild = guild_id,
                    "bridge binding saved"
                );
                Ok(())
            }
            Ok(Err(e)) => {
                tracing::error!(%e, "failed to save bridge binding");
                Err(e)
            }
            Err(join_err) => Err(StorageError::BlockingPanic(join_err.to_string())),
        }
    }

    /// Remove the bridge binding (no-op if absent).
    ///
    /// # Errors
    ///
    /// Returns `StorageError` on any redb or I/O failure.
    pub async fn clear_bridge_channel(&self) -> Result<(), StorageError> {
        let db = Arc::clone(&self.db);
        let result = tokio::task::spawn_blocking(move || remove_bridge(&db)).await;
        match result {
            Ok(Ok(())) => {
                tracing::debug!("bridge binding cleared");
                Ok(())
            }
            Ok(Err(e)) => {
                tracing::error!(%e, "failed to clear bridge binding");
                Err(e)
            }
            Err(join_err) => Err(StorageError::BlockingPanic(join_err.to_string())),
        }
    }

    /// Resolve a username to its UUID, checking the mapping table first.
    ///
    /// # Errors
    ///
    /// Returns `StorageError` on redb failure.
    #[allow(dead_code)]
    pub async fn resolve_uuid(&self, username: String) -> Result<Option<String>, StorageError> {
        let db = Arc::clone(&self.db);
        let result = tokio::task::spawn_blocking(move || read_uuid_mapping(&db, &username)).await;
        match result {
            Ok(Ok(uuid)) => Ok(uuid),
            Ok(Err(e)) => {
                tracing::warn!(%e, "failed to resolve uuid");
                Ok(None)
            }
            Err(join_err) => Err(StorageError::BlockingPanic(join_err.to_string())),
        }
    }

    /// Store a username → UUID mapping.
    ///
    /// # Errors
    ///
    /// Returns `StorageError` on redb failure.
    pub async fn store_uuid_mapping(
        &self,
        username: String,
        uuid: String,
    ) -> Result<(), StorageError> {
        let db = Arc::clone(&self.db);
        let result =
            tokio::task::spawn_blocking(move || write_uuid_mapping(&db, &username, &uuid)).await;
        match result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => {
                tracing::error!(%e, "failed to store uuid mapping");
                Err(e)
            }
            Err(join_err) => Err(StorageError::BlockingPanic(join_err.to_string())),
        }
    }

    /// Read cumulative stats for a player (by UUID).
    ///
    /// # Errors
    ///
    /// Returns `StorageError` on redb failure.
    pub async fn get_player_stats(
        &self,
        uuid: String,
    ) -> Result<Option<PlayerStats>, StorageError> {
        let db = Arc::clone(&self.db);
        let result = tokio::task::spawn_blocking(move || read_player_stats(&db, &uuid)).await;
        match result {
            Ok(Ok(stats)) => Ok(stats),
            Ok(Err(e)) => {
                tracing::warn!(%e, "failed to read player stats");
                Ok(None)
            }
            Err(join_err) => Err(StorageError::BlockingPanic(join_err.to_string())),
        }
    }

    /// Read daily play time for a specific (UUID, date) pair.
    ///
    /// # Errors
    ///
    /// Returns `StorageError` on redb failure.
    pub async fn get_daily_play_time(
        &self,
        uuid: String,
        date: String,
    ) -> Result<u64, StorageError> {
        let db = Arc::clone(&self.db);
        let result =
            tokio::task::spawn_blocking(move || read_daily_play_time(&db, &uuid, &date)).await;
        match result {
            Ok(Ok(secs)) => Ok(secs),
            Ok(Err(e)) => {
                tracing::warn!(%e, "failed to read daily play time");
                Ok(0)
            }
            Err(join_err) => Err(StorageError::BlockingPanic(join_err.to_string())),
        }
    }

    /// Read play time for the last N days (by UUID), returning (date, secs) pairs.
    ///
    /// # Errors
    ///
    /// Returns `StorageError` on redb failure.
    #[allow(dead_code)]
    pub async fn get_recent_play_time(
        &self,
        uuid: String,
        dates: Vec<String>,
    ) -> Result<Vec<(String, u64)>, StorageError> {
        let db = Arc::clone(&self.db);
        let result =
            tokio::task::spawn_blocking(move || read_recent_play_time(&db, &uuid, &dates)).await;
        match result {
            Ok(Ok(data)) => Ok(data),
            Ok(Err(e)) => {
                tracing::warn!(%e, "failed to read recent play time");
                Ok(Vec::new())
            }
            Err(join_err) => Err(StorageError::BlockingPanic(join_err.to_string())),
        }
    }

    /// Flush accumulated player deltas to redb in a single transaction.
    ///
    /// `durability` controls fsync behavior: use `Durability::None` for
    /// periodic flushes, `Durability::Immediate` for critical events.
    ///
    /// # Errors
    ///
    /// Returns `StorageError` on redb failure.
    pub async fn flush_player_deltas(
        &self,
        deltas: Vec<(String, PlayerDelta)>,
        daily_splits: Vec<(String, DailyTimeSplit)>,
        durability: redb::Durability,
    ) -> Result<(), StorageError> {
        let player_count = deltas.len();
        let daily_count = daily_splits.len();
        let db = Arc::clone(&self.db);
        let result = tokio::task::spawn_blocking(move || {
            write_player_deltas(&db, &deltas, &daily_splits, durability)
        })
        .await;
        match result {
            Ok(Ok(())) => {
                tracing::debug!(
                    players = player_count,
                    daily_entries = daily_count,
                    "player deltas flushed"
                );
                Ok(())
            }
            Ok(Err(e)) => {
                tracing::error!(%e, "failed to flush player deltas");
                Err(e)
            }
            Err(join_err) => Err(StorageError::BlockingPanic(join_err.to_string())),
        }
    }

    /// Read all players' stats for leaderboard queries.
    ///
    /// Returns `(uuid, username, stats)` tuples sorted by total play time descending.
    ///
    /// # Errors
    ///
    /// Returns `StorageError` on redb failure.
    #[allow(dead_code)]
    pub async fn get_all_player_stats(&self) -> Result<Vec<(String, PlayerStats)>, StorageError> {
        let db = Arc::clone(&self.db);
        let result = tokio::task::spawn_blocking(move || read_all_player_stats(&db)).await;
        match result {
            Ok(Ok(data)) => Ok(data),
            Ok(Err(e)) => {
                tracing::warn!(%e, "failed to read all player stats");
                Ok(Vec::new())
            }
            Err(join_err) => Err(StorageError::BlockingPanic(join_err.to_string())),
        }
    }

    /// Check whether privacy features (connection gating, /unsub filtering,
    /// mention processing) are globally enabled.
    ///
    /// In-memory only — read on every hot-path event.
    pub async fn is_privacy_enabled(&self) -> bool {
        *self.privacy_enabled.read().await
    }

    /// Enable or disable all privacy features globally.
    ///
    /// Writes to both in-memory and redb.
    ///
    /// # Errors
    ///
    /// Returns `StorageError` on persistence failure.
    pub async fn set_privacy_enabled(&self, enabled: bool) -> Result<(), StorageError> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || write_privacy_setting(&db, enabled))
            .await
            .map_err(|e| StorageError::BlockingPanic(e.to_string()))??;

        *self.privacy_enabled.write().await = enabled;
        Ok(())
    }

    /// Check whether the player profile dashboard feature is enabled.
    ///
    /// In-memory only — read on every command invocation.
    pub async fn is_player_profile_enabled(&self) -> bool {
        *self.player_profile_enabled.read().await
    }

    /// Enable or disable the player profile dashboard globally.
    ///
    /// Writes to both in-memory and redb.
    ///
    /// # Errors
    ///
    /// Returns `StorageError` on persistence failure.
    pub async fn set_player_profile_enabled(&self, enabled: bool) -> Result<(), StorageError> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || write_player_profile_setting(&db, enabled))
            .await
            .map_err(|e| StorageError::BlockingPanic(e.to_string()))??;

        *self.player_profile_enabled.write().await = enabled;
        Ok(())
    }

    /// Connect a Discord user ID to a Minecraft username (dual mapping).
    ///
    /// Returns `AlreadyClaimed` if the Minecraft username is already connected to
    /// a different Discord user.  If the Discord user was previously connected to
    /// a different username the old mapping is automatically removed.
    ///
    /// # Errors
    ///
    /// Returns `StorageError` on persistence failure or `AlreadyClaimed` on conflict.
    pub async fn set_connection(
        &self,
        discord_id: u64,
        mc_username: String,
    ) -> Result<(), StorageError> {
        {
            let cache = self.account_cache.read().await;
            if let Ok(idx) = cache.0.binary_search(&mc_username)
                && cache.1[idx] != discord_id
            {
                return Err(StorageError::AlreadyClaimed);
            }
        }

        let db = Arc::clone(&self.db);
        let mc = mc_username.clone();
        tokio::task::spawn_blocking(move || write_account_mapping(&db, discord_id, &mc))
            .await
            .map_err(|e| StorageError::BlockingPanic(e.to_string()))??;

        {
            let mut cache = self.account_cache.write().await;
            if let Some(pos) = cache.1.iter().position(|&id| id == discord_id) {
                let old_mc = cache.0.remove(pos);
                cache.1.remove(pos);
                tracing::info!(discord_id, old_mc = %old_mc, "removed previous mapping before re-connect");
            }
            if cache.0.binary_search(&mc_username).is_err() {
                let insert_pos = cache.0.binary_search(&mc_username).unwrap_err();
                cache.0.insert(insert_pos, mc_username);
                cache.1.insert(insert_pos, discord_id);
                tracing::info!(discord_id, mc_username = %cache.0[insert_pos], "account connected");
            }
        }

        Ok(())
    }

    /// Remove the account connection for a Discord user.
    ///
    /// No-op if the user was not connected.
    ///
    /// # Errors
    ///
    /// Returns `StorageError` on persistence failure.
    pub async fn remove_connection(&self, discord_id: u64) -> Result<(), StorageError> {
        let mc_username: Option<String> = {
            let cache = self.account_cache.read().await;
            cache
                .1
                .iter()
                .position(|&id| id == discord_id)
                .map(|pos| cache.0[pos].clone())
        };

        let Some(mc) = mc_username else {
            return Ok(());
        };

        let db = Arc::clone(&self.db);
        let mc_clone = mc.clone();
        tokio::task::spawn_blocking(move || remove_account_mapping(&db, discord_id, &mc_clone))
            .await
            .map_err(|e| StorageError::BlockingPanic(e.to_string()))??;

        {
            let mut cache = self.account_cache.write().await;
            if let Ok(idx) = cache.0.binary_search(&mc) {
                cache.0.remove(idx);
                cache.1.remove(idx);
                tracing::info!(discord_id, mc_username = %mc, "account disconnected");
            }
        }

        Ok(())
    }

    /// Look up a Discord user ID from a Minecraft username (hot path).
    ///
    /// In-memory only — O(log n) via binary search on the sorted `mc_usernames` vec.
    pub async fn get_dc_from_mc(&self, mc_username: &str) -> Option<u64> {
        let cache = self.account_cache.read().await;
        cache
            .0
            .binary_search_by(|s| s.as_str().cmp(mc_username))
            .ok()
            .map(|idx| cache.1[idx])
    }

    /// Look up a Minecraft username from a Discord user ID (less frequent).
    ///
    /// In-memory only — O(n) linear scan on `discord_ids`.
    pub async fn get_mc_from_dc(&self, discord_id: u64) -> Option<String> {
        let cache = self.account_cache.read().await;
        cache
            .1
            .iter()
            .position(|&id| id == discord_id)
            .map(|pos| cache.0[pos].clone())
    }

    /// Check whether a Discord user ID has any connected Minecraft account.
    ///
    /// In-memory only.
    pub async fn is_connected_dc(&self, discord_id: u64) -> bool {
        let cache = self.account_cache.read().await;
        cache.1.contains(&discord_id)
    }

    /// Opt a Discord user in or out of join/leave broadcasting.
    ///
    /// # Errors
    ///
    /// Returns `StorageError` on persistence failure.
    pub async fn set_join_leave_optout(
        &self,
        discord_id: u64,
        opted_out: bool,
    ) -> Result<(), StorageError> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || {
            write_optout_entry(&db, JOIN_LEAVE_OPTOUT, discord_id, opted_out)
        })
        .await
        .map_err(|e| StorageError::BlockingPanic(e.to_string()))??;

        {
            let mut set = self.join_leave_optout.write().await;
            if opted_out {
                set.insert(discord_id);
                tracing::info!(discord_id, "user opted out of join/leave broadcast");
            } else {
                set.remove(&discord_id);
                tracing::info!(discord_id, "user re-subscribed to join/leave broadcast");
            }
        }

        Ok(())
    }

    /// Check whether a Discord user has opted out of join/leave announcements.
    ///
    /// In-memory only — hot path, called on every join/leave event.
    pub async fn is_join_leave_opted_out(&self, discord_id: u64) -> bool {
        self.join_leave_optout.read().await.contains(&discord_id)
    }

    /// Opt a Discord user in or out of cross-chat mention pings.
    ///
    /// # Errors
    ///
    /// Returns `StorageError` on persistence failure.
    pub async fn set_mute_mention(&self, discord_id: u64, muted: bool) -> Result<(), StorageError> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || {
            write_optout_entry(&db, MUTE_MENTION, discord_id, muted)
        })
        .await
        .map_err(|e| StorageError::BlockingPanic(e.to_string()))??;

        {
            let mut set = self.mute_mention.write().await;
            if muted {
                set.insert(discord_id);
                tracing::info!(discord_id, "user muted cross-chat mentions");
            } else {
                set.remove(&discord_id);
                tracing::info!(discord_id, "user un-muted cross-chat mentions");
            }
        }

        Ok(())
    }

    /// Check whether a Discord user has muted cross-chat mention pings.
    ///
    /// In-memory only.
    pub async fn is_mention_muted(&self, discord_id: u64) -> bool {
        self.mute_mention.read().await.contains(&discord_id)
    }

    /// Check whether a Discord user is currently muted from sending bridge messages.
    ///
    /// In-memory only — checks expiry timestamp, auto-cleans expired entries.
    /// Returns `None` if not muted, `Some(0)` if permanently muted, `Some(ts)` with
    /// the expiry timestamp.
    pub async fn is_muted(&self, discord_id: u64) -> Option<u64> {
        let mut map = self.muted_users.write().await;
        let expiry = *map.get(&discord_id)?;
        if expiry == 0 {
            return Some(0);
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if now >= expiry {
            // Auto-cleanup expired mute from cache and redb
            map.remove(&discord_id);
            let db = Arc::clone(&self.db);
            let _ = tokio::task::spawn_blocking(move || remove_muted_user(&db, discord_id)).await;
            return None;
        }
        Some(expiry)
    }

    /// Mute a Discord user from sending bridge messages for a given duration.
    ///
    /// `duration_secs` of 0 means permanent. Writes to both in-memory and redb.
    ///
    /// # Errors
    ///
    /// Returns `StorageError` on persistence failure.
    pub async fn set_muted(&self, discord_id: u64, duration_secs: u64) -> Result<(), StorageError> {
        let expiry_ts = if duration_secs == 0 {
            0
        } else {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            now + duration_secs
        };

        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || write_muted_user(&db, discord_id, expiry_ts))
            .await
            .map_err(|e| StorageError::BlockingPanic(e.to_string()))??;

        {
            let mut map = self.muted_users.write().await;
            map.insert(discord_id, expiry_ts);
        }

        Ok(())
    }

    /// Unmute a Discord user, removing them from the bridge mute list.
    ///
    /// # Errors
    ///
    /// Returns `StorageError` on persistence failure.
    pub async fn unmute_user(&self, discord_id: u64) -> Result<(), StorageError> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || remove_muted_user(&db, discord_id))
            .await
            .map_err(|e| StorageError::BlockingPanic(e.to_string()))??;

        {
            let mut map = self.muted_users.write().await;
            map.remove(&discord_id);
        }

        Ok(())
    }

    /// Reverse-resolve a UUID to a username.
    ///
    /// # Errors
    ///
    /// Returns `StorageError` on redb failure.
    #[allow(dead_code)]
    pub async fn resolve_username(&self, uuid: String) -> Result<Option<String>, StorageError> {
        let db = Arc::clone(&self.db);
        let result = tokio::task::spawn_blocking(move || reverse_resolve_uuid(&db, &uuid)).await;
        match result {
            Ok(Ok(username)) => Ok(username),
            Ok(Err(e)) => {
                tracing::warn!(%e, "failed to reverse-resolve uuid");
                Ok(None)
            }
            Err(join_err) => Err(StorageError::BlockingPanic(join_err.to_string())),
        }
    }
}

fn load_privacy_enabled(db: &Database) -> bool {
    let Ok(rtxn) = db.begin_read() else {
        return true;
    };

    let Ok(table) = rtxn.open_table(SETTINGS) else {
        return true;
    };

    match table.get(PRIVACY_ENABLED_KEY) {
        Ok(Some(g)) => g.value(),
        _ => true,
    }
}

fn load_account_mappings(db: &Database) -> (Vec<String>, Vec<u64>) {
    let mut mc_usernames: Vec<String> = Vec::new();
    let mut discord_ids: Vec<u64> = Vec::new();

    let Ok(rtxn) = db.begin_read() else {
        return (mc_usernames, discord_ids);
    };

    let Ok(table) = rtxn.open_table(DC_TO_MC) else {
        return (mc_usernames, discord_ids);
    };

    let mut pairs: Vec<(String, u64)> = Vec::new();
    if let Ok(iter) = table.iter() {
        for (key, value) in iter.flatten() {
            pairs.push((value.value(), key.value()));
        }
    }

    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    for (mc, dc) in pairs {
        mc_usernames.push(mc);
        discord_ids.push(dc);
    }

    (mc_usernames, discord_ids)
}

fn load_optout_set(db: &Database, table_def: TableDefinition<u64, bool>) -> HashSet<u64> {
    let mut set = HashSet::new();

    let Ok(rtxn) = db.begin_read() else {
        return set;
    };

    let Ok(table) = rtxn.open_table(table_def) else {
        return set;
    };

    if let Ok(iter) = table.iter() {
        for (key, value) in iter.flatten() {
            if value.value() {
                set.insert(key.value());
            }
        }
    }

    set
}

fn write_account_mapping(
    db: &Database,
    discord_id: u64,
    mc_username: &str,
) -> Result<(), StorageError> {
    let wtxn = db.begin_write()?;
    {
        let mut dc_table = wtxn.open_table(DC_TO_MC)?;
        let mut mc_table = wtxn.open_table(MC_TO_DC)?;

        // TOCTOU guard: re-check inside the transaction that the MC username
        // is not already claimed by a different Discord user.
        if let Some(existing) = mc_table.get(mc_username.to_string())?
            && existing.value() != discord_id
        {
            return Err(StorageError::AlreadyClaimed);
        }

        let old_mc = dc_table.get(discord_id)?.map(|g| g.value());
        if let Some(ref old) = old_mc {
            mc_table.remove(old.clone())?;
        }

        dc_table.insert(discord_id, mc_username.to_string())?;
        mc_table.insert(mc_username.to_string(), discord_id)?;
    }
    wtxn.commit()?;
    Ok(())
}

fn remove_account_mapping(
    db: &Database,
    discord_id: u64,
    mc_username: &str,
) -> Result<(), StorageError> {
    let wtxn = db.begin_write()?;
    {
        let mut dc_table = wtxn.open_table(DC_TO_MC)?;
        let mut mc_table = wtxn.open_table(MC_TO_DC)?;
        dc_table.remove(discord_id)?;
        mc_table.remove(mc_username.to_string())?;
    }
    wtxn.commit()?;
    Ok(())
}

fn write_optout_entry(
    db: &Database,
    table_def: TableDefinition<u64, bool>,
    user_id: u64,
    value: bool,
) -> Result<(), StorageError> {
    let wtxn = db.begin_write()?;
    {
        let mut table = wtxn.open_table(table_def)?;
        if value {
            table.insert(user_id, true)?;
        } else {
            table.remove(user_id)?;
        }
    }
    wtxn.commit()?;
    Ok(())
}

fn write_privacy_setting(db: &Database, enabled: bool) -> Result<(), StorageError> {
    let wtxn = db.begin_write()?;
    {
        let mut table = wtxn.open_table(SETTINGS)?;
        table.insert(PRIVACY_ENABLED_KEY, enabled)?;
    }
    wtxn.commit()?;
    Ok(())
}

fn load_player_profile_enabled(db: &Database) -> bool {
    let Ok(rtxn) = db.begin_read() else {
        return true;
    };

    let Ok(table) = rtxn.open_table(SETTINGS) else {
        return true;
    };

    match table.get(PLAYER_PROFILE_ENABLED_KEY) {
        Ok(Some(g)) => g.value(),
        _ => true,
    }
}

fn write_player_profile_setting(db: &Database, enabled: bool) -> Result<(), StorageError> {
    let wtxn = db.begin_write()?;
    {
        let mut table = wtxn.open_table(SETTINGS)?;
        table.insert(PLAYER_PROFILE_ENABLED_KEY, enabled)?;
    }
    wtxn.commit()?;
    Ok(())
}

fn load_muted_users(db: &Database) -> HashMap<u64, u64> {
    let mut map = HashMap::new();

    let Ok(rtxn) = db.begin_read() else {
        return map;
    };

    let Ok(table) = rtxn.open_table(MUTED_USERS) else {
        return map;
    };

    if let Ok(iter) = table.iter() {
        for item in iter.flatten() {
            map.insert(item.0.value(), item.1.value());
        }
    }

    map
}

fn write_muted_user(db: &Database, discord_id: u64, expiry_ts: u64) -> Result<(), StorageError> {
    let wtxn = db.begin_write()?;
    {
        let mut table = wtxn.open_table(MUTED_USERS)?;
        if expiry_ts == 0 {
            table.insert(discord_id, 0)?;
        } else {
            table.insert(discord_id, expiry_ts)?;
        }
    }
    wtxn.commit()?;
    Ok(())
}

fn remove_muted_user(db: &Database, discord_id: u64) -> Result<(), StorageError> {
    let wtxn = db.begin_write()?;
    {
        let mut table = wtxn.open_table(MUTED_USERS)?;
        table.remove(discord_id)?;
    }
    wtxn.commit()?;
    Ok(())
}

fn read_bridge(db: &Database) -> Result<Option<u64>, StorageError> {
    let rtxn = db.begin_read()?;
    let table = rtxn.open_table(BRIDGE);

    let table = match table {
        Ok(t) => t,
        Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
        Err(e) => return Err(e.into()),
    };

    let value = table.get(CURRENT)?;
    Ok(value.map(|guard| {
        let (channel_id, _, _) = guard.value();
        channel_id
    }))
}

fn write_bridge(
    db: &Database,
    channel_id: u64,
    guild_id: u64,
    mc_server_address: &str,
) -> Result<(), StorageError> {
    let wtxn = db.begin_write()?;
    {
        let mut table = wtxn.open_table(BRIDGE)?;
        table.insert(
            CURRENT,
            (channel_id, guild_id, mc_server_address.to_string()),
        )?;
    }
    wtxn.commit()?;
    Ok(())
}

fn remove_bridge(db: &Database) -> Result<(), StorageError> {
    let wtxn = db.begin_write()?;
    {
        let mut table = wtxn.open_table(BRIDGE)?;
        table.remove(CURRENT)?;
    }
    wtxn.commit()?;
    Ok(())
}

fn read_uuid_mapping(db: &Database, username: &str) -> Result<Option<String>, StorageError> {
    let rtxn = db.begin_read()?;
    let table = rtxn.open_table(USERNAME_UUID);
    let table = match table {
        Ok(t) => t,
        Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    Ok(table.get(username.to_string())?.map(|g| g.value()))
}

fn write_uuid_mapping(db: &Database, username: &str, uuid: &str) -> Result<(), StorageError> {
    let wtxn = db.begin_write()?;
    {
        let mut table = wtxn.open_table(USERNAME_UUID)?;
        table.insert(username.to_string(), uuid.to_string())?;
    }
    wtxn.commit()?;
    Ok(())
}

fn read_player_stats(db: &Database, uuid: &str) -> Result<Option<PlayerStats>, StorageError> {
    let rtxn = db.begin_read()?;
    let table = rtxn.open_table(PLAYERS);
    let table = match table {
        Ok(t) => t,
        Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let guard = table.get(uuid.to_string())?;
    Ok(guard.map(|g| PlayerStats::from_tuple(g.value())))
}

fn read_daily_play_time(db: &Database, uuid: &str, date: &str) -> Result<u64, StorageError> {
    let rtxn = db.begin_read()?;
    let table = rtxn.open_table(DAILY_PLAY_TIME);
    let table = match table {
        Ok(t) => t,
        Err(redb::TableError::TableDoesNotExist(_)) => return Ok(0),
        Err(e) => return Err(e.into()),
    };
    Ok(table
        .get(&(uuid.to_string(), date.to_string()))?
        .map_or(0, |g| g.value()))
}

fn read_recent_play_time(
    db: &Database,
    uuid: &str,
    dates: &[String],
) -> Result<Vec<(String, u64)>, StorageError> {
    let rtxn = db.begin_read()?;
    let table = rtxn.open_table(DAILY_PLAY_TIME);
    let table = match table {
        Ok(t) => t,
        Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut result = Vec::with_capacity(dates.len());
    for date in dates {
        let secs = table
            .get(&(uuid.to_string(), date.clone()))?
            .map_or(0, |g| g.value());
        result.push((date.clone(), secs));
    }
    Ok(result)
}

fn read_all_player_stats(db: &Database) -> Result<Vec<(String, PlayerStats)>, StorageError> {
    let rtxn = db.begin_read()?;
    let table = rtxn.open_table(PLAYERS);
    let table = match table {
        Ok(t) => t,
        Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut result = Vec::new();
    for item in table.iter()? {
        let (key, value) = item?;
        result.push((key.value().clone(), PlayerStats::from_tuple(value.value())));
    }
    result.sort_by_key(|(_, s)| std::cmp::Reverse(s.total_play_time_secs));
    Ok(result)
}

fn reverse_resolve_uuid(db: &Database, uuid: &str) -> Result<Option<String>, StorageError> {
    let rtxn = db.begin_read()?;
    let table = rtxn.open_table(USERNAME_UUID);
    let table = match table {
        Ok(t) => t,
        Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    for item in table.iter()? {
        let (key, value) = item?;
        if value.value() == uuid {
            return Ok(Some(key.value().clone()));
        }
    }
    Ok(None)
}

fn write_player_deltas(
    db: &Database,
    deltas: &[(String, PlayerDelta)],
    daily_splits: &[(String, DailyTimeSplit)],
    durability: redb::Durability,
) -> Result<(), StorageError> {
    let mut wtxn = db.begin_write()?;
    wtxn.set_durability(durability)?;

    {
        let mut players = wtxn.open_table(PLAYERS)?;
        let mut daily = wtxn.open_table(DAILY_PLAY_TIME)?;

        for (uuid, delta) in deltas {
            let existing = players
                .get(uuid)?
                .map(|g| PlayerStats::from_tuple(g.value()));

            let mut stats = existing.unwrap_or_default();
            if stats.first_login_ts == 0 {
                stats.first_login_ts = delta.last_login_ts.unwrap_or(0);
            }
            if let Some(ts) = delta.last_login_ts {
                stats.last_login_ts = ts;
            }
            if let Some(ts) = delta.last_logout_ts {
                stats.last_logout_ts = ts;
            }
            stats.total_play_time_secs += delta.play_time_secs;
            stats.total_logins += delta.total_logins_delta;
            stats.total_deaths += delta.deaths;
            stats.total_advancements += delta.advancements;
            stats.total_messages += delta.messages;
            stats.total_commands += delta.commands;

            players.insert(uuid.clone(), stats.to_tuple())?;
        }

        for (uuid, split) in daily_splits {
            let key = (uuid.clone(), split.date.clone());
            let current = daily.get(&key)?.map_or(0, |g| g.value());
            daily.insert(key, current + split.secs)?;
        }
    }

    wtxn.commit()?;
    Ok(())
}

#[cfg(test)]
fn open_test_storage(path: &Path) -> Storage {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let db = Database::create(path).expect("failed to create test db");
    let (mc_usernames, discord_ids) = load_account_mappings(&db);
    let join_leave = load_optout_set(&db, JOIN_LEAVE_OPTOUT);
    let mute = load_optout_set(&db, MUTE_MENTION);
    let privacy = load_privacy_enabled(&db);
    let muted = load_muted_users(&db);
    let profile_toggle = load_player_profile_enabled(&db);

    Storage {
        db: Arc::new(db),
        mc_server_address: "localhost:25565".to_string(),
        account_cache: Arc::new(RwLock::new((mc_usernames, discord_ids))),
        join_leave_optout: Arc::new(RwLock::new(join_leave)),
        mute_mention: Arc::new(RwLock::new(mute)),
        muted_users: Arc::new(RwLock::new(muted)),
        privacy_enabled: Arc::new(RwLock::new(privacy)),
        player_profile_enabled: Arc::new(RwLock::new(profile_toggle)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("vvv_test_{name}_{}.redb", std::process::id()))
    }

    #[tokio::test]
    async fn get_returns_none_when_empty() {
        let path = temp_db_path("empty");
        let _ = std::fs::remove_file(&path);
        let storage = open_test_storage(&path);

        let channel = storage.get_bridge_channel().await.expect("read failed");
        assert!(channel.is_none());

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn set_then_get_roundtrip() {
        let path = temp_db_path("roundtrip");
        let _ = std::fs::remove_file(&path);
        let storage = open_test_storage(&path);

        storage
            .set_bridge_channel(123, 456)
            .await
            .expect("write failed");

        let channel = storage.get_bridge_channel().await.expect("read failed");
        assert_eq!(channel, Some(serenity::ChannelId::new(123)));

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn clear_then_get_none() {
        let path = temp_db_path("clear");
        let _ = std::fs::remove_file(&path);
        let storage = open_test_storage(&path);

        storage
            .set_bridge_channel(42, 99)
            .await
            .expect("write failed");
        storage.clear_bridge_channel().await.expect("clear failed");

        let channel = storage.get_bridge_channel().await.expect("read failed");
        assert!(channel.is_none());

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn set_overwrites_previous() {
        let path = temp_db_path("overwrite");
        let _ = std::fs::remove_file(&path);
        let storage = open_test_storage(&path);

        storage
            .set_bridge_channel(100, 200)
            .await
            .expect("first write failed");
        storage
            .set_bridge_channel(300, 400)
            .await
            .expect("second write failed");

        let channel = storage.get_bridge_channel().await.expect("read failed");
        assert_eq!(channel, Some(serenity::ChannelId::new(300)));

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn open_creates_parent_directories() {
        let dir = std::env::temp_dir().join(format!("vvv_test_dir_{}", std::process::id()));
        let path = dir.join("sub").join("vvv.redb");
        let _ = std::fs::remove_dir_all(&dir);

        let storage = open_test_storage(&path);
        let channel = storage.get_bridge_channel().await.expect("read failed");
        assert!(channel.is_none());

        assert!(path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn muted_user_not_muted_by_default() {
        let path = temp_db_path("muted_default");
        let _ = std::fs::remove_file(&path);
        let storage = open_test_storage(&path);

        let result = storage.is_muted(42).await;
        assert!(result.is_none());

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn mute_user_permanent() {
        let path = temp_db_path("muted_perm");
        let _ = std::fs::remove_file(&path);
        let storage = open_test_storage(&path);

        storage.set_muted(42, 0).await.expect("mute failed");
        let result = storage.is_muted(42).await;
        assert_eq!(result, Some(0));

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn mute_user_temporary() {
        let path = temp_db_path("muted_temp");
        let _ = std::fs::remove_file(&path);
        let storage = open_test_storage(&path);

        storage.set_muted(42, 60).await.expect("mute failed");
        let result = storage.is_muted(42).await;
        assert!(result.is_some_and(|ts| ts > 0));

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn unmute_user() {
        let path = temp_db_path("muted_unmute");
        let _ = std::fs::remove_file(&path);
        let storage = open_test_storage(&path);

        storage.set_muted(42, 0).await.expect("mute failed");
        assert!(storage.is_muted(42).await.is_some());

        storage.unmute_user(42).await.expect("unmute failed");
        let result = storage.is_muted(42).await;
        assert!(result.is_none());

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn mute_persists_across_storage_opens() {
        let path = temp_db_path("muted_persist");
        let _ = std::fs::remove_file(&path);
        {
            let storage = open_test_storage(&path);
            storage.set_muted(42, 0).await.expect("mute failed");
        }
        {
            let storage = open_test_storage(&path);
            let result = storage.is_muted(42).await;
            assert_eq!(result, Some(0));
        }
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn profile_toggle_defaults_to_enabled() {
        let path = temp_db_path("profile_toggle_default");
        let _ = std::fs::remove_file(&path);
        let storage = open_test_storage(&path);
        assert!(storage.is_player_profile_enabled().await);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn profile_toggle_set_and_persist() {
        let path = temp_db_path("profile_toggle_persist");
        let _ = std::fs::remove_file(&path);
        {
            let storage = open_test_storage(&path);
            storage
                .set_player_profile_enabled(false)
                .await
                .expect("set failed");
            assert!(!storage.is_player_profile_enabled().await);
        }
        {
            let storage = open_test_storage(&path);
            assert!(!storage.is_player_profile_enabled().await);
        }
        let _ = std::fs::remove_file(&path);
    }
}
