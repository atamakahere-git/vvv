use std::collections::HashMap;
use std::sync::Arc;

use chrono::{TimeZone, Utc};
use chrono_tz::Tz;
use tokio::sync::mpsc::Receiver;
use tokio::time::{Duration, interval};

use crate::log_parser::StatsEvent;
use crate::rcon::ReconnectingRcon;
use crate::storage::{DailyTimeSplit, PlayerDelta, Storage};

/// Accumulated deltas and session state for the stats tracker.
struct PendingState {
    uuid_cache: HashMap<String, String>,
    online_sessions: HashMap<String, i64>,
    pending_deltas: HashMap<String, PlayerDelta>,
    pending_daily: HashMap<String, u64>,
}

impl PendingState {
    fn new() -> Self {
        Self {
            uuid_cache: HashMap::new(),
            online_sessions: HashMap::new(),
            pending_deltas: HashMap::new(),
            pending_daily: HashMap::new(),
        }
    }

    fn resolve_uuid(&self, username: &str) -> String {
        self.uuid_cache
            .get(username)
            .cloned()
            .unwrap_or_else(|| username.to_string())
    }
}

/// Background task that records player stats from Minecraft events into redb.
///
/// Uses write-coalescing: in-memory accumulators are flushed to redb every 60
/// seconds with `Durability::None` (no fsync), or immediately with
/// `Durability::Immediate` on player leave / server stop.
pub struct StatsTracker {
    storage: Arc<Storage>,
    stats_rx: Receiver<StatsEvent>,
    rcon: Arc<ReconnectingRcon>,
    tz: Tz,
    state: PendingState,
}

impl StatsTracker {
    pub fn new(
        storage: Arc<Storage>,
        stats_rx: Receiver<StatsEvent>,
        rcon: Arc<ReconnectingRcon>,
        tz: Tz,
    ) -> Self {
        Self {
            storage,
            stats_rx,
            rcon,
            tz,
            state: PendingState::new(),
        }
    }

