use std::sync::LazyLock;

use regex::Regex;

/// Events extracted from Minecraft server log lines.
#[derive(Debug)]
pub enum MinecraftEvent {
    Chat {
        username: String,
        message: String,
    },
    Death {
        system_message: String,
    },
    PlayerJoinLeave {
        system_message: String,
        is_join: bool,
    },
    Advancement {
        system_message: String,
    },
}

const DEATH_PATTERNS: &[&str] = &[
    "was slain by",
    "was smashed by",
    "was impaled by",
    "was shot by",
    "was pummeled by",
    "was blown up by",
    "was skewered by",
    "was spit at by",
    "was struck by lightning",
    "was frozen to death",
    "was squashed by",
    "was squished too much",
    "was poked to death",
    "was pricked to death",
    "was doomed to fall",
    "fell from a high place",
    "hit the ground too hard",
    "fell out of the world",
    "didn't want to live",
    "experienced kinetic energy",
    "drowned",
    "suffocated in a wall",
    "starved to death",
    "burned to death",
    "went up in flames",
    "tried to swim in lava",
    "discovered the floor was lava",
    "withered away",
    "killed by magic",
    "left the confines of this world",
];

const IGNORE_PATTERNS: &[&str] = &[
    "lost connection:",
    "Logged in with entity id",
    "Saving chunks for level",
    "Stopping server",
    "Rcon connection from",
];

fn is_death_message(payload: &str) -> bool {
    DEATH_PATTERNS.iter().any(|p| payload.contains(p))
}

fn is_ignorable_system_message(payload: &str) -> bool {
    IGNORE_PATTERNS.iter().any(|p| payload.contains(p))
}

/// Parse a single Minecraft server log line into a structured event.
///
/// Returns `None` for unsupported or irrelevant lines (e.g. chunk saves, rcon connections).
pub fn parse_log_line(line: &str) -> Option<MinecraftEvent> {
    static CHAT_REGEX: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"^\[\d{2}:\d{2}:\d{2}\]\s\[[^\]]+/INFO\]:\s(?:\[Not Secure\]\s)?<(?P<username>[a-zA-Z0-9_]{3,16})>\s(?P<message>.+)$",
        )
        .unwrap()
    });

    static SYSTEM_REGEX: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^\[\d{2}:\d{2}:\d{2}\]\s\[[^\]]+/INFO\]:\s(?P<payload>.+)$").unwrap()
    });

    if let Some(captures) = CHAT_REGEX.captures(line) {
        let username = captures.name("username")?.as_str().to_string();
        let message = captures.name("message")?.as_str().to_string();
        return Some(MinecraftEvent::Chat { username, message });
    }

    if let Some(captures) = SYSTEM_REGEX.captures(line) {
        let payload = captures.name("payload")?.as_str();

        if payload.contains("joined the game") {
            return Some(MinecraftEvent::PlayerJoinLeave {
                system_message: payload.to_string(),
                is_join: true,
            });
        }

        if payload.contains("left the game") {
            return Some(MinecraftEvent::PlayerJoinLeave {
                system_message: payload.to_string(),
                is_join: false,
            });
        }

        if is_ignorable_system_message(payload) {
            return None;
        }

        if payload.contains("has made the advancement")
            || payload.contains("has completed the challenge")
        {
            return Some(MinecraftEvent::Advancement {
                system_message: payload.to_string(),
            });
        }

        if is_death_message(payload) {
            return Some(MinecraftEvent::Death {
                system_message: payload.to_string(),
            });
        }
    }

    None
}

/// Wraps the first word of a message in markdown bold (`**word**`).
pub fn bold_first_word(text: &str) -> String {
    if let Some((first_word, rest)) = text.split_once(' ') {
        format!("**{first_word}** {rest}")
    } else {
        format!("**{text}**")
    }
}
