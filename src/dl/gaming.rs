//! Gaming mode: while an online game runs, Stash slows itself down just enough to keep
//! the game's ping low, then gives the speed back a few seconds after the game ends.
//!
//! Two halves, on one small thread of their own (it sleeps when nothing's going on):
//!
//! * **Detection** (every 2 s, no admin rights): the foreground app is a game when it
//!   owns UDP sockets (`GetExtendedUdpTable`) and either fills its monitor (exclusive
//!   D3D full screen via `SHQueryUserNotificationState`, or borderless) or lives in a
//!   game library folder (Steam, Epic, Riot, Xbox…). Browsers, players and chat apps
//!   don't count. Once found, it stays "the game" while it keeps its sockets, even when
//!   you Alt+Tab out of it.
//! * **The controller** (LEDBAT-style, while a game runs): pings a public host a few
//!   times a second, keeps the lowest round trip of the last minutes as the baseline,
//!   and steers a cap on Stash's total speed so the queueing delay we add stays under
//!   ~20 ms: it cuts fast when the delay rises, creeps up when it's flat, never goes
//!   under a small floor (downloads keep moving) or over the user's own limit.
//!
//! The cap is a global that the HTTP limiter and the torrent session read; 0 = no cap.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// The cap on Stash's speed in bytes/s; 0 = none.
static CAP: AtomicU64 = AtomicU64::new(0);
/// A game is running (the UI shows a note).
static ACTIVE: AtomicBool = AtomicBool::new(false);
/// Settings > Downloads > Gaming mode.
static ENABLED: AtomicBool = AtomicBool::new(true);
static NOTIFY: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

/// The note the downloads window and the tray show while a game runs.
pub const NOTE: &str = "Gaming \u{2014} downloads slowed to keep your ping low";

/// Turn gaming mode on or off (from Settings).
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// A game is running and Stash is holding back.
pub fn active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// The speed limit to use: the user's own (`user`, 0 = none) or gaming mode's cap,
/// whichever is lower.
pub fn effective(user: u64) -> u64 {
    match (user, CAP.load(Ordering::Relaxed)) {
        (u, 0) => u,
        (0, c) => c,
        (u, c) => u.min(c),
    }
}

/// The torrent session's (download, upload) limits; 0 = none. While a game runs the
/// upload is held hard: uploads fill the slow side of most connections first.
pub fn torrent_limits(user_down: u64) -> (u64, u64) {
    let cap = CAP.load(Ordering::Relaxed);
    let up = if cap == 0 { 0 } else { (cap / 4).clamp(8 * 1024, 32 * 1024) };
    (effective(user_down), up)
}

/// The tray tooltip's text, when it changed since the last call (the UI thread asks on
/// every engine update).
pub fn tray_tip_changed() -> Option<String> {
    static LAST: AtomicBool = AtomicBool::new(false);
    let now = active();
    (LAST.swap(now, Ordering::Relaxed) != now).then(|| if now { format!("Stash\n{NOTE}") } else { "Stash".to_string() })
}

/// Start watching for games (once, at launch). `notify` pokes the UI when gaming mode
/// turns on or off.
pub fn start(notify: impl Fn() + Send + Sync + 'static) {
    if NOTIFY.set(Box::new(notify)).is_err() {
        return;
    }
    let _ = std::thread::Builder::new().name("stash-gaming".into()).spawn(run);
}

fn poke() {
    if let Some(n) = NOTIFY.get() {
        n();
    }
}

// ---------- the controller ----------

#[derive(Clone, Copy, Debug)]
pub struct Params {
    /// The queueing delay (ms over the baseline) we allow ourselves to add.
    pub target_ms: f64,
    /// Never slower than this (B/s), so downloads keep moving.
    pub floor_bps: f64,
    /// How often we may cut: the queue needs a moment to drain before the ping shows it.
    pub cooldown_s: f64,
    /// Growth per second when the delay is flat: this share of the cap...
    pub grow: f64,
    /// ...plus this share of the floor (so it gets going from the floor).
    pub grow_floor: f64,
    /// Growth per second near the speed where the ping last rose (the line's real
    /// capacity, most likely): this share of the cap.
    pub grow_near: f64,
    /// Samples are filtered over this window (the lowest wins: jitter only adds).
    pub window_s: f64,
    /// Each baseline bucket's length; the baseline is the lowest of `buckets` of them.
    pub bucket_s: f64,
    pub buckets: usize,
}

