//! The engine's newer settings (retry, schedule, proxy, sorting, what happens when the
//! queue is done), kept apart from `Config` so that one stays a small `Copy` value.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::sort::Category;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ProxyMode {
    /// Whatever Windows uses (Settings > Network > Proxy).
    #[default]
    System,
    /// Straight to the server, even when Windows has a proxy set.
    Off,
    Http,
    Socks5,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Proxy {
    pub mode: ProxyMode,
    pub host: String,
    pub port: u16,
    /// A user name for the proxy; its password lives in Windows Credential Manager.
    pub user: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum WhenDone {
    #[default]
    Nothing,
    Sleep,
    ShutDown,
    Quit,
}

impl WhenDone {
    pub fn verb(self) -> &'static str {
        match self {
            WhenDone::Nothing => "",
            WhenDone::Sleep => "go to sleep",
            WhenDone::ShutDown => "shut down",
            WhenDone::Quit => "quit",
        }
    }
}

/// Times are minutes after local midnight.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Schedule {
    /// Every day at this time, start (resume) everything waiting.
    pub start_at: Option<u16>,
    /// Every day at this time, pause everything.
    pub stop_at: Option<u16>,
    /// Only download between these times (may run past midnight: 23:00 to 07:00).
    pub window: Option<(u16, u16)>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// Try failed downloads again by themselves (network down, server busy).
    pub auto_retry: bool,
    /// Keep the PC from sleeping while something downloads.
    pub keep_awake: bool,
    pub schedule: Schedule,
    /// Put finished downloads into a folder for their type.
    pub auto_sort: bool,
    /// A folder per type instead of the default (a subfolder of where it was saved).
    pub sort_folders: Vec<(Category, PathBuf)>,
    pub proxy: Proxy,
    /// What happens once everything has downloaded.
    pub when_done: WhenDone,
    /// Hold downloads on a metered connection (a phone's hotspot, a data plan).
    pub pause_metered: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            auto_retry: true,
            keep_awake: true,
            schedule: Schedule::default(),
            auto_sort: false,
            sort_folders: Vec::new(),
            proxy: Proxy::default(),
            when_done: WhenDone::Nothing,
            pause_metered: false,
        }
    }
}

/// "07:30" (or "7:30", "730", "7") to minutes after midnight.
pub fn parse_time(s: &str) -> Option<u16> {
    let s = s.trim().to_ascii_lowercase();
    let (s, pm) = match (s.strip_suffix("pm"), s.strip_suffix("am")) {
        (Some(x), _) => (x.trim().to_string(), Some(true)),
        (_, Some(x)) => (x.trim().to_string(), Some(false)),
        _ => (s, None),
    };
    let (h, m) = match s.split_once(':') {
        Some((h, m)) => (h.trim().parse::<u16>().ok()?, m.trim().parse::<u16>().ok()?),
        None if s.len() >= 3 => (s[..s.len() - 2].parse().ok()?, s[s.len() - 2..].parse().ok()?),
        None => (s.parse().ok()?, 0),
    };
    let h = match pm {
        Some(true) if h < 12 => h + 12,
        Some(false) if h == 12 => 0,
        _ => h,
    };
    (h < 24 && m < 60).then_some(h * 60 + m)
}

pub fn format_time(min: u16) -> String {
    format!("{:02}:{:02}", min / 60, min % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times() {
        assert_eq!(parse_time("07:30"), Some(450));
        assert_eq!(parse_time("7:30"), Some(450));
        assert_eq!(parse_time("730"), Some(450));
        assert_eq!(parse_time("23"), Some(23 * 60));
        assert_eq!(parse_time("11:15 pm"), Some(23 * 60 + 15));
        assert_eq!(parse_time("12am"), Some(0));
        assert_eq!(parse_time("24:00"), None);
        assert_eq!(parse_time("x"), None);
        assert_eq!(format_time(450), "07:30");
    }

    #[test]
    fn old_settings_load() {
        let p: Prefs = serde_json::from_str("{}").unwrap();
        assert!(p.auto_retry && p.keep_awake && !p.auto_sort);
    }
}
