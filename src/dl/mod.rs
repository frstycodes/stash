//! The download engine: a queue of HTTP and BitTorrent downloads with priorities,
//! a limit on how many run at once, a shared speed limit, and resume across restarts.
//!
//! It runs on a small tokio runtime on its own thread. The UI thread talks to it with
//! commands and reads a snapshot back; the engine calls `notify` when the snapshot
//! changes (at most a few times a second while something downloads, never when idle).

pub mod gaming;
mod http;
mod limiter;
mod torrent;
#[cfg(test)]
mod bench;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use limiter::Limiter;

/// Progress is pushed to the UI this often while something is downloading.
const TICK: Duration = Duration::from_millis(500);
/// The job list is written to disk at most this often while downloads run.
const SAVE_EVERY: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Priority {
    Low,
    #[default]
    Normal,
    High,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Http,
    Torrent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    Queued,
    Active,
    Paused,
    Done,
    Seeding,
    Failed,
}

impl State {
    pub fn finished(self) -> bool {
        matches!(self, State::Done | State::Seeding)
    }
}

/// Engine settings (from the app's settings file).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Config {
    pub max_active: usize,
    pub connections: usize,
    pub limit_bps: u64, // 0 = unlimited
    pub seed: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self { max_active: 3, connections: 8, limit_bps: 0, seed: false }
    }
}

/// What the UI shows for one download.
#[derive(Clone, Debug, PartialEq)]
pub struct JobView {
    pub id: u64,
    pub kind: Kind,
    pub name: String,
    pub source: String,
    pub dir: PathBuf,
    /// The finished file (or a torrent's folder), once known.
    pub path: Option<PathBuf>,
    pub state: State,
    /// What an active download is doing right now ("Connecting…", "Checking files…").
    pub phase: Option<&'static str>,
    pub priority: Priority,
    pub total: Option<u64>,
    pub done: u64,
    pub speed: u64,
    pub up_speed: u64,
    pub peers: Option<usize>,
    pub connections: usize,
    pub resumable: bool,
    pub error: Option<String>,
    pub added: u64,
    /// When it finished (unix seconds), for grouping history by day.
    pub finished_at: Option<u64>,
    /// The page the download came from (the browser's referrer), if known.
    pub referrer: Option<String>,
    /// The link carries an expiry that has passed (signed links), so it can't be fetched again.
    pub expired: bool,
}

impl JobView {
    pub fn fraction(&self) -> Option<f32> {
        let total = self.total.filter(|t| *t > 0)?;
        Some((self.done as f64 / total as f64).clamp(0.0, 1.0) as f32)
    }

    pub fn eta_secs(&self) -> Option<u64> {
        let total = self.total?;
        (self.speed > 0).then(|| total.saturating_sub(self.done) / self.speed)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub jobs: Vec<JobView>,
    pub down: u64,
    pub up: u64,
    /// Bumps on every change, so the UI can skip identical snapshots.
    pub version: u64,
}

impl Snapshot {
    /// Overall progress of everything downloading now (for the stack's progress bar).
    pub fn active_fraction(&self) -> Option<f32> {
        let active: Vec<&JobView> = self.jobs.iter().filter(|j| j.state == State::Active).collect();
        if active.is_empty() {
            return None;
        }
        let total: u64 = active.iter().filter_map(|j| j.total).sum();
        let done: u64 = active.iter().filter(|j| j.total.is_some()).map(|j| j.done).sum();
        Some(if total == 0 { 0.0 } else { (done as f64 / total as f64) as f32 })
    }
}

#[derive(Clone, Debug, Default)]
pub struct AddRequest {
    /// An http(s) URL, a magnet link, or the path of a .torrent file.
    pub source: String,
    pub dir: PathBuf,
    pub priority: Priority,
    pub paused: bool,
    /// The file name the browser chose, if a browser handed this over.
    pub name: Option<String>,
    /// Extra request headers (a browser's cookies and referrer), for downloads behind a sign-in.
    pub headers: Vec<(String, String)>,
}

static ENGINE: std::sync::OnceLock<Engine> = std::sync::OnceLock::new();

/// Start the engine once, at launch.
pub fn init(config: Config, notify: impl Fn() + Send + Sync + 'static) {
    let notify = Arc::new(notify);
    let poke = notify.clone();
    ENGINE.get_or_init(|| Engine::start(config, move || notify()));
    gaming::start(move || poke());
}

pub fn engine_opt() -> Option<&'static Engine> {
    ENGINE.get()
}

/// A file name from outside (a browser, a torrent) made safe to save under; None if
/// nothing usable is left.
pub fn safe_name(name: &str) -> Option<String> {
    Some(http::sanitize(name)).filter(|n| !n.is_empty())
}

pub fn engine() -> &'static Engine {
    ENGINE.get().expect("download engine not started")
}

enum Cmd {
    Add(AddRequest),
    Pause(u64),
    Resume(u64),
    StartNow(u64),
    /// `permanent`: the file skips the Recycle Bin (Shift+Delete).
    Remove { id: u64, delete_files: bool, permanent: bool },
    Again(u64),
    Priority(u64, Priority),
    PauseAll,
    ResumeAll,
    ClearFinished,
    Configure(Config),
    Shutdown(std::sync::mpsc::Sender<()>),
}

/// Messages from running downloads back to the engine.
pub(crate) enum Ev {
    /// The server told us the file's name and size.
    HttpMeta { id: u64, name: String, total: Option<u64>, resumable: bool, temp: PathBuf },
    HttpDone { id: u64, path: PathBuf, total: u64 },
    TorrentMeta { id: u64, name: String, total: u64, info_hash: String, torrent_file: PathBuf, path: PathBuf },
    TorrentAdded { id: u64, handle: Arc<librqbit::ManagedTorrent> },
    Failed { id: u64, error: String },
}