    /// Run the event loop, consuming stats events until the channel closes.
    pub async fn run(mut self) {
        let mut flush_timer = interval(Duration::from_mins(1));
        flush_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                Some(event) = self.stats_rx.recv() => {
                    self.handle_event(event).await;
                }
                _ = flush_timer.tick() => {
                    self.flush(redb::Durability::None).await;
                }
            }
        }
    }

    async fn handle_event(&mut self, event: StatsEvent) {
        match event {
            StatsEvent::UuidResolved { username, uuid } => {
                self.state.uuid_cache.insert(username.clone(), uuid.clone());
                if let Err(e) = self.storage.store_uuid_mapping(username, uuid).await {
                    tracing::warn!(%e, "failed to persist uuid mapping");
                }
            }
            StatsEvent::Join { username } => {
                self.handle_join(&username).await;
            }
            StatsEvent::Leave { username } | StatsEvent::Disconnect { username, .. } => {
                self.handle_leave(&username).await;
            }
            StatsEvent::Death { username } => {
                let uuid = self.state.resolve_uuid(&username);
                self.state.pending_deltas.entry(uuid).or_default().deaths += 1;
            }
            StatsEvent::Advancement { username, .. } => {
                let uuid = self.state.resolve_uuid(&username);
                self.state
                    .pending_deltas
                    .entry(uuid)
                    .or_default()
                    .advancements += 1;
            }
            StatsEvent::Chat { username } => {
                let uuid = self.state.resolve_uuid(&username);
                self.state.pending_deltas.entry(uuid).or_default().messages += 1;
            }
            StatsEvent::Command { username } => {
                let uuid = self.state.resolve_uuid(&username);
                self.state.pending_deltas.entry(uuid).or_default().commands += 1;
            }
            StatsEvent::ServerStart => {
                tracing::info!("stats tracker: server starting");
            }
            StatsEvent::ServerStop => {
                tracing::info!("stats tracker: server stopping, flushing all sessions");
                self.flush_all_sessions();
                self.flush(redb::Durability::Immediate).await;
            }
        }
    }

    async fn handle_join(&mut self, username: &str) {
        let now = Utc::now().timestamp();
        self.state.online_sessions.insert(username.to_string(), now);

        let uuid = self.state.resolve_uuid(username);
        let delta = self.state.pending_deltas.entry(uuid.clone()).or_default();
        delta.last_login_ts = Some(now);
        delta.total_logins_delta += 1;

        let stats = self.storage.get_player_stats(uuid.clone()).await;
        let existing = stats.ok().flatten();
        delta.is_new_player = existing.is_none();

        if let Some(stats) = &existing {
            self.send_login_reminder(username, stats.last_login_ts);
        } else {
            self.send_new_player_welcome(username);
        }
    }

    async fn handle_leave(&mut self, username: &str) {
        let Some(session_start) = self.state.online_sessions.remove(username) else {
            return;
        };

        let now = Utc::now().timestamp();
        let session_secs = u64::try_from((now - session_start).max(0)).unwrap_or(0);
        let uuid = self.state.resolve_uuid(username);

        let delta = self.state.pending_deltas.entry(uuid.clone()).or_default();
        delta.play_time_secs += session_secs;
        delta.last_logout_ts = Some(now);

        for (date, secs) in split_session_by_day(session_start, now, self.tz) {
            *self
                .state
                .pending_daily
                .entry(format!("{uuid}\x1f{date}"))
                .or_insert(0) += secs;
        }

        self.flush(redb::Durability::Immediate).await;
    }

    fn flush_all_sessions(&mut self) {
        let now = Utc::now().timestamp();
        let usernames: Vec<String> = self.state.online_sessions.keys().cloned().collect();
        for username in &usernames {
            if let Some(&session_start) = self.state.online_sessions.get(username) {
                let session_secs = u64::try_from((now - session_start).max(0)).unwrap_or(0);
                let uuid = self.state.resolve_uuid(username);
                let delta = self.state.pending_deltas.entry(uuid.clone()).or_default();
                delta.play_time_secs += session_secs;
                delta.last_logout_ts = Some(now);
                for (date, secs) in split_session_by_day(session_start, now, self.tz) {
                    *self
                        .state
                        .pending_daily
                        .entry(format!("{uuid}\x1f{date}"))
                        .or_insert(0) += secs;
                }
            }
        }
        self.state.online_sessions.clear();
    }

    async fn flush(&mut self, durability: redb::Durability) {
        if self.state.pending_deltas.is_empty() && self.state.pending_daily.is_empty() {
            return;
        }

        let deltas: Vec<_> = self.state.pending_deltas.drain().collect();
        let daily_splits: Vec<(String, DailyTimeSplit)> = self
            .state
            .pending_daily
            .drain()
            .filter_map(|(combined, secs)| {
                combined.split_once('\x1f').map(|(uuid, date)| {
                    (
                        uuid.to_string(),
                        DailyTimeSplit {
                            date: date.to_string(),
                            secs,
                        },
                    )
                })
            })
            .collect();

        if let Err(e) = self
            .storage
            .flush_player_deltas(deltas, daily_splits, durability)
            .await
        {
            tracing::error!(%e, "stats flush failed");
        }
    }

    fn send_login_reminder(&self, username: &str, last_login_ts: i64) {
        let last_login_str = if last_login_ts > 0 {
            let dt = self.tz.timestamp_opt(last_login_ts, 0).single();
            dt.map_or("unknown".to_string(), |dt| {
                dt.format("%Y-%m-%d %H:%M %Z").to_string()
            })
        } else {
            "unknown".to_string()
        };

        let yesterday_date = {
            let now_local = self.tz.from_utc_datetime(&Utc::now().naive_utc());
            let yesterday = now_local.date_naive() - chrono::Duration::days(1);
            yesterday.format("%Y-%m-%d").to_string()
        };

        let uuid = self.state.resolve_uuid(username);
        let storage = Arc::clone(&self.storage);
        let rcon = Arc::clone(&self.rcon);
        let user = username.to_string();
        tokio::spawn(async move {
            let yesterday_secs = storage
                .get_daily_play_time(uuid, yesterday_date)
                .await
                .unwrap_or(0);

            let yesterday_str = format_duration(yesterday_secs);
            let msg = format!(
                r#"tellraw @a {{"text":"[Stats] Welcome back {user}! Last login: {last_login_str}. Yesterday you played {yesterday_str}.","color":"gold"}}"#
            );

            match tokio::time::timeout(Duration::from_secs(5), rcon.send_command(msg)).await {
                Ok(Ok(_)) => tracing::info!(username = %user, "login reminder sent"),
                Ok(Err(e)) => {
                    tracing::warn!(%e, username = %user, "failed to send login reminder via rcon");
                }
                Err(_) => tracing::warn!(username = %user, "rcon login reminder timed out"),
            }
        });
    }

    fn send_new_player_welcome(&self, username: &str) {
        let msg = format!(
            r#"tellraw @a {{"text":"[Stats] Welcome {username}! This is your first login.","color":"green"}}"#
        );

        let rcon = Arc::clone(&self.rcon);
        let user = username.to_string();
        tokio::spawn(async move {
            match tokio::time::timeout(Duration::from_secs(5), rcon.send_command(msg)).await {
                Ok(Ok(_)) => tracing::info!(username = %user, "new player welcome sent"),
                Ok(Err(e)) => {
                    tracing::warn!(%e, username = %user, "failed to send new player welcome via rcon");
                }
                Err(_) => tracing::warn!(username = %user, "rcon welcome timed out"),
            }
        });
    }
}