impl Default for Params {
    fn default() -> Self {
        Self { target_ms: 20.0, floor_bps: 64.0 * 1024.0, cooldown_s: 1.5, grow: 0.15, grow_floor: 0.5, grow_near: 0.02, window_s: 0.5, bucket_s: 60.0, buckets: 5 }
    }
}

/// The latency-driven cap. Pure: time comes in from outside, so tests can drive it.
pub struct Controller {
    p: Params,
    cap: f64,
    /// Lowest round trip per bucket: (bucket start, min ms).
    base: VecDeque<(f64, f64)>,
    recent: VecDeque<(f64, f64)>,
    losses: u32,
    last_cut: f64,
    last_step: Option<f64>,
    /// Since when we've sat on the floor with the delay still high.
    stuck_since: Option<f64>,
    /// The cap when the ping last rose: the line is full somewhere around it.
    knee: Option<f64>,
}

impl Controller {
    pub fn new(p: Params) -> Self {
        Self { cap: p.floor_bps, p, base: VecDeque::new(), recent: VecDeque::new(), losses: 0, last_cut: f64::NEG_INFINITY, last_step: None, stuck_since: None, knee: None }
    }

    pub fn cap(&self) -> f64 {
        self.cap
    }

    /// The baseline round trip (ms), once there is one.
    pub fn base(&self) -> Option<f64> {
        self.base.iter().map(|b| b.1).reduce(f64::min)
    }

    /// The filtered queueing delay (ms) right now.
    pub fn delay(&self) -> Option<f64> {
        let cur = self.recent.iter().map(|r| r.1).reduce(f64::min)?;
        Some((cur - self.base()?).max(0.0))
    }

    /// A ping came back after `rtt` ms, or was lost (None).
    pub fn sample(&mut self, t: f64, rtt: Option<f64>) {
        let Some(rtt) = rtt else {
            self.losses += 1;
            return;
        };
        self.losses = 0;
        self.recent.push_back((t, rtt));
        while self.recent.front().is_some_and(|r| r.0 < t - self.p.window_s) {
            self.recent.pop_front();
        }
        match self.base.back_mut() {
            Some(b) if t - b.0 < self.p.bucket_s => b.1 = b.1.min(rtt),
            _ => {
                self.base.push_back((t, rtt));
                while self.base.len() > self.p.buckets {
                    self.base.pop_front();
                }
            }
        }
    }

    /// Move the cap. `measured` is what Stash moves now (B/s), `ceiling` the user's own
    /// limit (0 = none). Returns the new cap.
    pub fn step(&mut self, t: f64, measured: f64, ceiling: f64) -> f64 {
        let dt = self.last_step.map_or(0.0, |l| (t - l).clamp(0.0, 2.0));
        self.last_step = Some(t);
        let p = self.p;
        let floor = if ceiling > 0.0 { p.floor_bps.min(ceiling) } else { p.floor_bps };
        let cool = t - self.last_cut >= p.cooldown_s;
        match self.delay() {
            // nothing to go on yet: wait at the floor
            None if self.losses < 2 => {}
            d => {
                let q = d.unwrap_or(f64::INFINITY);
                if q > p.target_ms || self.losses >= 2 {
                    if cool {
                        // cut from what we actually move (a cap far above it does nothing)
                        let hard = q > 3.0 * p.target_ms || self.losses >= 3;
                        let from = self.cap.min(measured.max(floor));
                        if !hard {
                            // a spike from elsewhere says nothing about our line
                            self.knee = Some(from);
                        }
                        self.cap = from * if hard { 0.5 } else { 0.7 };
                        self.last_cut = t;
                    }
                } else if q < p.target_ms * 0.5 && cool {
                    // creep up, slower as the delay nears half the target
                    let room = 1.0 - q / (p.target_ms * 0.5);
                    // fast while well under the knee, gently near it, picking up past it
                    let share = match self.knee {
                        Some(k) if self.cap > 0.85 * k && self.cap < 1.15 * k => p.grow_near,
                        Some(k) if self.cap >= 1.15 * k => p.grow * 0.5,
                        _ => p.grow,
                    };
                    let grown = self.cap + (share * self.cap + p.grow_floor * floor) * room * dt;
                    // don't run away from what's actually used (e.g. a slow server)
                    self.cap = grown.min((measured * 2.0).max(self.cap).max(floor * 4.0));
                }
                // The floor and still too slow for a while: it isn't us. The baseline
                // moved (a new route, someone else's traffic): start it afresh.
                if self.cap <= floor * 1.01 && q.is_finite() && q > p.target_ms {
                    let since = *self.stuck_since.get_or_insert(t);
                    if t - since > 20.0 {
                        self.base.clear();
                        self.knee = None;
                        self.stuck_since = None;
                    }
                } else {
                    self.stuck_since = None;
                }
            }
        }
        self.cap = self.cap.max(floor);
        if ceiling > 0.0 {
            self.cap = self.cap.min(ceiling);
        }
        self.cap
    }
}

