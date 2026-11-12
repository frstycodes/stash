//! The engine's newer behaviour, kept out of `mod.rs`: automatic retries and mirrors,
//! sign-ins, holding downloads (no network, metered, outside download hours), the daily
//! start and stop times, keeping the PC awake, and noticing when the queue is done.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::prefs::{Prefs, Proxy, ProxyMode};
use super::retry::{self, Failure};
use super::sort::Category;
use super::{Actor, Job, Snapshot, State as JobState, auth, net, schedule};

/// How often waiting downloads, the network and the clock are looked at.
pub(super) const SLOW_TICK: Duration = Duration::from_secs(3);

pub(super) const WAIT_RETRY: &str = "Trying again soon";
pub(super) const WAIT_NETWORK: &str = "Waiting for the network";
pub(super) const WAIT_METERED: &str = "Paused on a metered connection";
pub(super) const WAIT_HOURS: &str = "Waiting for download hours";
pub(super) const WAIT_LOGIN: &str = "Waiting for you to sign in";

/// Where downloads go by type, when sorting is on.
#[derive(Clone, Debug)]
pub struct SortRule {
    pub overrides: Vec<(Category, PathBuf)>,
}

impl SortRule {
    pub fn dir_for(&self, dir: &Path, name: &str) -> PathBuf {
        super::sort::folder_for(dir, name, &self.overrides).unwrap_or_else(|| dir.to_path_buf())
    }
}

static PRESET: Mutex<Option<Prefs>> = Mutex::new(None);

/// The settings the engine starts with (call before `init`, so nothing starts before
/// download hours, the proxy and the rest apply).
pub fn preset_prefs(p: Prefs) {
    *PRESET.lock().unwrap() = Some(p);
}

pub(super) struct State {
    pub prefs: Prefs,
    /// why nothing may start now
    pub held: Option<&'static str>,
    pub net: net::NetState,
    pub minute: u16,
    pub awake: net::Awake,
    /// something downloaded since the queue last ran dry
    pub had_work: bool,
    pub finished_in_run: bool,
    pub queue_done: u64,
}

impl State {
    pub fn new() -> Self {
        State {
            prefs: PRESET.lock().unwrap().take().unwrap_or_default(),
            held: None,
            net: net::NetState::default(),
            minute: schedule::local_minute(),
            awake: net::Awake::default(),
            had_work: false,
            finished_in_run: false,
            queue_done: 0,
        }
    }
}

