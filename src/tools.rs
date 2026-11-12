//! Helper programs Stash fetches the first time they're needed: yt-dlp (reads video
//! pages) and ffmpeg (joins a video's picture and sound, converts audio).
//!
//! They live per user in `%LOCALAPPDATA%\Stash\tools`, each checked against the
//! checksums its project publishes before it's kept. yt-dlp is kept current: a week
//! after the last check, the next use looks for a newer release in the background.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

const YTDLP_REPO: &str = "https://github.com/yt-dlp/yt-dlp";
/// yt-dlp's own builds of ffmpeg (patched for it), the shared-library build: half the
/// size of the static one.
const FFMPEG_ZIP: &str = "ffmpeg-master-latest-win64-gpl-shared.zip";
const FFMPEG_BASE: &str = "https://github.com/yt-dlp/FFmpeg-Builds/releases/download/latest";
const UPDATE_EVERY: Duration = Duration::from_secs(7 * 24 * 3600);
/// Run hidden: no console window flashes up.
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// What the UI shows about the tools.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    /// A tool downloading right now.
    pub fetching: Option<Fetch>,
    pub ytdlp: Option<String>,
    pub ffmpeg: Option<String>,
    /// The last fetch failed, in plain words.
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Fetch {
    pub name: &'static str,
    pub done: u64,
    pub total: Option<u64>,
}

static STATUS: Mutex<Option<Status>> = Mutex::new(None);
static NOTIFY: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();
/// One install at a time.
static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Called (from any thread) whenever the status changes.
pub fn set_notify(f: impl Fn() + Send + Sync + 'static) {
    let _ = NOTIFY.set(Box::new(f));
}

fn changed() {
    if let Some(f) = NOTIFY.get() {
        f();
    }
}

pub fn status() -> Status {
    let mut s = STATUS.lock().unwrap();
    if s.is_none() {
        // versions are read once, in the background
        *s = Some(Status::default());
        drop(s);
        runtime().spawn(async { refresh_versions().await });
        return Status::default();
    }
    s.clone().unwrap_or_default()
}

fn edit(f: impl FnOnce(&mut Status)) {
    let mut s = STATUS.lock().unwrap();
    f(s.get_or_insert_with(Status::default));
    drop(s);
    changed();
}

/// A small runtime for work the UI starts (probing a link, updating the tools).
pub fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .max_blocking_threads(2)
            .thread_name("stash-tools")
            .enable_all()
            .build()
            .expect("tools runtime")
    })
}

fn client() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent(concat!("Stash/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(20))
            .read_timeout(Duration::from_secs(30))
            .build()
            .expect("http client")
    })
}

pub fn dir() -> PathBuf {
    if let Some(d) = std::env::var_os("STASH_TOOLS") {
        return PathBuf::from(d);
    }
    // a test copy (STASH_DATA) keeps its own tools
    if let Some(d) = std::env::var_os("STASH_DATA") {
        return PathBuf::from(d).join("tools");
    }
    PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default()).join("Stash").join("tools")
}

pub fn ytdlp_path() -> PathBuf {
    dir().join("yt-dlp.exe")
}

/// The folder holding ffmpeg.exe and ffprobe.exe (yt-dlp's `--ffmpeg-location`).
pub fn ffmpeg_dir() -> PathBuf {
    dir().join("ffmpeg")
}

pub fn has_ytdlp() -> bool {
    ytdlp_path().is_file()
}

pub fn has_ffmpeg() -> bool {
    ffmpeg_dir().join("ffmpeg.exe").is_file()
}

/// A command for a tool that runs without a console window.
pub fn command(exe: &Path) -> tokio::process::Command {
    let mut c = tokio::process::Command::new(exe);
    c.creation_flags(CREATE_NO_WINDOW)
        .stdin(std::process::Stdio::null())
        .env("PYTHONUTF8", "1")
        .env("PYTHONIOENCODING", "utf-8");
    c
}

/// yt-dlp, fetched first if it isn't here yet.
pub async fn ytdlp() -> Result<PathBuf, String> {
    let exe = ytdlp_path();
    if exe.is_file() {
        update_later();
        return Ok(exe);
    }
    let _g = LOCK.lock().await;
    if !exe.is_file() {
        install_ytdlp().await?;
    }
    Ok(exe)
}