// ---------- the thread ----------

const DETECT_EVERY: Duration = Duration::from_secs(2);
const PING_EVERY: Duration = Duration::from_millis(300);
/// The game has to be gone this long before speed comes back...
const HOLD: Duration = Duration::from_secs(5);
/// ...and then it comes back over this long.
const RELEASE: Duration = Duration::from_secs(4);

enum Phase {
    Idle,
    Gaming { ctl: Controller },
    Releasing { since: Instant, from: f64 },
}

fn run() {
    let t0 = Instant::now();
    let mut det = Detector::default();
    let mut pinger = Pinger::new();
    let mut phase = Phase::Idle;
    let mut gone_since: Option<Instant> = None;
    let mut next_detect = Instant::now();
    let mut log = Log::open();
    loop {
        let now = Instant::now();
        let enabled = ENABLED.load(Ordering::Relaxed);
        if now >= next_detect {
            next_detect = now + DETECT_EVERY;
            let game = enabled && det.poll(t0.elapsed().as_secs_f64());
            match (&phase, game) {
                (Phase::Gaming { .. }, true) => gone_since = None,
                (Phase::Gaming { .. }, false) => {
                    let since = *gone_since.get_or_insert(now);
                    if !enabled || now - since >= HOLD {
                        gone_since = None;
                        phase = Phase::Releasing { since: now, from: CAP.load(Ordering::Relaxed) as f64 };
                        ACTIVE.store(false, Ordering::Relaxed);
                        log.line(format_args!("game over: {}", det.name()));
                        poke();
                    }
                }
                (_, true) => {
                    // a fresh baseline: start at the floor, then let the pings lead
                    let ctl = Controller::new(Params::default());
                    CAP.store(ctl.cap() as u64, Ordering::Relaxed);
                    phase = Phase::Gaming { ctl };
                    ACTIVE.store(true, Ordering::Relaxed);
                    log.line(format_args!("game: {}", det.name()));
                    poke();
                }
                _ => {}
            }
        }
        match &mut phase {
            Phase::Idle => std::thread::sleep(next_detect.saturating_duration_since(Instant::now())),
            Phase::Gaming { ctl } => {
                let rtt = pinger.ping();
                let t = t0.elapsed().as_secs_f64();
                ctl.sample(t, rtt);
                let snap = super::engine_opt().map(|e| e.snapshot());
                let measured = snap.as_ref().map_or(0.0, |s| (s.down + s.up) as f64);
                let ceiling = USER_LIMIT.load(Ordering::Relaxed) as f64;
                let cap = ctl.step(t, measured, ceiling);
                CAP.store(cap.max(1.0) as u64, Ordering::Relaxed);
                log.line(format_args!(
                    "t={t:.1} rtt={} base={} q={} moving={:.0}KB/s cap={:.0}KB/s",
                    rtt.map_or("lost".into(), |r| format!("{r:.0}")),
                    ctl.base().map_or("-".into(), |b| format!("{b:.0}")),
                    ctl.delay().map_or("-".into(), |q| format!("{q:.0}")),
                    measured / 1024.0,
                    cap / 1024.0
                ));
                let rest = PING_EVERY.saturating_sub(Instant::now() - now);
                std::thread::sleep(rest);
            }
            Phase::Releasing { since, from } => {
                // double every half second or so, then let go
                let f = since.elapsed().as_secs_f64() / RELEASE.as_secs_f64();
                if f >= 1.0 {
                    CAP.store(0, Ordering::Relaxed);
                    phase = Phase::Idle;
                    log.line(format_args!("full speed"));
                } else {
                    CAP.store((*from * 256f64.powf(f)).max(1.0) as u64, Ordering::Relaxed);
                    std::thread::sleep(Duration::from_millis(250));
                }
            }
        }
    }
}

