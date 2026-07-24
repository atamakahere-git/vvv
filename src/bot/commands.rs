use std::borrow::Cow;
use std::fmt::Write;
use std::time::Instant;

use base64::Engine;
use poise::serenity_prelude as serenity;
use rust_mc_status::StatusExt;

use super::types::PendingVerification;
use super::{BotError, Context};

fn url_to_hostport(url: &url::Url) -> Cow<'_, str> {
    let host = url.host_str().unwrap_or("localhost");
    match url.port() {
        Some(p) => Cow::Owned(format!("{host}:{p}")),
        None => Cow::Borrowed(host),
    }
}

fn ping_help() -> String {
    String::from("Use this to check if I'm alive!")
}

fn info_help() -> String {
    String::from("Get full detailed list of real-time online players and active server metadata.")
}

fn start_bridge_help() -> String {
    String::from("Use in a channel to bridge it with Minecraft chat")
}

/// Check if I'm alive!
#[poise::command(slash_command, prefix_command, help_text_fn = ping_help)]
pub async fn ping(ctx: Context<'_>) -> Result<(), BotError> {
    tracing::info!(user = %ctx.author().name, "command /ping executed");
    ctx.say("Villager's Verse Viaduct is online! ~help for commands.")
        .await?;
    Ok(())
}

fn parse_player_list(response: &str) -> Vec<&str> {
    let names_blob = response
        .split_once("online:")
        .map(|(_, rest)| rest)
        .or_else(|| {
            response.find("online").map(|pos| {
                response[pos + 6..]
                    .trim_start()
                    .trim_start_matches(&[':', '.', ' '][..])
            })
        });

    names_blob.map_or_else(Vec::new, |blob| {
        blob.split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .collect()
    })
}

/// Get list of online players in game right now.
#[poise::command(
    slash_command,
    prefix_command,
    aliases("players", "now_playing", "online_players"),
    help_text_fn = info_help
)]
pub async fn info(ctx: Context<'_>) -> Result<(), BotError> {
    ctx.defer()
        .await
        .inspect_err(|e| tracing::warn!("failed to defer interaction: {e}"))
        .ok();

    let query_address = url_to_hostport(&ctx.data().mc_server_address);

    tracing::info!(
        user = %ctx.author().name,
        server = %query_address,
        "command /info executed"
    );

    let rcon_response = match ctx
        .data()
        .rcon_client
        .send_command("list".to_string())
        .await
    {
        Ok(res) => res,
        Err(e) => format!("Error executing RCON list: {e:?}"),
    };

    let parsed_players = parse_player_list(&rcon_response);

    let (motd, latency_ms, favicon_b64, total_players_online, max_players_limit) = {
        let mut motd = String::from("Minecraft Server Status");
        let mut latency_ms = 0.0;
        let mut favicon_b64: Option<String> = None;
        let mut total_players_online = 0;
        let mut max_players_limit = 20;

        if let Ok(status) = ctx
            .data()
            .mc_status_client
            .java(query_address.as_ref())
            .await
        {
            latency_ms = status.latency_ms();
            motd.clone_from(&status.motd().to_owned());
            favicon_b64 = status.favicon().map(str::to_owned);
            total_players_online = status.players_online();
            max_players_limit = status.players_max();
        }
        (
            motd,
            latency_ms,
            favicon_b64,
            total_players_online,
            max_players_limit,
        )
    };

    let embed_description = if parsed_players.is_empty() {
        if total_players_online > 0 {
            String::from("⚠️ *Failed to safely map names via RCON, but players are active.*")
        } else {
            String::from("*No players are currently online.*")
        }
    } else {
        let mut desc = String::from("👥 **Current Online Players:**\n\n");
        for (index, player_name) in parsed_players.iter().enumerate() {
            let _ = writeln!(desc, "{}. `{player_name}`", index + 1);
        }
        desc
    };

    let (reply, embed) = {
        let mut embed = serenity::CreateEmbed::new()
            .title(format!("🎮 {motd}"))
            .description(embed_description)
            .color(0x9b5_9b6)
            .field(
                "Players Online",
                format!("`{total_players_online}/{max_players_limit}`"),
                true,
            )
            .field("Latency", format!("`{latency_ms:.1}ms`"), true);

        let mut reply = poise::CreateReply::default();

        if let Some(base64_data) = favicon_b64 {
            let clean_b64 = base64_data
                .strip_prefix("data:image/png;base64,")
                .unwrap_or(&base64_data);
            if let Ok(image_bytes) = base64::engine::general_purpose::STANDARD.decode(clean_b64) {
                let attachment = serenity::CreateAttachment::bytes(image_bytes, "server_icon.png");
                reply = reply.attachment(attachment);
                embed = embed.thumbnail("attachment://server_icon.png");
            }
        }
        (reply, embed)
    };

    ctx.send(reply.embed(embed)).await?;
    Ok(())
}