/// ffmpeg's folder, fetched first if it isn't here yet.
pub async fn ffmpeg() -> Result<PathBuf, String> {
    if has_ffmpeg() {
        return Ok(ffmpeg_dir());
    }
    let _g = LOCK.lock().await;
    if !has_ffmpeg() {
        install_ffmpeg().await?;
    }
    Ok(ffmpeg_dir())
}

/// Fetch both now (the Settings row's Update), replacing what's here if it's out of date.
pub async fn update_all() -> Result<(), String> {
    let _g = LOCK.lock().await;
    let r = async {
        let latest = latest_ytdlp_tag().await?;
        if !has_ytdlp() || installed_ytdlp().await.as_deref() != Some(latest.as_str()) {
            install_ytdlp().await?;
        } else {
            stamp();
        }
        // ffmpeg's builds are rolling; fetch it again only when it's missing
        if !has_ffmpeg() {
            install_ffmpeg().await?;
        }
        Ok::<(), String>(())
    }
    .await;
    if let Err(e) = &r {
        edit(|s| s.error = Some(e.clone()));
    }
    refresh_versions().await;
    r
}

/// Delete the tools (the Settings row's Remove). They come back when next needed.
pub async fn remove() {
    let _g = LOCK.lock().await;
    let d = dir();
    // a running tool can't be deleted; move it aside, it goes next time
    let _ = std::fs::remove_dir_all(&d);
    if d.exists() {
        let aside = d.with_file_name(format!("tools.old-{}", now()));
        let _ = std::fs::rename(&d, &aside);
        let _ = std::fs::remove_dir_all(&aside);
    }
    edit(|s| {
        s.ytdlp = None;
        s.ffmpeg = None;
        s.error = None;
    });
}

/// A week after the last check, look for a newer yt-dlp (in the background).
fn update_later() {
    let stamp_file = dir().join("yt-dlp.checked");
    let fresh = std::fs::read_to_string(&stamp_file).ok().and_then(|t| t.trim().parse::<u64>().ok()).is_some_and(|t| now().saturating_sub(t) < UPDATE_EVERY.as_secs());
    if fresh {
        return;
    }
    static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if STARTED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    runtime().spawn(async {
        let Ok(_g) = LOCK.try_lock() else { return };
        if let Ok(latest) = latest_ytdlp_tag().await {
            if installed_ytdlp().await.as_deref() != Some(latest.as_str()) {
                let _ = install_ytdlp().await;
            } else {
                stamp();
            }
        }
        STARTED.store(false, std::sync::atomic::Ordering::Relaxed);
    });
}

fn stamp() {
    let _ = std::fs::write(dir().join("yt-dlp.checked"), now().to_string());
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

/// The newest release's tag ("2026.11.06"): where `/releases/latest` redirects to.
async fn latest_ytdlp_tag() -> Result<String, String> {
    let r = client().head(format!("{YTDLP_REPO}/releases/latest")).send().await.map_err(net)?;
    let url = r.url().clone();
    let tag = url.path_segments().and_then(|mut s| s.next_back().map(str::to_string)).unwrap_or_default();
    if tag.is_empty() || tag == "latest" || !tag.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-') {
        return Err("Couldn't find the newest version of the video tools.".into());
    }
    Ok(tag)
}

async fn installed_ytdlp() -> Option<String> {
    let out = command(&ytdlp_path()).arg("--version").output().await.ok()?;
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !v.is_empty()).then_some(v)
}

async fn installed_ffmpeg() -> Option<String> {
    let out = command(&ffmpeg_dir().join("ffmpeg.exe")).arg("-version").output().await.ok()?;
    ffmpeg_version(&String::from_utf8_lossy(&out.stdout))
}

/// "ffmpeg version N-121583-g4348bde2d2-20251110 Copyright…" to "2025.11.10"
/// (the build's date; a release build's number as it is).
pub fn ffmpeg_version(out: &str) -> Option<String> {
    let token = out.lines().next()?.split_whitespace().nth(2)?;
    let tail = token.rsplit('-').next().unwrap_or("");
    if tail.len() == 8 && tail.chars().all(|c| c.is_ascii_digit()) {
        return Some(format!("{}.{}.{}", &tail[0..4], &tail[4..6], &tail[6..8]));
    }
    Some(token.chars().take(20).collect())
}