/// The user's own limit (B/s, 0 = none), so the cap never goes over it.
static USER_LIMIT: AtomicU64 = AtomicU64::new(0);

/// The engine's configured limit (B/s), for the controller's ceiling.
pub fn set_user_limit(bps: u64) {
    USER_LIMIT.store(bps, Ordering::Relaxed);
}

/// STASH_GAMING_LOG (or STASH_FAKE_GAME) writes what the controller does to
/// gaming.log in the data folder.
struct Log(Option<std::fs::File>);

impl Log {
    fn open() -> Self {
        let on = std::env::var_os("STASH_GAMING_LOG").is_some() || std::env::var_os("STASH_FAKE_GAME").is_some();
        Log(on.then(|| std::fs::OpenOptions::new().create(true).append(true).open(super::data_dir().join("gaming.log")).ok()).flatten())
    }
    fn line(&mut self, a: std::fmt::Arguments) {
        if let Some(f) = &mut self.0 {
            use std::io::Write;
            let _ = writeln!(f, "{a}");
        }
    }
}

// ---------- detection ----------

/// Apps that use UDP and go full screen but aren't games.
const NOT_GAMES: &[&str] = &[
    "chrome.exe", "msedge.exe", "firefox.exe", "brave.exe", "opera.exe", "opera_gx.exe", "vivaldi.exe", "arc.exe", "iexplore.exe", "msedgewebview2.exe",
    "vlc.exe", "mpc-hc64.exe", "mpc-hc.exe", "mpc-be64.exe", "potplayermini64.exe", "potplayer.exe", "mpv.exe", "wmplayer.exe", "video.ui.exe", "microsoft.media.player.exe",
    "spotify.exe", "discord.exe", "teams.exe", "ms-teams.exe", "zoom.exe", "slack.exe", "skype.exe", "obs64.exe", "explorer.exe", "applicationframehost.exe",
    "steam.exe", "steamwebhelper.exe", "epicgameslauncher.exe", "battle.net.exe", "eadesktop.exe", "galaxyclient.exe", "riotclientservices.exe", "upc.exe",
    "stash.exe",
];

/// Folders games are installed in.
const GAME_DIRS: &[&str] = &[
    "\\steamapps\\common\\", "\\epic games\\", "\\riot games\\", "\\xboxgames\\", "\\ea games\\", "\\electronic arts\\", "\\ubisoft game launcher\\games\\",
    "\\gog galaxy\\games\\", "\\rockstar games\\", "\\battlestate games\\", "\\roblox\\versions\\",
];

/// Online games whose installs could be anywhere.
const GAME_EXES: &[&str] = &[
    "valorant-win64-shipping.exe", "cs2.exe", "csgo.exe", "dota2.exe", "league of legends.exe", "fortniteclient-win64-shipping.exe", "r5apex.exe", "r5apex_dx12.exe",
    "overwatch.exe", "cod.exe", "rocketleague.exe", "gta5.exe", "gta5_enhanced.exe", "robloxplayerbeta.exe", "tslgame.exe", "destiny2.exe", "rainbowsix.exe",
    "rainbowsix_vulkan.exe", "eldenring.exe", "marvel-win64-shipping.exe", "deadlock.exe", "wow.exe", "ffxiv_dx11.exe", "pathofexile.exe", "pathofexile_x64.exe",
];

#[derive(Default)]
struct Detector {
    /// The game we found, while it keeps its sockets.
    game: Option<(u32, String)>,
    /// A candidate seen on the last poll (two in a row make a game).
    maybe: Option<u32>,
    exes: HashMap<u32, String>,
}

impl Detector {
    fn name(&self) -> String {
        self.game.as_ref().map_or_else(|| "(fake)".into(), |g| g.1.clone())
    }

    fn poll(&mut self, t: f64) -> bool {
        if let Some(on) = fake(t) {
            return on;
        }
        let udp = udp_owners();
        // the game we know: still there while it keeps its sockets (Alt+Tab doesn't end it)
        if let Some((pid, _)) = &self.game {
            if udp.contains(pid) {
                return true;
            }
            self.game = None;
        }
        self.exes.retain(|pid, _| udp.contains(pid));
        let Some((pid, filled)) = foreground() else {
            self.maybe = None;
            return false;
        };
        if pid == std::process::id() || !udp.contains(&pid) {
            self.maybe = None;
            return false;
        }
        let exe = self.exes.entry(pid).or_insert_with(|| exe_path(pid).unwrap_or_default().to_lowercase()).clone();
        let file = exe.rsplit('\\').next().unwrap_or("");
        let looks = !exe.is_empty()
            && !NOT_GAMES.contains(&file)
            && (filled || full_screen_d3d() || GAME_DIRS.iter().any(|d| exe.contains(d)) || GAME_EXES.contains(&file));
        if !looks {
            self.maybe = None;
            return false;
        }
        if self.maybe == Some(pid) {
            self.game = Some((pid, file.to_string()));
            return true;
        }
        self.maybe = Some(pid);
        false
    }
}