/// Link Minecraft log parsing events directly into this channel.
#[poise::command(
    slash_command,
    prefix_command,
    help_text_fn = start_bridge_help,
    check = "is_owner_or_admin"
)]
pub async fn start_bridge(ctx: Context<'_>) -> Result<(), BotError> {
    let current_channel_id = ctx.channel_id();
    let guild_id = ctx.guild_id().map_or(0, serenity::GuildId::get);

    {
        let bridge = &ctx.data().bridge_channel;
        let mut lock = bridge.write().await;
        *lock = Some(current_channel_id);
    }

    ctx.data()
        .storage
        .set_bridge_channel(current_channel_id.get(), guild_id)
        .await
        .inspect_err(|e| tracing::error!(%e, "failed to persist bridge binding"))
        .ok();

    tracing::info!(
        user = %ctx.author().name,
        channel = %current_channel_id,
        "bridge started"
    );

    ctx.say(format!(
        "🟢 **Bridge Established!** Minecraft chat will now sync to <#{current_channel_id}>."
    ))
    .await?;
    Ok(())
}

/// Sever the active live-chat stream connection in this channel.
#[poise::command(slash_command, prefix_command, check = "is_owner_or_admin")]
pub async fn stop_bridge(ctx: Context<'_>) -> Result<(), BotError> {
    let current_channel_id = ctx.channel_id();
    let bridge = &ctx.data().bridge_channel;

    let was_bridged = {
        let mut lock = bridge.write().await;
        let is_current = *lock == Some(current_channel_id);
        if is_current {
            *lock = None;
        }
        is_current
    };

    if was_bridged {
        ctx.data()
            .storage
            .clear_bridge_channel()
            .await
            .inspect_err(|e| tracing::error!(%e, "failed to persist bridge clear"))
            .ok();
        tracing::info!(
            user = %ctx.author().name,
            channel = %current_channel_id,
            "bridge stopped"
        );

        ctx.send(
            poise::CreateReply::default().embed(
                serenity::CreateEmbed::new()
                    .title("🛑 Bridge Severed!")
                    .description(format!(
                        "The live-chat stream to <#{current_channel_id}> has been disconnected."
                    ))
                    .color(0xe7_4c3c),
            ),
        )
        .await?;
    } else {
        ctx.say(format!(
            "❌ This channel (<#{current_channel_id}>) isn't currently bound to an active bridge."
        ))
        .await?;
    }
    Ok(())
}

fn leaderboard_help() -> String {
    String::from("Show the top 10 players by total play time.")
}

/// Format seconds as a human-readable duration (e.g. "1d 3h 46m").
fn format_duration(secs: u64) -> String {
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3_600;
    let minutes = (secs % 3_600) / 60;
    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{days}d"));
    }
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0 || parts.is_empty() {
        parts.push(format!("{minutes}m"));
    }
    parts.join(" ")
}

