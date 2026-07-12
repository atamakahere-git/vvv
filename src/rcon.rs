use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use mc_rcon::RconClient;

/// Errors that can occur when connecting to or using a Minecraft RCON server.
#[derive(Debug, thiserror::Error)]
pub enum RconError {
    #[error("rcon connection failed: {0}")]
    Connect(#[from] std::io::Error),
    #[error("rcon error: {0}")]
    Rcon(String),
}

/// An RCON client wrapper that automatically reconnects on send failure.
///
/// Uses an internal `std::sync::Mutex` so all callers can share a single
/// `Arc<ReconnectingRcon>` without an external lock.
///
/// The primary API is the async `send_command` which wraps the synchronous
/// implementation in `spawn_blocking` to avoid stalling the Tokio runtime.
/// Reconnects are rate-limited to one attempt every 5 seconds, and the
/// TCP connect phase has a 3-second timeout via a detached OS thread.
pub struct ReconnectingRcon {
    address: String,
    password: String,
    client: Mutex<Option<RconClient>>,
    last_connect_attempt: Mutex<Option<Instant>>,
}

const RECONNECT_COOLDOWN: Duration = Duration::from_secs(5);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

impl ReconnectingRcon {
    /// Connect to a Minecraft RCON server.
    ///
    /// # Errors
    ///
    /// Returns `RconError` if the connection or authentication fails.
    pub fn connect(address: String, password: String) -> Result<Self, RconError> {
        let client = Self::create_client(&address, &password)?;
        tracing::info!("RCON connected to {address}");
        Ok(Self {
            address,
            password,
            client: Mutex::new(Some(client)),
            last_connect_attempt: Mutex::new(None),
        })
    }

    /// Asynchronous primary API — wraps `send_command_sync` in `spawn_blocking`
    /// so blocking TCP I/O never stalls the Tokio runtime.
    ///
    /// # Errors
    ///
    /// Returns `RconError` on connection failure, authentication failure,
    /// or if the blocking task panics.
    pub async fn send_command(self: &Arc<Self>, command: String) -> Result<String, RconError> {
        let me = Arc::clone(self);
        tokio::task::spawn_blocking(move || me.send_command_sync(&command))
            .await
            .map_err(|e| RconError::Rcon(format!("blocking task failed: {e}")))?
    }

    /// Synchronous send with automatic reconnection on failure.
    fn send_command_sync(&self, command: &str) -> Result<String, RconError> {
        {
            let guard = self.client.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(ref client) = *guard {
                match client.send_command(command) {
                    Ok(result) => return Ok(result),
                    Err(e) => {
                        tracing::warn!("rcon send failed: {e}, will attempt reconnect");
                    }
                }
            }
        }

        if !self.should_reconnect() {
            return Err(RconError::Rcon(
                "reconnect rate limited, last attempt was <5s ago".to_string(),
            ));
        }

        tracing::info!("reconnecting RCON to {}...", self.address);
        match Self::create_client_with_timeout(&self.address, &self.password, CONNECT_TIMEOUT) {
            Ok(new_client) => {
                let mut guard = self
                    .client
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                let result = new_client.send_command(command);
                *guard = Some(new_client);
                tracing::info!("RCON reconnected successfully");
                result.map_err(|e| RconError::Rcon(format!("{e}")))
            }
            Err(e) => {
                let mut guard = self
                    .client
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                *guard = None;
                tracing::error!(%e, "RCON reconnection failed");
                Err(e)
            }
        }
    }

    /// Returns `true` if enough time has elapsed since the last reconnect attempt.
    fn should_reconnect(&self) -> bool {
        let mut guard = self
            .last_connect_attempt
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let now = Instant::now();
        let allow = guard.is_none_or(|t| now.duration_since(t) >= RECONNECT_COOLDOWN);
        if allow {
            *guard = Some(now);
        }
        allow
    }

    fn create_client(address: &str, password: &str) -> Result<RconClient, RconError> {
        let client = RconClient::connect(address.to_string())?;
        client
            .log_in(password)
            .map_err(|e| RconError::Rcon(format!("{e}")))?;
        Ok(client)
    }

    /// Creates a new RCON client with a bounded connect timeout.
    ///
    /// Spawns a detached OS thread for the blocking `create_client` call and
    /// waits on a channel with `recv_timeout`. If the timeout fires the caller
    /// gets an error; the orphaned thread finishes naturally when the OS TCP
    /// connect completes or times out.
    fn create_client_with_timeout(
        address: &str,
        password: &str,
        timeout: Duration,
    ) -> Result<RconClient, RconError> {
        let (tx, rx) = std::sync::mpsc::channel();
        let addr = address.to_string();
        let pwd = password.to_string();
        std::thread::spawn(move || {
            let result = Self::create_client(&addr, &pwd);
            let _ = tx.send(result);
        });
        rx.recv_timeout(timeout)
            .map_err(|_| RconError::Rcon(format!("RCON connect timeout after {}s", timeout.as_secs())))?
    }
}