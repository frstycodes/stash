//! BitTorrent downloads through librqbit. The session (DHT, listener) only starts
//! when the first torrent does, so people who never use torrents pay nothing for it.
//!
//! The session keeps its own record of its torrents (and which pieces they have) under
//! the data folder, so a restart picks up where it left off without checking every
//! piece again, and torrents that were seeding carry on.

use std::collections::{HashMap, HashSet};
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use librqbit::{AddTorrent, AddTorrentOptions, AddTorrentResponse, Session, SessionOptions};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::{oneshot, OnceCell};

use super::Ev;

/// The torrent settings (Settings > Torrents).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Options {
    /// KB/s; 0 = unlimited. Separate from the download limit.
    pub upload_kbps: u64,
    /// Stop seeding once this much has been uploaded per byte downloaded; 0 = no limit.
    pub seed_ratio: f32,
    /// Stop seeding after this many minutes; 0 = no limit.
    pub seed_minutes: u64,
    /// Ask the router to forward the listening port (UPnP). Takes effect when the
    /// BitTorrent session next starts.
    pub upnp: bool,
    /// Ask which files to download when a torrent has more than one.
    pub choose_files: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { upload_kbps: 0, seed_ratio: 0.0, seed_minutes: 0, upnp: true, choose_files: true }
    }
}

impl Options {
    /// A "t-<key>:<value>" setting from the Settings page; false if it isn't one.
    pub fn apply(&mut self, key: &str, value: &str) -> bool {
        match key {
            "t-up" => self.upload_kbps = value.parse().unwrap_or(0),
            "t-ratio" => self.seed_ratio = value.parse::<f32>().unwrap_or(0.0).max(0.0),
            "t-minutes" => self.seed_minutes = value.parse().unwrap_or(0),
            "t-upnp" => self.upnp = !self.upnp,
            "t-choose" => self.choose_files = !self.choose_files,
            _ => return false,
        }
        true
    }
}

static OPTIONS: Mutex<Options> = Mutex::new(Options { upload_kbps: 0, seed_ratio: 0.0, seed_minutes: 0, upnp: true, choose_files: true });

pub fn options() -> Options {
    *OPTIONS.lock().unwrap()
}

pub fn set_options(o: Options) {
    *OPTIONS.lock().unwrap() = o;
}

/// Has a seeding torrent given back enough? `uploaded` over this run and earlier ones,
/// `total` what was downloaded, `seeded_secs` time spent seeding.
pub fn seed_limit_reached(uploaded: u64, total: u64, seeded_secs: u64, o: &Options) -> bool {
    let by_ratio = o.seed_ratio > 0.0 && total > 0 && uploaded as f64 >= total as f64 * o.seed_ratio as f64;
    let by_time = o.seed_minutes > 0 && seeded_secs >= o.seed_minutes * 60;
    by_ratio || by_time
}

/// Add what's been uploaded since the last look. librqbit's count starts again when a
/// torrent is restarted, so a smaller number than last time is all new.
pub fn add_uploaded(total: &mut u64, last: &mut u64, now: u64) {
    *total += if now >= *last { now - *last } else { now };
    *last = now;
}

/// The files to ask librqbit for: None when every file is chosen (so files it adds
/// later, like padding, aren't held back), else the chosen indices, sorted.
pub fn only_files(chosen: &[usize], files: &[FileEntry]) -> Option<Vec<usize>> {
    let set: HashSet<usize> = chosen.iter().copied().collect();
    if files.iter().all(|f| set.contains(&f.index)) {
        return None;
    }
    let mut v: Vec<usize> = set.into_iter().collect();
    v.sort_unstable();
    Some(v)
}

/// Does `new` want anything `old` didn't (None = everything)?
pub fn selection_grew(old: Option<&[usize]>, new: Option<&[usize]>) -> bool {
    match (old, new) {
        (None, _) => false,
        (Some(_), None) => true,
        (Some(o), Some(n)) => n.iter().any(|i| !o.contains(i)),
    }
}