/// The HTTP client, through the proxy the settings ask for.
pub(super) fn client(proxy: &Proxy) -> reqwest::Client {
    let mut b = reqwest::Client::builder()
        .user_agent(concat!("Mozilla/5.0 (Windows NT 10.0; Win64; x64) Stash/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(Duration::from_secs(30));
    match proxy.mode {
        ProxyMode::System => {}
        ProxyMode::Off => b = b.no_proxy(),
        ProxyMode::Http | ProxyMode::Socks5 if !proxy.host.trim().is_empty() => {
            let pass = (!proxy.user.is_empty()).then(|| auth::load(&proxy_key(proxy)).map(|l| l.pass).unwrap_or_default());
            let enc = |s: &str| percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string();
            let host = proxy.host.trim();
            let url = match (proxy.mode, &pass) {
                // socks5h: names are looked up through the proxy too
                (ProxyMode::Socks5, Some(p)) => format!("socks5h://{}:{}@{host}:{}", enc(&proxy.user), enc(p), proxy.port),
                (ProxyMode::Socks5, None) => format!("socks5h://{host}:{}", proxy.port),
                _ => format!("http://{host}:{}", proxy.port),
            };
            if let Ok(mut px) = reqwest::Proxy::all(&url) {
                if let (ProxyMode::Http, Some(p)) = (proxy.mode, &pass) {
                    px = px.basic_auth(&proxy.user, p);
                }
                b = b.proxy(px);
            }
        }
        _ => {}
    }
    b.build().unwrap_or_else(|_| reqwest::Client::new())
}

fn proxy_key(p: &Proxy) -> String {
    format!("proxy/{}:{}", p.host.trim().to_ascii_lowercase(), p.port)
}

/// Keep the proxy's password in Windows Credential Manager (empty: forget it).
pub fn set_proxy_password(p: &Proxy, pass: &str) {
    if pass.is_empty() {
        auth::delete(&proxy_key(p));
    } else {
        auth::store(&proxy_key(p), &auth::Login { user: p.user.clone(), pass: pass.to_string() });
    }
}

/// A download of `source` into `dir` that's already on this PC: a finished download of
/// the same link whose file is still there, or a file of the name the link suggests.
pub fn existing_download(snap: &Snapshot, source: &str, dir: &Path) -> Option<PathBuf> {
    let source = source.trim();
    let done = snap.jobs.iter().filter(|j| j.source == source && j.state.finished()).filter_map(|j| j.path.clone()).find(|p| p.exists());
    done.or_else(|| {
        let lower = source.to_ascii_lowercase();
        if !(lower.starts_with("http") || lower.starts_with("ftp")) {
            return None;
        }
        let name = super::http::name_from_url(source)?;
        Some(dir.join(name)).filter(|p| p.is_file())
    })
}

impl Job {
    /// The link in use now: the source, or the mirror it failed over to.
    pub(super) fn url(&self) -> &str {
        match self.mirror {
            0 => &self.source,
            n => self.mirrors.get(n - 1).map(String::as_str).unwrap_or(&self.source),
        }
    }

    fn requeue(&mut self) {
        self.state = JobState::Queued;
        self.speed = 0;
        self.phase = None;
        self.connections = 0;
    }
}

impl Actor {
    /// Is anything waiting on the clock or the network?
    pub(super) fn watching(&self) -> bool {
        let s = &self.x.prefs.schedule;
        s.start_at.is_some() || s.stop_at.is_some() || self.jobs.iter().any(|j| matches!(j.state, JobState::Queued | JobState::Active))
    }

    pub(super) async fn on_slow(&mut self) {
        let pending = self.jobs.iter().any(|j| matches!(j.state, JobState::Queued | JobState::Active));
        // the network only matters while something wants it
        self.x.net = if pending { net::state() } else { net::NetState::default() };
        // the daily start and stop times
        let m = schedule::local_minute();
        for ev in schedule::events(&self.x.prefs.schedule, self.x.minute, m) {
            self.dirty = true;
            match ev {
                schedule::Event::Start => {
                    for j in self.jobs.iter_mut().filter(|j| matches!(j.state, JobState::Paused | JobState::Failed) && j.needs_login.is_none()) {
                        j.state = JobState::Queued;
                        j.error = None;
                        j.tries = 0;
                    }
                }
                schedule::Event::Stop => {
                    let ids: Vec<u64> = self.jobs.iter().filter(|j| matches!(j.state, JobState::Active | JobState::Queued)).map(|j| j.id).collect();
                    for id in ids {
                        self.pause(id).await;
                    }
                }
            }
        }
        self.x.minute = m;
        self.update_hold().await;
        // a retry that's due shows as plainly waiting again
        let now = Instant::now();
        for j in self.jobs.iter_mut().filter(|j| j.state == JobState::Queued && j.retry_at.is_some_and(|t| t <= now)) {
            j.retry_at = None;
        }
    }

    /// Hold everything (or let it go) as the network and the download hours say.
    pub(super) async fn update_hold(&mut self) {
        let p = &self.x.prefs;
        let held = if self.x.net.offline {
            Some(WAIT_NETWORK)
        } else if p.pause_metered && self.x.net.metered {
            Some(WAIT_METERED)
        } else if !schedule::allowed(&p.schedule, self.x.minute) {
            Some(WAIT_HOURS)
        } else {
            None
        };
        if held == self.x.held {
            return;
        }
        self.x.held = held;
        self.dirty = true;
        if held.is_some() {
            let ids: Vec<u64> = self.jobs.iter().filter(|j| j.state == JobState::Active).map(|j| j.id).collect();
            for id in ids {
                self.pause(id).await;
                if let Some(j) = self.job(id) {
                    j.state = JobState::Queued; // goes on by itself once the hold lifts
                }
            }
        }
        for j in self.jobs.iter_mut().filter(|j| j.state == JobState::Queued) {
            j.wait = held.or(j.retry_at.map(|_| WAIT_RETRY));
        }
    }

    /// After every change: keep the PC awake while downloading, and notice when
    /// everything is done.
    pub(super) fn after_change(&mut self) {
        let active = self.jobs.iter().any(|j| j.state == JobState::Active);
        let pending = active || self.jobs.iter().any(|j| j.state == JobState::Queued);
        if active {
            self.x.had_work = true;
        }
        if self.x.had_work && !pending {
            if self.x.finished_in_run {
                self.x.queue_done += 1;
            }
            self.x.had_work = false;
            self.x.finished_in_run = false;
        }
        let awake = self.x.prefs.keep_awake && active;
        self.x.awake.set(awake);
        for j in self.jobs.iter_mut().filter(|j| j.state == JobState::Queued && j.wait.is_none()) {
            j.wait = self.x.held;
        }
    }

    pub(super) async fn set_prefs(&mut self, p: Prefs) {
        self.x.prefs = p;
        // (a changed proxy, or its password: new downloads go through it)
        self.client = client(&self.x.prefs.proxy);
        self.update_hold().await;
    }

    pub(super) fn set_limit(&mut self, id: u64, bps: u64) {
        if let Some(l) = self.running.get(&id).and_then(|r| r.limit.clone()) {
            l.set(bps);
        }
        if let Some(j) = self.job(id) {
            j.limit_bps = bps;
        }
    }

    pub(super) async fn refresh(&mut self, id: u64, url: String) {
        let url = url.trim().to_string();
        if !super::is_source(&url) || !self.jobs.iter().any(|j| j.id == id && !j.state.finished()) {
            return;
        }
        if self.running.contains_key(&id) {
            self.pause(id).await;
        }
        if let Some(j) = self.job(id) {
            j.source = url;
            j.mirror = 0;
            // the new link may be a different copy: the size decides whether the part
            // already downloaded fits (the old ETag may not match another server's)
            j.validator = None;
            j.tries = 0;
            j.retry_at = None;
            j.error = None;
            j.needs_login = None;
            j.wait = None;
            j.state = JobState::Queued;
        }
    }

    pub(super) fn sign_in(&mut self, id: u64, user: String, pass: String, remember: bool) {
        let Some(j) = self.job(id) else { return };
        let Some(site) = j.needs_login.clone().or_else(|| auth::site_of(j.url())) else { return };
        j.needs_login = None;
        j.login_tries = 0;
        j.error = None;
        j.wait = None;
        if !j.state.finished() {
            j.state = JobState::Queued;
        }
        let login = auth::Login { user, pass };
        if remember {
            auth::store(&site, &login);
        }
        auth::set(&site, login);
        // others waiting on the same site go on too
        for o in self.jobs.iter_mut().filter(|o| o.needs_login.as_deref() == Some(site.as_str())) {
            o.needs_login = None;
            o.login_tries = 0;
            o.wait = None;
            o.state = JobState::Queued;
        }
    }

    pub(super) async fn on_http_failed(&mut self, id: u64, f: Failure) {
        self.persist_progress();
        self.running.remove(&id);
        let (auto, offline) = (self.x.prefs.auto_retry, self.x.net.offline);
        let Some(j) = self.jobs.iter_mut().find(|j| j.id == id) else { return };
        if j.state != JobState::Active {
            return;
        }
        j.requeue();
        // a sign-in: one we know (this session, or kept in Credential Manager), else ask
        if let Some(challenge) = &f.auth {
            let site = auth::site_of(j.url()).unwrap_or_default();
            if !auth::has(&site) {
                if let Some(l) = auth::load(&site) {
                    auth::set(&site, l);
                }
            }
            auth::challenged(&site, challenge);
            j.login_tries += 1;
            if auth::has(&site) && j.login_tries < 2 {
                return; // straight back in the queue, signed in
            }
            if j.login_tries >= 2 {
                auth::forget(&site); // that one didn't work
            }
            j.state = JobState::Paused;
            j.needs_login = Some(site);
            j.wait = Some(WAIT_LOGIN);
            j.error = Some(f.msg);
            return;
        }
        // the file changed on the server: what we have is no use, start it over
        if f.changed {
            if let Some(t) = j.temp.take() {
                let _ = std::fs::remove_file(t);
            }
            j.remaining.clear();
            j.done = 0;
            j.total = None;
            j.validator = None;
        }
        // another link to the same file: try it right away
        if !j.mirrors.is_empty() {
            j.mirror = (j.mirror + 1) % (j.mirrors.len() + 1);
            if j.mirror != 0 {
                j.error = Some(f.msg);
                return;
            }
        }
        if (auto && f.transient || f.changed) && j.tries < retry::MAX_TRIES {
            j.error = Some(f.msg);
            if offline {
                // back when the network is (doesn't count as a try)
                j.wait = Some(WAIT_NETWORK);
                return;
            }
            j.tries += 1;
            j.retry_at = Some(Instant::now() + retry::backoff(j.tries, f.retry_after));
            j.wait = Some(WAIT_RETRY);
            return;
        }
        j.state = JobState::Failed;
        j.error = Some(f.msg);
        j.wait = None;
    }

    pub(super) fn on_verified(&mut self, id: u64, result: Result<bool, String>) {
        let Some(j) = self.job(id) else { return };
        if j.state != JobState::Active {
            return;
        }
        j.state = JobState::Done;
        j.phase = None;
        match result {
            Ok(ok) => {
                j.verified = Some(ok);
                j.error = (!ok).then(|| "The checksum doesn't match: the file may be damaged, or not the one expected".to_string());
            }
            Err(e) => {
                j.verified = Some(false);
                j.error = Some(e);
            }
        }
    }
}
