use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use poise::serenity_prelude as serenity;
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};

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
}

/// redb-backed persistence for the bridge binding and player stats.
///
/// Holds a long-lived `Arc<Database>` (`Send + Sync`) opened once at startup.
/// Each transaction runs inside `spawn_blocking` because redb transactions are
/// not `Send`.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Storage {
    db: Arc<Database>,
    mc_server_address: String,
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

        Ok(Self {
            db: Arc::new(db),
            mc_server_address,
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

#[allow(dead_code)]
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

#[allow(dead_code)]
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

#[allow(dead_code)]
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

#[allow(dead_code)]
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
    Storage::open(path, "localhost:25565".to_string()).expect("failed to open test storage")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("ruze_test_{name}_{}.redb", std::process::id()))
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
        let dir = std::env::temp_dir().join(format!("ruze_test_dir_{}", std::process::id()));
        let path = dir.join("sub").join("ruze.redb");
        let _ = std::fs::remove_dir_all(&dir);

        let storage = open_test_storage(&path);
        let channel = storage.get_bridge_channel().await.expect("read failed");
        assert!(channel.is_none());

        assert!(path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
