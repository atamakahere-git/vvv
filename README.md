# 🏘️ VVV: Villager's Verse Viaduct

> **AI Disclaimer:** This project is an AI-assisted fork of [OscillatingBlock's original Ruze](https://github.com/OscillatingBlock/Ruze), which was created as a toy learning project. The codebase has been heavily refactored and extended with new features using AI assistance (DeepSeek-V4-Pro and V4-Flash). The original author's work provided the foundation; all subsequent modifications and additions are AI-generated.

A lightweight, high-performance Discord–Minecraft chat bridge bot. Built in Rust using `poise` and `serenity`, VVV establishes a seamless, two-way communication channel between your Discord server and a Minecraft server.

Unlike traditional bridges, VVV uses direct log-tailing to read Minecraft server events **without requiring any mods or plugins** installed on the server itself.

---

## Features

- **Zero-Client Bridge** — Tails Minecraft server logs natively via `linemux`. No client mods, Forge, Fabric, or Paper plugins required.
- **Live Chat Sync** — Forwards Minecraft in-game chat to Discord and vice versa, with JSON-safe escaping.
- **Rich Server Events** — Deaths (95+ death message patterns), advancements, join/leave/disconnect, issued commands, server lifecycle (start/stop/save), and `/say` broadcasts all formatted and beamed into Discord.
- **Leave Deduplication** — When a player disconnects with a reason, the generic follow-up "left the game" is suppressed to avoid duplicate messages.
- **Persistent Bridge State** — Channel bindings survive bot restarts. Stored in an embedded `redb` database.
- **Player Info** — `/info` queries the server via both RCON and Server List Ping, displaying online players, MOTD, latency, and the server icon.
- **Account Linking** — `/connect` links Discord ↔ Minecraft accounts via in-game verification codes.
- **Player Profile Dashboard** — `/profile` shows a multi-page dashboard with NBT player data (health, position, inventory), cumulative and daily stats from redb, and advancement progress — all in one command.
- **Player Stats** — Play time, deaths, advancements, messages, commands tracked per player with daily breakdowns.
- **Mention Cross-Translation** — `@DiscordUser` in MC → Discord ping; `@MCPlayer` in Discord → `@playername` in MC.
- **Privacy Controls** — `/unsub`/`/sub` for join/leave announcements, `/mutemention`/`/unmutemention` for mention muting.
- **Admin Tools** — `/connect_admin` (owner only) to manually link accounts, `/mute`/`/unmute` (owner/admin) to block users from sending to the bridge.
- **Guild Welcome** — Sends a themed embed message to the system channel when a new member joins the Discord server.
- **Structured Logging** — Full `tracing`-based observability with RFC 3339 timestamps and configurable verbosity (`RUST_LOG`).
- **Secure Access** — Critical bridge commands restricted to the Bot Owner and Server Administrators.

---

## Minecraft Server Configuration

Enable **RCON** (for sending Discord → Minecraft messages and querying player lists) in your Minecraft server's `server.properties`. Server List Ping (used by `/info` for MOTD and latency) works out-of-the-box on vanilla servers:

```properties
enable-rcon=true
rcon.port=25575
rcon.password=your_secure_rcon_password_here
```

Restart your Minecraft server after saving these changes.

---

## Configuration

VVV uses a layered configuration system. Values are resolved from **lowest to highest priority**:

| Priority | Source | Purpose |
|---|---|---|
| 1 (lowest) | `/etc/vvv.toml` | System-wide defaults (all users) |
| 2 | `$XDG_CONFIG_HOME/vvv.toml` | Per-user config (falls back to `~/.config/vvv.toml`) |
| 3 | `$HOME/.vvv.toml` | Per-user home-directory override |
| 4 (highest) | Environment variables | `VVV_*` prefix |

Each layer merges on top of the previous one — later sources override earlier ones. Environment variables always take precedence.

### TOML configuration file

Create a `vvv.toml` in any of the supported paths above. All sections are required unless a default is noted.

```toml
[discord]
# Your Discord Bot Application token
token = "your_discord_bot_token_here"

[rcon]
# RCON server address and port (default: "localhost:25575")
address = "localhost:25575"
# RCON password set in server.properties
password = "your_secure_rcon_password_here"

[minecraft]
# Minecraft server address and port for status pings (default: "localhost:25565")
server_address = "localhost:25565"
# Optional: Path to the Minecraft world directory for player profile data.
# Enables the ~profile command. Must contain playerdata/, stats/, advancements/ subdirectories.
# world_directory = "/var/minecraft/world"

[bot]
# Discord user ID of the bot owner
owner_id = 123456789012345678
# Optional: Discord guild (server) ID for instant slash command registration.
# When set, commands appear immediately in this guild instead of waiting
# for Discord's global sync (which can take up to an hour).
# guild_id = 1234567890123456789

[log]
# Absolute path to the Minecraft server's latest.log file
path = "/var/minecraft/logs/latest.log"

[storage]
# Optional override for the redb database file path (default: $XDG_STATE_HOME/vvv/vvv.redb)
# database_path = "/custom/path/vvv.redb"

[stats]
# IANA timezone name for daily play-time attribution (default: "UTC")
# timezone = "Asia/Kolkata"
```

### Environment variables (highest priority)

Each TOML field has a corresponding `VVV_*` environment variable. Use these for Docker, systemd, or CI deployments.

| Variable | TOML field | Required | Default |
|---|---|---|---|
| `VVV_DISCORD_TOKEN` | `discord.token` | Yes | — |
| `VVV_LOG_PATH` | `log.path` | Yes | — |
| `VVV_RCON_PASSWORD` | `rcon.password` | Yes | — |
| `VVV_RCON_ADDRESS` | `rcon.address` | No | `localhost:25575` |
| `VVV_MC_SERVER_ADDRESS` | `minecraft.server_address` | No | `localhost:25565` |
| `VVV_OWNER_ID` | `bot.owner_id` | Yes | — |
| `VVV_GUILD_ID` | `bot.guild_id` | No | — (instant slash cmd sync) |
| `VVV_DATABASE_PATH` | `storage.database_path` | No | `$XDG_STATE_HOME/vvv/vvv.redb` |
| `VVV_STATS_TIMEZONE` | `stats.timezone` | No | `UTC` |
| `VVV_MC_WORLD_DIRECTORY` | `minecraft.world_directory` | No | — (disables `/profile` if unset) |

### Discord Setup

Enable **Privileged Gateway Intents** in the [Discord Developer Portal](https://discord.com/developers/applications):

1. Select your Application → **Bot** tab.
2. Under **Privileged Gateway Intents**, enable **Server Members Intent** and **Message Content Intent**.

---

## Quick Start

### 1. Create a Discord bot

Go to the [Discord Developer Portal](https://discord.com/developers/applications), create a new application, then create a bot under the **Bot** tab. Copy the token.

### 2. Configure your Minecraft server

Add to `server.properties`:

```properties
enable-rcon=true
rcon.port=25575
rcon.password=your_password_here
```

### 3. Run VVV

```bash
# Using env vars (quickest)
VVV_DISCORD_TOKEN=your_token \
VVV_LOG_PATH=/var/minecraft/logs/latest.log \
VVV_RCON_PASSWORD=your_password \
VVV_OWNER_ID=123456789012345678 \
cargo run --release

# Or using a config file
mkdir -p ~/.config
cat > ~/.config/vvv.toml << 'EOF'
[discord]
token = "your_discord_bot_token_here"

[rcon]
password = "your_secure_rcon_password_here"

[bot]
owner_id = 123456789012345678

[log]
path = "/var/minecraft/logs/latest.log"
EOF

cargo run --release
```

### 4. Start the bridge

In your Discord server, run `~start_bridge` in the channel you want to bridge.

---

## Build & Run

```bash
# Debug build
cargo run

# Release build (recommended for production)
cargo run --release
```

### Logging

VVV outputs structured logs to stderr. Control verbosity via the `RUST_LOG` environment variable:

```bash
# Default — info and above only
cargo run --release

# Verbose — debug messages included
RUST_LOG=debug cargo run --release

# Everything — trace-level detail
RUST_LOG=trace cargo run --release

# Silence noisy crates, show only VVV logs
RUST_LOG=info,serenity=warn,tracing=warn cargo run --release
```

Log format includes RFC 3339 timestamps, file location, and structured fields:

```
2026-06-17T10:30:00.123Z  INFO main.rs:18 starting VVV bridge...
2026-06-17T10:30:00.125Z  INFO consts.rs:88 loading configuration...
2026-06-17T10:30:00.127Z DEBUG consts.rs:97 merged /home/user/.config/vvv.toml
2026-06-17T10:30:00.128Z  INFO consts.rs:106 configuration validated
2026-06-17T10:30:00.130Z  INFO main.rs:32 log watcher started | path=/var/mc/logs/latest.log
2026-06-17T10:30:00.131Z  INFO rcon.rs:16 RCON connected to localhost:25575
2026-06-17T10:30:00.132Z  INFO main.rs:62 Discord → Minecraft relay started
2026-06-17T10:30:00.133Z  INFO main.rs:73 bridge is now running
2026-06-17T10:30:03.456Z  INFO handler.rs:123 bot logged in | name=VVV#1234
2026-06-17T10:30:04.789Z DEBUG log_parser.rs:91 chat event parsed | username=Herobrine
2026-06-17T10:30:04.790Z  INFO main.rs:46 mc→dc | username=Herobrine
2026-06-17T10:30:10.001Z  INFO commands.rs:24 command /ping executed | user=Admin
```

---

## Bot Commands

All commands support both `~` prefix (`~ping`) and Slash Commands (`/ping`).

### Everyone

| Command | Description |
|---|---|
| `~help` | List all commands or get detailed help for a specific one |
| `~ping` | Check if the bot is alive |
| `~info` | Query the server for online players, MOTD, latency, and server icon |
| `~connect <mc_username>` | Link your Discord account to a Minecraft username (in-game verification) |
| `~disconnect` | Unlink your Discord account from Minecraft |
| `~profile [player]` | View player dashboard with stats, advancements, daily playtime, and inventory |
| `~leaderboard` | Show the top 10 players by total play time |
| `~sub` | Opt in to join/leave announcements |
| `~unsub` | Opt out of join/leave announcements |
| `~mutemention` | Mute cross-chat mention pings |
| `~unmutemention` | Re-enable cross-chat mention pings |

### Owner / Admin

| Command | Description |
|---|---|
| `~start_bridge` | Bind the bridge to the current channel |
| `~stop_bridge` | Unbind the bridge from the current channel |
| `~mute <user> [duration]` | Mute a user from sending Discord→MC bridge messages (default 5m) |
| `~unmute <user>` | Unmute a previously muted user |
| `~privacy [enable|disable]` | Toggle global privacy features |
| `~profile_toggle [enable|disable]` | Toggle the player profile dashboard on or off |

### Owner Only

| Command | Description |
|---|---|
| `~connect_admin <mc_username> <@user>` | Manually link a Discord user to a Minecraft username (bypasses verification) |

**Events forwarded to bridged channels:** chat messages, join/leave/disconnect, deaths, advancements, issued server commands, server-say broadcasts, and server lifecycle events (start, stop, save).

---

## Deployment

### Systemd (recommended for production)

```ini
[Unit]
Description=VVV Discord-Minecraft Bridge
After=network.target minecraft.service

[Service]
Type=simple
User=minecraft
Environment=VVV_DISCORD_TOKEN=your_token
Environment=VVV_LOG_PATH=/var/minecraft/logs/latest.log
Environment=VVV_RCON_PASSWORD=your_password
Environment=VVV_OWNER_ID=123456789012345678
ExecStart=/usr/local/bin/vvv
Restart=on-failure
RestartSec=10

[Install]
WantedBy=multi-user.target
```

### Docker

```dockerfile
FROM rust:alpine AS builder
WORKDIR /app
COPY . .
RUN cargo build --release

FROM alpine:latest
COPY --from=builder /app/target/release/vvv /usr/local/bin/vvv
CMD ["vvv"]
```

```bash
docker build -t vvv .
docker run -d \
  -e VVV_DISCORD_TOKEN=your_token \
  -e VVV_LOG_PATH=/var/minecraft/logs/latest.log \
  -e VVV_RCON_PASSWORD=your_password \
  -e VVV_OWNER_ID=123456789012345678 \
  -v /path/to/minecraft/logs:/var/minecraft/logs:ro \
  vvv
```

### Docker Compose

```yaml
version: "3.8"
services:
  vvv:
    build: .
    environment:
      - VVV_DISCORD_TOKEN=${VVV_DISCORD_TOKEN}
      - VVV_LOG_PATH=/var/minecraft/logs/latest.log
      - VVV_RCON_PASSWORD=${VVV_RCON_PASSWORD}
      - VVV_OWNER_ID=${VVV_OWNER_ID}
    volumes:
      - /path/to/minecraft/logs:/var/minecraft/logs:ro
    restart: unless-stopped
```

### Data layout

| Path | Purpose |
|---|---|
| `$XDG_CONFIG_HOME/vvv/vvv.toml` | User configuration |
| `$XDG_STATE_HOME/vvv/vvv.redb` | redb database (bridge state, accounts, stats) |

---

## Architecture

```
src/
  main.rs          — Entry point: channel setup, task spawning, glue
  consts.rs        — Configuration loading (TOML + env vars, XDG paths)
  log_parser.rs    — Minecraft log line → structured event parsing
  playerdata.rs    — NBT player data parser, stats/advancements JSON, profile embeds
  rcon.rs          — RCON client with auto-reconnect
  stats.rs         — Player stats tracker (write-coalescing)
  storage.rs       — redb persistence layer (bridge state, accounts, stats)
  bot/
    mod.rs         — BotError enum, Context type alias
    types.rs       — Data, event types, Minecraft→Discord formatting
    handler.rs     — Framework setup, event_handler, MC→DC forwarding, welcome
    commands.rs    — All poise slash/prefix commands
```

### Data Flow

**Minecraft → Discord:**
```
latest.log line → log_parser::parse_log_line()
  → MinecraftEvent::to_stats_event() → stats tracker
  → MinecraftEvent::into_discord() → handler::forward_mc_events()
  → Discord channel
```

**Discord → Minecraft:**
```
Discord message → event_handler (Message event)
  → mute check, privacy check, silent prefix check
  → process_dc_mentions() (DC→MC mention translation)
  → dc_event_tx → spawn_dc_to_mc_relay()
  → RCON tellraw command → Minecraft server
```

---

## Testing

```bash
# Run all tests
cargo test

# Run with output
cargo test -- --nocapture

# Run clippy
cargo clippy
```

---

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| Bot doesn't start | Missing config field | Check `VVV_*` env vars or config file |
| "RCON connection failed" | Wrong address/password, or firewall | Verify `server.properties` and network |
| No events in Discord | Bridge not started | Run `~start_bridge` in the target channel |
| Duplicate "left the game" | Leave dedup not working | Check `RECENT_DISCONNECTS` in `log_parser.rs` |
| Stats not recording | Stats channel closed | Check `stats_tx` in `main.rs` |
| Slash commands not appearing | Global sync delay | Set `guild_id` in config for instant registration |
| `/connect` fails | Already linked | Use `/disconnect` first |
| Mentions not working | Account not linked or mention muted | Use `/connect` and check `/mutemention` |