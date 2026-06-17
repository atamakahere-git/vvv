use mc_rcon::RconClient;

/// Connect to a Minecraft RCON server.
///
/// Reads `RCON_SERVER_ADDRESS` (defaults to `localhost:25575`) and `RCON_PASSWORD`
/// from the environment.
///
/// # Errors
///
/// Returns `std::io::Error` if the TCP connection fails or `RCON_PASSWORD` is not set.
///
/// Logs a warning if authentication fails; the client is still returned so the
/// caller can decide how to handle it.
pub fn connect() -> std::io::Result<RconClient> {
    let address = std::env::var("RCON_SERVER_ADDRESS").unwrap_or_else(|_| {
        tracing::warn!("RCON_SERVER_ADDRESS not set, defaulting to localhost:25575");
        "localhost:25575".to_string()
    });

    let client = RconClient::connect(address).map_err(|e| {
        tracing::error!("Unable to connect to Minecraft RCON server: {e}");
        std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "RCON connection failed")
    })?;

    let password = std::env::var("RCON_PASSWORD").map_err(|_| {
        tracing::error!("RCON_PASSWORD environment variable not set");
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "RCON_PASSWORD not set")
    })?;

    if let Err(e) = client.log_in(&password) {
        tracing::error!("Failed to authenticate with RCON server: {e}");
    }

    Ok(client)
}