/// One file in a torrent, as the file chooser shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct FileEntry {
    /// Its index in the torrent (what librqbit's `only_files` takes).
    pub index: usize,
    /// Its path inside the torrent ("Season 1\\Episode 1.mkv").
    pub name: String,
    pub size: u64,
}

/// A torrent's files and which of them are wanted (None = all).
#[derive(Clone, Debug, Default)]
pub struct FileList {
    pub name: String,
    pub files: Arc<Vec<FileEntry>>,
    pub only: Option<Vec<usize>>,
}

impl FileList {
    pub fn wanted(&self, index: usize) -> bool {
        self.only.as_ref().is_none_or(|o| o.contains(&index))
    }
}

#[derive(Default)]
struct Known {
    torrent_file: Option<PathBuf>,
    files: Option<Arc<Vec<FileEntry>>>,
    name: String,
    only: Option<Vec<usize>>,
}

/// What the UI can ask about each torrent download, kept out of the snapshot (a torrent
/// can have thousands of files).
static KNOWN: Mutex<Option<HashMap<u64, Known>>> = Mutex::new(None);

fn known<R>(f: impl FnOnce(&mut HashMap<u64, Known>) -> R) -> R {
    f(KNOWN.lock().unwrap().get_or_insert_with(HashMap::new))
}

/// Note a torrent download's saved .torrent and its chosen files (the engine calls this).
pub fn remember(id: u64, torrent_file: Option<PathBuf>, only: Option<Vec<usize>>) {
    known(|k| {
        let e = k.entry(id).or_default();
        if torrent_file.is_some() && e.torrent_file != torrent_file {
            e.torrent_file = torrent_file;
            e.files = None;
        }
        e.only = only;
    });
}

pub fn forget(id: u64) {
    known(|k| k.remove(&id));
}

/// A torrent download's files, if its metadata is known (read from the saved .torrent
/// the first time it's asked for after a restart).
pub fn file_list(id: u64) -> Option<FileList> {
    let (file, cached) = known(|k| k.get(&id).map(|e| (e.torrent_file.clone(), e.files.clone().map(|f| (f, e.name.clone())))))?;
    let (files, name) = match cached {
        Some(c) => c,
        None => {
            let bytes = std::fs::read(file?).ok()?;
            let (name, files) = parse_files(&bytes)?;
            let files = Arc::new(files);
            known(|k| {
                if let Some(e) = k.get_mut(&id) {
                    e.files = Some(files.clone());
                    e.name = name.clone();
                }
            });
            (files, name)
        }
    };
    let only = known(|k| k.get(&id).and_then(|e| e.only.clone()));
    Some(FileList { name, files, only })
}

fn parse_files(bytes: &[u8]) -> Option<(String, Vec<FileEntry>)> {
    let t = librqbit_core::torrent_metainfo::torrent_from_bytes(bytes).ok()?;
    let info = t.info.data.validate().ok()?;
    Some((info.name().map(|n| n.into_owned()).unwrap_or_default(), entries(&info)))
}

fn entries<B: AsRef<[u8]>>(info: &librqbit_core::torrent_metainfo::ValidatedTorrentMetaV1Info<B>) -> Vec<FileEntry> {
    info.iter_file_details()
        .enumerate()
        // BEP 47 padding files only line pieces up: nobody wants to see or pick them
        .filter(|(_, d)| !d.attrs().padding)
        .map(|(index, d)| FileEntry { index, name: d.filename.to_string(), size: d.len })
        .collect()
}

#[derive(Clone, Default)]
pub struct Lazy(Arc<OnceCell<Arc<Session>>>);

impl Lazy {
    pub fn get(&self) -> Option<Arc<Session>> {
        self.0.get().cloned()
    }

