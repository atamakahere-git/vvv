<img src="https://i.pinimg.com/originals/5d/15/4b/5d154b68de57a87600fe9b98d692802c.gif" alt="Reze from Chainsaw Man" width="250"/>

# 💥 RUZE

RUZE is a lightweight, high-performance Discord-Minecraft bridge bot themed around Reze from *Chainsaw Man*. Built in Rust using the `poise` and `serenity` frameworks, RUZE establishes a seamless, two-way communication channel between your Discord server and a Minecraft server.

Unlike traditional bridges, RUZE uses direct log-tailing to read Minecraft server events without requiring any mods or plugins installed on the server itself.

---

## Features

* **Zero-Client Bridge:** Uses `linemux` to tail and parse local Minecraft server logs natively. No client mods, Forge, Fabric, or Paper plugins required!
* **Live Chat Sync:** Forwards Minecraft in-game chat to Discord and vice versa.
* **Server Events:** Automatically formats and beams Minecraft **achievements** and **death messages** directly into your Discord channel.
* **Reze Aesthetics:** Includes beautifully styled, character-themed embeds and interactive help menus.
* **Secure Access:** Features a custom validation gate restricting critical bridge management commands to the Bot Owner and Server Administrators.

---

## Prerequisites & Server Configuration

To allow RUZE to communicate with your Minecraft server, you must enable **RCON** (for sending Discord messages into Minecraft and querying player lists) and **Query** (for server status updates) in your Minecraft server's configuration.

### 1. Update `server.properties`

Open your Minecraft server's `server.properties` file and ensure the following options are set:

```properties
# Enable RCON (Remote Control)
enable-rcon=true
rcon.port=25575
rcon.password=your_secure_rcon_password_here

# Enable Query Port
enable-query=true
query.port=25565

```

*Restart your Minecraft server after saving these changes.*

---

## Required Environment Variables

RUZE requires a `.env` file in the root directory of the project to manage paths, API tokens, and security credentials securely. **All of the following variables must be configured for the bot to start.**

Create a file named `.env` and populate it with your configuration:

```env
# Path to your Minecraft server's active log file
LOG_PATH=/path/to/your/minecraft/logs/latest.log

# Your Discord Bot Application Token
DISCORD_TOKEN=your_discord_bot_token_here

# The RCON password set in your server.properties
RCON_PASSWORD=your_secure_rcon_password_here

# The IP and Port configuration for RCON (use localhost:25575 if hosted on the same machine)
RCON_SERVER_ADDRESS=localhost:25575

```

---

## Getting Started

### 1. Enable Privileged Gateway Intents

Because RUZE utilizes event loops to listen for server members joining and standard text commands, you must enable **Privileged Gateway Intents** in the Discord Developer Portal:

1. Go to the [Discord Developer Portal](https://www.google.com/search?q=https://discord.com/developers/applications).
2. Select your Application and navigate to the **Bot** tab.
3. Scroll down to **Privileged Gateway Intents** and turn **ON** the **Server Members Intent** and **Message Content Intent**.

### 2. Compile and Run

Ensure you have the Rust toolchain installed. Clone the repository, navigate to the folder, and execute:

```bash
cargo run --release

```

---

## Bot Commands

All standard commands utilize the `~` prefix or can be invoked via Slash Commands.

* `~help` — Displays a beautiful, filtered menu of all available commands.
* `~ping`  — Checks if the bot is alive and operational.
* `~online_players`  — Leverages RCON to query the Minecraft server and returns a clean list of all players currently active in-game.
* `~start_bridge` — (*Owner/Admin Only*) Binds the active `linemux` log stream to the current Discord channel to initiate the live chat bridge.

```
```
