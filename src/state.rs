//! What actui remembers between sessions, saved next to the config: the layout
//! you dragged the panes and columns into, and which repos you actually work in
//! (which orders the sidebar under `sort = "used"`).

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
    /// Pane widths, once you've changed them. Declared before the tables below:
    /// TOML wants plain values ahead of them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sidebar_width: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    detail_width: Option<u16>,
    /// Runs-table columns you've resized, by column name.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    columns: HashMap<String, u16>,
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

    /// The pane widths you last set, if you ever did.
    pub fn sidebar_width(&self) -> Option<u16> {
        self.sidebar_width
    }

    pub fn detail_width(&self) -> Option<u16> {
        self.detail_width
    }

    pub fn columns(&self) -> &HashMap<String, u16> {
        &self.columns
    }

    /// The setters only mark the file dirty when something actually changed, so
    /// a session that touched nothing doesn't rewrite it.
    pub fn set_sidebar_width(&mut self, width: u16) {
        if self.sidebar_width != Some(width) {
            self.sidebar_width = Some(width);
            self.dirty = true;
        }
    }

    pub fn set_detail_width(&mut self, width: u16) {
        if self.detail_width != Some(width) {
            self.detail_width = Some(width);
            self.dirty = true;
        }
    }

    pub fn set_columns(&mut self, columns: HashMap<String, u16>) {
        if self.columns != columns {
            self.columns = columns;
            self.dirty = true;
        }
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
        s.set_detail_width(30);
        s.set_columns(HashMap::from([("repo".to_string(), 18)]));
        let text = toml::to_string_pretty(&s).unwrap();
        let back: State = toml::from_str(&text).unwrap();
        assert!(back.score("org/api") > 0.0, "wrote: {text}");
        assert_eq!(back.sidebar_width(), Some(34), "wrote: {text}");
        assert_eq!(back.detail_width(), Some(30), "wrote: {text}");
        assert_eq!(back.columns().get("repo"), Some(&18), "wrote: {text}");
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