pub async fn refresh_versions() {
    let y = if has_ytdlp() { installed_ytdlp().await } else { None };
    let f = if has_ffmpeg() { installed_ffmpeg().await } else { None };
    edit(|s| {
        s.ytdlp = y;
        s.ffmpeg = f;
    });
}

async fn install_ytdlp() -> Result<(), String> {
    let r = async {
        let tag = latest_ytdlp_tag().await?;
        let base = format!("{YTDLP_REPO}/releases/download/{tag}");
        let sums = text(&format!("{base}/SHA2-256SUMS")).await?;
        let want = sum_for(&sums, "yt-dlp.exe").ok_or("The video tools' checksums are missing.")?;
        let d = dir();
        std::fs::create_dir_all(&d).map_err(|e| format!("Can't create {}: {e}", d.display()))?;
        let tmp = d.join("yt-dlp.exe.download");
        let got = fetch(&format!("{base}/yt-dlp.exe"), &tmp, "yt-dlp").await?;
        if !got.eq_ignore_ascii_case(&want) {
            let _ = std::fs::remove_file(&tmp);
            return Err("The video tools didn't match their published checksum, so they weren't kept.".into());
        }
        replace(&tmp, &ytdlp_path())?;
        stamp();
        Ok(())
    }
    .await;
    finish_fetch(&r);
    if r.is_ok() {
        let v = installed_ytdlp().await;
        edit(|s| s.ytdlp = v);
    }
    r
}

async fn install_ffmpeg() -> Result<(), String> {
    let r = async {
        let d = dir();
        std::fs::create_dir_all(&d).map_err(|e| format!("Can't create {}: {e}", d.display()))?;
        let zip = d.join("ffmpeg.zip.download");
        // the rolling build can change between reading its checksum and fetching it: try twice
        let mut ok = false;
        for _ in 0..2 {
            let sums = text(&format!("{FFMPEG_BASE}/checksums.sha256")).await?;
            let want = sum_for(&sums, FFMPEG_ZIP).ok_or("ffmpeg's checksums are missing.")?;
            let got = fetch(&format!("{FFMPEG_BASE}/{FFMPEG_ZIP}"), &zip, "ffmpeg").await?;
            if got.eq_ignore_ascii_case(&want) {
                ok = true;
                break;
            }
        }
        if !ok {
            let _ = std::fs::remove_file(&zip);
            return Err("ffmpeg didn't match its published checksum, so it wasn't kept.".into());
        }
        let r = unzip_bin(&zip, &d).await;
        let _ = std::fs::remove_file(&zip);
        r
    }
    .await;
    finish_fetch(&r);
    if r.is_ok() {
        let v = installed_ffmpeg().await;
        edit(|s| s.ffmpeg = v);
    }
    r
}

fn finish_fetch(r: &Result<(), String>) {
    edit(|s| {
        s.fetching = None;
        s.error = r.as_ref().err().cloned();
    });
}

/// Unpack the zip's `bin` folder (ffmpeg, ffprobe and their libraries) into `ffmpeg\`,
/// with Windows' own tar.
async fn unzip_bin(zip: &Path, dir: &Path) -> Result<(), String> {
    let staging = dir.join("ffmpeg.unpack");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
    let tar = PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into())).join("System32").join("tar.exe");
    let out = command(&tar)
        .arg("-xf")
        .arg(zip)
        .arg("-C")
        .arg(&staging)
        .arg("*/bin/*")
        .output()
        .await
        .map_err(|e| format!("Can't unpack ffmpeg: {e}"))?;
    if !out.status.success() {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(format!("Can't unpack ffmpeg: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let bin = find_bin(&staging).ok_or("ffmpeg's download had no programs in it.")?;
    let dest = ffmpeg_dir();
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
    for e in std::fs::read_dir(&bin).map_err(|e| e.to_string())?.flatten() {
        let name = e.file_name().to_string_lossy().to_ascii_lowercase();
        // the player isn't needed
        if name == "ffplay.exe" {
            continue;
        }
        std::fs::rename(e.path(), dest.join(e.file_name())).map_err(|e| format!("Can't install ffmpeg: {e}"))?;
    }
    let _ = std::fs::remove_dir_all(&staging);
    if !has_ffmpeg() {
        return Err("ffmpeg's download had no ffmpeg in it.".into());
    }
    Ok(())
}

fn find_bin(dir: &Path) -> Option<PathBuf> {
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() {
            if p.join("ffmpeg.exe").is_file() {
                return Some(p);
            }
            if let Some(b) = find_bin(&p) {
                return Some(b);
            }
        }
    }
    None
}

/// Put a new file in place of an old one that may be running (Windows lets a running
/// program be renamed, not replaced).
fn replace(new: &Path, dest: &Path) -> Result<(), String> {
    let old = dest.with_extension("old.exe");
    let _ = std::fs::remove_file(&old);
    if dest.exists() {
        std::fs::rename(dest, &old).map_err(|e| format!("Can't update the video tools: {e}"))?;
    }
    std::fs::rename(new, dest).map_err(|e| format!("Can't install the video tools: {e}"))?;
    let _ = std::fs::remove_file(&old);
    Ok(())
}

/// The checksum for `file` in a `sha256sum`-style list.
pub fn sum_for(sums: &str, file: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let mut parts = l.split_whitespace();
        let hash = parts.next()?;
        let name = parts.next()?.trim_start_matches('*');
        (name == file && hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit())).then(|| hash.to_ascii_lowercase())
    })
}

