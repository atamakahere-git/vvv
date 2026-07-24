use std::collections::HashMap;
use std::fmt::Write;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum PlayerDataError {
    #[error("player data file not found: {0}")]
    DataFileNotFound(String),
    #[error("failed to read file `{path}`: {source}")]
    ReadFile {
        path: String,
        source: std::io::Error,
    },
    #[error("failed to parse NBT data from `{path}`: {source}")]
    NbtParse {
        path: String,
        source: fastnbt::error::Error,
    },
    #[error("failed to parse JSON from `{path}`: {source}")]
    JsonParse {
        path: String,
        source: serde_json::Error,
    },
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct NbtPlayerData {
    #[serde(default)]
    pub data_version: i32,
    #[serde(rename = "Dimension", default)]
    pub dimension: String,
    #[serde(rename = "Pos", default)]
    pub pos: Vec<f64>,
    #[serde(rename = "Health", default)]
    pub health: f32,
    #[serde(rename = "AbsorptionAmount", default)]
    pub absorption: f32,
    #[serde(rename = "foodLevel", default)]
    pub food_level: i32,
    #[serde(rename = "foodSaturationLevel", default)]
    pub food_saturation: f32,
    #[serde(rename = "XpLevel", default)]
    pub xp_level: i32,
    #[serde(rename = "XpP", default)]
    pub xp_progress: f32,
    #[serde(rename = "XpTotal", default)]
    pub xp_total: i32,
    #[serde(rename = "Score", default)]
    pub score: i32,
    #[serde(rename = "playerGameType", default)]
    pub game_type: i32,
    #[serde(rename = "Inventory", default)]
    pub inventory: Vec<InventoryItem>,
    #[serde(rename = "EnderItems", default)]
    pub ender_items: Vec<InventoryItem>,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct InventoryItem {
    #[serde(rename = "Slot")]
    pub slot: i8,
    pub id: String,
    #[serde(alias = "Count")]
    pub count: i32,
    #[serde(default)]
    pub components: Option<ItemComponents>,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct ItemComponents {
    #[serde(rename = "minecraft:custom_name", default)]
    pub custom_name: Option<String>,
}

#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub struct DisplayItem {
    pub slot: i8,
    pub item_id: String,
    pub count: i32,
    pub display_name: String,
}

#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub struct MinecraftStats {
    pub play_time_ticks: u64,
    pub play_time_secs: u64,
    pub deaths: u64,
    pub mob_kills: u64,
    pub damage_dealt: u64,
    pub damage_taken: u64,
    pub distance_walked_cm: u64,
    pub distance_sprinted_cm: u64,
    pub distance_fallen_cm: u64,
    pub distance_flown_cm: u64,
    pub distance_swum_cm: u64,
    pub jumps: u64,
    pub animals_bred: u64,
    pub fish_caught: u64,
    pub times_slept: u64,
    pub mined: Vec<(String, u64)>,
    pub picked_up: Vec<(String, u64)>,
    pub used: Vec<(String, u64)>,
    pub dropped: Vec<(String, u64)>,
    pub crafted: Vec<(String, u64)>,
    pub broken: Vec<(String, u64)>,
    pub killed: Vec<(String, u64)>,
    pub killed_by: Vec<(String, u64)>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RawStatsJson {
    pub stats: Option<RawStats>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RawStats {
    #[serde(rename = "minecraft:mined", default)]
    pub mined: Option<HashMap<String, u64>>,
    #[serde(rename = "minecraft:picked_up", default)]
    pub picked_up: Option<HashMap<String, u64>>,
    #[serde(rename = "minecraft:used", default)]
    pub used: Option<HashMap<String, u64>>,
    #[serde(rename = "minecraft:dropped", default)]
    pub dropped: Option<HashMap<String, u64>>,
    #[serde(rename = "minecraft:crafted", default)]
    pub crafted: Option<HashMap<String, u64>>,
    #[serde(rename = "minecraft:broken", default)]
    pub broken: Option<HashMap<String, u64>>,
    #[serde(rename = "minecraft:killed", default)]
    pub killed: Option<HashMap<String, u64>>,
    #[serde(rename = "minecraft:killed_by", default)]
    pub killed_by: Option<HashMap<String, u64>>,
    #[serde(rename = "minecraft:custom", default)]
    pub custom: Option<HashMap<String, u64>>,
}

#[derive(Debug, Clone, Default)]
pub struct Advancements {
    pub total: usize,
    pub earned: usize,
    pub vanilla_total: usize,
    pub vanilla_earned: usize,
    pub modded_total: usize,
    pub modded_earned: usize,
    pub earned_list: Vec<(String, String)>,
    pub in_progress: Vec<(String, usize, usize)>,
}

pub type AdvancementMap = HashMap<String, AdvancementEntry>;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AdvancementEntry {
    #[serde(default)]
    pub done: bool,
    #[serde(default)]
    pub criteria: HashMap<String, String>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct PlayerProfile {
    pub username: String,
    pub uuid: String,
    pub player_data: Option<NbtPlayerData>,
    pub stats: Option<MinecraftStats>,
    pub advancements: Option<Advancements>,
}

fn read_file(path: &Path) -> Result<Vec<u8>, PlayerDataError> {
    let path_str = path.to_string_lossy().to_string();
    std::fs::read(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            PlayerDataError::DataFileNotFound(path_str)
        } else {
            PlayerDataError::ReadFile {
                path: path_str,
                source: e,
            }
        }
    })
}

pub fn load_nbt_player_data(
    world_dir: &Path,
    uuid: &str,
) -> Result<Option<NbtPlayerData>, PlayerDataError> {
    let file_path = world_dir.join("playerdata").join(format!("{uuid}.dat"));
    match read_file(&file_path) {
        Ok(data) => {
            let decoder = GzDecoder::new(Cursor::new(data));
            fastnbt::from_reader(decoder)
                .map(Some)
                .map_err(|e| PlayerDataError::NbtParse {
                    path: file_path.to_string_lossy().to_string(),
                    source: e,
                })
        }
        Err(PlayerDataError::DataFileNotFound(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

pub fn load_stats(world_dir: &Path, uuid: &str) -> Result<Option<MinecraftStats>, PlayerDataError> {
    let file_path = world_dir.join("stats").join(format!("{uuid}.json"));
    match read_file(&file_path) {
        Ok(data) => {
            let raw: RawStatsJson =
                serde_json::from_slice(&data).map_err(|e| PlayerDataError::JsonParse {
                    path: file_path.to_string_lossy().to_string(),
                    source: e,
                })?;
            Ok(raw.stats.map(transform_stats))
        }
        Err(PlayerDataError::DataFileNotFound(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

fn transform_stats(raw: RawStats) -> MinecraftStats {
    let custom = raw.custom.unwrap_or_default();

    MinecraftStats {
        play_time_ticks: custom.get("minecraft:play_time").copied().unwrap_or(0),
        play_time_secs: custom.get("minecraft:play_time").copied().unwrap_or(0) / 20,
        deaths: custom.get("minecraft:deaths").copied().unwrap_or(0),
        mob_kills: custom.get("minecraft:mob_kills").copied().unwrap_or(0),
        damage_dealt: custom.get("minecraft:damage_dealt").copied().unwrap_or(0) / 10,
        damage_taken: custom.get("minecraft:damage_taken").copied().unwrap_or(0) / 10,
        distance_walked_cm: custom.get("minecraft:walk_one_cm").copied().unwrap_or(0),
        distance_sprinted_cm: custom.get("minecraft:sprint_one_cm").copied().unwrap_or(0),
        distance_fallen_cm: custom.get("minecraft:fall_one_cm").copied().unwrap_or(0),
        distance_flown_cm: custom.get("minecraft:fly_one_cm").copied().unwrap_or(0),
        distance_swum_cm: custom.get("minecraft:swim_one_cm").copied().unwrap_or(0),
        jumps: custom.get("minecraft:jump").copied().unwrap_or(0),
        animals_bred: custom.get("minecraft:animals_bred").copied().unwrap_or(0),
        fish_caught: custom.get("minecraft:fish_caught").copied().unwrap_or(0),
        times_slept: custom.get("minecraft:sleep_in_bed").copied().unwrap_or(0),
        mined: sort_stats(raw.mined.unwrap_or_default()),
        picked_up: sort_stats(raw.picked_up.unwrap_or_default()),
        used: sort_stats(raw.used.unwrap_or_default()),
        dropped: sort_stats(raw.dropped.unwrap_or_default()),
        crafted: sort_stats(raw.crafted.unwrap_or_default()),
        broken: sort_stats(raw.broken.unwrap_or_default()),
        killed: sort_stats(raw.killed.unwrap_or_default()),
        killed_by: sort_stats(raw.killed_by.unwrap_or_default()),
    }
}

fn sort_stats(map: HashMap<String, u64>) -> Vec<(String, u64)> {
    let mut v: Vec<_> = map.into_iter().collect();
    v.sort_by_key(|&(_, count)| std::cmp::Reverse(count));
    v
}

pub fn load_advancements(
    world_dir: &Path,
    uuid: &str,
) -> Result<Option<Advancements>, PlayerDataError> {
    let file_path = world_dir.join("advancements").join(format!("{uuid}.json"));
    match read_file(&file_path) {
        Ok(data) => {
            let raw: serde_json::Value =
                serde_json::from_slice(&data).map_err(|e| PlayerDataError::JsonParse {
                    path: file_path.to_string_lossy().to_string(),
                    source: e,
                })?;

            let map: AdvancementMap = parse_advancement_map(&raw);

            Ok(Some(compute_advancements(&map)))
        }
        Err(PlayerDataError::DataFileNotFound(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

fn parse_advancement_map(raw: &serde_json::Value) -> AdvancementMap {
    let mut map = AdvancementMap::new();
    if let Some(obj) = raw.as_object() {
        for (key, value) in obj {
            if key == "DataVersion" {
                continue;
            }
            if let Ok(entry) = serde_json::from_value::<AdvancementEntry>(value.clone()) {
                map.insert(key.clone(), entry);
            }
        }
    }
    map
}

fn compute_advancements(map: &AdvancementMap) -> Advancements {
    let mut adv = Advancements::default();
    let mut earned = Vec::new();
    let mut in_progress = Vec::new();

    for (id, entry) in map {
        if id.starts_with("minecraft:recipes/") {
            continue;
        }

        adv.total += 1;
        let is_vanilla = id.starts_with("minecraft:");

        if is_vanilla {
            adv.vanilla_total += 1;
        } else {
            adv.modded_total += 1;
        }

        if entry.done {
            adv.earned += 1;
            if is_vanilla {
                adv.vanilla_earned += 1;
            } else {
                adv.modded_earned += 1;
            }

            let latest_criterion = entry.criteria.values().max().cloned().unwrap_or_default();
            earned.push((id.clone(), latest_criterion));
        } else if !entry.criteria.is_empty() {
            let completed = entry.criteria.len();
            if completed > 0 {
                in_progress.push((id.clone(), completed, 1));
            }
        }
    }

    earned.sort_by(|a, b| b.1.cmp(&a.1));

    adv.earned_list = earned;
    adv.in_progress = in_progress;

    adv
}

pub fn load_player_profile(world_dir: &Path, uuid: &str, username: &str) -> PlayerProfile {
    let player_data = match load_nbt_player_data(world_dir, uuid) {
        Ok(data) => data,
        Err(e) => {
            tracing::warn!(%uuid, %e, "failed to load NBT player data");
            None
        }
    };

    let stats = match load_stats(world_dir, uuid) {
        Ok(data) => data,
        Err(e) => {
            tracing::warn!(%uuid, %e, "failed to load stats");
            None
        }
    };

    let advancements = match load_advancements(world_dir, uuid) {
        Ok(data) => data,
        Err(e) => {
            tracing::warn!(%uuid, %e, "failed to load advancements");
            None
        }
    };

    PlayerProfile {
        username: username.to_string(),
        uuid: uuid.to_string(),
        player_data,
        stats,
        advancements,
    }
}

pub fn inventory_items(items: &[InventoryItem]) -> Vec<DisplayItem> {
    items
        .iter()
        .map(|item| {
            let display_name = item_display_name(&item.id, item.components.as_ref());
            DisplayItem {
                slot: item.slot,
                item_id: item.id.clone(),
                count: item.count,
                display_name,
            }
        })
        .collect()
}

fn extract_text_from_json(json_str: &str) -> String {
    let s = json_str.trim();
    if let Some(start) = s.find("\"text\"") {
        let after = &s[start + 6..];
        if let Some(col_start) = after.find(':') {
            let after_colon = after[col_start + 1..].trim();
            if let Some(inner) = after_colon.strip_prefix('"')
                && let Some(end) = inner.find('"')
            {
                return inner[..end].to_string();
            }
        }
    }
    json_str.to_string()
}

fn item_display_name(id: &str, components: Option<&ItemComponents>) -> String {
    if let Some(comp) = components
        && let Some(ref name_json) = comp.custom_name
    {
        extract_text_from_json(name_json)
    } else {
        id.strip_prefix("minecraft:")
            .unwrap_or(id)
            .replace('_', " ")
    }
}

pub fn format_dimension(dim: &str) -> &str {
    match dim {
        "minecraft:overworld" => "Overworld",
        "minecraft:the_nether" => "The Nether",
        "minecraft:the_end" => "The End",
        _ => dim,
    }
}

pub fn game_type_name(gt: i32) -> &'static str {
    match gt {
        0 => "Survival",
        1 => "Creative",
        2 => "Adventure",
        3 => "Spectator",
        _ => "Unknown",
    }
}

pub fn format_play_time(secs: u64) -> String {
    if secs == 0 {
        return "0m".to_string();
    }
    let days = secs / 86400;
    let hours = (secs % 86400) / 3600;
    let minutes = (secs % 3600) / 60;
    if days > 0 {
        format!("{days}d {hours}h {minutes}m")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

pub fn format_distance(cm: u64) -> String {
    if cm == 0 {
        return "0m".to_string();
    }
    let meters = cm / 100;
    if meters >= 1000 {
        format!("{:.1}km", meters as f64 / 1000.0)
    } else {
        format!("{meters}m")
    }
}

fn progress_bar(current: f32, max: f32, width: usize) -> String {
    let ratio = (current / max).clamp(0.0, 1.0);
    let filled = (ratio * width as f32).round() as usize;
    let mut bar = String::with_capacity(width);
    for i in 0..filled {
        if i == filled.saturating_sub(1) && filled > 1 {
            bar.push('▌');
        } else {
            bar.push('█');
        }
    }
    for _ in filled..width {
        bar.push('░');
    }
    bar
}

#[allow(clippy::too_many_lines)]
pub fn build_profile_embeds(
    profile: &PlayerProfile,
    redb_stats: Option<&crate::storage::PlayerStats>,
    daily_play_time: &[(String, u64)],
) -> Vec<poise::serenity_prelude::CreateEmbed> {
    let mut embeds = Vec::new();
    embeds.push(build_overview_embed(profile, redb_stats));
    embeds.push(build_stats_embed(profile, redb_stats, daily_play_time));
    if profile.advancements.is_some() {
        embeds.push(build_advancements_embed(profile));
    }
    embeds
}

fn build_overview_embed(
    profile: &PlayerProfile,
    redb_stats: Option<&crate::storage::PlayerStats>,
) -> poise::serenity_prelude::CreateEmbed {
    if let Some(ref pd) = profile.player_data {
        let health_bar = progress_bar(pd.health, 20.0, 20);
        let food_bar = progress_bar(pd.food_level as f32, 20.0, 20);
        let xp_percent = (pd.xp_progress * 100.0).round() as u32;

        let mut overview = String::new();
        let _ = writeln!(overview, "❤️ **Health:** {}/20", pd.health.ceil() as i32);
        let _ = writeln!(overview, "```{health_bar}```");
        if pd.absorption > 0.0 {
            let _ = writeln!(
                overview,
                "🟡 **Absorption:** {}",
                pd.absorption.ceil() as i32
            );
        }
        let _ = writeln!(overview, "🍖 **Hunger:** {}/20", pd.food_level);
        let _ = writeln!(overview, "```{food_bar}```");
        let _ = writeln!(
            overview,
            "⭐ **Level:** {}  •  {}% to next  •  {} total XP",
            pd.xp_level, xp_percent, pd.xp_total
        );
        let _ = writeln!(overview, "🎯 **Mode:** {}", game_type_name(pd.game_type));
        let _ = writeln!(
            overview,
            "🗺️ **Dimension:** {}",
            format_dimension(&pd.dimension)
        );
        if pd.pos.len() >= 3 {
            let _ = writeln!(
                overview,
                "📍 **Position:** {:.1}, {:.1}, {:.1}",
                pd.pos[0], pd.pos[1], pd.pos[2]
            );
        }
        let _ = writeln!(overview, "💯 **Score:** {}", pd.score);

        if let Some(rs) = redb_stats
            && (rs.total_logins > 0 || rs.total_messages > 0 || rs.total_commands > 0)
        {
            let _ = writeln!(overview);
            let _ = writeln!(overview, "📋 **Activity (tracked):**");
            let _ = writeln!(
                overview,
                "🔑 Logins: {}  •  💬 Messages: {}  •  ⌨️ Commands: {}",
                rs.total_logins, rs.total_messages, rs.total_commands
            );
            if rs.first_login_ts > 0 {
                let _ = writeln!(overview, "🕐 First seen: <t:{}:R>", rs.first_login_ts);
            }
            if rs.last_login_ts > 0 {
                let _ = writeln!(overview, "🟢 Last login: <t:{}:R>", rs.last_login_ts);
            }
        }

        let items = inventory_items(&pd.inventory);
        let used_slots = items.len();
        let total_slots: usize = 41;

        let mut inv_summary = String::new();
        let _ = writeln!(
            inv_summary,
            "🎒 **Inventory:** {used_slots}/{total_slots} slots used"
        );

        let show_items: Vec<&DisplayItem> = items.iter().take(8).collect();
        if !show_items.is_empty() {
            let _ = writeln!(inv_summary, "```");
            for item in &show_items {
                let count_str = if item.count > 1 {
                    format!("{}x ", item.count)
                } else {
                    String::new()
                };
                let _ = writeln!(inv_summary, "{count_str}{}", item.display_name);
            }
            if items.len() > 8 {
                let _ = writeln!(inv_summary, "... and {} more", items.len() - 8);
            }
            let _ = writeln!(inv_summary, "```");
        }

        poise::serenity_prelude::CreateEmbed::default()
            .title(format!("🎮 {}", profile.username))
            .color(0x00_AA00)
            .description(format!("{overview}\n{inv_summary}"))
    } else {
        poise::serenity_prelude::CreateEmbed::default()
            .title(format!("🎮 {}", profile.username))
            .color(0x00_AA00)
            .description(format!(
                "Player data file not yet available for **{}**.\nJoin the server to generate it.",
                profile.username
            ))
    }
}

fn build_stats_embed(
    profile: &PlayerProfile,
    redb_stats: Option<&crate::storage::PlayerStats>,
    daily_play_time: &[(String, u64)],
) -> poise::serenity_prelude::CreateEmbed {
    let file_stats = profile.stats.as_ref();

    let mut desc = String::new();

    if let Some(rs) = redb_stats {
        let playtime = format_play_time(rs.total_play_time_secs);
        let _ = writeln!(desc, "⏱️ **Play Time:** {playtime}");
        let _ = writeln!(
            desc,
            "💀 **Deaths:** {}  •  ⚔️ **Mob Kills:** {}  •  🔑 **Logins:** {}",
            rs.total_deaths,
            file_stats.map_or(0, |s| s.mob_kills),
            rs.total_logins
        );
        let _ = writeln!(
            desc,
            "💬 **Messages:** {}  •  ⌨️ **Commands:** {}  •  🏆 **Advancements:** {}",
            rs.total_messages, rs.total_commands, rs.total_advancements
        );
    } else if let Some(stats) = file_stats {
        let playtime = format_play_time(stats.play_time_secs);
        let _ = writeln!(desc, "⏱️ **Play Time:** {playtime}");
        let _ = writeln!(
            desc,
            "💀 **Deaths:** {}  •  ⚔️ **Mob Kills:** {}",
            stats.deaths, stats.mob_kills
        );
    }

    if let Some(stats) = file_stats {
        let walked = format_distance(stats.distance_walked_cm);
        let flown = format_distance(stats.distance_flown_cm);
        let swum = format_distance(stats.distance_swum_cm);

        let _ = writeln!(desc);
        let _ = writeln!(desc, "**Movement (from stats):**");
        let _ = writeln!(
            desc,
            "👣 Walked: {walked}  •  ✈️ Flown: {flown}  •  🏊 Swum: {swum}"
        );
        let _ = writeln!(desc, "🦘 Jumps: {}", fmt_num(stats.jumps));
        let _ = writeln!(
            desc,
            "🗡️ **Damage Dealt:** {}  •  💥 **Damage Taken:** {}",
            fmt_num(stats.damage_dealt),
            fmt_num(stats.damage_taken)
        );

        let top_killed = top_entries(&stats.killed, 5, "Nothing");
        let top_mined = top_entries(&stats.mined, 5, "Nothing");
        let _ = writeln!(desc);
        let _ = writeln!(desc, "🔝 **Top Killed:** {top_killed}");
        let _ = writeln!(desc, "⛏️ **Top Mined:** {top_mined}");
    }

    if !daily_play_time.is_empty() {
        let week_total: u64 = daily_play_time.iter().map(|(_, s)| *s).sum();
        let _ = writeln!(desc);
        let _ = writeln!(desc, "📅 **This Week:** {}", format_play_time(week_total));
        let _ = writeln!(desc, "```");
        for (date, secs) in daily_play_time {
            let bar = daily_bar(*secs);
            let _ = writeln!(desc, "{date}  {bar}");
        }
        let _ = writeln!(desc, "```");
    }

    if desc.is_empty() {
        desc.push_str("No statistics data available for this player.");
    }

    poise::serenity_prelude::CreateEmbed::default()
        .title(format!("📊 Statistics — {}", profile.username))
        .color(0x0034_98DB)
        .description(desc)
}

fn build_advancements_embed(profile: &PlayerProfile) -> poise::serenity_prelude::CreateEmbed {
    if let Some(ref adv) = profile.advancements {
        let overall_pct = if adv.total > 0 {
            (adv.earned as f64 / adv.total as f64 * 100.0) as u32
        } else {
            0
        };
        let vanilla_pct = if adv.vanilla_total > 0 {
            (adv.vanilla_earned as f64 / adv.vanilla_total as f64 * 100.0) as u32
        } else {
            0
        };
        let modded_pct = if adv.modded_total > 0 {
            (adv.modded_earned as f64 / adv.modded_total as f64 * 100.0) as u32
        } else {
            0
        };

        let mut desc = String::new();
        let _ = writeln!(
            desc,
            "📈 **Overall:** {}/{} ({}%)",
            adv.earned, adv.total, overall_pct
        );
        let _ = writeln!(
            desc,
            "🧱 **Vanilla:** {}/{} ({}%)",
            adv.vanilla_earned, adv.vanilla_total, vanilla_pct
        );
        if adv.modded_total > 0 {
            let _ = writeln!(
                desc,
                "✨ **Modded:** {}/{} ({}%)",
                adv.modded_earned, adv.modded_total, modded_pct
            );
        }

        if !adv.earned_list.is_empty() {
            let _ = writeln!(desc);
            let _ = writeln!(desc, "⭐ **Recently Earned:**");
            for (id, _) in adv.earned_list.iter().take(5) {
                let display = format_advancement_name(id);
                let _ = writeln!(desc, "• {display}");
            }
        }

        if !adv.in_progress.is_empty() {
            let _ = writeln!(desc);
            let _ = writeln!(desc, "📋 **In Progress:**");
            for (id, completed, total) in adv.in_progress.iter().take(5) {
                let display = format_advancement_name(id);
                let _ = writeln!(desc, "• {display} — {completed}/{total}");
            }
        }

        poise::serenity_prelude::CreateEmbed::default()
            .title(format!("🏆 Advancements — {}", profile.username))
            .color(0x00E6_7E22)
            .description(desc)
    } else {
        poise::serenity_prelude::CreateEmbed::default()
            .title(format!("🏆 Advancements — {}", profile.username))
            .color(0x00E6_7E22)
            .description("No advancement data available for this player.")
    }
}

fn fmt_num(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn daily_bar(secs: u64) -> String {
    if secs == 0 {
        return "        —".to_string();
    }
    let max_bar = secs.min(3600_u64);
    let filled = ((max_bar as f64 / 3600.0) * 8.0).ceil() as usize;
    let mut bar = String::with_capacity(16);
    bar.push_str(&format_play_time(secs));
    while bar.len() < 8 {
        bar.push(' ');
    }
    bar.push(' ');
    for _ in 0..filled {
        bar.push('█');
    }
    for _ in filled..8 {
        bar.push('░');
    }
    bar
}

fn top_entries(list: &[(String, u64)], count: usize, fallback: &str) -> String {
    if list.is_empty() {
        return fallback.to_string();
    }
    list.iter()
        .take(count)
        .map(|(k, v)| {
            let name = k.strip_prefix("minecraft:").unwrap_or(k).replace('_', " ");
            format!("{name} ({v})")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_advancement_name(id: &str) -> String {
    if let Some(name) = id.strip_prefix("minecraft:") {
        if let Some(short) = name.split('/').nth(1) {
            return short.replace('_', " ");
        }
        return name.replace('_', " ");
    }

    if let Some(name) = id.strip_prefix("blazeandcave:") {
        if let Some(short) = name.split('/').nth(1) {
            return format!("[BACAP] {}", short.replace('_', " "));
        }
        return format!("[BACAP] {}", name.replace('_', " "));
    }

    id.replace('_', " ")
}

pub fn world_dir_from_config(config_path: Option<&String>) -> Option<PathBuf> {
    config_path.map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn sample_world_dir() -> &'static Path {
        Path::new("playerdata")
    }

    #[test]
    fn parse_nbt_player_data() {
        let uuid = "0ea1daff-54aa-346f-9930-42c185cef5d2";
        let result = load_nbt_player_data(sample_world_dir(), uuid);
        let data = result.expect("should parse NBT file");
        assert!(data.is_some(), "player data should exist for UUID {uuid}");
        let pd = data.unwrap();
        assert!(pd.health >= 0.0, "health should be non-negative");
        assert!(pd.pos.len() == 3, "position should have 3 components");
        assert!(!pd.dimension.is_empty(), "dimension should not be empty");
        assert!(pd.xp_level >= 0, "xp_level should be non-negative");
    }

    #[test]
    fn parse_stats_json() {
        let uuid = "0ea1daff-54aa-346f-9930-42c185cef5d2";
        let result = load_stats(sample_world_dir(), uuid);
        let stats = result.expect("should parse stats JSON");
        assert!(stats.is_some(), "stats should exist for UUID {uuid}");
    }

    #[test]
    fn parse_advancements_json() {
        let uuid = "0ea1daff-54aa-346f-9930-42c185cef5d2";
        let result = load_advancements(sample_world_dir(), uuid);
        let adv = result.expect("should parse advancements JSON");
        assert!(adv.is_some(), "advancements should exist for UUID {uuid}");
        let a = adv.unwrap();
        assert!(a.total > 0, "advancements total should be > 0");
        assert!(a.vanilla_total > 0, "vanilla advancements should be > 0");
    }

    #[test]
    fn parse_modded_advancements() {
        let uuid = "8def70a4-897b-3416-8a84-d68b00190cd8";
        let result = load_advancements(sample_world_dir(), uuid);
        let adv = result.expect("should parse advancements JSON");
        let a = adv.unwrap();
        assert!(a.modded_total > 0, "should have modded advancements");
        assert!(a.vanilla_total > 0, "should have vanilla advancements");
    }

    #[test]
    fn load_full_profile() {
        let uuid = "0ea1daff-54aa-346f-9930-42c185cef5d2";
        let profile = load_player_profile(sample_world_dir(), uuid, "TestPlayer");
        assert_eq!(profile.username, "TestPlayer");
        assert_eq!(profile.uuid, uuid);
        assert!(profile.player_data.is_some());
        assert!(profile.stats.is_some());
        assert!(profile.advancements.is_some());
    }

    #[test]
    fn build_embeds_for_profile() {
        let uuid = "0ea1daff-54aa-346f-9930-42c185cef5d2";
        let profile = load_player_profile(sample_world_dir(), uuid, "TestPlayer");
        let embeds = build_profile_embeds(&profile, None, &[]);
        assert!(!embeds.is_empty(), "should produce embed pages");
    }

    #[test]
    fn missing_player_returns_none() {
        let result =
            load_nbt_player_data(sample_world_dir(), "00000000-0000-0000-0000-000000000000");
        let data = result.expect("should succeed");
        assert!(data.is_none(), "missing player should return None");
    }

    #[test]
    fn format_play_time_outputs() {
        assert_eq!(format_play_time(0), "0m");
        assert_eq!(format_play_time(90), "1m");
        assert_eq!(format_play_time(3661), "1h 1m");
        assert_eq!(format_play_time(90061), "1d 1h 1m");
    }

    #[test]
    fn format_distance_outputs() {
        assert_eq!(format_distance(0), "0m");
        assert_eq!(format_distance(5000), "50m");
        assert_eq!(format_distance(150_000), "1.5km");
    }

    #[test]
    fn item_display_name_from_id() {
        let name = item_display_name("minecraft:diamond_pickaxe", None);
        assert_eq!(name, "diamond pickaxe");
    }

    #[test]
    fn advancement_name_formatting() {
        let name = format_advancement_name("minecraft:story/mine_stone");
        assert_eq!(name, "mine stone");
        let name = format_advancement_name("blazeandcave:animal/which_came_first");
        assert_eq!(name, "[BACAP] which came first");
    }
}