    /// The session, started the first time. `events` hears `SessionStarted` once, after
    /// it has brought back the torrents it had last time.
    pub async fn get_or_start(&self, limit_bps: u64, events: &UnboundedSender<Ev>) -> Result<Arc<Session>, String> {
        self.0
            .get_or_try_init(|| async {
                let state = super::data_dir().join("torrents");
                let _ = std::fs::create_dir_all(&state);
                let o = options();
                let opts = SessionOptions {
                    dht: Some(librqbit::DhtSessionConfig {
                        persistence: Some(librqbit::dht::DhtPersistenceConfig {
                            config_filename: Some(state.join("dht.json")),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    listen: Some(librqbit::ListenerOptions { enable_upnp_port_forwarding: o.upnp, ..Default::default() }),
                    client_name_and_version: Some(concat!("Stash ", env!("CARGO_PKG_VERSION")).into()),
                    // remember the torrents and which pieces they have: no full re-check
                    // on every launch (fast resume), and seeding carries on
                    persistence: Some(librqbit::SessionPersistenceConfig::Json { folder: Some(state.join("session")) }),
                    fastresume: true,
                    ..Default::default()
                };
                let s = Session::new_with_opts(crate::shell::downloads_dir(), opts).await.map_err(|e| format!("Can't start BitTorrent: {e:#}"))?;
                set_limit(&s, limit_bps);
                set_upload_limit(&s, o.upload_kbps * 1024);
                let _ = events.send(Ev::SessionStarted);
                Ok(s)
            })
            .await
            .cloned()
    }
}

pub fn set_limit(s: &Session, bps: u64) {
    USER_DOWN.store(bps, std::sync::atomic::Ordering::Relaxed);
    // gaming mode may hold the download lower and the upload hard
    let (down, up) = super::gaming::torrent_limits(bps);
    APPLIED.store(super::gaming::effective(0), std::sync::atomic::Ordering::Relaxed);
    s.ratelimits.set_download_bps(NonZeroU32::new(down.min(u32::MAX as u64) as u32));
    // the user's own upload limit and gaming mode's: whichever is lower (0 = none)
    let user_up = USER_UP.load(std::sync::atomic::Ordering::Relaxed);
    let up = match (up, user_up) {
        (0, u) | (u, 0) => u,
        (a, b) => a.min(b),
    };
    s.ratelimits.set_upload_bps(NonZeroU32::new(up.min(u32::MAX as u64) as u32));
}

/// The upload limit from Settings (bytes/s, 0 = none).
static USER_UP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// The download limit last given to `set_limit`, so a new upload limit keeps it.
static USER_DOWN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Gaming mode's cap when the session's limits were last set.
static APPLIED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Re-set the session's limits when gaming mode's cap moved (every engine tick).
pub fn follow_gaming(s: &Session, bps: u64) {
    if APPLIED.load(std::sync::atomic::Ordering::Relaxed) != super::gaming::effective(0) {
        set_limit(s, bps);
    }
}

pub fn set_upload_limit(s: &Session, bps: u64) {
    USER_UP.store(bps, std::sync::atomic::Ordering::Relaxed);
    set_limit(s, USER_DOWN.load(std::sync::atomic::Ordering::Relaxed));
}

/// Start the session on its own (at launch, to carry on seeding).
pub async fn start_session(session: Lazy, limit_bps: u64, events: UnboundedSender<Ev>) {
    if let Err(e) = session.get_or_start(limit_bps, &events).await {
        crate::stash::log(e);
    }
}

pub struct Task {
    pub id: u64,
    pub source: String,
    pub dir: PathBuf,
    /// The .torrent saved on an earlier run: re-adding it checks the files on disk and resumes.
    pub torrent_file: Option<PathBuf>,
    pub limit_bps: u64,
    pub session: Lazy,
    pub client: reqwest::Client,
    pub events: UnboundedSender<Ev>,
    /// The browser's cookies and referrer, for a .torrent behind a sign-in.
    pub headers: Vec<(String, String)>,
    /// The files chosen earlier (None = all).
    pub only_files: Option<Vec<usize>>,
    /// Ask which files to download before starting (when there's more than one).
    pub ask_files: bool,
}

impl Task {
    pub async fn run(self) {
        let (id, events) = (self.id, self.events.clone());
        if let Err(e) = self.go().await {
            let _ = events.send(Ev::Failed { id, error: e });
        }
    }

    async fn go(self) -> Result<(), String> {
        let session = self.session.get_or_start(self.limit_bps, &self.events).await?;

        // 1. The torrent's metadata: from the saved file, a .torrent (local or on the web),
        //    or from peers for a magnet link.
        let bytes = match &self.torrent_file {
            Some(f) => std::fs::read(f).map_err(|e| e.to_string())?,
            None => self.fetch().await?,
        };
        let listed = match &bytes[..] {
            [] => {
                let opts = AddTorrentOptions { list_only: true, ..Default::default() };
                session.add_torrent(AddTorrent::from_url(self.source.clone()), Some(opts)).await
            }
            b => {
                let opts = AddTorrentOptions { list_only: true, ..Default::default() };
                session.add_torrent(AddTorrent::from_bytes(b.to_vec()), Some(opts)).await
            }
        }
        .map_err(|e| format!("{e:#}"))?;
        let AddTorrentResponse::ListOnly(list) = listed else { return Err("Unexpected answer from BitTorrent".into()) };

        let info_hash = list.info_hash.as_string();
        let name = list.info.name().map(|n| n.into_owned()).unwrap_or_else(|| info_hash.clone());
        // never "", "." or "..": the folder would be the Downloads folder (or above it)
        let name = super::safe_name(&name).unwrap_or_else(|| info_hash.clone());
        let total = list.info.lengths().total_length();
        let multi = list.info.info().files.is_some();
        let output = if multi { self.dir.join(&name) } else { self.dir.clone() };
        let path = if multi { output.clone() } else { self.dir.join(&name) };

        let saved = super::data_dir().join("torrents").join(format!("{info_hash}.torrent"));
        if !saved.exists() {
            let _ = std::fs::write(&saved, &list.torrent_bytes);
        }
        let files = Arc::new(entries(&list.info));
        known(|k| {
            let e = k.entry(self.id).or_default();
            e.torrent_file = Some(saved.clone());
            e.files = Some(files.clone());
            e.name = name.clone();
            e.only = self.only_files.clone();
        });
        let _ = self.events.send(Ev::TorrentMeta { id: self.id, name, total, info_hash, torrent_file: saved, path });

        // 2. Which files: ask first when there's a choice to make.
        let mut only = self.only_files.clone();
        if self.ask_files && files.len() > 1 {
            let (reply, chosen) = oneshot::channel();
            let _ = self.events.send(Ev::TorrentFiles { id: self.id, reply });
            match chosen.await {
                Ok(c) => only = c,
                // paused or removed while the question was open: ask again next time
                Err(_) => return Ok(()),
            }
        }

        // 3. Download into the chosen folder. `overwrite` lets it pick up files it already
        //    has (after a restart), checking every piece first.
        let opts = AddTorrentOptions {
            overwrite: true,
            output_folder: Some(output.to_string_lossy().into_owned()),
            initial_peers: Some(list.seen_peers.clone()),
            only_files: only.clone(),
            ..Default::default()
        };
        let added = session.add_torrent(AddTorrent::from_bytes(list.torrent_bytes.clone()), Some(opts)).await.map_err(|e| format!("{e:#}"))?;
        let handle = match added {
            AddTorrentResponse::Added(_, h) | AddTorrentResponse::AlreadyManaged(_, h) => h,
            AddTorrentResponse::ListOnly(_) => return Err("Unexpected answer from BitTorrent".into()),
        };
        // one the session brought back from last time keeps its old choice: use this one
        if handle.only_files() != only {
            let all: HashSet<usize> = match &only {
                Some(o) => o.iter().copied().collect(),
                None => (0..list.info.iter_file_details().count()).collect(),
            };
            let _ = session.update_only_files(&handle, &all).await;
        }
        if handle.is_paused() {
            let _ = session.unpause(&handle).await;
        }
        let _ = self.events.send(Ev::TorrentAdded { id: self.id, handle });
        Ok(())
    }

    /// A .torrent's bytes; empty for a magnet link (the session resolves those).
    async fn fetch(&self) -> Result<Vec<u8>, String> {
        let s = self.source.trim();
        let lower = s.to_ascii_lowercase();
        if lower.starts_with("magnet:") {
            Ok(Vec::new())
        } else if lower.starts_with("http://") || lower.starts_with("https://") {
            let mut req = self.client.get(s);
            // the browser's cookies and referrer, for a tracker that wants a sign-in
            for (k, v) in &self.headers {
                if !k.eq_ignore_ascii_case("range") && !k.eq_ignore_ascii_case("host") {
                    req = req.header(k.as_str(), v.as_str());
                }
            }
            let r = req.send().await.map_err(|e| e.to_string())?;
            if !r.status().is_success() {
                return Err(format!("The server answered {}", r.status()));
            }
            Ok(r.bytes().await.map_err(|e| e.to_string())?.to_vec())
        } else {
            std::fs::read(s).map_err(|e| format!("Can't read the torrent file: {e}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(ratio: f32, minutes: u64) -> Options {
        Options { seed_ratio: ratio, seed_minutes: minutes, ..Default::default() }
    }

    #[test]
    fn no_limits_never_stop() {
        assert!(!seed_limit_reached(u64::MAX / 2, 100, u64::MAX / 2, &opts(0.0, 0)));
    }

    #[test]
    fn ratio_limit() {
        let o = opts(1.0, 0);
        assert!(!seed_limit_reached(99, 100, 10_000, &o));
        assert!(seed_limit_reached(100, 100, 0, &o));
        let o = opts(2.0, 0);
        assert!(!seed_limit_reached(150, 100, 0, &o));
        assert!(seed_limit_reached(200, 100, 0, &o));
        // nothing downloaded (unknown size): the ratio can't decide
        assert!(!seed_limit_reached(1_000, 0, 0, &o));
    }

    #[test]
    fn time_limit() {
        let o = opts(0.0, 30);
        assert!(!seed_limit_reached(0, 100, 30 * 60 - 1, &o));
        assert!(seed_limit_reached(0, 100, 30 * 60, &o));
    }

    #[test]
    fn either_limit_stops() {
        let o = opts(1.0, 60);
        assert!(seed_limit_reached(100, 100, 0, &o));
        assert!(seed_limit_reached(0, 100, 3600, &o));
        assert!(!seed_limit_reached(50, 100, 1800, &o));
    }

    #[test]
    fn uploaded_survives_restarts() {
        let (mut total, mut last) = (0, 0);
        add_uploaded(&mut total, &mut last, 100);
        add_uploaded(&mut total, &mut last, 250);
        assert_eq!(total, 250);
        // the torrent restarted: its own count began again
        add_uploaded(&mut total, &mut last, 40);
        assert_eq!(total, 290);
    }

    fn files(n: usize) -> Vec<FileEntry> {
        (0..n).map(|i| FileEntry { index: i * 2, name: format!("f{i}"), size: 10 }).collect()
    }

    #[test]
    fn choosing_everything_means_no_filter() {
        let f = files(3); // indices 0, 2, 4 (1 and 3 are padding)
        assert_eq!(only_files(&[0, 2, 4], &f), None);
        assert_eq!(only_files(&[4, 0], &f), Some(vec![0, 4]));
        assert_eq!(only_files(&[], &f), Some(vec![]));
    }

    #[test]
    fn growing_a_selection() {
        assert!(!selection_grew(None, Some(&[1])));
        assert!(selection_grew(Some(&[1]), None));
        assert!(selection_grew(Some(&[1]), Some(&[1, 2])));
        assert!(!selection_grew(Some(&[1, 2]), Some(&[2])));
    }

    #[test]
    fn options_from_settings_commands() {
        let mut o = Options::default();
        assert!(o.apply("t-ratio", "2"));
        assert!(o.apply("t-minutes", "90"));
        assert!(o.apply("t-up", "512"));
        assert!(o.apply("t-upnp", ""));
        assert_eq!((o.seed_ratio, o.seed_minutes, o.upload_kbps, o.upnp), (2.0, 90, 512, false));
        assert!(!o.apply("max", "3"));
        // older settings files have no torrent block: the defaults keep UPnP on
        let d: Options = serde_json::from_str("{}").unwrap();
        assert!(d.upnp && d.choose_files);
    }
}
