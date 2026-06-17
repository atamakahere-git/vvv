use std::fmt::Write;

use base64::Engine;
use poise::serenity_prelude as serenity;

use super::{BotError, Context};

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
    ctx.say("UwU Helloo!").await?;
    Ok(())
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

    let query_address = &ctx.data().mc_server_address;

    tracing::info!(
        user = %ctx.author().name,
        server = %query_address,
        "command /info executed"
    );

    let rcon_guard = ctx.data().rcon_client.lock().await;
    let rcon_response = match rcon_guard.send_command("list") {
        Ok(res) => res,
        Err(e) => format!("Error executing RCON list: {e:?}"),
    };
    drop(rcon_guard);

    let parsed_players: Vec<String> =
        if let Some((_, names_blob)) = rcon_response.split_once("online:") {
            names_blob
                .split(',')
                .map(|name| name.trim().to_string())
                .filter(|name| !name.is_empty())
                .collect()
        } else {
            Vec::new()
        };

    let mut motd = "Minecraft Server Status".to_string();
    let mut latency_ms = 0.0;
    let mut favicon_b64: Option<String> = None;
    let mut total_players_online = 0;
    let mut max_players_limit = 20;

    if let Ok(status) = ctx
        .data()
        .mc_status_client
        .ping(query_address, rust_mc_status::ServerEdition::Java)
        .await
    {
        latency_ms = status.latency;
        if let rust_mc_status::ServerData::Java(java_data) = status.data {
            motd = java_data.description;
            favicon_b64 = java_data.favicon;
            total_players_online = java_data.players.online;
            max_players_limit = java_data.players.max;
        }
    }

    let mut embed_description = String::new();
    if parsed_players.is_empty() {
        if total_players_online > 0 {
            embed_description
                .push_str("⚠️ *Failed to safely map names via RCON, but players are active.*");
        } else {
            embed_description.push_str("*No players are currently online.*");
        }
    } else {
        embed_description.push_str("👥 **Current Online Players:**\n\n");
        for (index, player_name) in parsed_players.iter().enumerate() {
            let _ = writeln!(embed_description, "{}. `{player_name}`", index + 1);
        }
    }

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

    {
        let shared_list = &ctx.data().target_channel_id_list;
        let mut lock = shared_list.write().await;
        if !lock.contains(&current_channel_id) {
            lock.push(current_channel_id);
        }
    }

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
    let shared_list = &ctx.data().target_channel_id_list;

    let mut lock = shared_list.write().await;
    let len_before = lock.len();
    lock.retain(|&id| id != current_channel_id);
    let was_bridged = lock.len() < len_before;

    if was_bridged {
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
                .clone()
                .or_else(|| command.description.clone())
                .unwrap_or_else(|| {
                    "No detailed documentation available for this command.".to_string()
                });

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
            (
                format!("`~{}`", command.name),
                description.to_string(),
                false,
            )
        })
        .collect();

    let embed = serenity::CreateEmbed::new()
        .title("💥 Hello! こんにちは！~\n\n")
        .description("Here's a list of all the commands you can use:")
        .color(0x34_98db)
        .fields(embed_fields)
        .footer(serenity::CreateEmbedFooter::new(
            "Use ~command or \"hey reze, command\" to use any of these commands",
        ))
        .thumbnail("https://media1.giphy.com/media/v1.Y2lkPTc5MGI3NjExN3hja2kyZ3NqdXFxZHlzMWowNXdxcWtpMzA3aW9hNGVuNngwcDZ4OCZlcD12MV9pbnRlcm5hbF9naWZfYnlfaWQmY3Q9Zw/IKFVtPf8jP6KJH16dB/giphy.gif");

    ctx.send(poise::CreateReply::default().embed(embed)).await?;
    Ok(())
}
