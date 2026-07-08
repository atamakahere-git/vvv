use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use poise::serenity_prelude as serenity;
use redb::{Database, ReadableDatabase, TableDefinition};

/// Single-row bridge binding table.
///
/// Key is the constant `"current"`; value is the tuple
/// `(channel_id, guild_id, mc_server_address)`.
const BRIDGE: TableDefinition<&str, (u64, u64, String)> = TableDefinition::new("bridge");

/// Sentinel key for the single bridge row.
const CURRENT: &str = "current";

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
    #[error("I/O error at `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("storage worker panicked: {0}")]
    BlockingPanic(String),
}

/// redb-backed persistence for the bridge binding.
///
/// Holds a long-lived `Arc<Database>` (`Send + Sync`) opened once at startup.
/// Each transaction runs inside `spawn_blocking` because redb transactions are
/// not `Send`.
#[derive(Debug, Clone)]
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