pub fn is_source(s: &str) -> bool {
    let s = s.trim();
    let lower = s.to_ascii_lowercase();
    lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("magnet:?")
        || (lower.ends_with(".torrent") && Path::new(s).is_file())
}

fn is_torrent_source(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    lower.starts_with("magnet:") || lower.ends_with(".torrent") || lower.split('?').next().is_some_and(|p| p.ends_with(".torrent"))
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

/// A downloading app's icon, read early: a copy of just its headers and icon resources
/// (see `http::early_icon`), which the shell reads like the real file.
pub fn app_icon(id: u64) -> PathBuf {
    data_dir().join("icons").join(format!("{id}.exe"))
}

/// A downloading photo's preview, read early: the small picture a camera or phone puts
/// at the start of a JPEG (its EXIF thumbnail), saved as a JPEG of its own.
pub fn photo_preview(id: u64) -> PathBuf {
    data_dir().join("icons").join(format!("{id}.jpg"))
}

/// Whichever early picture a download has so far (an app's icon, a photo's preview).
pub fn early_picture(id: u64) -> Option<PathBuf> {
    [app_icon(id), photo_preview(id)].into_iter().find(|p| p.exists())
}

pub fn data_dir() -> PathBuf {
    // STASH_DATA moves the download list and torrent state elsewhere (tests)
    if let Some(d) = std::env::var_os("STASH_DATA") {
        return PathBuf::from(d);
    }
    PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default()).join("Stash")
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Job {
    id: u64,
    kind: Kind,
    source: String,
    name: String,
    dir: PathBuf,
    priority: Priority,
    state: State,
    /// Tie-break within a priority: lower runs first.
    order: u64,
    total: Option<u64>,
    done: u64,
    added: u64,
    #[serde(default)]
    finished_at: Option<u64>,
    #[serde(default)]
    path: Option<PathBuf>,
    #[serde(default)]
    error: Option<String>,
    // HTTP: the partial file and the byte ranges still to fetch, for resuming.
    #[serde(default)]
    temp: Option<PathBuf>,
    #[serde(default)]
    remaining: Vec<(u64, u64)>,
    #[serde(default)]
    resumable: bool,
    // Torrent: the saved .torrent, so it can be re-added after a restart.
    #[serde(default)]
    torrent_file: Option<PathBuf>,
    #[serde(default)]
    info_hash: Option<String>,
    #[serde(default)]
    headers: Vec<(String, String)>,
    #[serde(default)]
    named_by_browser: bool,

    #[serde(skip)]
    speed: u64,
    #[serde(skip)]
    up_speed: u64,
    #[serde(skip)]
    peers: Option<usize>,
    #[serde(skip)]
    phase: Option<&'static str>,
    #[serde(skip)]
    connections: usize,
    #[serde(skip)]
    last_done: u64,
}

impl Job {
    /// A browser's cookies are only for getting this download; once it's done they go.
    fn forget_cookies(&mut self) {
        self.headers.retain(|(k, _)| !k.eq_ignore_ascii_case("cookie") && !k.eq_ignore_ascii_case("authorization"));
    }

    fn view(&self) -> JobView {
        JobView {
            id: self.id,
            kind: self.kind,
            name: self.name.clone(),
            source: self.source.clone(),
            dir: self.dir.clone(),
            path: self.path.clone(),
            state: self.state,
            phase: self.phase,
            priority: self.priority,
            total: self.total,
            done: self.done,
            speed: self.speed,
            up_speed: self.up_speed,
            peers: self.peers,
            connections: self.connections,
            resumable: self.resumable || self.kind == Kind::Torrent,
            error: self.error.clone(),
            added: self.added,
            finished_at: self.finished_at,
            referrer: self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("referer")).map(|(_, v)| v.clone()),
            expired: link_expired(&self.source),
        }
    }
}

/// The handle the UI thread keeps.
pub struct Engine {
    tx: UnboundedSender<Cmd>,
    snapshot: Arc<Mutex<Snapshot>>,
}

impl Engine {
    pub fn start(config: Config, notify: impl Fn() + Send + Sync + 'static) -> Engine {
        let (tx, rx) = unbounded_channel();
        let snapshot = Arc::new(Mutex::new(Snapshot::default()));
        let shared = snapshot.clone();
        std::thread::Builder::new()
            .name("stash-downloads".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .max_blocking_threads(4)
                    .thread_name("stash-dl")
                    .enable_all()
                    .build()
                    .expect("download runtime");
                rt.block_on(Actor::new(config, shared, Box::new(notify)).run(rx));
            })
            .expect("download thread");
        Engine { tx, snapshot }
    }

    pub fn snapshot(&self) -> Snapshot {
        self.snapshot.lock().unwrap().clone()
    }

    fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }

    pub fn add(&self, req: AddRequest) {
        self.send(Cmd::Add(req));
    }
    pub fn pause(&self, id: u64) {
        self.send(Cmd::Pause(id));
    }
    pub fn resume(&self, id: u64) {
        self.send(Cmd::Resume(id));
    }
    pub fn start_now(&self, id: u64) {
        self.send(Cmd::StartNow(id));
    }
    /// Fetch a finished (or failed) download again from its link, as a new download.
    /// A finished one whose file is gone is replaced; otherwise the old entry stays.
    pub fn again(&self, id: u64) {
        self.send(Cmd::Again(id));
    }
    pub fn remove(&self, id: u64, delete_files: bool) {
        self.send(Cmd::Remove { id, delete_files, permanent: false });
    }

    /// Remove a download and delete its file for good, not to the Recycle Bin.
    pub fn remove_permanently(&self, id: u64) {
        self.send(Cmd::Remove { id, delete_files: true, permanent: true });
    }
    pub fn set_priority(&self, id: u64, p: Priority) {
        self.send(Cmd::Priority(id, p));
    }
    pub fn pause_all(&self) {
        self.send(Cmd::PauseAll);
    }
    pub fn resume_all(&self) {
        self.send(Cmd::ResumeAll);
    }
    pub fn clear_finished(&self) {
        self.send(Cmd::ClearFinished);
    }
    pub fn configure(&self, c: Config) {
        self.send(Cmd::Configure(c));
    }

    /// Save progress and stop every download (on quit). Waits up to a second.
    pub fn shutdown(&self) {
        let (tx, rx) = std::sync::mpsc::channel();
        self.send(Cmd::Shutdown(tx));
        let _ = rx.recv_timeout(Duration::from_secs(1));
    }
}

