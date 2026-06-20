use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use regex::Regex;

/// Tracks recent disconnects so the subsequent "left the game" can be suppressed.
static RECENT_DISCONNECTS: LazyLock<Mutex<HashMap<String, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Debug, PartialEq)]
pub enum MinecraftEvent {
    Chat {
        username: String,
        message: String,
    },
    Join {
        username: String,
    },
    Leave {
        username: String,
    },
    Disconnect {
        username: String,
        reason: String,
    },
    Death {
        username: String,
        message: String,
    },
    Advancement {
        username: String,
        advancement: String,
    },
    Command {
        username: String,
        command: String,
    },
    ServerSay {
        message: String,
    },
    PlayerList {
        current: u32,
        max: u32,
        players: Vec<String>,
    },
    ServerStart,
    ServerStop,
    SaveComplete,
}

const DEATH_PATTERNS: &[&str] = &[
    "was slain by",
    "was shot by",
    "was blown up by",
    "was smashed by",
    "was impaled on",
    "was impaled by",
    "was pummeled by",
    "was skewered by",
    "was squashed by",
    "was fireballed by",
    "was speared by",
    "was stung to death",
    "was poked to death",
    "was pricked to death",
    "was burned to a crisp",
    "was struck by lightning",
    "was frozen to death",
    "was obliterated",
    "was roasted in dragon's breath",
    "was killed",
    "burned to death",
    "went up in flames",
    "went off with a bang",
    "drowned",
    "starved to death",
    "suffocated in a wall",
    "fell from a high place",
    "fell out of the world",
    "fell while climbing",
    "fell off",
    "hit the ground too hard",
    "experienced kinetic energy",
    "didn't want to live",
    "was doomed to fall",
    "tried to swim in lava",
    "discovered the floor was lava",
    "died because not just the floor is lava",
    "withered away",
    "killed by magic",
    "froze to death",
    "left the confines of this world",
    "was squished too much",
    "blew up",
    "walked into",
];

const IGNORE_PATTERNS: &[&str] = &[
    "[Rcon:",
    "[AuthMe]",
    "UUID of player",
    "Logged in with entity id",
    "Saving chunks for level",
    "Rcon connection from",
    "[bootstrap]",
    "[PluginInitializerManager]",
    "Environment:",
    "Loaded ",
    "Starting json RPC",
    "Json-RPC Management",
    "Preparing level",
    "Done (",
    "was kicked due to",
];

const PRIVATE_COMMAND_PATTERNS: &[&str] = &[
    "/msg", "/tell", "/w", "/whisper", "/reply", "/r", "/teammsg", "/tm", "/me",
];

fn is_death_message(payload: &str) -> bool {
    DEATH_PATTERNS.iter().any(|p| payload.contains(p))
}

fn is_ignorable_system_message(payload: &str) -> bool {
    IGNORE_PATTERNS.iter().any(|p| payload.contains(p))
}

fn is_private_command(command: &str) -> bool {
    PRIVATE_COMMAND_PATTERNS.iter().any(|&prefix| {
        command == prefix
            || command
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with(' '))
    })
}

const SILENT_PREFIXES: &[&str] = &["@silent", "@s"];

