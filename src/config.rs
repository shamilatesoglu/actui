//! Optional config loaded from `~/.config/actui/config.toml`.

use serde::Deserialize;
use std::path::PathBuf;

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
    /// How many to pull for the one repo the sidebar is scoped to, where you're
    /// actually reading its history. Capped at GitHub's 100 per page.
    pub scoped_runs: u32,
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
    /// Color theme: "auto" (follow the terminal's own background, falling back
    /// to the desktop's light/dark setting), "dark", or "light".
    pub theme: String,
    /// Show the repos sidebar on start (`p` toggles it at runtime).
    pub sidebar: bool,
    /// Repos kept at the top of the sidebar, in this order.
    pub pinned: Vec<String>,
    /// Sidebar order below the pinned repos: "alpha" (A→Z, the default) or
    /// "used" (the repos you work in most, decayed over time).
    pub sort: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            refresh_secs: 60,
            active_refresh_secs: 15,
            live_refresh_secs: 2,
            runs_per_repo: 15,
            scoped_runs: 100,
            concurrency: 10,
            max_repos: 60,
            skip_archived: true,
            include: Vec::new(),
            exclude: Vec::new(),
            notify: true,
            bell: true,
            theme: "auto".to_string(),
            sidebar: true,
            pinned: Vec::new(),
            sort: "alpha".to_string(),
        }
    }
}

/// Where the config and state live: `$XDG_CONFIG_HOME/actui`, else
/// `~/.config/actui` — on macOS too, where a terminal user looks for it rather
/// than in `~/Library/Application Support`.
#[cfg(not(windows))]
pub fn dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| Some(dirs::home_dir()?.join(".config")))?;
    Some(base.join("actui"))
}

/// Where the config and state live: `%APPDATA%\actui`.
#[cfg(windows)]
pub fn dir() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("actui"))
}

/// Read one of actui's files. Up to 0.7.4 macOS kept them in
/// `~/Library/Application Support/actui`, so that is the fallback until the
/// next save writes the file to `dir()`.
pub fn read(name: &str) -> Option<String> {
    let legacy = || Some(dirs::config_dir()?.join("actui").join(name));
    [dir().map(|d| d.join(name)), legacy()]
        .into_iter()
        .flatten()
        .find_map(|path| std::fs::read_to_string(path).ok())
}

impl Config {
    pub fn load() -> Self {
        let Some(text) = read("config.toml") else {
            return Self::default();
        };
        toml::from_str(&text).unwrap_or_default()
    }

    /// Runs to request for a repo: the deeper read for the scoped one.
    pub fn runs_for(&self, full_name: &str, scoped: Option<&str>) -> u32 {
        if scoped == Some(full_name) {
            self.scoped_runs.clamp(self.runs_per_repo, 100)
        } else {
            self.runs_per_repo
        }
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