struct Running {
    task: tokio::task::JoinHandle<()>,
    http: Option<Arc<http::Progress>>,
}

struct Actor {
    config: Config,
    jobs: Vec<Job>,
    next_id: u64,
    next_order: u64,
    running: HashMap<u64, Running>,
    torrents: HashMap<u64, Arc<librqbit::ManagedTorrent>>,
    session: torrent::Lazy,
    client: reqwest::Client,
    limiter: Arc<Limiter>,
    ev_tx: UnboundedSender<Ev>,
    ev_rx: Option<UnboundedReceiver<Ev>>,
    shared: Arc<Mutex<Snapshot>>,
    notify: Box<dyn Fn() + Send + Sync>,
    version: u64,
    dirty: bool,
    last_save: Instant,
    last_tick: Instant,
}

impl Actor {
    fn new(config: Config, shared: Arc<Mutex<Snapshot>>, notify: Box<dyn Fn() + Send + Sync>) -> Self {
        let (ev_tx, ev_rx) = unbounded_channel();
        let mut jobs = load();
        for j in &mut jobs {
            // Anything that was running when we quit picks up where it left off.
            if j.state == State::Active {
                j.state = State::Queued;
            }
            if j.state == State::Seeding {
                j.state = State::Done;
            }
        }
        let next_id = jobs.iter().map(|j| j.id).max().unwrap_or(0) + 1;
        let next_order = jobs.iter().map(|j| j.order).max().unwrap_or(0) + 1;
        let client = reqwest::Client::builder()
            .user_agent(concat!("Mozilla/5.0 (Windows NT 10.0; Win64; x64) Stash/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(20))
            .read_timeout(Duration::from_secs(30))
            .build()
            .expect("http client");
        gaming::set_user_limit(config.limit_bps);
        Actor {
            limiter: Arc::new(Limiter::new(config.limit_bps)),
            config,
            jobs,
            next_id,
            next_order,
            running: HashMap::new(),
            torrents: HashMap::new(),
            session: torrent::Lazy::default(),
            client,
            ev_tx,
            ev_rx: Some(ev_rx),
            shared,
            notify,
            version: 0,
            dirty: false,
            last_save: Instant::now(),
            last_tick: Instant::now(),
        }
    }

    async fn run(mut self, mut rx: UnboundedReceiver<Cmd>) {
        let mut ev_rx = self.ev_rx.take().unwrap();
        self.schedule();
        self.publish();
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let busy = self.jobs.iter().any(|j| matches!(j.state, State::Active | State::Seeding));
            tokio::select! {
                cmd = rx.recv() => {
                    let Some(cmd) = cmd else { break };
                    if let Cmd::Shutdown(done) = cmd {
                        self.stop_all();
                        save(&self.jobs);
                        let _ = done.send(());
                        break;
                    }
                    self.on_cmd(cmd).await;
                }
                Some(ev) = ev_rx.recv() => self.on_event(ev).await,
                // only tick while something is moving: an idle engine does no work
                _ = tick.tick(), if busy => self.on_tick().await,
            }
            self.schedule();
            self.publish();
            if self.dirty && (self.last_save.elapsed() >= SAVE_EVERY || !self.jobs.iter().any(|j| j.state == State::Active)) {
                self.persist_progress();
                save(&self.jobs);
                self.dirty = false;
                self.last_save = Instant::now();
            }
        }
    }

    fn job(&mut self, id: u64) -> Option<&mut Job> {
        self.jobs.iter_mut().find(|j| j.id == id)
    }

    async fn on_cmd(&mut self, cmd: Cmd) {
        self.dirty = true;
        match cmd {
            Cmd::Add(req) => self.add(req),
            Cmd::Pause(id) => self.pause(id).await,
            Cmd::Resume(id) => {
                if let Some(j) = self.job(id) {
                    if matches!(j.state, State::Paused | State::Failed) {
                        j.state = State::Queued;
                        j.error = None;
                    }
                }
            }
            Cmd::StartNow(id) => {
                if let Some(j) = self.job(id).filter(|j| !j.state.finished() && j.state != State::Active) {
                    j.error = None;
                    j.state = State::Queued;
                }
                self.start(id);
            }
            Cmd::Remove { id, delete_files, permanent } => self.remove(id, delete_files, permanent).await,
            Cmd::Again(id) => {
                let Some(old) = self.jobs.iter().find(|j| j.id == id).cloned() else { return };
                let gone = old.state.finished() && old.path.as_ref().is_some_and(|p| !p.exists());
                if gone || old.state == State::Failed {
                    self.remove(id, false, false).await;
                }
                self.add(AddRequest {
                    source: old.source.clone(),
                    dir: old.dir.clone(),
                    priority: old.priority,
                    paused: false,
                    name: old.named_by_browser.then(|| old.name.clone()),
                    headers: old.headers.clone(),
                });
            }
            Cmd::Priority(id, p) => {
                if let Some(j) = self.job(id) {
                    j.priority = p;
                }
            }
            Cmd::PauseAll => {
                let ids: Vec<u64> = self.jobs.iter().filter(|j| matches!(j.state, State::Active | State::Queued)).map(|j| j.id).collect();
                for id in ids {
                    self.pause(id).await;
                }
            }
            Cmd::ResumeAll => {
                for j in self.jobs.iter_mut().filter(|j| matches!(j.state, State::Paused | State::Failed)) {
                    j.state = State::Queued;
                    j.error = None;
                }
            }
            Cmd::ClearFinished => {
                let ids: Vec<u64> = self.jobs.iter().filter(|j| j.state == State::Done).map(|j| j.id).collect();
                for id in ids {
                    self.remove(id, false, false).await;
                }
            }
            Cmd::Configure(c) => {
                gaming::set_user_limit(c.limit_bps);
                if c.limit_bps != self.config.limit_bps {
                    self.limiter.set(c.limit_bps);
                    if let Some(s) = self.session.get() {
                        torrent::set_limit(&s, c.limit_bps);
                    }
                }
                if c.seed != self.config.seed && !c.seed {
                    // stop seeding what's finished
                    let ids: Vec<u64> = self.jobs.iter().filter(|j| j.state == State::Seeding).map(|j| j.id).collect();
                    for id in ids {
                        self.pause(id).await;
                        if let Some(j) = self.job(id) {
                            j.state = State::Done;
                        }
                    }
                }
                self.config = c;
            }
            Cmd::Shutdown(_) => {}
        }
    }

    fn add(&mut self, req: AddRequest) {
        let source = req.source.trim().to_string();
        if source.is_empty() {
            return;
        }
        // the same link again: don't start a second copy of a download still on the list
        if let Some(j) = self.jobs.iter_mut().find(|j| j.source == source && !j.state.finished()) {
            if matches!(j.state, State::Paused | State::Failed) && !req.paused {
                j.state = State::Queued;
                j.error = None;
            }
            return;
        }
        let kind = if is_torrent_source(&source) { Kind::Torrent } else { Kind::Http };
        let name = req.name.clone().filter(|n| !n.trim().is_empty()).unwrap_or_else(|| guess_name(&source));
        let job = Job {
            id: self.next_id,
            kind,
            source,
            name,
            dir: req.dir,
            priority: req.priority,
            state: if req.paused { State::Paused } else { State::Queued },
            order: self.next_order,
            total: None,
            done: 0,
            added: now(),
            finished_at: None,
            path: None,
            error: None,
            temp: None,
            remaining: Vec::new(),
            resumable: false,
            torrent_file: None,
            info_hash: None,
            named_by_browser: req.name.is_some(),
            headers: req.headers,
            speed: 0,
            up_speed: 0,
            peers: None,
            phase: None,
            connections: 0,
            last_done: 0,
        };
        self.next_id += 1;
        self.next_order += 1;
        self.jobs.push(job);
    }

    /// Start queued downloads, highest priority (then oldest) first, up to the limit.
    fn schedule(&mut self) {
        let active = self.jobs.iter().filter(|j| j.state == State::Active).count();
        let free = self.config.max_active.max(1).saturating_sub(active);
        if free == 0 {
            return;
        }
        let mut queued: Vec<&Job> = self.jobs.iter().filter(|j| j.state == State::Queued).collect();
        queued.sort_by(|a, b| b.priority.cmp(&a.priority).then(a.order.cmp(&b.order)));
        let ids: Vec<u64> = queued.iter().take(free).map(|j| j.id).collect();
        for id in ids {
            self.start(id);
        }
    }

    fn start(&mut self, id: u64) {
        if self.running.contains_key(&id) {
            return;
        }
        let connections = self.config.connections.max(1);
        let client = self.client.clone();
        let limiter = self.limiter.clone();
        let events = self.ev_tx.clone();
        let Some(job) = self.jobs.iter_mut().find(|j| j.id == id) else { return };
        if job.state != State::Queued {
            return;
        }
        job.state = State::Active;
        job.error = None;
        job.speed = 0;
        job.last_done = job.done;
        self.dirty = true;
        match job.kind {
            Kind::Http => {
                job.phase = Some("Connecting…");
                let progress = Arc::new(http::Progress::new(job.done, &job.remaining));
                let task = http::Task {
                    id,
                    url: job.source.clone(),
                    dir: job.dir.clone(),
                    name: (!job.name.is_empty()).then(|| job.name.clone()),
                    temp: job.temp.clone().filter(|t| t.exists()),
                    total: job.total,
                    resumable: job.resumable,
                    headers: job.headers.clone(),
                    from_browser: job.named_by_browser,
                    connections,
                    client,
                    limiter,
                    progress: progress.clone(),
                    events,
                };
                let handle = tokio::spawn(task.run());
                self.running.insert(id, Running { task: handle, http: Some(progress) });
            }
            Kind::Torrent => {
                if let Some(handle) = self.torrents.get(&id).cloned() {
                    // paused earlier in this session: just carry on
                    job.phase = None;
                    let session = self.session.get();
                    let handle2 = handle.clone();
                    let task = tokio::spawn(async move {
                        if let Some(s) = session {
                            let _ = s.unpause(&handle2).await;
                        }
                    });
                    self.running.insert(id, Running { task, http: None });
                    return;
                }
                job.phase = Some(if job.torrent_file.is_some() { "Checking files…" } else { "Fetching metadata…" });
                let task = torrent::Task {
                    id,
                    source: job.source.clone(),
                    dir: job.dir.clone(),
                    torrent_file: job.torrent_file.clone().filter(|f| f.exists()),
                    limit_bps: self.config.limit_bps,
                    session: self.session.clone(),
                    client,
                    events,
                };
                let handle = tokio::spawn(task.run());
                self.running.insert(id, Running { task: handle, http: None });
            }
        }
    }

    async fn pause(&mut self, id: u64) {
        self.persist_progress();
        if let Some(r) = self.running.remove(&id) {
            if r.http.is_some() {
                r.task.abort();
            }
        }
        if let (Some(h), Some(s)) = (self.torrents.get(&id).cloned(), self.session.get()) {
            let _ = s.pause(&h).await;
        }
        if let Some(j) = self.job(id) {
            if matches!(j.state, State::Active | State::Queued | State::Seeding) {
                j.state = if j.state == State::Seeding { State::Done } else { State::Paused };
                j.speed = 0;
                j.up_speed = 0;
                j.phase = None;
                j.connections = 0;
            }
        }
    }

    async fn remove(&mut self, id: u64, delete_files: bool, permanent: bool) {
        if let Some(r) = self.running.remove(&id) {
            r.task.abort();
        }
        let handle = self.torrents.remove(&id);
        let Some(pos) = self.jobs.iter().position(|j| j.id == id) else { return };
        let job = self.jobs.remove(pos);
        if let (Some(h), Some(s)) = (handle, self.session.get()) {
            // stop it; its files are recycled below rather than deleted by the session
            let _ = s.delete(librqbit::api::TorrentIdOrHash::Id(h.id()), delete_files && !job.state.finished()).await;
        }
        if let Some(t) = &job.temp {
            let _ = std::fs::remove_file(t);
        }
        if let Some(f) = &job.torrent_file {
            let _ = std::fs::remove_file(f);
        }
        // a finished download's file (or a torrent's folder) goes to the Recycle Bin,
        // unless the user asked for it to go for good (Shift+Delete, after a confirmation)
        if delete_files {
            // never the folder it was saved into (or anything outside it): a torrent named
            // "." or ".." must not take the Downloads folder with it
            let inside = |p: &&PathBuf| p.parent().is_some_and(|d| d.starts_with(&job.dir)) && p.file_name().is_some();
            if let Some(p) = job.path.as_ref().filter(|p| p.exists()).filter(inside) {
                if permanent { crate::shell::delete_permanently(p) } else { crate::shell::recycle(p) }
            }
        }
    }

    fn stop_all(&mut self) {
        self.persist_progress();
        for (_, r) in self.running.drain() {
            r.task.abort();
        }
    }

    /// Copy live HTTP progress into the jobs, for saving.
    fn persist_progress(&mut self) {
        for j in &mut self.jobs {
            if let Some(p) = self.running.get(&j.id).and_then(|r| r.http.as_ref()) {
                j.done = p.done.load(Ordering::Relaxed);
                j.remaining = p.remaining();
            }
        }
    }

    async fn on_event(&mut self, ev: Ev) {
        self.dirty = true;
        match ev {
            Ev::HttpMeta { id, name, total, resumable, temp } => {
                if let Some(j) = self.job(id) {
                    j.name = name;
                    j.total = total;
                    j.resumable = resumable;
                    j.temp = Some(temp);
                    j.phase = None;
                }
            }
            Ev::HttpDone { id, path, total } => {
                self.running.remove(&id);
                if let Some(j) = self.job(id) {
                    j.state = State::Done;
                    j.name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    j.path = Some(path);
                    j.total = Some(total);
                    j.done = total;
                    j.temp = None;
                    j.remaining.clear();
                    j.speed = 0;
                    j.connections = 0;
                    j.finished_at = Some(now());
                    j.forget_cookies();
                }
            }
            Ev::TorrentMeta { id, name, total, info_hash, torrent_file, path } => {
                if let Some(j) = self.job(id) {
                    j.name = name;
                    j.total = Some(total);
                    j.info_hash = Some(info_hash);
                    j.torrent_file = Some(torrent_file);
                    j.path = Some(path);
                    j.phase = Some("Checking files…");
                }
            }
            Ev::TorrentAdded { id, handle } => {
                let still_wanted = self.jobs.iter().any(|j| j.id == id && j.state == State::Active);
                if still_wanted {
                    self.torrents.insert(id, handle);
                } else if let Some(s) = self.session.get() {
                    // paused or removed while it was starting
                    if self.jobs.iter().any(|j| j.id == id) {
                        let _ = s.pause(&handle).await;
                        self.torrents.insert(id, handle);
                    } else {
                        let _ = s.delete(librqbit::api::TorrentIdOrHash::Id(handle.id()), false).await;
                    }
                }
            }
            Ev::Failed { id, error } => {
                self.persist_progress();
                self.running.remove(&id);
                if let Some(j) = self.job(id) {
                    if j.state == State::Active {
                        j.state = State::Failed;
                        j.error = Some(error);
                        j.speed = 0;
                        j.phase = None;
                        j.connections = 0;
                    }
                }
            }
        }
    }

    async fn on_tick(&mut self) {
        let dt = self.last_tick.elapsed().as_secs_f64().max(0.05);
        self.last_tick = Instant::now();
        let seed = self.config.seed;
        let mut stop_seeding = Vec::new();
        let mut failed = Vec::new();
        for j in &mut self.jobs {
            if !matches!(j.state, State::Active | State::Seeding) {
                continue;
            }
            match j.kind {
                Kind::Http => {
                    let Some(p) = self.running.get(&j.id).and_then(|r| r.http.as_ref()) else { continue };
                    j.done = p.done.load(Ordering::Relaxed);
                    j.connections = p.connections.load(Ordering::Relaxed);
                    let instant = (j.done.saturating_sub(j.last_done)) as f64 / dt;
                    // smooth the speed so the number (and the time left) doesn't jitter
                    j.speed = if j.speed == 0 { instant as u64 } else { (j.speed as f64 * 0.7 + instant * 0.3) as u64 };
                    j.last_done = j.done;
                }
                Kind::Torrent => {
                    let Some(h) = self.torrents.get(&j.id) else { continue };
                    let st = h.stats();
                    j.done = st.progress_bytes;
                    if st.total_bytes > 0 {
                        j.total = Some(st.total_bytes);
                    }
                    if let Some(e) = st.error.clone() {
                        j.state = State::Failed;
                        j.error = Some(e);
                        j.speed = 0;
                        j.phase = None;
                        // let go of it, so Resume or Retry can start it afresh
                        if let Some(r) = self.running.remove(&j.id) {
                            r.task.abort();
                        }
                        failed.push(j.id);
                        self.dirty = true;
                        continue;
                    }
                    use librqbit::TorrentStatsState as S;
                    j.phase = match st.state {
                        S::Initializing { .. } => Some("Checking files…"),
                        S::Live if j.done == 0 && st.live.as_ref().is_some_and(|l| l.snapshot.peer_stats.live == 0) => Some("Finding peers…"),
                        _ => None,
                    };
                    if let Some(live) = &st.live {
                        j.speed = live.download_speed.as_bytes();
                        j.up_speed = live.upload_speed.as_bytes();
                        j.peers = Some(live.snapshot.peer_stats.live as usize);
                    }
                    if st.finished && j.state == State::Active {
                        j.finished_at = Some(now());
                        j.speed = 0;
                        j.phase = None;
                        if seed {
                            j.state = State::Seeding;
                        } else {
                            j.state = State::Done;
                            j.up_speed = 0;
                            j.peers = None;
                            stop_seeding.push(j.id);
                        }
                        self.running.remove(&j.id);
                        self.dirty = true;
                    }
                }
            }
        }
        if let Some(s) = self.session.get() {
            torrent::follow_gaming(&s, self.config.limit_bps);
            for id in failed {
                if let Some(h) = self.torrents.remove(&id) {
                    let _ = s.delete(librqbit::api::TorrentIdOrHash::Id(h.id()), false).await;
                }
            }
            for id in stop_seeding {
                if let Some(h) = self.torrents.get(&id) {
                    let _ = s.pause(h).await;
                }
            }
        }
        self.dirty |= self.jobs.iter().any(|j| j.state == State::Active);
    }

    fn publish(&mut self) {
        let jobs: Vec<JobView> = self.jobs.iter().map(Job::view).collect();
        let mut snap = self.shared.lock().unwrap();
        if snap.jobs == jobs {
            return;
        }
        self.version += 1;
        snap.down = jobs.iter().map(|j| j.speed).sum();
        snap.up = jobs.iter().map(|j| j.up_speed).sum();
        snap.jobs = jobs;
        snap.version = self.version;
        drop(snap);
        (self.notify)();
    }
}

/// Signed links say when they stop working: `expires=`, `exp=`, `se=` (Azure) or
/// `X-Amz-Date` + `X-Amz-Expires` (S3). True when that moment has passed.
pub fn link_expired(url: &str) -> bool {
    let Some(query) = url.split_once('?').map(|(_, q)| q) else { return false };
    let params: Vec<(String, String)> = query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.to_ascii_lowercase(), percent_encoding::percent_decode_str(v).decode_utf8_lossy().into_owned()))
        .collect();
    let get = |k: &str| params.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
    let now = now();
    for key in ["expires", "exp", "expire", "e"] {
        if let Some(t) = get(key).and_then(|v| v.parse::<u64>().ok()) {
            // unix seconds (or milliseconds)
            let t = if t > 100_000_000_000 { t / 1000 } else { t };
            if t > 1_000_000_000 {
                return t < now;
            }
        }
    }
    if let (Some(date), Some(secs)) = (get("x-amz-date"), get("x-amz-expires").and_then(|v| v.parse::<u64>().ok())) {
        if let Some(start) = parse_amz_date(date) {
            return start + secs < now;
        }
    }
    if let Some(se) = get("se") {
        if let Some(t) = parse_iso_date(se) {
            return t < now;
        }
    }
    false
}