/// DC→MC: checks if text starts with @silent or @s (exact or followed by space).
pub fn is_silent_message_prefix(text: &str) -> bool {
    SILENT_PREFIXES.iter().any(|&prefix| {
        text.get(..prefix.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
            && (text.len() == prefix.len() || text.as_bytes()[prefix.len()] == b' ')
    })
}

/// MC→DC: checks if @s appears as a standalone token anywhere in the message.
pub fn contains_silent_token(text: &str) -> bool {
    text.split_whitespace()
        .any(|w| w.eq_ignore_ascii_case("@s"))
}

pub fn parse_log_line(line: &str) -> Option<MinecraftEvent> {
    if let Some(event) = try_chat(line) {
        return Some(event);
    }
    if let Some(event) = try_server_say(line) {
        return Some(event);
    }

    let payload = extract_system_payload(line)?;

    if is_ignorable_system_message(payload) {
        tracing::trace!("ignored system line: {payload}");
        return None;
    }

    if let Some(event) = try_death(payload) {
        return Some(event);
    }
    if let Some(event) = try_join(payload) {
        return Some(event);
    }
    if let Some(event) = try_leave(payload) {
        return Some(event);
    }
    if let Some(event) = try_disconnect(payload) {
        return Some(event);
    }
    if let Some(event) = try_command(payload) {
        return Some(event);
    }
    if let Some(event) = try_advancement(payload) {
        return Some(event);
    }
    if let Some(event) = try_player_list(payload) {
        return Some(event);
    }
    if let Some(event) = try_server_start(payload) {
        return Some(event);
    }
    if let Some(event) = try_server_stop(payload) {
        return Some(event);
    }
    if let Some(event) = try_save_complete(payload) {
        return Some(event);
    }

    None
}

fn try_chat(line: &str) -> Option<MinecraftEvent> {
    static REGEX: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"^\[\d{2}:\d{2}:\d{2}\]\s\[[^\]]+/INFO\]:\s(?:\[Not Secure\]\s)?<(?P<username>[a-zA-Z0-9_]{3,16})>\s(?P<message>.+)$",
        )
        .expect("valid static chat regex pattern")
    });

    let captures = REGEX.captures(line)?;
    let username = captures.name("username")?.as_str().to_owned();
    let message = captures.name("message")?.as_str().to_owned();
    if contains_silent_token(&message) {
        tracing::debug!(%username, %message, "ignored silent chat message");
        return None;
    }
    tracing::debug!(%username, "chat event parsed");
    Some(MinecraftEvent::Chat { username, message })
}

fn try_server_say(line: &str) -> Option<MinecraftEvent> {
    static REGEX: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"^\[\d{2}:\d{2}:\d{2}\]\s\[[^\]]+/INFO\]:\s(?:\[Not Secure\]\s)?\[Server\]\s(?P<message>.+)$",
        )
        .expect("valid static server-say regex pattern")
    });

    let captures = REGEX.captures(line)?;
    let message = captures.name("message")?.as_str().to_owned();
    tracing::debug!("server-say event parsed");
    Some(MinecraftEvent::ServerSay { message })
}

fn extract_system_payload(line: &str) -> Option<&str> {
    static REGEX: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^\[\d{2}:\d{2}:\d{2}\]\s\[[^\]]+/INFO\]:\s(?P<payload>.+)$")
            .expect("valid static system regex pattern")
    });

    let captures = REGEX.captures(line)?;
    captures.name("payload").map(|m| m.as_str())
}

fn try_death(payload: &str) -> Option<MinecraftEvent> {
    static EXTRACT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(?P<username>[a-zA-Z0-9_]{3,16})\s(?P<message>.+)$")
            .expect("valid static death-extract regex pattern")
    });

    if !is_death_message(payload) {
        return None;
    }

    let captures = EXTRACT.captures(payload)?;
    let username = captures.name("username")?.as_str().to_owned();
    let message = captures.name("message")?.as_str().to_owned();
    tracing::info!(%username, "death event parsed");
    Some(MinecraftEvent::Death { username, message })
}

fn try_join(payload: &str) -> Option<MinecraftEvent> {
    static REGEX: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(?P<username>[a-zA-Z0-9_]{3,16})\sjoined the game$")
            .expect("valid static join regex pattern")
    });

    let captures = REGEX.captures(payload)?;
    let username = captures.name("username")?.as_str().to_owned();
    tracing::info!(%username, "player joined");
    Some(MinecraftEvent::Join { username })
}

fn try_leave(payload: &str) -> Option<MinecraftEvent> {
    static REGEX: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(?P<username>[a-zA-Z0-9_]{3,16})\sleft the game$")
            .expect("valid static leave regex pattern")
    });

    let captures = REGEX.captures(payload)?;
    let username = captures.name("username")?.as_str().to_owned();

    // Suppress "left the game" when a "lost connection" just preceded it.
    if let Ok(mut map) = RECENT_DISCONNECTS.lock()
        && map.remove(&username).is_some()
    {
        tracing::info!(%username, "suppressed leave after disconnect");
        return None;
    }

    tracing::info!(%username, "player left");
    Some(MinecraftEvent::Leave { username })
}

