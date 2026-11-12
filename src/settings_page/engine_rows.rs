//! Settings > Download Engine: retries, keeping awake, metered connections, download
//! hours, sorting by type, the proxy, and what happens when everything's done.

use super::*;
use crate::dl::prefs::{ProxyMode, WhenDone, format_time};

impl SettingsPage {
    pub(super) fn engine_section(&self, v: &Values) -> Section {
        let p = &v.settings.downloads.engine;
        let s = &p.schedule;
        let mut hours = Vec::new();
        if let Some(t) = s.start_at {
            hours.push(format!("Starts at {}", format_time(t)));
        }
        if let Some(t) = s.stop_at {
            hours.push(format!("stops at {}", format_time(t)));
        }
        if let Some((a, b)) = s.window {
            hours.push(format!("only {} to {}", format_time(a), format_time(b)));
        }
        let hours = if hours.is_empty() { "Any time. Set daily start and stop times, or quiet hours.".to_string() } else { capitalize(&hours.join(", ")) };
        let proxy = match p.proxy.mode {
            ProxyMode::System => "The one Windows uses.".to_string(),
            ProxyMode::Off => "None: straight to each server.".to_string(),
            ProxyMode::Http => format!("HTTP proxy at {}:{}", p.proxy.host, p.proxy.port),
            ProxyMode::Socks5 => format!("SOCKS5 proxy at {}:{}", p.proxy.host, p.proxy.port),
        };
        let when = [WhenDone::Nothing, WhenDone::Sleep, WhenDone::ShutDown, WhenDone::Quit].iter().position(|w| *w == p.when_done);
        let edit = |id: &str| Ctl::Buttons(vec![(format!("dle:edit:{id}"), "Edit\u{2026}".into(), None, 70.0, false)]);
        Section {
            title: "Download Engine",
            keys: "engine downloads",
            rows: vec![
                Row::new("Try failed downloads again", Some("When the network drops or a server is busy."), "retry auto again resume network failed error", Ctl::Switch {
                    id: "dle-retry",
                    on: p.auto_retry,
                    enabled: true,
                }),
                Row::new("Keep the PC awake while downloading", Some("Sleep waits until they're done."), "sleep awake power standby", Ctl::Switch {
                    id: "dle-awake",
                    on: p.keep_awake,
                    enabled: true,
                }),
                Row::new("Pause on metered connections", Some("Holds downloads on a phone's hotspot or a data plan."), "metered data cost hotspot mobile cellular", Ctl::Switch {
                    id: "dle-metered",
                    on: p.pause_metered,
                    enabled: true,
                }),
                Row::new("Download hours", Some(&hours), "schedule scheduler time quiet hours night start stop timer", edit("schedule")),
                Row::new("Sort into folders by type", Some("Videos, Music, Pictures, Documents, Programs and Archives."), "sort folders type category organize auto", Ctl::Switch {
                    id: "dle-sort",
                    on: p.auto_sort,
                    enabled: true,
                }),
                Row::new("Folders for each type", Some("Where each type goes when sorting is on."), "sort folders type category location", edit("folders")),
                Row::new("Proxy", Some(&proxy), "proxy socks socks5 http network firewall", edit("proxy")),
                Row::new("When downloads finish", Some("After a 30-second countdown you can cancel."), "shutdown shut down sleep quit exit finish done after queue", Ctl::Seg {
                    id: "dle-when",
                    opts: vec!["Nothing".into(), "Sleep".into(), "Shut down".into(), "Quit".into()],
                    sel: when,
                    custom: None,
                }),
            ],
            foot: Some("Right-click a download to limit its speed, or to give it a new link.".into()),
        }
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect::<String>() + ".",
        None => String::new(),
    }
}
