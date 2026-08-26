//! What actui remembers between sessions, saved next to the config: which repos
//! you actually work in (so the sidebar can put those first instead of making
//! you hunt for them) and how wide you dragged the sidebar.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// A use counts for half as much once this many days have passed, so a repo you
/// stopped touching drifts down instead of sitting at the top forever.
const HALF_LIFE_DAYS: f64 = 7.0;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RepoUse {
    pub hits: u32,
    pub last: Option<DateTime<Utc>>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct State {
    /// Sidebar width, once you've changed it. Declared before `repo`: TOML
    /// wants plain values ahead of tables.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sidebar_width: Option<u16>,
    /// Keyed by repo full name (`owner/name`).
    #[serde(default)]
    repo: HashMap<String, RepoUse>,
    #[serde(skip)]
    dirty: bool,
}

impl State {
    pub fn load() -> Self {
        let Some(path) = path() else {
            return Self::default();
        };
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        toml::from_str(&text).unwrap_or_default()
    }

    /// Count one deliberate use of a repo: resting on one of its runs, opening a
    /// log, or dispatching a workflow.
    pub fn record(&mut self, repo: &str) {
        let entry = self.repo.entry(repo.to_string()).or_default();
        entry.hits = entry.hits.saturating_add(1);
        entry.last = Some(Utc::now());
        self.dirty = true;
    }

    /// The sidebar width you last set, if you ever did.
    pub fn sidebar_width(&self) -> Option<u16> {
        self.sidebar_width
    }

    pub fn set_sidebar_width(&mut self, width: u16) {
        self.sidebar_width = Some(width);
        self.dirty = true;
    }

    /// How strongly a repo ranks: its uses, halved for every week since the last.
    pub fn score(&self, repo: &str) -> f64 {
        let Some(use_) = self.repo.get(repo) else {
            return 0.0;
        };
        let Some(last) = use_.last else {
            return use_.hits as f64;
        };
        let days = (Utc::now() - last).num_seconds().max(0) as f64 / 86_400.0;
        use_.hits as f64 * 0.5_f64.powf(days / HALF_LIFE_DAYS)
    }

    /// Write the file if anything changed. Failures stay silent — a lost history
    /// only costs the sidebar its order.
    pub fn save(&self) {
        if !self.dirty {
            return;
        }
        let (Some(path), Ok(text)) = (path(), toml::to_string_pretty(self)) else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, text);
    }
}

fn path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("actui").join("state.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_uses_outrank_older_ones() {
        let mut s = State::default();
        s.record("org/api");
        s.repo.insert(
            "org/stale".into(),
            RepoUse { hits: 8, last: Some(Utc::now() - chrono::Duration::days(28)) },
        );
        // 8 hits halved four times (four weeks) = 0.5, under one fresh hit.
        assert!(s.score("org/api") > s.score("org/stale"));
        assert_eq!(s.score("org/never-seen"), 0.0);
    }

    #[test]
    fn survives_a_round_trip_through_toml() {
        let mut s = State::default();
        s.record("org/api");
        s.set_sidebar_width(34);
        let text = toml::to_string_pretty(&s).unwrap();
        let back: State = toml::from_str(&text).unwrap();
        assert!(back.score("org/api") > 0.0, "wrote: {text}");
        assert_eq!(back.sidebar_width(), Some(34), "wrote: {text}");
    }

    #[test]
    fn hits_accumulate() {
        let mut s = State::default();
        s.record("org/api");
        let one = s.score("org/api");
        s.record("org/api");
        assert!(s.score("org/api") > one);
    }
}