/// "20260101T120000Z"
fn parse_amz_date(s: &str) -> Option<u64> {
    // (it comes from the link, so it may be anything: never slice blindly)
    let iso = format!("{}-{}-{}T{}:{}:{}Z", s.get(0..4)?, s.get(4..6)?, s.get(6..8)?, s.get(9..11)?, s.get(11..13)?, s.get(13..15)?);
    parse_iso_date(&iso)
}

/// "2026-01-01T12:00:00Z" (or just the date) to unix seconds.
fn parse_iso_date(s: &str) -> Option<u64> {
    let num = |a: usize, b: usize| s.get(a..b)?.parse::<i64>().ok();
    let (y, m, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hh, mm, ss) = (num(11, 13).unwrap_or(0), num(14, 16).unwrap_or(0), num(17, 19).unwrap_or(0));
    // days from civil (Howard Hinnant)
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    u64::try_from(days * 86400 + hh * 3600 + mm * 60 + ss).ok()
}

/// A placeholder name until the server (or the torrent) says otherwise.
fn guess_name(source: &str) -> String {
    let lower = source.to_ascii_lowercase();
    if lower.starts_with("magnet:") {
        return source
            .split(['?', '&'])
            .find_map(|kv| kv.strip_prefix("dn="))
            .map(|n| percent_encoding::percent_decode_str(&n.replace('+', " ")).decode_utf8_lossy().into_owned())
            .unwrap_or_else(|| "Magnet link".into());
    }
    if !lower.starts_with("http") {
        return Path::new(source).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    }
    http::name_from_url(source).unwrap_or_default()
}