/// Show the top 10 players by total play time.
#[poise::command(
    slash_command,
    prefix_command,
    aliases("top", "rank"),
    help_text_fn = leaderboard_help
)]
pub async fn leaderboard(ctx: Context<'_>) -> Result<(), BotError> {
    tracing::info!(user = %ctx.author().name, "command /leaderboard executed");

    let storage = &ctx.data().storage;
    let all_stats = storage.get_all_player_stats().await.unwrap_or_default();

    if all_stats.is_empty() {
        ctx.say("❌ No player stats recorded yet.").await?;
        return Ok(());
    }

    let mut desc = String::new();
    for (i, (uuid, stats)) in all_stats.iter().take(10).enumerate() {
        let username = storage
            .resolve_username(uuid.clone())
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| uuid[..8].to_string());
        let _ = writeln!(
            desc,
            "{}. **{username}** — `{}`",
            i + 1,
            format_duration(stats.total_play_time_secs)
        );
    }

    let embed = serenity::CreateEmbed::new()
        .title("🏆 Play Time Leaderboard")
        .color(0xf1_c4_0f)
        .description(desc);

    ctx.send(poise::CreateReply::default().embed(embed)).await?;
    Ok(())
}

fn generate_verification_code() -> String {
    let value: u64 = rand::random();
    format!("{value:016X}")
}

