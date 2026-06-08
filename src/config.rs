//! Optional config loaded from `~/.config/actui/config.toml`.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Seconds between background refreshes when everything is idle.
    pub refresh_secs: u64,
    /// Faster refresh interval used while any run is queued or in progress.
    pub active_refresh_secs: u64,
    /// Tight refresh interval for the one run feeding an open live step view,
    /// so steps advance near real-time. Clamped to 1–10s.
    pub live_refresh_secs: u64,
    /// How many recent runs to pull per repo.
    pub runs_per_repo: u32,
    /// Max repos fetched concurrently.
    pub concurrency: usize,
    /// Cap on how many repos to scan, taken from the most-recently-pushed first.
    /// Keeps the API rate limit safe when you belong to large orgs. 0 = no cap.
    pub max_repos: usize,
    /// Skip archived repos.
    pub skip_archived: bool,
    /// Only include repos whose full_name contains one of these substrings.
    /// Empty = include all owned/org repos.
    pub include: Vec<String>,
    /// Exclude repos whose full_name contains one of these substrings.
    pub exclude: Vec<String>,
    /// Post an OS desktop notification when a watched run finishes.
    pub notify: bool,
    /// Ring the terminal bell when a watched run finishes.
    pub bell: bool,
    /// Color theme: "auto" (follow the system light/dark setting), "dark", or
    /// "light".
    pub theme: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            refresh_secs: 60,
            active_refresh_secs: 15,
            live_refresh_secs: 2,
            runs_per_repo: 15,
            concurrency: 3,
            max_repos: 60,
            skip_archived: true,
            include: Vec::new(),
            exclude: Vec::new(),
            notify: true,
            bell: true,
            theme: "auto".to_string(),
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let Some(dir) = dirs::config_dir() else {
            return Self::default();
        };
        let path = dir.join("actui").join("config.toml");
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        toml::from_str(&text).unwrap_or_default()
    }

    pub fn keep_repo(&self, full_name: &str) -> bool {
        if self.exclude.iter().any(|e| full_name.contains(e)) {
            return false;
        }
        if self.include.is_empty() {
            return true;
        }
        self.include.iter().any(|i| full_name.contains(i))
    }
}