fn store() -> PathBuf {
    data_dir().join("downloads.json")
}

fn load() -> Vec<Job> {
    let f = store();
    let Ok(text) = std::fs::read_to_string(&f) else { return Vec::new() };
    match serde_json::from_str::<Vec<Job>>(&text) {
        Ok(mut jobs) => {
            // browser cookies are only kept while a download still needs them
            for j in jobs.iter_mut().filter(|j| j.state.finished()) {
                j.forget_cookies();
            }
            jobs
        }
        Err(_) => {
            // unreadable: keep it aside rather than letting the next save overwrite it
            let _ = std::fs::rename(&f, f.with_extension(format!("json.unreadable-{}", now())));
            Vec::new()
        }
    }
}

fn save(jobs: &[Job]) {
    let f = store();
    let _ = std::fs::create_dir_all(data_dir());
    if let Ok(json) = serde_json::to_string(jobs) {
        // write then rename, so a crash mid-write never loses the list
        let tmp = f.with_extension("json.tmp");
        let written = std::fs::File::create(&tmp).and_then(|mut file| {
            use std::io::Write;
            file.write_all(json.as_bytes())?;
            file.sync_all()
        });
        if written.is_ok() {
            let _ = std::fs::rename(&tmp, &f);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn odd_link_dates_dont_crash() {
        assert_eq!(parse_amz_date("2026é0101T1200000Z"), None);
        assert_eq!(parse_amz_date("ééééééééé"), None);
        assert!(parse_amz_date("20260101T120000Z").is_some());
    }

    #[test]
    fn names_from_outside_are_made_safe() {
        assert_eq!(safe_name(".."), None);
        assert_eq!(safe_name("."), None);
        assert_eq!(safe_name("  "), None);
        assert_eq!(safe_name("C:evil.exe").as_deref(), Some("C_evil.exe"));
        assert_eq!(safe_name("a:b").as_deref(), Some("a_b"));
        assert_eq!(safe_name("invoice\u{202E}txt.exe").as_deref(), Some("invoicetxt.exe"));
        assert_eq!(safe_name("con.txt").as_deref(), Some("_con.txt"));
        assert_eq!(safe_name("COM1").as_deref(), Some("_COM1"));
        assert_eq!(safe_name("report.pdf").as_deref(), Some("report.pdf"));
    }

    /// Shift+Delete's delete skips the Recycle Bin, and takes folders too.
    #[test]
    fn delete_permanently() {
        let dir = std::env::temp_dir().join("stash-permanent-delete-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("inner")).unwrap();
        let file = dir.join("file.txt");
        std::fs::write(&file, b"x").unwrap();
        std::fs::write(dir.join("inner").join("more.txt"), b"y").unwrap();
        crate::shell::delete_permanently(&file);
        assert!(!file.exists());
        crate::shell::delete_permanently(&dir);
        assert!(!dir.exists());
    }

    #[test]
    fn expiry() {
        assert!(link_expired("https://x.net/f.pdf?sig=8f&expires=1730000000"));
        assert!(!link_expired("https://x.net/f.pdf?sig=8f&expires=4102444800"));
        assert!(link_expired("https://b.s3.amazonaws.com/k?X-Amz-Date=20240101T000000Z&X-Amz-Expires=3600&X-Amz-Signature=x"));
        assert!(link_expired("https://a.blob.core.windows.net/c/f?se=2024-01-01T00:00:00Z&sig=x"));
        assert!(!link_expired("https://github.com/rsms/inter/releases/download/v4.1/Inter-4.1.zip"));
        assert!(!link_expired("https://example.com/watch?e=5"));
        assert_eq!(parse_iso_date("1970-01-02T00:00:00Z"), Some(86400));
    }

    /// Network test: `cargo test --release dl::tests::torrent -- --ignored --nocapture`
    /// (Big Buck Bunny, Creative Commons). Passes once data arrives from peers.
    #[test]
    #[ignore]
    fn torrent() {
        let dir = std::env::temp_dir().join("stash-dl-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        unsafe { std::env::set_var("STASH_DATA", dir.join("data")) };
        let magnet = "magnet:?xt=urn:btih:dd8255ecdc7ca55fb0bbf81323d87062db1f6d1c&dn=Big+Buck+Bunny&tr=udp%3A%2F%2Fexplodie.org%3A6969&tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337%2Fannounce&tr=udp%3A%2F%2Ftracker.empire-js.us%3A1337&tr=wss%3A%2F%2Ftracker.btorrent.xyz&tr=wss%3A%2F%2Ftracker.openwebtorrent.com";
        let engine = Engine::start(Config::default(), || {});
        engine.add(AddRequest { source: magnet.into(), dir: dir.clone(), ..Default::default() });
        let start = Instant::now();
        loop {
            std::thread::sleep(Duration::from_secs(1));
            let s = engine.snapshot();
            let j = s.jobs.last().unwrap();
            println!("{:?} {:?} {} {}/{:?} {} B/s peers={:?} {:?}", j.state, j.phase, j.name, j.done, j.total, j.speed, j.peers, j.error);
            if j.done > 1_000_000 || j.state == State::Failed || start.elapsed().as_secs() > 120 {
                assert!(j.done > 1_000_000, "no data from peers");
                engine.remove(j.id, true);
                std::thread::sleep(Duration::from_secs(1));
                break;
            }
        }
    }

    /// Network test: an app shows its own icon while it's still downloading.
    /// `cargo test --release dl::tests::early_icon -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn early_icon() {
        let dir = std::env::temp_dir().join("stash-dl-test-icon");
        unsafe { std::env::set_var("STASH_DATA", dir.join("data")) };
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let url = std::env::var("STASH_TEST_URL").unwrap_or("https://www.python.org/ftp/python/3.12.7/python-3.12.7-amd64.exe".into());
        // slow enough that the icon must come before the end
        let engine = Engine::start(Config { max_active: 1, connections: 4, limit_bps: 1_500_000, seed: false }, || {});
        engine.add(AddRequest { source: url, dir: dir.clone(), ..Default::default() });
        let start = Instant::now();
        loop {
            std::thread::sleep(Duration::from_millis(250));
            let s = engine.snapshot();
            let j = s.jobs.last().unwrap();
            let icon = app_icon(j.id);
            if icon.exists() {
                println!("icon after {:?} at {}/{:?}: {} bytes on disk", start.elapsed(), j.done, j.total, std::fs::metadata(&icon).unwrap().len());
                assert_ne!(j.state, State::Done, "the icon should come before the download ends");
                crate::shell::init_com_thread();
                assert!(crate::shell::thumbnail(&icon, 48).is_some(), "the shell can't read an icon from it");
                engine.remove(j.id, true);
                std::thread::sleep(Duration::from_secs(1));
                break;
            }
            assert!(j.state != State::Failed && start.elapsed().as_secs() < 60, "no icon: {:?} {:?}", j.state, j.error);
        }
    }

    /// Network test: a camera photo shows its own preview while it's still downloading.
    /// `cargo test dl::tests::early_photo -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn early_photo() {
        let dir = std::env::temp_dir().join("stash-dl-test-photo");
        unsafe { std::env::set_var("STASH_DATA", dir.join("data")) };
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let url = std::env::var("STASH_TEST_URL").unwrap_or("https://raw.githubusercontent.com/ianare/exif-samples/master/jpg/Nikon_D70.jpg".into());
        // a trickle, so the preview must come first
        let engine = Engine::start(Config { max_active: 1, connections: 1, limit_bps: 2_000, seed: false }, || {});
        engine.add(AddRequest { source: url, dir: dir.clone(), ..Default::default() });
        let start = Instant::now();
        loop {
            std::thread::sleep(Duration::from_millis(200));
            let s = engine.snapshot();
            let j = s.jobs.last().unwrap();
            let preview = photo_preview(j.id);
            if preview.exists() {
                println!("preview after {:?} at {}/{:?}: {} bytes", start.elapsed(), j.done, j.total, std::fs::metadata(&preview).unwrap().len());
                assert_ne!(j.state, State::Done, "the preview should come before the download ends");
                crate::shell::init_com_thread();
                let px = crate::shell::thumbnail(&preview, 64).expect("the shell can't read the preview");
                println!("preview picture: {}x{}", px.width, px.height);
                engine.remove(j.id, true);
                std::thread::sleep(Duration::from_secs(1));
                break;
            }
            assert!(j.state != State::Failed && j.state != State::Done && start.elapsed().as_secs() < 30, "no preview: {:?} {:?}", j.state, j.error);
        }
    }

    /// Network test: `cargo test --release dl::tests::http -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn http() {
        let dir = std::env::temp_dir().join("stash-dl-test-http");
        unsafe { std::env::set_var("STASH_DATA", dir.join("data")) };
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let url = std::env::var("STASH_TEST_URL").unwrap_or("https://proof.ovh.net/files/10Mb.dat".into());
        let engine = Engine::start(Config { max_active: 2, connections: 6, limit_bps: 0, seed: false }, || {});
        // the same file twice at once: each gets its own name ("x (2).dat", not "x.dat (2)")
        // (the same link twice is one download, so the second asks with a query)
        engine.add(AddRequest { source: url.clone(), dir: dir.clone(), ..Default::default() });
        engine.add(AddRequest { source: format!("{url}?again"), dir: dir.clone(), ..Default::default() });
        let start = Instant::now();
        loop {
            std::thread::sleep(Duration::from_millis(500));
            let s = engine.snapshot();
            for j in &s.jobs {
                println!("{:?} {:?} {}/{:?} {} B/s conns={} {:?}", j.state, j.phase, j.done, j.total, j.speed, j.connections, j.error);
            }
            let over = s.jobs.iter().all(|j| matches!(j.state, State::Done | State::Failed));
            if over || start.elapsed().as_secs() > 90 {
                let mut names = Vec::new();
                for j in &s.jobs {
                    assert_eq!(j.state, State::Done);
                    let p = j.path.clone().unwrap();
                    assert_eq!(std::fs::metadata(&p).unwrap().len(), j.total.unwrap());
                    // marked as from the internet, like a browser would
                    let mut zone = p.as_os_str().to_owned();
                    zone.push(":Zone.Identifier");
                    assert!(std::fs::read_to_string(PathBuf::from(zone)).unwrap().contains("ZoneId=3"));
                    println!("saved {}", p.display());
                    names.push(p.file_name().unwrap().to_string_lossy().into_owned());
                    engine.remove(j.id, false);
                }
                names.sort();
                assert_eq!(names.len(), 2);
                assert_ne!(names[0], names[1]);
                assert!(names.iter().all(|n| n.ends_with(".dat")), "{names:?}");
                break;
            }
        }
    }
}