/// STASH_FAKE_GAME=1 pretends a game runs all the time; "ON:OFF" (seconds) runs one for
/// ON seconds, then none for OFF, over and over. For trying gaming mode out.
fn fake(t: f64) -> Option<bool> {
    let v = std::env::var("STASH_FAKE_GAME").ok()?;
    match v.split_once(':') {
        Some((on, off)) => {
            let (on, off) = (on.trim().parse::<f64>().ok()?, off.trim().parse::<f64>().ok()?);
            Some(t % (on + off).max(1.0) < on)
        }
        None => Some(!matches!(v.trim(), "" | "0")),
    }
}

/// Exclusive full-screen Direct3D, or a full-screen app with notifications held back.
fn full_screen_d3d() -> bool {
    use windows::Win32::UI::Shell::{QUNS_BUSY, QUNS_RUNNING_D3D_FULL_SCREEN, SHQueryUserNotificationState};
    unsafe { SHQueryUserNotificationState() }.is_ok_and(|s| s == QUNS_RUNNING_D3D_FULL_SCREEN || s == QUNS_BUSY)
}

/// The foreground window's process, and whether the window covers its whole monitor
/// (borderless full screen).
fn foreground() -> Option<(u32, bool)> {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow};
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetShellWindow, GetWindowRect, GetWindowThreadProcessId};
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() || hwnd == GetShellWindow() {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return None;
        }
        let mut r = RECT::default();
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let filled = GetWindowRect(hwnd, &mut r).is_ok()
            && GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut mi).as_bool()
            && r.left <= mi.rcMonitor.left
            && r.top <= mi.rcMonitor.top
            && r.right >= mi.rcMonitor.right
            && r.bottom >= mi.rcMonitor.bottom;
        Some((pid, filled))
    }
}

fn exe_path(pid: u32) -> Option<String> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW};
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(h);
        ok.then(|| String::from_utf16_lossy(&buf[..len as usize]))
    }
}

/// Every process with a UDP socket open (IPv4 or IPv6).
fn udp_owners() -> HashSet<u32> {
    use windows::Win32::NetworkManagement::IpHelper::{GetExtendedUdpTable, MIB_UDP6ROW_OWNER_PID, MIB_UDPROW_OWNER_PID, UDP_TABLE_OWNER_PID};
    let mut out = HashSet::new();
    for (af, row) in [(2u32, size_of::<MIB_UDPROW_OWNER_PID>()), (23u32, size_of::<MIB_UDP6ROW_OWNER_PID>())] {
        let mut size = 0u32;
        unsafe {
            let _ = GetExtendedUdpTable(None, &mut size, false, af, UDP_TABLE_OWNER_PID, 0);
        }
        // a little slack: sockets come and go between the two calls
        let mut buf = vec![0u64; (size as usize + 4096) / 8 + 1];
        let mut size = (buf.len() * 8) as u32;
        if unsafe { GetExtendedUdpTable(Some(buf.as_mut_ptr().cast()), &mut size, false, af, UDP_TABLE_OWNER_PID, 0) } != 0 {
            continue;
        }
        let n = buf[0] as u32 as usize; // dwNumEntries
        let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), buf.len() * 8) };
        for i in 0..n {
            let at = 4 + i * row; // rows follow the count (both rows are 4-byte aligned)
            let Some(r) = bytes.get(at..at + row) else { break };
            let pid = u32::from_ne_bytes(r[row - 4..].try_into().unwrap()); // dwOwningPid is last
            if pid > 4 {
                out.insert(pid);
            }
        }
    }
    out
}

// ---------- pings ----------

struct Pinger {
    icmp: Option<windows::Win32::Foundation::HANDLE>,
    /// The hosts to try, in order; we stay with the first that answers.
    targets: Vec<u32>,
    at: usize,
    misses: u32,
}

