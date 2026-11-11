//! Updates for the GitHub builds (the installer and the portable exe). Stash asks
//! GitHub for the latest release once a day (and when you ask), downloads a newer build
//! in the background, checks it against the release's SHA-256 list, and then offers to
//! restart into it. The Microsoft Store build is updated by the Store instead.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};

const REPO: &str = "frstycodes/stash";
pub const STORE: &str = "ms-windows-store://pdp/?productid=9PMVDZ3LZ490";
const DAY: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Idle,
    Checking,
    UpToDate,
    Downloading(String),
    /// A verified build, ready to install.
    Ready { version: String, file: PathBuf },
    Failed(String),
}

static STATUS: Mutex<Status> = Mutex::new(Status::Idle);
/// The "Download updates automatically" setting.
static AUTO: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub fn set_auto(on: bool) {
    AUTO.store(on, std::sync::atomic::Ordering::Relaxed);
}
static NOTIFY: Mutex<Option<Box<dyn Fn() + Send>>> = Mutex::new(None);

pub fn status() -> Status {
    STATUS.lock().unwrap().clone()
}

fn set(s: Status) {
    *STATUS.lock().unwrap() = s;
    if let Some(n) = NOTIFY.lock().unwrap().as_ref() {
        n();
    }
}

/// How this copy of Stash was installed, which decides how it updates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Install {
    /// From the Microsoft Store: the Store updates it.
    Store,
    /// With Stash-Setup.exe, into %LOCALAPPDATA%\Programs\Stash.
    Installer,
    /// A portable exe, anywhere.
    Portable,
    /// A development build (target\debug or target\release): never updates itself.
    Dev,
}

pub fn install() -> Install {
    if crate::shell::is_packaged() {
        return Install::Store;
    }
    let Ok(exe) = std::env::current_exe() else { return Install::Dev };
    let s = exe.to_string_lossy().to_lowercase();
    if s.contains("\\target\\release\\") || s.contains("\\target\\debug\\") {
        return Install::Dev;
    }
    let programs = std::env::var_os("LOCALAPPDATA").map(|l| PathBuf::from(l).join("Programs").join("Stash").to_string_lossy().to_lowercase());
    if programs.is_some_and(|p| s.starts_with(&p)) { Install::Installer } else { Install::Portable }
}

/// Call once at startup: clears what a previous update left behind, and checks for
/// updates now and once a day if `auto` (a setting). `notify` runs when the status changes.
pub fn start(notify: impl Fn() + Send + 'static) {
    *NOTIFY.lock().unwrap() = Some(Box::new(notify));
    cleanup();
    if !matches!(install(), Install::Installer | Install::Portable) {
        return;
    }
    std::thread::Builder::new()
        .name("stash-updates".into())
        .spawn(move || {
            // right away (on its own thread, so startup doesn't wait), then daily
            loop {
                if AUTO.load(std::sync::atomic::Ordering::Relaxed) {
                    run_check();
                }
                std::thread::sleep(DAY);
            }
        })
        .ok();
}

/// "Check for Updates" in Settings.
pub fn check_now() {
    if matches!(status(), Status::Checking | Status::Downloading(_)) {
        return;
    }
    std::thread::spawn(run_check);
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

fn version(s: &str) -> Option<(u64, u64, u64)> {
    let mut p = s.trim_start_matches('v').split(['.', '-']).map(|n| n.parse::<u64>().ok());
    Some((p.next()??, p.next()??, p.next()??))
}

pub fn newer(latest: &str, current: &str) -> bool {
    matches!((version(latest), version(current)), (Some(l), Some(c)) if l > c)
}

fn updates_dir() -> PathBuf {
    PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default()).join("Stash").join("updates")
}

fn run_check() {
    // a build already waiting: nothing to do
    if matches!(status(), Status::Ready { .. }) {
        return;
    }
    set(Status::Checking);
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => return set(Status::Failed(e.to_string())),
    };
    match rt.block_on(fetch()) {
        Ok(s) => set(s),
        Err(e) => {
            crate::stash::log(format!("update check failed: {e}"));
            set(Status::Failed(e));
        }
    }
}