/// Split a session (`start_utc`, `end_utc`) into (`date`, `secs`) pairs at midnight
/// boundaries in the configured timezone.
fn split_session_by_day(start_utc: i64, end_utc: i64, tz: Tz) -> Vec<(String, u64)> {
    if start_utc >= end_utc {
        return Vec::new();
    }

    let start_local = tz.timestamp_opt(start_utc, 0).single();
    let end_local = tz.timestamp_opt(end_utc, 0).single();

    let (Some(start_local), Some(end_local)) = (start_local, end_local) else {
        return Vec::new();
    };

    let start_date = start_local.date_naive();
    let end_date = end_local.date_naive();

    if start_date == end_date {
        let secs = u64::try_from(end_utc - start_utc).unwrap_or(0);
        return vec![(start_date.format("%Y-%m-%d").to_string(), secs)];
    }

    let mut result = Vec::new();
    let mut current = start_date;

    // First day: from start to midnight
    let midnight_after_start = tz
        .from_local_datetime(&current.and_hms_opt(0, 0, 0).unwrap())
        .single()
        .map(|dt| dt.timestamp() + 86_400);
    if let Some(midnight_ts) = midnight_after_start {
        let secs = u64::try_from((midnight_ts - start_utc).max(0)).unwrap_or(0);
        if secs > 0 {
            result.push((current.format("%Y-%m-%d").to_string(), secs));
        }
    }

    current = current.succ_opt().unwrap_or(current);
    while current < end_date {
        let secs: u64 = 86_400;
        result.push((current.format("%Y-%m-%d").to_string(), secs));
        current = current.succ_opt().unwrap_or(current);
    }

    // Last day: from midnight to end
    let midnight_before_end = tz
        .from_local_datetime(&end_date.and_hms_opt(0, 0, 0).unwrap())
        .single()
        .map(|dt| dt.timestamp());
    if let Some(midnight_ts) = midnight_before_end {
        let secs = u64::try_from((end_utc - midnight_ts).max(0)).unwrap_or(0);
        if secs > 0 {
            result.push((end_date.format("%Y-%m-%d").to_string(), secs));
        }
    }

    result
}

/// Format seconds as a human-readable duration string.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_same_day_session() {
        let tz: Tz = "UTC".parse().unwrap();
        let start = TimeZone::timestamp_opt(&Utc, 1_700_000_000, 0).unwrap();
        let end = TimeZone::timestamp_opt(&Utc, 1_700_001_800, 0).unwrap();
        let splits = split_session_by_day(start.timestamp(), end.timestamp(), tz);
        assert_eq!(splits.len(), 1);
        assert_eq!(splits[0].1, 1800);
    }

    #[test]
    fn split_cross_midnight_session() {
        let tz: Tz = "UTC".parse().unwrap();
        // 23:00 to 01:00 next day = 2 hours total
        let start = TimeZone::timestamp_opt(&Utc, 1_700_004_000, 0).unwrap();
        let end = TimeZone::timestamp_opt(&Utc, 1_700_011_200, 0).unwrap();
        let splits = split_session_by_day(start.timestamp(), end.timestamp(), tz);
        assert_eq!(splits.len(), 2);
        let total: u64 = splits.iter().map(|s| s.1).sum();
        assert_eq!(total, 7200);
    }

    #[test]
    fn split_multi_day_session() {
        let tz: Tz = "UTC".parse().unwrap();
        // 3.5 days
        let start = TimeZone::timestamp_opt(&Utc, 1_700_000_000, 0).unwrap();
        let end = TimeZone::timestamp_opt(&Utc, 1_700_000_000 + (3 * 86_400 + 43_200), 0).unwrap();
        let splits = split_session_by_day(start.timestamp(), end.timestamp(), tz);
        let total: u64 = splits.iter().map(|s| s.1).sum();
        assert_eq!(total, 3 * 86_400 + 43_200);
    }

    #[test]
    fn format_duration_zero() {
        assert_eq!(format_duration(0), "0m");
    }

    #[test]
    fn format_duration_minutes_only() {
        assert_eq!(format_duration(300), "5m");
    }

    #[test]
    fn format_duration_hours_and_minutes() {
        assert_eq!(format_duration(3_900), "1h 5m");
    }

    #[test]
    fn format_duration_days_hours_minutes() {
        assert_eq!(format_duration(100_000), "1d 3h 46m");
    }

    #[test]
    fn split_ist_timezone() {
        let tz: Tz = "Asia/Kolkata".parse().unwrap();
        // IST is UTC+5:30, so midnight IST = 18:30 UTC previous day
        let start = 1_700_000_000;
        let end = 1_700_007_000;
        let splits = split_session_by_day(start, end, tz);
        let total: u64 = splits.iter().map(|s| s.1).sum();
        assert_eq!(total, 7000);
    }
}
