//! On-disk state: settings, request history and saved requests.
//!
//! Every write is atomic (temp file + rename) so a crash or a full disk can
//! never leave the pilot with a half-written history that refuses to load.

use crate::engine::HttpMethod;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const MAX_HISTORY: usize = 200;

/// A request the pilot can replay, either from history or from a collection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RequestSnapshot {
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub headers: String,
    #[serde(default)]
    pub body: String,
}

impl RequestSnapshot {
    pub fn method_enum(&self) -> HttpMethod {
        HttpMethod::parse(&self.method).unwrap_or(HttpMethod::Get)
    }

    /// True when two requests are the same flight, ignoring when they flew.
    pub fn same_request(&self, other: &Self) -> bool {
        self.method == other.method
            && self.url == other.url
            && self.headers == other.headers
            && self.body == other.body
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    #[serde(flatten)]
    pub request: RequestSnapshot,
    #[serde(default)]
    pub status: u16,
    #[serde(default)]
    pub duration_ms: u64,
    /// Seconds since the Unix epoch.
    #[serde(default)]
    pub at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedRequest {
    pub name: String,
    #[serde(flatten)]
    pub request: RequestSnapshot,
}

/// Persisted preferences. Every field has a default so an older or partial
/// config file still loads instead of resetting the pilot's setup.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub timeout_secs: u64,
    pub connect_timeout_secs: u64,
    pub insecure: bool,
    pub follow_redirects: bool,
    pub max_body_mb: u64,
    pub wrap_response: bool,
    pub mouse: bool,
    pub pretty_json: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            timeout_secs: 30,
            connect_timeout_secs: 10,
            insecure: false,
            follow_redirects: true,
            max_body_mb: 32,
            wrap_response: true,
            mouse: true,
            pretty_json: true,
        }
    }
}

/// Resolves the data directory, honouring `HCP_DATA_DIR` for tests and for
/// pilots who keep their tooling state somewhere specific.
pub fn data_dir() -> Option<PathBuf> {
    if let Some(custom) = std::env::var_os("HCP_DATA_DIR") {
        if !custom.is_empty() {
            return Some(PathBuf::from(custom));
        }
    }
    dirs::data_dir().map(|d| d.join("hcp"))
}

fn path_for(file: &str) -> Option<PathBuf> {
    data_dir().map(|d| d.join(file))
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    let raw = std::fs::read_to_string(path).ok()?;
    // A corrupt file must never take the app down; it is simply ignored and
    // will be overwritten by the next successful save.
    serde_json::from_str(&raw).ok()
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    let tmp = path.with_extension("tmp");
    let data = serde_json::to_vec_pretty(value).context("could not serialise state")?;
    std::fs::write(&tmp, data).with_context(|| format!("could not write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("could not save {}", path.display()))?;
    Ok(())
}

pub fn load_settings() -> Settings {
    path_for("config.json")
        .and_then(|p| read_json(&p))
        .unwrap_or_default()
}

pub fn save_settings(settings: &Settings) -> Result<()> {
    let path = path_for("config.json").context("no data directory available on this platform")?;
    write_json(&path, settings)
}

pub fn load_history() -> Vec<HistoryEntry> {
    path_for("history.json")
        .and_then(|p| read_json::<Vec<HistoryEntry>>(&p))
        .unwrap_or_default()
}

pub fn save_history(entries: &[HistoryEntry]) -> Result<()> {
    let path = path_for("history.json").context("no data directory available on this platform")?;
    write_json(&path, &entries)
}

pub fn load_collection() -> Vec<SavedRequest> {
    path_for("collection.json")
        .and_then(|p| read_json::<Vec<SavedRequest>>(&p))
        .unwrap_or_default()
}

pub fn save_collection(items: &[SavedRequest]) -> Result<()> {
    let path = path_for("collection.json").context("no data directory available on this platform")?;
    write_json(&path, &items)
}

pub fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Pushes an entry onto the front of the history, collapsing an immediate
/// repeat of the same request and trimming to `MAX_HISTORY`.
pub fn push_history(history: &mut Vec<HistoryEntry>, entry: HistoryEntry) {
    if let Some(first) = history.first() {
        if first.request.same_request(&entry.request) {
            history[0] = entry;
            return;
        }
    }
    history.insert(0, entry);
    history.truncate(MAX_HISTORY);
}

/// "just now" / "4m ago" / "3d ago" — readable without pulling in a date crate.
pub fn relative_time(at: u64, now: u64) -> String {
    if at == 0 {
        return "—".to_string();
    }
    let secs = now.saturating_sub(at);
    match secs {
        0..=5 => "just now".to_string(),
        6..=59 => format!("{secs}s ago"),
        60..=3599 => format!("{}m ago", secs / 60),
        3600..=86399 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(url: &str) -> RequestSnapshot {
        RequestSnapshot {
            method: "GET".into(),
            url: url.into(),
            headers: String::new(),
            body: String::new(),
        }
    }

    fn entry(url: &str, at: u64) -> HistoryEntry {
        HistoryEntry {
            request: snap(url),
            status: 200,
            duration_ms: 10,
            at,
        }
    }

    #[test]
    fn repeated_request_updates_in_place_instead_of_flooding_history() {
        let mut h = Vec::new();
        push_history(&mut h, entry("a", 1));
        push_history(&mut h, entry("a", 2));
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].at, 2);
        push_history(&mut h, entry("b", 3));
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].request.url, "b");
    }

    #[test]
    fn history_is_capped() {
        let mut h = Vec::new();
        for i in 0..(MAX_HISTORY + 50) {
            push_history(&mut h, entry(&format!("u{i}"), i as u64));
        }
        assert_eq!(h.len(), MAX_HISTORY);
        assert_eq!(h[0].request.url, format!("u{}", MAX_HISTORY + 49));
    }

    #[test]
    fn settings_tolerate_partial_files() {
        let s: Settings = serde_json::from_str(r#"{"timeout_secs": 5}"#).unwrap();
        assert_eq!(s.timeout_secs, 5);
        assert!(s.follow_redirects, "missing fields must fall back to defaults");
    }

    #[test]
    fn relative_times_read_naturally() {
        assert_eq!(relative_time(0, 100), "—");
        assert_eq!(relative_time(100, 102), "just now");
        assert_eq!(relative_time(100, 400), "5m ago");
        assert_eq!(relative_time(1, 1 + 86400 * 3), "3d ago");
    }

    #[test]
    fn snapshots_round_trip_through_json() {
        let e = entry("https://x.dev", 42);
        let raw = serde_json::to_string(&e).unwrap();
        let back: HistoryEntry = serde_json::from_str(&raw).unwrap();
        assert!(back.request.same_request(&e.request));
        assert_eq!(back.at, 42);
    }
}