fn try_disconnect(payload: &str) -> Option<MinecraftEvent> {
    static REGEX: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(?P<username>[a-zA-Z0-9_]{3,16})\slost connection:\s(?P<reason>.+)$")
            .expect("valid static disconnect regex pattern")
    });

    let captures = REGEX.captures(payload)?;
    let username = captures.name("username")?.as_str().to_owned();
    let reason = captures.name("reason")?.as_str().to_owned();

    if let Ok(mut map) = RECENT_DISCONNECTS.lock() {
        map.insert(username.clone(), reason.clone());
    }

    tracing::info!(%username, %reason, "player disconnected");
    Some(MinecraftEvent::Disconnect { username, reason })
}

fn try_command(payload: &str) -> Option<MinecraftEvent> {
    static REGEX: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"^(?P<username>[a-zA-Z0-9_]{3,16})\sissued server command:\s(?P<command>/[^\n]+)$",
        )
        .expect("valid static command regex pattern")
    });

    let captures = REGEX.captures(payload)?;
    let username = captures.name("username")?.as_str().to_owned();
    if username == "SERVER" {
        tracing::debug!("ignored SERVER command");
        return None;
    }
    let command = captures.name("command")?.as_str().to_owned();
    if is_private_command(&command) {
        tracing::debug!(%username, %command, "ignored private command");
        return None;
    }
    tracing::info!(%username, %command, "command event parsed");
    Some(MinecraftEvent::Command { username, command })
}

fn try_advancement(payload: &str) -> Option<MinecraftEvent> {
    static REGEX: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"^(?P<username>[a-zA-Z0-9_]{3,16})\shas (?:made the advancement|completed the challenge)\s\[(?P<advancement>.+)\]$",
        )
        .expect("valid static advancement regex pattern")
    });

    let captures = REGEX.captures(payload)?;
    let username = captures.name("username")?.as_str().to_owned();
    let advancement = captures.name("advancement")?.as_str().to_owned();
    tracing::info!(%username, %advancement, "advancement earned");
    Some(MinecraftEvent::Advancement {
        username,
        advancement,
    })
}