fn net(e: reqwest::Error) -> String {
    if e.is_timeout() || e.is_connect() {
        "Couldn't reach GitHub to get the video tools. Check your connection.".into()
    } else {
        format!("Couldn't get the video tools: {e}")
    }
}

async fn text(url: &str) -> Result<String, String> {
    let r = client().get(url).send().await.map_err(net)?;
    if !r.status().is_success() {
        return Err(format!("Couldn't get the video tools (the server said {}).", r.status()));
    }
    r.text().await.map_err(net)
}

/// Download to `dest`, showing progress; the SHA-256 of what arrived, as hex.
async fn fetch(url: &str, dest: &Path, name: &'static str) -> Result<String, String> {
    use futures_util::StreamExt;
    use std::io::Write;
    let r = client().get(url).send().await.map_err(net)?;
    if !r.status().is_success() {
        return Err(format!("Couldn't get {name} (the server said {}).", r.status()));
    }
    let total = r.content_length();
    edit(|s| {
        s.fetching = Some(Fetch { name, done: 0, total });
        s.error = None;
    });
    let mut file = std::fs::File::create(dest).map_err(|e| format!("Can't save {name}: {e}"))?;
    let mut hash = Sha256::new();
    let mut done = 0u64;
    let mut shown = Instant::now();
    let mut stream = r.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(net)?;
        hash.update(&chunk);
        file.write_all(&chunk).map_err(|e| format!("Can't save {name}: {e}"))?;
        done += chunk.len() as u64;
        if shown.elapsed() > Duration::from_millis(200) {
            shown = Instant::now();
            edit(|s| s.fetching = Some(Fetch { name, done, total }));
        }
    }
    file.sync_all().map_err(|e| format!("Can't save {name}: {e}"))?;
    Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_lists() {
        let sums = "e51e69e89489f199530f13d32a4342da3ddb2cf34842f364603a066db05afbf3  yt-dlp\nF5389D3698ECDE4EF06F4E6B1CE0294FE9AC1396228C8990B8D3416D6D177A7D *yt-dlp.exe\nbad  yt-dlp_x86.exe\n";
        assert_eq!(sum_for(sums, "yt-dlp.exe").as_deref(), Some("f5389d3698ecde4ef06f4e6b1ce0294fe9ac1396228c8990b8d3416d6d177a7d"));
        assert_eq!(sum_for(sums, "yt-dlp").map(|s| s.len()), Some(64));
        assert_eq!(sum_for(sums, "yt-dlp_x86.exe"), None);
        assert_eq!(sum_for(sums, "nope"), None);
    }

    #[test]
    fn ffmpeg_versions() {
        assert_eq!(ffmpeg_version("ffmpeg version N-121583-g4348bde2d2-20251110 Copyright (c) 2000-2025").as_deref(), Some("2025.11.10"));
        assert_eq!(ffmpeg_version("ffmpeg version 7.1 Copyright").as_deref(), Some("7.1"));
        assert_eq!(ffmpeg_version(""), None);
    }
}