fn is_valid_mc_username(name: &str) -> bool {
    (3..=16).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn connect_help() -> String {
    String::from(
        "Connect your Discord account with a Minecraft username for mentions and privacy features.",
    )
}

/// Connect your Discord account with a Minecraft username.
///
/// A random verification code will be generated and you must type
/// `@s CONFIRM-<code>` in Minecraft chat within 30 seconds to prove ownership.
#[poise::command(slash_command, prefix_command, help_text_fn = connect_help)]
pub async fn connect(
    ctx: Context<'_>,
    #[description = "Minecraft username"] mc_username: String,
) -> Result<(), BotError> {
    let discord_id = ctx.author().id.get();
    tracing::info!(
        user = %ctx.author().name,
        discord_id = %discord_id,
        mc_username = %mc_username,
        "command /connect executed"
    );

    if !ctx.data().storage.is_privacy_enabled().await {
        ctx.say("❌ Privacy features are currently disabled by the bot owner.")
            .await?;
        return Ok(());
    }

    if !is_valid_mc_username(&mc_username) {
        ctx.say("❌ Invalid Minecraft username. Must be 3–16 characters, using only letters, numbers, and underscores.").await?;
        return Ok(());
    }

    if ctx.data().storage.is_connected_dc(discord_id).await {
        ctx.say("❌ You are already connected to a Minecraft account. Use `/disconnect` first if you want to switch.".to_string())
            .await?;
        return Ok(());
    }

    if let Some(existing_dc) = ctx.data().storage.get_dc_from_mc(&mc_username).await {
        if existing_dc == discord_id {
            ctx.say("✅ You are already connected to this Minecraft account.")
                .await?;
            return Ok(());
        }
        ctx.say(format!(
            "❌ The Minecraft account `{mc_username}` is already linked to another Discord user."
        ))
        .await?;
        return Ok(());
    }

    {
        let guard = ctx.data().pending_verifications.lock().await;
        if guard.values().any(|v| v.discord_user_id == discord_id) {
            ctx.say("❌ You already have a pending verification. Wait for it to expire (30s) or complete it first.").await?;
            return Ok(());
        }
    }

    let code = generate_verification_code();

    {
        let mut guard = ctx.data().pending_verifications.lock().await;
        guard.insert(
            code.clone(),
            PendingVerification {
                discord_user_id: discord_id,
                mc_username: mc_username.clone(),
                expires_at: Instant::now() + std::time::Duration::from_secs(30),
                attempts: 0,
            },
        );
    }

    ctx.say(format!(
        "🔐 To verify you own `{mc_username}`, type this in Minecraft chat within **30 seconds**:\n\n```@s CONFIRM-{code}```"
    ))
    .await?;
    Ok(())
}

fn disconnect_help() -> String {
    String::from("Disconnect your Discord account from any linked Minecraft username.")
}

/// Remove the connection between your Discord account and Minecraft username.
#[poise::command(slash_command, prefix_command, help_text_fn = disconnect_help)]
pub async fn disconnect(ctx: Context<'_>) -> Result<(), BotError> {
    let discord_id = ctx.author().id.get();
    tracing::info!(
        user = %ctx.author().name,
        discord_id = %discord_id,
        "command /disconnect executed"
    );

    if !ctx.data().storage.is_connected_dc(discord_id).await {
        ctx.say(
            "❌ You haven't connected a Minecraft account yet. Use `/connect <username>` first.",
        )
        .await?;
        return Ok(());
    }

    let mc_username = ctx.data().storage.get_mc_from_dc(discord_id).await;
    ctx.data().storage.remove_connection(discord_id).await?;

    let msg = if let Some(mc) = mc_username {
        format!("🔓 Disconnected. Your Minecraft account **{mc}** is no longer linked.")
    } else {
        "🔓 Disconnected.".to_string()
    };

    ctx.say(msg).await?;
    Ok(())
}

fn alerts_join_help() -> String {
    String::from("Toggle join/leave announcements for your Minecraft activity.")
}

/// Toggle whether your Minecraft join/leave activity is broadcast in the bridge.
///
/// You must `/connect` first.  Shows current state after toggling.
#[poise::command(slash_command, prefix_command, help_text_fn = alerts_join_help)]
pub async fn alerts_join(ctx: Context<'_>) -> Result<(), BotError> {
    let discord_id = ctx.author().id.get();
    tracing::info!(user = %ctx.author().name, discord_id = %discord_id, "command /alerts join executed");

    if !ctx.data().storage.is_privacy_enabled().await {
        ctx.say("❌ Privacy features are currently disabled by the bot owner.")
            .await?;
        return Ok(());
    }

    if !ctx.data().storage.is_connected_dc(discord_id).await {
        ctx.say("❌ You must `/connect` your Minecraft account first.").await?;
        return Ok(());
    }

    let currently_opted_out = ctx.data().storage.is_join_leave_opted_out(discord_id).await;
    let new_state = !currently_opted_out;
    ctx.data()
        .storage
        .set_join_leave_optout(discord_id, new_state)
        .await?;

    if new_state {
        ctx.say("🔇 Your Minecraft activity will no longer be broadcast in the bridge.").await?;
    } else {
        ctx.say("🔊 Your Minecraft activity will now be broadcast in the bridge.").await?;
    }
    Ok(())
}

fn alerts_mentions_help() -> String {
    String::from("Toggle cross-chat mention pings when your MC name is mentioned.")
}

/// Toggle whether you are pinged in Discord when your Minecraft name is mentioned.
///
/// You must `/connect` first.  Shows current state after toggling.
#[poise::command(slash_command, prefix_command, help_text_fn = alerts_mentions_help)]
pub async fn alerts_mentions(ctx: Context<'_>) -> Result<(), BotError> {
    let discord_id = ctx.author().id.get();
    tracing::info!(user = %ctx.author().name, discord_id = %discord_id, "command /alerts mentions executed");

    if !ctx.data().storage.is_privacy_enabled().await {
        ctx.say("❌ Privacy features are currently disabled by the bot owner.")
            .await?;
        return Ok(());
    }

    if !ctx.data().storage.is_connected_dc(discord_id).await {
        ctx.say("❌ You must `/connect` your Minecraft account first.").await?;
        return Ok(());
    }

    let currently_muted = ctx.data().storage.is_mention_muted(discord_id).await;
    let new_state = !currently_muted;
    ctx.data()
        .storage
        .set_mute_mention(discord_id, new_state)
        .await?;

    if new_state {
        ctx.say("🔕 You will not be pinged when your Minecraft name is mentioned.").await?;
    } else {
        ctx.say("🔔 You will be pinged when your Minecraft name is mentioned.").await?;
    }
    Ok(())
}

fn connect_admin_help() -> String {
    String::from(
        "Manually link a Discord user to a Minecraft username (bypasses verification). Owner only.",
    )
}

/// Manually link a Discord user to a Minecraft username — bypasses verification.
///
/// Bot owner only.  Connects the mentioned Discord user to the given Minecraft
/// username immediately without requiring in-game verification.
#[poise::command(slash_command, prefix_command, help_text_fn = connect_admin_help, check = "is_owner")]
pub async fn connect_admin(
    ctx: Context<'_>,
    #[description = "Minecraft username"] mc_username: String,
    #[description = "Discord user to link"] discord_user: serenity::User,
) -> Result<(), BotError> {
    let discord_id = discord_user.id.get();
    tracing::info!(
        user = %ctx.author().name,
        target_discord = %discord_user.name,
        target_discord_id = %discord_id,
        mc_username = %mc_username,
        "command /connect_admin executed"
    );

    if !is_valid_mc_username(&mc_username) {
        ctx.say("❌ Invalid Minecraft username. Must be 3–16 characters, using only letters, numbers, and underscores.").await?;
        return Ok(());
    }

    if let Some(existing_dc) = ctx.data().storage.get_dc_from_mc(&mc_username).await {
        ctx.say(format!(
            "❌ The Minecraft account `{mc_username}` is already linked to <@{existing_dc}>."
        ))
        .await?;
        return Ok(());
    }

    match ctx
        .data()
        .storage
        .set_connection(discord_id, mc_username.clone())
        .await
    {
        Ok(()) => {
            tracing::info!(
                user = %ctx.author().name,
                discord_id,
                mc_username = %mc_username,
                "connect_admin: account linked"
            );
            ctx.say(format!("✅ Linked <@{discord_id}> → **{mc_username}**."))
                .await?;
        }
        Err(e) => {
            tracing::error!(%e, "connect_admin: failed to link account");
            ctx.say(format!("❌ Failed to link account: {e}")).await?;
        }
    }

    Ok(())
}

/// Parse a duration string like "5m", "1h", "1d", "30s" into seconds.
/// Returns None if the string is malformed.
fn parse_duration(input: &str) -> Option<u64> {
    let input = input.trim().to_lowercase();
    if input.is_empty() {
        return None;
    }
    let (num_str, unit) = input.split_at(input.len() - 1);
    let num: u64 = num_str.parse().ok()?;
    match unit {
        "s" => Some(num),
        "m" => Some(num * 60),
        "h" => Some(num * 3600),
        "d" => Some(num * 86400),
        _ => None,
    }
}

fn mute_help() -> String {
    String::from(
        "Mute a user from sending messages to the bridge. Duration format: 5m, 1h, 1d (default 5m).",
    )
}

/// Mute a Discord user from sending Discord → Minecraft bridge messages.
///
/// Owner or admin only.  The muted user's messages will still appear in Discord
/// but will not be relayed to the Minecraft server.  Use `/unmute` to reverse.
#[poise::command(slash_command, prefix_command, help_text_fn = mute_help, check = "is_owner_or_admin")]
pub async fn mute(
    ctx: Context<'_>,
    #[description = "Discord user to mute"] user: serenity::User,
    #[description = "Duration (e.g. 5m, 1h, 1d) — default 5m"] duration: Option<String>,
) -> Result<(), BotError> {
    let target_id = user.id.get();
    let duration_str = duration.as_deref().unwrap_or("5m");
    tracing::info!(
        user = %ctx.author().name,
        target = %user.name,
        target_id = %target_id,
        duration = %duration_str,
        "command /mute executed"
    );

    let secs = parse_duration(duration_str).unwrap_or_else(|| {
        tracing::warn!(input = %duration_str, "invalid duration format, falling back to 5m");
        300
    });

    ctx.data().storage.set_muted(target_id, secs).await?;

    let duration_display = if secs == 0 {
        "permanently".to_string()
    } else {
        let minutes = secs / 60;
        let hours = minutes / 60;
        if hours > 0 && minutes.is_multiple_of(60) {
            format!("{hours}h")
        } else if minutes > 0 {
            format!("{minutes}m")
        } else {
            format!("{secs}s")
        }
    };

    tracing::info!(
        target = %user.name,
        target_id = %target_id,
        duration = %duration_display,
        "mute applied"
    );
    ctx.say(format!(
        "🔇 **{muted}** is muted from the bridge for **{duration_display}**.",
        muted = user.name,
        duration_display = duration_display,
    ))
    .await?;

    Ok(())
}

fn unmute_help() -> String {
    String::from("Unmute a user who was previously muted from the bridge.")
}

/// Unmute a Discord user, re-enabling their Discord → Minecraft bridge messages.
///
/// Owner or admin only.
#[poise::command(slash_command, prefix_command, help_text_fn = unmute_help, check = "is_owner_or_admin")]
pub async fn unmute(
    ctx: Context<'_>,
    #[description = "Discord user to unmute"] user: serenity::User,
) -> Result<(), BotError> {
    let target_id = user.id.get();
    tracing::info!(
        user = %ctx.author().name,
        target = %user.name,
        target_id = %target_id,
        "command /unmute executed"
    );

    ctx.data().storage.unmute_user(target_id).await?;

    tracing::info!(
        target = %user.name,
        target_id = %target_id,
        "unmute applied"
    );
    ctx.say(format!(
        "🔊 **{unmuted}** is no longer muted from the bridge.",
        unmuted = user.name,
    ))
    .await?;

    Ok(())
}

/// Verify the command invoker is the bot owner only (not server admins).
pub async fn is_owner(ctx: Context<'_>) -> Result<bool, BotError> {
    if ctx.framework().options().owners.contains(&ctx.author().id) {
        return Ok(true);
    }
    ctx.say("❌ **Access Denied:** This command is restricted to the Bot Owner.")
        .await?;
    Ok(false)
}

fn privacy_help() -> String {
    String::from("Enable or disable all privacy features (connection gating, filtering, mentions).")
}

/// Enable or disable the privacy features globally.
///
/// Bot owner only.  When disabled the bridge runs as an open pipe in both
/// directions; when enabled connected users can opt in/out and mentions work.
#[poise::command(slash_command, prefix_command, help_text_fn = privacy_help, check = "is_owner")]
pub async fn privacy(
    ctx: Context<'_>,
    #[description = "enable or disable"] action: Option<String>,
) -> Result<(), BotError> {
    let action = action.as_deref().unwrap_or("status");

    match action.to_lowercase().as_str() {
        "enable" | "on" => {
            ctx.data().storage.set_privacy_enabled(true).await?;
            tracing::info!(user = %ctx.author().name, "privacy features enabled");
            ctx.say("🔒 **Privacy features enabled.**\n\nDC→MC requires /connect, MC→DC respects /alerts settings, and cross-chat mentions are active.").await?;
        }
        "disable" | "off" => {
            ctx.data().storage.set_privacy_enabled(false).await?;
            tracing::info!(user = %ctx.author().name, "privacy features disabled");
            ctx.say("🔓 **Privacy features disabled.**\n\nThe bridge is an open pipe in both directions.").await?;
        }
        _ => {
            let enabled = ctx.data().storage.is_privacy_enabled().await;
            let status = if enabled {
                "🔒 enabled"
            } else {
                "🔓 disabled"
            };
            ctx.say(format!("**Privacy features** are currently **{status}**.\n\nUse `/privacy enable` or `/privacy disable` to toggle.")).await?;
        }
    }

    Ok(())
}

/// Verify the command invoker is the bot owner or a server administrator.
pub async fn is_owner_or_admin(ctx: Context<'_>) -> Result<bool, BotError> {
    if ctx.framework().options().owners.contains(&ctx.author().id) {
        return Ok(true);
    }

    let Some(guild_id) = ctx.guild_id() else {
        ctx.say("❌ **Access Denied:** This command is restricted to the Bot Owner and Server Administrators.").await?;
        return Ok(false);
    };

    let Some(member) = ctx.author_member().await else {
        ctx.say("❌ **Access Denied:** This command is restricted to the Bot Owner and Server Administrators.").await?;
        return Ok(false);
    };

    // Scoped so CacheRef is dropped before the .await below (Send requirement)
    let is_admin = guild_id
        .to_guild_cached(ctx.serenity_context())
        .is_some_and(|guild| {
            guild
                .member_permissions(&member)
                .contains(serenity::Permissions::ADMINISTRATOR)
        });

    if is_admin {
        return Ok(true);
    }

    ctx.say("❌ **Access Denied:** This command is restricted to the Bot Owner and Server Administrators.").await?;
    Ok(false)
}

/// View player profile: stats, advancements, and inventory dashboard.
#[poise::command(slash_command, prefix_command)]
pub async fn profile(
    ctx: Context<'_>,
    #[description = "Minecraft username (leave empty for your account)"] username: Option<String>,
) -> Result<(), BotError> {
    tracing::info!(
        user = %ctx.author().name,
        target = ?username,
        "command /profile executed"
    );

    if !ctx.data().storage.is_player_profile_enabled().await {
        ctx.say("🚫 **Profile dashboard is disabled by the bot owner.**").await?;
        return Ok(());
    }

    let resolved_username = if let Some(name) = username {
        name
    } else {
        let Some(mc_name) = ctx
            .data()
            .storage
            .get_mc_from_dc(ctx.author().id.get())
            .await
        else {
            ctx.say("❌ No Minecraft account linked. Use `/connect <username>` to link or provide a username: `/profile <username>`").await?;
            return Ok(());
        };
        mc_name
    };

    let uuid = ctx
        .data()
        .storage
        .resolve_uuid(resolved_username.to_lowercase())
        .await
        .ok()
        .flatten();

    let Some(uuid) = uuid else {
        ctx.say(format!(
            "❌ UUID not found for **{resolved_username}** in the database. The player may not have joined the server yet."
        ))
        .await?;
        return Ok(());
    };

    let Some(ref world_dir) = ctx.data().world_directory else {
        ctx.say("❌ World directory not configured. Set `minecraft.world_directory` in config or `VVV_MC_WORLD_DIRECTORY` env var.").await?;
        return Ok(());
    };

    ctx.defer()
        .await
        .inspect_err(|e| tracing::warn!("failed to defer interaction: {e}"))
        .ok();

    let storage = &ctx.data().storage;
    let redb_stats = storage
        .get_player_stats(uuid.clone())
        .await
        .ok()
        .flatten();

    let now = chrono::Local::now();
    let dates: Vec<String> = (0..7)
        .map(|i| {
            (now - chrono::Duration::days(i))
                .format("%Y-%m-%d")
                .to_string()
        })
        .collect();

    let daily = storage
        .get_recent_play_time(uuid.clone(), dates)
        .await
        .unwrap_or_default();

    let profile = crate::playerdata::load_player_profile(world_dir, &uuid, &resolved_username);

    let embeds = crate::playerdata::build_profile_embeds(&profile, redb_stats.as_ref(), &daily);

    let mut reply = poise::CreateReply::default();
    for embed in embeds {
        reply = reply.embed(embed);
    }

    ctx.send(reply).await?;

    Ok(())
}

fn profile_toggle_help() -> String {
    String::from("Enable or disable the player profile dashboard feature.")
}

/// Toggle the player profile dashboard on or off.
///
/// Bot owner only.  When disabled, /profile returns a "disabled by admin" message.
#[poise::command(slash_command, prefix_command, help_text_fn = profile_toggle_help, check = "is_owner")]
pub async fn profile_toggle(
    ctx: Context<'_>,
    #[description = "enable, disable, or status"] action: Option<String>,
) -> Result<(), BotError> {
    let action = action.as_deref().unwrap_or("status");

    match action.to_lowercase().as_str() {
        "enable" | "on" => {
            ctx.data()
                .storage
                .set_player_profile_enabled(true)
                .await?;
            tracing::info!(user = %ctx.author().name, "profile dashboard enabled");
            ctx.say("📊 **Player profile dashboard enabled.**\n\nPlayers can now use `/profile` to view detailed stats and advancements.")
                .await?;
        }
        "disable" | "off" => {
            ctx.data()
                .storage
                .set_player_profile_enabled(false)
                .await?;
            tracing::info!(user = %ctx.author().name, "profile dashboard disabled");
            ctx.say("🚫 **Player profile dashboard disabled.**\n\nThe `/profile` command is now unavailable to all users.")
                .await?;
        }
        _ => {
            let enabled = ctx.data().storage.is_player_profile_enabled().await;
            let status = if enabled {
                "📊 enabled"
            } else {
                "🚫 disabled"
            };
            ctx.say(format!(
                "**Player profile dashboard** is currently **{status}**.\n\nUse `/profile_toggle enable` or `/profile_toggle disable` to toggle."
            ))
            .await?;
        }
    }

    Ok(())
}

/// Show all available commands or get detailed help for a specific one.
#[poise::command(slash_command, prefix_command)]
pub async fn help(ctx: Context<'_>, command_name: Option<String>) -> Result<(), BotError> {
    tracing::info!(
        user = %ctx.author().name,
        target = ?command_name,
        "command /help executed"
    );

    if let Some(target) = command_name {
        if let Some(command) = ctx
            .framework()
            .options()
            .commands
            .iter()
            .find(|c| c.name == target)
        {
            let detailed_help = command
                .help_text
                .as_deref()
                .or(command.description.as_deref())
                .unwrap_or("No detailed documentation available for this command.");

            ctx.send(
                poise::CreateReply::default().embed(
                    serenity::CreateEmbed::new()
                        .title(format!("ℹ️ Detailed Help: /{}", command.name))
                        .description(detailed_help)
                        .color(0x34_98db),
                ),
            )
            .await?;
            return Ok(());
        }
        ctx.say(format!("❌ Command `{target}` not found.")).await?;
        return Ok(());
    }

    let embed_fields: Vec<_> = ctx
        .framework()
        .options()
        .commands
        .iter()
        .map(|command| {
            let description = command
                .description
                .as_deref()
                .unwrap_or("No description provided.");
            (format!("`~{}`", command.name), description, false)
        })
        .collect();

    let mut embed = serenity::CreateEmbed::new()
        .title("💥 Hello! こんにちは！~\n\n")
        .description("Here's a list of all the commands you can use:")
        .color(0x34_98db)
        .fields(embed_fields)
        .thumbnail("https://media1.giphy.com/media/v1.Y2lkPTc5MGI3NjExN3hja2kyZ3NqdXFxZHlzMWowNXdxcWtpMzA3aW9hNGVuNngwcDZ4OCZlcD12MV9pbnRlcm5hbF9naWZfYnlfaWQmY3Q9Zw/IKFVtPf8jP6KJH16dB/giphy.gif");

    if !ctx.data().storage.is_privacy_enabled().await {
        embed = embed.footer(serenity::CreateEmbedFooter::new(
            "🔓 Privacy features are disabled by the bot owner. Connect/sub/mute commands have no effect.",
        ));
    } else if !ctx
        .data()
        .storage
        .is_connected_dc(ctx.author().id.get())
        .await
    {
        embed = embed.footer(serenity::CreateEmbedFooter::new(
            "💡 Use /connect <mc-username> to link your account — required for your messages to reach Minecraft.",
        ));
    } else {
        embed = embed.footer(serenity::CreateEmbedFooter::new(
            "Use ~command to use any of these commands",
        ));
    }

    ctx.send(poise::CreateReply::default().embed(embed)).await?;
    Ok(())
}