impl Pinger {
    fn new() -> Self {
        use windows::Win32::NetworkManagement::IpHelper::IcmpCreateFile;
        let ip = |a: [u8; 4]| u32::from_ne_bytes(a); // network byte order, as Windows wants
        let mut targets = vec![ip([1, 1, 1, 1]), ip([8, 8, 8, 8])];
        targets.extend(gateway());
        Pinger { icmp: unsafe { IcmpCreateFile() }.ok(), targets, at: 0, misses: 0 }
    }

    /// One round trip in ms, or None if it was lost (or ICMP is unavailable).
    fn ping(&mut self) -> Option<f64> {
        use windows::Win32::NetworkManagement::IpHelper::{ICMP_ECHO_REPLY, IcmpSendEcho};
        let h = self.icmp?;
        let data = [0x53u8; 32];
        let mut reply = [0u64; (size_of::<ICMP_ECHO_REPLY>() + 32 + 8 + 64) / 8];
        let n = unsafe {
            IcmpSendEcho(h, self.targets[self.at], data.as_ptr().cast(), data.len() as u16, None, reply.as_mut_ptr().cast(), (reply.len() * 8) as u32, 800)
        };
        let r = unsafe { &*reply.as_ptr().cast::<ICMP_ECHO_REPLY>() };
        if n > 0 && r.Status == 0 {
            self.misses = 0;
            // the API counts whole milliseconds; a 0 is "under one"
            return Some(r.RoundTripTime.max(1) as f64);
        }
        self.misses += 1;
        // a host that never answers (ICMP blocked): try the next one
        if self.misses >= 6 {
            self.misses = 0;
            self.at = (self.at + 1) % self.targets.len();
        }
        None
    }
}