async fn fetch() -> Result<Status, String> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("Stash/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(600))
        .build()
        .map_err(|e| e.to_string())?;
    let r = client
        .get(format!("https://api.github.com/repos/{REPO}/releases/latest"))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|_| "Can't reach GitHub".to_string())?;
    if !r.status().is_success() {
        return Err(format!("GitHub answered {}", r.status()));
    }
    let release: Release = r.json().await.map_err(|e| e.to_string())?;
    let latest = release.tag_name.trim_start_matches('v').to_string();
    if release.draft || release.prerelease || !newer(&latest, env!("CARGO_PKG_VERSION")) {
        return Ok(Status::UpToDate);
    }
    let want = match install() {
        Install::Installer => format!("Stash-Setup-{latest}.exe"),
        Install::Portable if cfg!(target_arch = "aarch64") => format!("Stash-{latest}-arm64-portable.exe"),
        Install::Portable => format!("Stash-{latest}-portable.exe"),
        _ => return Ok(Status::UpToDate),
    };
    let asset = release.assets.iter().find(|a| a.name == want).ok_or("This release has no build for this copy of Stash")?;
    let sums = release.assets.iter().find(|a| a.name == "SHA256SUMS.txt").ok_or("This release has no checksums")?;
    let sums = client.get(&sums.browser_download_url).send().await.map_err(|e| e.to_string())?.text().await.map_err(|e| e.to_string())?;
    let expected = sums
        .lines()
        .find_map(|l| {
            let (hash, name) = l.split_once(char::is_whitespace)?;
            (name.trim().trim_start_matches('*') == want).then(|| hash.trim().to_ascii_lowercase())
        })
        .ok_or("The checksums don't list this build")?;

    let dir = updates_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let file = dir.join(&want);
    // downloaded already (an earlier run)?
    if file.exists() && sha256(&file).as_deref() == Some(expected.as_str()) {
        return Ok(Status::Ready { version: latest, file });
    }
    set(Status::Downloading(latest.clone()));
    let bytes = client
        .get(&asset.browser_download_url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())?
        .bytes()
        .await
        .map_err(|e| e.to_string())?;
    let got = hex(&Sha256::digest(&bytes));
    if got != expected {
        return Err("The download didn't match its checksum".into());
    }
    let part = file.with_extension("part");
    std::fs::write(&part, &bytes).map_err(|e| e.to_string())?;
    std::fs::rename(&part, &file).map_err(|e| e.to_string())?;
    Ok(Status::Ready { version: latest, file })
}

fn sha256(path: &Path) -> Option<String> {
    std::fs::read(path).ok().map(|b| hex(&Sha256::digest(&b)))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Whether this copy updates itself from GitHub (the installer and portable builds).
pub fn self_updating() -> bool {
    matches!(install(), Install::Installer | Install::Portable)
}

/// Install the downloaded build and restart. The installer runs silently and starts
/// the new Stash when it's done; a portable exe swaps itself in place.
pub fn apply() -> Result<(), String> {
    let Status::Ready { file, .. } = status() else { return Err("No update is ready".into()) };
    match install() {
        Install::Installer => {
            std::process::Command::new(&file).arg("/S").spawn().map_err(|e| e.to_string())?;
        }
        Install::Portable => {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let old = exe.with_extension("exe.old");
            let _ = std::fs::remove_file(&old);
            // a running exe can be renamed, not overwritten
            std::fs::rename(&exe, &old).map_err(|e| e.to_string())?;
            if let Err(e) = std::fs::copy(&file, &exe) {
                let _ = std::fs::rename(&old, &exe);
                return Err(e.to_string());
            }
            std::process::Command::new(&exe).arg("--updated").spawn().map_err(|e| e.to_string())?;
        }
        _ => return Err("This copy of Stash updates elsewhere".into()),
    }
    Ok(())
}

/// After an update: the old portable exe and the downloaded installer can go.
fn cleanup() {
    if let Ok(exe) = std::env::current_exe() {
        let old = exe.with_extension("exe.old");
        for _ in 0..10 {
            if !old.exists() || std::fs::remove_file(&old).is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(200)); // the old copy may still be exiting
        }
    }
    if let Ok(entries) = std::fs::read_dir(updates_dir()) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            // keep a build that's newer than us (downloaded, not yet installed)
            let ver = name.trim_start_matches("Stash-Setup-").trim_start_matches("Stash-").split(['-', '.']).take(3).collect::<Vec<_>>().join(".");
            if !newer(&ver, env!("CARGO_PKG_VERSION")) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(newer("0.3.0", "0.2.1"));
        assert!(newer("v1.0.0", "0.9.9"));
        assert!(!newer("0.2.1", "0.2.1"));
        assert!(!newer("0.2.0", "0.2.1"));
        assert!(!newer("garbage", "0.2.1"));
        assert!(newer("0.10.0", "0.9.0"));
    }
}