fn try_player_list(payload: &str) -> Option<MinecraftEvent> {
    static REGEX: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"^There are (?P<current>\d+) of a max of (?P<max>\d+) players online:?\s*(?P<players>.+)?$",
        )
        .expect("valid static player-list regex pattern")
    });

    let captures = REGEX.captures(payload)?;
    let current: u32 = captures.name("current")?.as_str().parse().ok()?;
    let max: u32 = captures.name("max")?.as_str().parse().ok()?;
    let players = captures
        .name("players")
        .map(|m| {
            m.as_str()
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    tracing::debug!(current, max, ?players, "player list event");
    Some(MinecraftEvent::PlayerList {
        current,
        max,
        players,
    })
}

fn try_server_start(payload: &str) -> Option<MinecraftEvent> {
    if payload.starts_with("Starting minecraft server version") {
        tracing::info!("server starting detected");
        Some(MinecraftEvent::ServerStart)
    } else {
        None
    }
}

fn try_server_stop(payload: &str) -> Option<MinecraftEvent> {
    if payload == "Stopping the server" || payload == "Stopping server" {
        tracing::warn!("server stopping detected");
        Some(MinecraftEvent::ServerStop)
    } else {
        None
    }
}

fn try_save_complete(payload: &str) -> Option<MinecraftEvent> {
    if payload == "Saved the game" {
        tracing::debug!("world save detected");
        Some(MinecraftEvent::SaveComplete)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_chat_message() {
        let line = r"[12:11:32] [Async Chat Thread - #3/INFO]: [Not Secure] <karambit> I will put them inbox";
        let event = parse_log_line(line).unwrap();
        assert_eq!(
            event,
            MinecraftEvent::Chat {
                username: "karambit".into(),
                message: "I will put them inbox".into()
            }
        );
    }

    #[test]
    fn parse_chat_without_not_secure() {
        let line = r"[12:11:32] [Async Chat Thread - #3/INFO]: <karambit> hello world";
        let event = parse_log_line(line).unwrap();
        assert_eq!(
            event,
            MinecraftEvent::Chat {
                username: "karambit".into(),
                message: "hello world".into()
            }
        );
    }

    #[test]
    fn parse_server_say() {
        let line = r"[10:12:46] [Server thread/INFO]: [Not Secure] [Server] playing still?";
        let event = parse_log_line(line).unwrap();
        assert_eq!(
            event,
            MinecraftEvent::ServerSay {
                message: "playing still?".into()
            }
        );
    }

    #[test]
    fn parse_join() {
        let line = r"[02:20:03] [Server thread/INFO]: Vodka_not_Rum joined the game";
        let event = parse_log_line(line).unwrap();
        assert_eq!(
            event,
            MinecraftEvent::Join {
                username: "Vodka_not_Rum".into()
            }
        );
    }

    #[test]
    fn parse_leave() {
        let line = r"[02:19:55] [Server thread/INFO]: Vodka_not_Rum left the game";
        let event = parse_log_line(line).unwrap();
        assert_eq!(
            event,
            MinecraftEvent::Leave {
                username: "Vodka_not_Rum".into()
            }
        );
    }

    #[test]
    fn parse_disconnect() {
        let line = r"[00:52:15] [Server thread/INFO]: atamaka lost connection: Timed out";
        let event = parse_log_line(line).unwrap();
        assert_eq!(
            event,
            MinecraftEvent::Disconnect {
                username: "atamaka".into(),
                reason: "Timed out".into()
            }
        );
    }

    #[test]
    fn parse_disconnect_multiple_ips() {
        let line = r"[12:00:01] [Server thread/INFO]: test_user lost connection: Login timeout exceeded, you have been kicked from the server, please try again!";
        let event = parse_log_line(line).unwrap();
        assert!(matches!(event, MinecraftEvent::Disconnect { .. }));
        if let MinecraftEvent::Disconnect { username, reason } = event {
            assert_eq!(username, "test_user");
            assert!(reason.contains("Login timeout"));
        }
    }

    #[test]
    fn parse_disconnect_same_username() {
        let line = r"[12:00:01] [Server thread/INFO]: test_user lost connection: The same username is already playing on the server!";
        let event = parse_log_line(line).unwrap();
        assert!(matches!(event, MinecraftEvent::Disconnect { .. }));
    }

    #[test]
    fn parse_leave_after_disconnect_is_suppressed() {
        RECENT_DISCONNECTS.lock().unwrap().clear();

        let dc_line = r"[00:52:15] [Server thread/INFO]: dedup_user lost connection: Test reason";
        let event = parse_log_line(dc_line).unwrap();
        assert!(matches!(event, MinecraftEvent::Disconnect { .. }));

        let leave_line = r"[00:52:16] [Server thread/INFO]: dedup_user left the game";
        assert!(parse_log_line(leave_line).is_none());
    }

    #[test]
    fn parse_death_slain() {
        let line = r"[02:22:16] [Server thread/INFO]: tess was slain by Vodka_not_Rum";
        let event = parse_log_line(line).unwrap();
        assert_eq!(
            event,
            MinecraftEvent::Death {
                username: "tess".into(),
                message: "was slain by Vodka_not_Rum".into()
            }
        );
    }

    #[test]
    fn parse_death_kinetic_energy() {
        let line = r"[04:42:23] [Server thread/INFO]: karambit experienced kinetic energy while trying to escape Endermite";
        let event = parse_log_line(line).unwrap();
        assert_eq!(
            event,
            MinecraftEvent::Death {
                username: "karambit".into(),
                message: "experienced kinetic energy while trying to escape Endermite".into()
            }
        );
    }

    #[test]
    fn parse_death_froze() {
        let line = r"[14:13:56] [Server thread/INFO]: karambit froze to death";
        let event = parse_log_line(line).unwrap();
        assert_eq!(
            event,
            MinecraftEvent::Death {
                username: "karambit".into(),
                message: "froze to death".into()
            }
        );
    }

    #[test]
    fn parse_death_burned_to_crisp() {
        let line = r"[02:46:06] [Server thread/INFO]: Vodka_not_Rum was burned to a crisp while fighting Nami";
        let event = parse_log_line(line).unwrap();
        assert_eq!(
            event,
            MinecraftEvent::Death {
                username: "Vodka_not_Rum".into(),
                message: "was burned to a crisp while fighting Nami".into()
            }
        );
    }

    #[test]
    fn parse_death_drowned() {
        let line = r"[12:00:01] [Server thread/INFO]: TestPlayer drowned";
        let event = parse_log_line(line).unwrap();
        assert_eq!(
            event,
            MinecraftEvent::Death {
                username: "TestPlayer".into(),
                message: "drowned".into()
            }
        );
    }

    #[test]
    fn parse_death_fell_from_high_place() {
        let line = r"[12:00:01] [Server thread/INFO]: TestPlayer fell from a high place";
        let event = parse_log_line(line).unwrap();
        assert_eq!(
            event,
            MinecraftEvent::Death {
                username: "TestPlayer".into(),
                message: "fell from a high place".into()
            }
        );
    }

    #[test]
    fn parse_death_impaled_on_stalagmite() {
        let line = r"[12:00:01] [Server thread/INFO]: TestPlayer was impaled on a stalagmite";
        let event = parse_log_line(line).unwrap();
        assert_eq!(
            event,
            MinecraftEvent::Death {
                username: "TestPlayer".into(),
                message: "was impaled on a stalagmite".into()
            }
        );
    }

    #[test]
    fn parse_death_blew_up() {
        let line = r"[12:00:01] [Server thread/INFO]: TestPlayer blew up";
        let event = parse_log_line(line).unwrap();
        assert_eq!(
            event,
            MinecraftEvent::Death {
                username: "TestPlayer".into(),
                message: "blew up".into()
            }
        );
    }

    #[test]
    fn parse_death_fireballed() {
        let line = r"[12:00:01] [Server thread/INFO]: TestPlayer was fireballed by Ghast";
        let event = parse_log_line(line).unwrap();
        assert_eq!(
            event,
            MinecraftEvent::Death {
                username: "TestPlayer".into(),
                message: "was fireballed by Ghast".into()
            }
        );
    }

    #[test]
    fn parse_death_speared() {
        let line = r"[12:00:01] [Server thread/INFO]: TestPlayer was speared by Zombie";
        let event = parse_log_line(line).unwrap();
        assert_eq!(
            event,
            MinecraftEvent::Death {
                username: "TestPlayer".into(),
                message: "was speared by Zombie".into()
            }
        );
    }

    #[test]
    fn parse_death_stung() {
        let line = r"[12:00:01] [Server thread/INFO]: TestPlayer was stung to death";
        let event = parse_log_line(line).unwrap();
        assert_eq!(
            event,
            MinecraftEvent::Death {
                username: "TestPlayer".into(),
                message: "was stung to death".into()
            }
        );
    }

    #[test]
    fn parse_death_obliterated() {
        let line = r"[12:00:01] [Server thread/INFO]: TestPlayer was obliterated by a sonically-charged shriek";
        let event = parse_log_line(line).unwrap();
        assert_eq!(
            event,
            MinecraftEvent::Death {
                username: "TestPlayer".into(),
                message: "was obliterated by a sonically-charged shriek".into()
            }
        );
    }

    #[test]
    fn parse_death_roasted() {
        let line = r"[12:00:01] [Server thread/INFO]: TestPlayer was roasted in dragon's breath";
        let event = parse_log_line(line).unwrap();
        assert_eq!(
            event,
            MinecraftEvent::Death {
                username: "TestPlayer".into(),
                message: "was roasted in dragon's breath".into()
            }
        );
    }

    #[test]
    fn parse_death_went_off_with_a_bang() {
        let line = r"[12:00:01] [Server thread/INFO]: TestPlayer went off with a bang";
        let event = parse_log_line(line).unwrap();
        assert_eq!(
            event,
            MinecraftEvent::Death {
                username: "TestPlayer".into(),
                message: "went off with a bang".into()
            }
        );
    }

    #[test]
    fn parse_death_floor_is_lava() {
        let line =
            r"[12:00:01] [Server thread/INFO]: TestPlayer died because not just the floor is lava";
        let event = parse_log_line(line).unwrap();
        assert_eq!(
            event,
            MinecraftEvent::Death {
                username: "TestPlayer".into(),
                message: "died because not just the floor is lava".into()
            }
        );
    }

    #[test]
    fn parse_death_was_killed() {
        let line = r"[12:00:01] [Server thread/INFO]: TestPlayer was killed";
        let event = parse_log_line(line).unwrap();
        assert_eq!(
            event,
            MinecraftEvent::Death {
                username: "TestPlayer".into(),
                message: "was killed".into()
            }
        );
    }

    #[test]
    fn parse_death_fell_while_climbing() {
        let line = r"[12:00:01] [Server thread/INFO]: TestPlayer fell while climbing";
        let event = parse_log_line(line).unwrap();
        assert_eq!(
            event,
            MinecraftEvent::Death {
                username: "TestPlayer".into(),
                message: "fell while climbing".into()
            }
        );
    }

    #[test]
    fn parse_death_fell_off_scaffolding() {
        let line = r"[12:00:01] [Server thread/INFO]: TestPlayer fell off scaffolding";
        let event = parse_log_line(line).unwrap();
        assert_eq!(
            event,
            MinecraftEvent::Death {
                username: "TestPlayer".into(),
                message: "fell off scaffolding".into()
            }
        );
    }

    #[test]
    fn parse_death_walked_into_fire() {
        let line =
            r"[12:00:01] [Server thread/INFO]: TestPlayer walked into fire while fighting Zombie";
        let event = parse_log_line(line).unwrap();
        assert_eq!(
            event,
            MinecraftEvent::Death {
                username: "TestPlayer".into(),
                message: "walked into fire while fighting Zombie".into()
            }
        );
    }

    #[test]
    fn parse_death_walked_into_cactus() {
        let line = r"[12:00:01] [Server thread/INFO]: TestPlayer walked into a cactus while trying to escape Zombie";
        let event = parse_log_line(line).unwrap();
        assert_eq!(
            event,
            MinecraftEvent::Death {
                username: "TestPlayer".into(),
                message: "walked into a cactus while trying to escape Zombie".into()
            }
        );
    }

    #[test]
    fn parse_command() {
        let line = r"[12:00:01] [Server thread/INFO]: atamaka issued server command: /tp nava39";
        let event = parse_log_line(line).unwrap();
        assert_eq!(
            event,
            MinecraftEvent::Command {
                username: "atamaka".into(),
                command: "/tp nava39".into()
            }
        );
    }

    #[test]
    fn parse_advancement() {
        let line =
            r"[12:00:01] [Server thread/INFO]: TestPlayer has made the advancement [Stone Age]";
        let event = parse_log_line(line).unwrap();
        assert_eq!(
            event,
            MinecraftEvent::Advancement {
                username: "TestPlayer".into(),
                advancement: "Stone Age".into()
            }
        );
    }

    #[test]
    fn parse_advancement_challenge() {
        let line = r"[12:00:01] [Server thread/INFO]: TestPlayer has completed the challenge [Cover Me in Debris]";
        let event = parse_log_line(line).unwrap();
        assert_eq!(
            event,
            MinecraftEvent::Advancement {
                username: "TestPlayer".into(),
                advancement: "Cover Me in Debris".into()
            }
        );
    }

    #[test]
    fn parse_player_list() {
        let line = r"[12:00:01] [Server thread/INFO]: There are 2 of a max of 10 players online: atamaka, nava39";
        let event = parse_log_line(line).unwrap();
        assert_eq!(
            event,
            MinecraftEvent::PlayerList {
                current: 2,
                max: 10,
                players: vec!["atamaka".into(), "nava39".into()]
            }
        );
    }

    #[test]
    fn parse_player_list_empty() {
        let line = r"[12:00:01] [Server thread/INFO]: There are 0 of a max of 10 players online:";
        let event = parse_log_line(line).unwrap();
        assert_eq!(
            event,
            MinecraftEvent::PlayerList {
                current: 0,
                max: 10,
                players: vec![]
            }
        );
    }

    #[test]
    fn parse_player_list_empty_no_colon() {
        let line = r"[12:00:01] [Server thread/INFO]: There are 0 of a max of 10 players online";
        let event = parse_log_line(line).unwrap();
        assert_eq!(
            event,
            MinecraftEvent::PlayerList {
                current: 0,
                max: 10,
                players: vec![]
            }
        );
    }

    #[test]
    fn parse_server_start() {
        let line = r"[12:58:05] [Server thread/INFO]: Starting minecraft server version 1.21.11";
        let event = parse_log_line(line).unwrap();
        assert_eq!(event, MinecraftEvent::ServerStart);
    }

    #[test]
    fn parse_server_stop() {
        let line = r"[19:55:43] [Server thread/INFO]: Stopping the server";
        let event = parse_log_line(line).unwrap();
        assert_eq!(event, MinecraftEvent::ServerStop);
    }

    #[test]
    fn parse_server_stop_variant() {
        let line = r"[19:55:44] [Server thread/INFO]: Stopping server";
        let event = parse_log_line(line).unwrap();
        assert_eq!(event, MinecraftEvent::ServerStop);
    }

    #[test]
    fn parse_save_complete() {
        let line = r"[12:00:01] [Server thread/INFO]: Saved the game";
        let event = parse_log_line(line).unwrap();
        assert_eq!(event, MinecraftEvent::SaveComplete);
    }

    #[test]
    fn ignore_rcon_command() {
        let line = r"[10:17:33] [Server thread/INFO]: [Rcon: Saved the game]";
        assert!(parse_log_line(line).is_none());
    }

    #[test]
    fn ignore_bootstrap() {
        let line = r"[12:57:38] [ServerMain/INFO]: [bootstrap] Running Java 25 on Linux 7.0.4+deb13-amd64 (amd64)";
        assert!(parse_log_line(line).is_none());
    }

    #[test]
    fn ignore_plugins() {
        let line =
            r"[12:57:53] [ServerMain/INFO]: [PluginInitializerManager] Initialized 7 plugins";
        assert!(parse_log_line(line).is_none());
    }

    #[test]
    fn ignore_uuid_lookup() {
        let line = r"[02:20:02] [User Authenticator #206/INFO]: UUID of player Vodka_not_Rum is 9e78961d-0a81-3dd7-b80f-d1abf718d3e8";
        assert!(parse_log_line(line).is_none());
    }

    #[test]
    fn ignore_entity_login() {
        let line = r"[02:20:03] [Server thread/INFO]: Vodka_not_Rum[/[2401:9640:...]:9570] logged in with entity id 1624273 at ([world_nether]462.2, 128.0, 275.2)";
        assert!(parse_log_line(line).is_none());
    }

    #[test]
    fn ignore_saving_chunks() {
        let line = r"[Server thread/INFO]: Saving chunks for level 'ServerLevel[world]'/minecraft:overworld";
        assert!(parse_log_line(line).is_none());
    }

    #[test]
    fn ignore_done_startup() {
        let line = "[12:58:16] [Server thread/INFO]: Done (39.180s)! For help, type \"help\"";
        assert!(parse_log_line(line).is_none());
    }

    #[test]
    fn ignore_warning_moved_wrongly() {
        let line = r"[00:00:58] [Server thread/WARN]: atamaka moved wrongly!";
        assert!(parse_log_line(line).is_none());
    }

    #[test]
    fn ignore_server_command_say() {
        let line = r"[Server thread/INFO]: SERVER issued server command: /say hi";
        assert!(parse_log_line(line).is_none());
    }

    #[test]
    fn ignore_arbitrary_line() {
        assert!(parse_log_line("garbage").is_none());
    }
}