/// The default gateway (the router), a last resort to ping.
fn gateway() -> Option<u32> {
    use windows::Win32::NetworkManagement::IpHelper::{GetBestRoute, MIB_IPFORWARDROW};
    let mut row: MIB_IPFORWARDROW = unsafe { std::mem::zeroed() };
    let ok = unsafe { GetBestRoute(u32::from_ne_bytes([1, 1, 1, 1]), None, &mut row) } == 0;
    let hop = row.dwForwardNextHop;
    (ok && hop != 0).then_some(hop)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KB: f64 = 1024.0;

    /// A bottleneck link with a FIFO queue in front of it: `rate` B/s goes in, `capacity`
    /// drains it, and the queue shows up as extra round trip.
    struct Link {
        capacity: f64,
        queue: f64,
        base_ms: f64,
    }

    impl Link {
        fn tick(&mut self, rate: f64, dt: f64) -> f64 {
            self.queue = (self.queue + (rate - self.capacity) * dt).max(0.0);
            // the modem's buffer holds about a second
            self.queue = self.queue.min(self.capacity);
            self.base_ms + self.queue / self.capacity * 1000.0
        }
        fn delivered(&self, rate: f64) -> f64 {
            rate.min(self.capacity)
        }
    }

    /// A small deterministic random source (no dependency).
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> f64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (self.0 >> 11) as f64 / (1u64 << 53) as f64
        }
    }

    /// Runs the controller against a link for `secs`, Stash wanting `demand` B/s.
    /// Returns per-step (cap, queueing ms, delivered).
    fn simulate(ctl: &mut Controller, link: &mut Link, secs: f64, start: f64, demand: f64, extra: &mut dyn FnMut(f64) -> f64) -> Vec<(f64, f64, f64)> {
        let dt = 0.3;
        let mut out = Vec::new();
        let mut t = start;
        let mut rate = 0.0;
        while t < start + secs {
            let rtt = link.tick(rate, dt) + extra(t);
            ctl.sample(t, Some(rtt));
            let cap = ctl.step(t, link.delivered(rate), 0.0);
            rate = cap.min(demand);
            out.push((cap, rtt - link.base_ms, link.delivered(rate)));
            t += dt;
        }
        out
    }

    fn mean(v: impl Iterator<Item = f64>) -> f64 {
        let v: Vec<f64> = v.collect();
        v.iter().sum::<f64>() / v.len().max(1) as f64
    }

    #[test]
    fn flat_ping_creeps_up_to_full_speed() {
        let mut c = Controller::new(Params::default());
        let mut t = 0.0;
        // a link much faster than we want: the ping never moves
        while t < 60.0 {
            c.sample(t, Some(30.0));
            let cap = c.cap();
            c.step(t, cap, 0.0);
            t += 0.3;
        }
        assert!(c.cap() > 5.0 * 1024.0 * KB, "only got to {:.0} KB/s", c.cap() / KB);
        // and slowly: after 5 s it's still small
        let mut c = Controller::new(Params::default());
        let mut t = 0.0;
        while t < 5.0 {
            c.sample(t, Some(30.0));
            let cap = c.cap();
            c.step(t, cap, 0.0);
            t += 0.3;
        }
        assert!(c.cap() < 512.0 * KB, "grew too fast: {:.0} KB/s", c.cap() / KB);
    }

    #[test]
    fn never_over_the_user_limit_or_under_the_floor() {
        let mut c = Controller::new(Params::default());
        let mut t = 0.0;
        while t < 120.0 {
            c.sample(t, Some(20.0));
            let cap = c.step(t, c.cap(), 300.0 * KB);
            assert!(cap <= 300.0 * KB + 0.5);
            t += 0.3;
        }
        assert!((c.cap() - 300.0 * KB).abs() < 1.0);
        // the ping only ever rises: still at the floor, never below it
        let mut c = Controller::new(Params::default());
        let mut t = 0.0;
        while t < 30.0 {
            c.sample(t, Some(20.0 + t * 10.0));
            let cap = c.step(t, 10.0 * 1024.0 * KB, 0.0);
            assert!(cap >= Params::default().floor_bps - 0.5);
            t += 0.3;
        }
        // a user limit under the floor wins
        let mut c = Controller::new(Params::default());
        assert!(c.step(0.0, 0.0, 16.0 * KB) <= 16.0 * KB);
    }

    #[test]
    fn keeps_the_queue_short_on_a_real_link() {
        // 2 MB/s line, 25 ms base ping, Stash wants everything it can get
        let mut c = Controller::new(Params::default());
        let mut link = Link { capacity: 2048.0 * KB, queue: 0.0, base_ms: 25.0 };
        let run = simulate(&mut c, &mut link, 180.0, 0.0, f64::INFINITY, &mut |_| 0.0);
        let late = &run[run.len() / 3..];
        let q = mean(late.iter().map(|r| r.1));
        let used = mean(late.iter().map(|r| r.2)) / link.capacity;
        let worst = late.iter().map(|r| r.1).fold(0.0, f64::max);
        assert!(q < 20.0, "average queueing delay {q:.1} ms");
        assert!(worst < 80.0, "worst queueing delay {worst:.1} ms");
        assert!(used > 0.5, "only used {:.0}% of the line", used * 100.0);
    }

    #[test]
    fn backs_off_fast_on_a_spike_and_recovers() {
        let mut c = Controller::new(Params::default());
        let mut link = Link { capacity: 4096.0 * KB, queue: 0.0, base_ms: 20.0 };
        let run = simulate(&mut c, &mut link, 90.0, 0.0, f64::INFINITY, &mut |_| 0.0);
        let before = run.last().unwrap().0;
        // someone else on the network: +80 ms for 6 s
        let spike = simulate(&mut c, &mut link, 6.0, 90.0, f64::INFINITY, &mut |_| 80.0);
        // within ~2.5 s it has been cut twice, and it ends near the floor
        let after_2s = spike[(2.5 / 0.3) as usize].0;
        assert!(after_2s <= before * 0.3, "{:.0} -> {:.0} KB/s", before / KB, after_2s / KB);
        assert!(spike.last().unwrap().0 < before * 0.1);
        // it climbs back once the spike is over
        let back = simulate(&mut c, &mut link, 60.0, 96.0, f64::INFINITY, &mut |_| 0.0);
        assert!(back.last().unwrap().0 > before * 0.4, "only back to {:.0} KB/s", back.last().unwrap().0 / KB);
    }

    #[test]
    fn noisy_ping_does_not_collapse_to_the_floor() {
        // Wi-Fi: every sample gets 0-15 ms of jitter, and one in 20 a 60 ms blip
        let mut c = Controller::new(Params::default());
        let mut link = Link { capacity: 4096.0 * KB, queue: 0.0, base_ms: 15.0 };
        let mut rng = Lcg(7);
        let run = simulate(&mut c, &mut link, 180.0, 0.0, f64::INFINITY, &mut |_| {
            let blip = if rng.next() < 0.05 { 60.0 } else { 0.0 };
            rng.next() * 15.0 + blip
        });
        let late = &run[run.len() / 3..];
        let used = mean(late.iter().map(|r| r.2)) / link.capacity;
        assert!(used > 0.4, "only used {:.0}% of the line", used * 100.0);
        let q = mean(late.iter().map(|r| link_queue(r)));
        assert!(q < 40.0, "average delay {q:.1} ms");
    }

    fn link_queue(r: &(f64, f64, f64)) -> f64 {
        r.1
    }

    #[test]
    fn lost_pings_count_as_congestion() {
        let mut c = Controller::new(Params::default());
        let mut t = 0.0;
        while t < 30.0 {
            c.sample(t, Some(20.0));
            let cap = c.cap();
            c.step(t, cap, 0.0);
            t += 0.3;
        }
        let before = c.cap();
        for _ in 0..3 {
            c.sample(t, None);
            c.step(t, before, 0.0);
            t += 0.3;
        }
        assert!(c.cap() <= before * 0.7);
    }

    #[test]
    fn a_new_baseline_when_the_floor_doesnt_help() {
        // the route changed: the ping is 60 ms higher for good, whatever we do
        let mut c = Controller::new(Params::default());
        let mut t = 0.0;
        while t < 20.0 {
            c.sample(t, Some(20.0));
            let cap = c.cap();
            c.step(t, cap, 0.0);
            t += 0.3;
        }
        while t < 80.0 {
            c.sample(t, Some(80.0));
            let cap = c.cap();
            c.step(t, cap, 0.0);
            t += 0.3;
        }
        assert!(c.base().unwrap() > 70.0, "baseline stuck at {:?}", c.base());
        assert!(c.cap() > Params::default().floor_bps * 1.5, "still at the floor");
    }

    #[test]
    fn effective_limit() {
        CAP.store(0, Ordering::Relaxed);
        assert_eq!(effective(0), 0);
        assert_eq!(effective(500), 500);
        CAP.store(100, Ordering::Relaxed);
        assert_eq!(effective(0), 100);
        assert_eq!(effective(500), 100);
        assert_eq!(effective(50), 50);
        CAP.store(0, Ordering::Relaxed);
    }

    #[test]
    fn fake_game_cycle() {
        unsafe { std::env::set_var("STASH_FAKE_GAME", "10:5") };
        assert_eq!(fake(3.0), Some(true));
        assert_eq!(fake(12.0), Some(false));
        assert_eq!(fake(16.0), Some(true));
        unsafe { std::env::remove_var("STASH_FAKE_GAME") };
        assert_eq!(fake(3.0), None);
    }

    /// Live check: `cargo test dl::gaming::tests::live -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live() {
        let mut p = Pinger::new();
        for _ in 0..5 {
            println!("ping {:?}", p.ping());
        }
        println!("udp owners: {}", udp_owners().len());
        println!("foreground: {:?}", foreground().map(|(pid, f)| (pid, f, exe_path(pid))));
        println!("d3d full screen: {}", full_screen_d3d());
        println!("gateway: {:?}", gateway().map(|g| g.to_ne_bytes()));
    }

    /// Live check with a real download and a fake game (25 s on, 20 s off):
    /// `cargo test --release dl::gaming::tests::live_download -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_download() {
        let dir = std::env::temp_dir().join("stash-test").join("gaming");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        unsafe {
            std::env::set_var("STASH_DATA", dir.join("data"));
            std::env::set_var("STASH_FAKE_GAME", "25:20");
        }
        let _ = std::fs::create_dir_all(dir.join("data"));
        super::super::init(super::super::Config::default(), || {});
        let e = super::super::engine();
        e.add(super::super::AddRequest { source: "https://proof.ovh.net/files/1Gb.dat".into(), dir: dir.clone(), ..Default::default() });
        for s in 0..80 {
            std::thread::sleep(Duration::from_secs(1));
            let snap = e.snapshot();
            let j = snap.jobs.first().map(|j| format!("{:?} {:?} {:?}", j.state, j.phase, j.error)).unwrap_or_default();
            println!("{s:>3}s game={} cap={:>6}KB/s moving={:>6}KB/s {j}", active(), CAP.load(Ordering::Relaxed) / 1024, snap.down / 1024);
        }
        e.shutdown();
        let _ = std::fs::copy(dir.join("data").join("gaming.log"), std::env::temp_dir().join("stash-test").join("gaming-last.log"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
