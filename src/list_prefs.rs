//! How the downloads list is ordered and kept: the sort (date added, name, size, site,
//! type), optional grouping by site, and clearing finished items off the list after a
//! while. Saved in the settings as `list`; changed with "dlw:" commands.

use serde::{Deserialize, Serialize};

use crate::dl::JobView;
use crate::search;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SortBy {
    /// Newest first, in day groups (Today, Yesterday…).
    #[default]
    Added,
    Name,
    /// Largest first.
    Size,
    Site,
    Type,
}

pub const SORTS: [(SortBy, &str, &str); 5] =
    [(SortBy::Added, "added", "Date Added"), (SortBy::Name, "name", "Name"), (SortBy::Size, "size", "Size"), (SortBy::Site, "site", "Site"), (SortBy::Type, "type", "Type")];

/// The list's settings (in `Settings::list`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ListSettings {
    pub sort: SortBy,
    /// Group the list by the site each download came from.
    pub group_site: bool,
    /// Take finished downloads off the list after this many days (0 = never). Their
    /// files stay where they are.
    pub auto_clear_days: u32,
}

/// The Settings choices for auto-clear: days, label.
pub const CLEAR_AFTER: [(u32, &str); 4] = [(0, "Never"), (1, "1 day"), (7, "1 week"), (30, "1 month")];

/// Apply a "dlw:<key>[:<value>]" command. True if anything changed.
pub fn apply(s: &mut ListSettings, cmd: &str) -> bool {
    let Some(rest) = cmd.strip_prefix("dlw:") else { return false };
    let (key, value) = rest.split_once(':').unwrap_or((rest, ""));
    let before = s.clone();
    match key {
        "sort" => {
            if let Some((by, _, _)) = SORTS.iter().find(|(_, id, _)| *id == value) {
                s.sort = *by;
            }
        }
        "group" => s.group_site = !s.group_site,
        "clear" => s.auto_clear_days = value.parse().unwrap_or(0),
        _ => {}
    }
    *s != before
}

/// Where a download came from: the page for a magnet, else the link's site.
pub fn site_of(j: &JobView) -> String {
    search::site(j.referrer.as_deref().filter(|_| j.source.starts_with("magnet:")).unwrap_or(&j.source))
}

fn type_rank(name: &str) -> usize {
    let t = search::file_type(name);
    search::TYPES.iter().position(|(x, _, _)| *x == t).unwrap_or(search::TYPES.len())
}

/// The label for a type group ("Videos").
pub fn type_label(name: &str) -> &'static str {
    match search::file_type(name) {
        search::FileType::Video => "Videos",
        search::FileType::Audio => "Audio",
        search::FileType::Archive => "Archives",
        search::FileType::Document => "Documents",
        search::FileType::Image => "Images",
        search::FileType::App => "Apps",
        search::FileType::Other => "Other",
    }
}

/// Order two downloads by the chosen sort (ties: newest first).
pub fn compare(by: SortBy, a: &JobView, b: &JobView) -> std::cmp::Ordering {
    let when = |j: &JobView| j.finished_at.unwrap_or(j.added);
    let newest = when(b).cmp(&when(a)).then(b.id.cmp(&a.id));
    let name = |j: &JobView| j.name.to_lowercase();
    match by {
        SortBy::Added => newest,
        SortBy::Name => name(a).cmp(&name(b)).then(newest),
        SortBy::Size => b.total.unwrap_or(0).cmp(&a.total.unwrap_or(0)).then(newest),
        SortBy::Site => site_of(a).cmp(&site_of(b)).then(name(a).cmp(&name(b))).then(newest),
        SortBy::Type => type_rank(&a.name).cmp(&type_rank(&b.name)).then(name(a).cmp(&name(b))).then(newest),
    }
}

/// Finished downloads that have been on the list longer than `days` (by when they
/// finished), to take off it.
pub fn expired(jobs: &[JobView], days: u32, now: u64) -> Vec<u64> {
    if days == 0 {
        return Vec::new();
    }
    let limit = days as u64 * 86_400;
    jobs.iter()
        .filter(|j| j.state == crate::dl::State::Done)
        .filter(|j| now.saturating_sub(j.finished_at.unwrap_or(j.added)) >= limit)
        .map(|j| j.id)
        .collect()
}

/// Milliseconds until the next local midnight (and a second), given the local offset.
pub fn ms_to_midnight(now: u64, offset: i64) -> u64 {
    let local = now as i64 + offset;
    let next = (local.div_euclid(86_400) + 1) * 86_400;
    ((next - local) as u64 * 1000 + 1000).max(1000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dl::{Kind, Priority, State};

    fn job(id: u64, name: &str, src: &str, total: u64, at: u64) -> JobView {
        JobView {
            id,
            kind: Kind::Http,
            name: name.into(),
            source: src.into(),
            dir: Default::default(),
            path: None,
            state: State::Done,
            phase: None,
            priority: Priority::Normal,
            total: Some(total),
            done: total,
            speed: 0,
            up_speed: 0,
            peers: None,
            connections: 0,
            resumable: true,
            error: None,
            added: at,
            finished_at: Some(at),
            referrer: None,
            expired: false,
        }
    }

    #[test]
    fn sorts() {
        let a = job(1, "b.zip", "https://x.org/b.zip", 10, 100);
        let b = job(2, "A.mp4", "https://www.a.com/a.mp4", 50, 200);
        let mut v = vec![a.clone(), b.clone()];
        v.sort_by(|x, y| compare(SortBy::Added, x, y));
        assert_eq!(v[0].id, 2);
        v.sort_by(|x, y| compare(SortBy::Name, x, y));
        assert_eq!(v[0].id, 2);
        v.sort_by(|x, y| compare(SortBy::Size, x, y));
        assert_eq!(v[0].id, 2);
        v.sort_by(|x, y| compare(SortBy::Site, x, y));
        assert_eq!(v[0].id, 2, "a.com before x.org");
        v.sort_by(|x, y| compare(SortBy::Type, x, y));
        assert_eq!(v[0].id, 2, "videos before archives");
    }

    #[test]
    fn commands() {
        let mut s = ListSettings::default();
        assert!(apply(&mut s, "dlw:sort:size"));
        assert_eq!(s.sort, SortBy::Size);
        assert!(!apply(&mut s, "dlw:sort:size"));
        assert!(apply(&mut s, "dlw:group"));
        assert!(s.group_site);
        assert!(apply(&mut s, "dlw:clear:7"));
        assert_eq!(s.auto_clear_days, 7);
        assert!(!apply(&mut s, "dls:max:3"));
    }

    #[test]
    fn auto_clear() {
        let day = 86_400;
        let jobs = vec![job(1, "a", "", 1, 10 * day), job(2, "b", "", 1, 2 * day)];
        assert!(expired(&jobs, 0, 11 * day).is_empty());
        assert_eq!(expired(&jobs, 7, 11 * day), vec![2]);
        let mut running = job(3, "c", "", 1, 0);
        running.state = State::Active;
        assert!(expired(&[running], 1, 11 * day).is_empty());
    }

    #[test]
    fn midnight() {
        // 23:59:00 local → a minute (and a second) to go
        assert_eq!(ms_to_midnight(86_400 - 60, 0), 61_000);
        // UTC+1: 22:59:00 UTC is 23:59:00 local
        assert_eq!(ms_to_midnight(86_400 - 3_660, 3_600), 61_000);
    }
}
