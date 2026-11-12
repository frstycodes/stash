//! Getting work to the running Stash: a second launch (from the Start menu, a magnet
//! link or a .torrent file) forwards its arguments with WM_COPYDATA; the browser
//! extensions talk to `stash.exe` as a native messaging host, which forwards the same way.

use std::io::{Read, Write};

use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, SendMessageTimeoutW, SMTO_ABORTIFHUNG, WM_COPYDATA};
use windows::core::{PCWSTR, w};

pub const MAGIC: usize = 0x5354_4153; // "STAS"

/// A download a browser handed over.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BrowserDownload {
    pub url: String,
    pub filename: Option<String>,
    pub referrer: Option<String>,
    pub cookies: Option<String>,
    pub user_agent: Option<String>,
}

impl BrowserDownload {
    pub fn headers(&self) -> Vec<(String, String)> {
        let mut h = Vec::new();
        if let Some(r) = self.referrer.as_ref().filter(|r| !r.is_empty()) {
            h.push(("Referer".into(), r.clone()));
        }
        if let Some(c) = self.cookies.as_ref().filter(|c| !c.is_empty()) {
            h.push(("Cookie".into(), c.clone()));
        }
        if let Some(u) = self.user_agent.as_ref().filter(|u| !u.is_empty()) {
            h.push(("User-Agent".into(), u.clone()));
        }
        h
    }

    /// Just the file name (browsers may hand over a full path).
    pub fn name(&self) -> Option<String> {
        let f = self.filename.as_ref()?;
        crate::dl::safe_name(f.rsplit(['\\', '/']).next()?)
    }
}

pub enum Request {
    /// Bring up the downloads window.
    Show,
    /// Links to confirm in the New Download sheet.
    Open(Vec<String>),
    /// Downloads to start right away (from a browser).
    Add(Vec<BrowserDownload>),
}

pub fn encode(r: &Request) -> String {
    match r {
        Request::Show => "show".into(),
        Request::Open(links) => std::iter::once("open".to_string()).chain(links.iter().cloned()).collect::<Vec<_>>().join("\n"),
        Request::Add(d) => std::iter::once("add".to_string()).chain(d.iter().filter_map(|d| serde_json::to_string(d).ok())).collect::<Vec<_>>().join("\n"),
    }
}

pub fn decode(s: &str) -> Request {
    let mut lines = s.lines();
    match lines.next().unwrap_or("") {
        "open" => Request::Open(lines.map(str::to_string).filter(|l| !l.trim().is_empty()).collect()),
        "add" => Request::Add(lines.filter_map(|l| serde_json::from_str(l).ok()).collect()),
        _ => Request::Show,
    }
}

/// What a launch with these arguments asks for.
pub fn from_args(args: &[String]) -> Request {
    let links: Vec<String> = args.iter().filter(|a| !a.starts_with("--")).cloned().collect();
    if links.is_empty() { Request::Show } else { Request::Open(links) }
}

fn running() -> Option<HWND> {
    unsafe { FindWindowW(w!("StashTile"), PCWSTR::null()).ok() }
}

/// Send a request to the running Stash. False if it isn't running.
pub fn forward(r: &Request) -> bool {
    let Some(hwnd) = running() else { return false };
    let wide: Vec<u16> = encode(r).encode_utf16().collect();
    let cds = COPYDATASTRUCT { dwData: MAGIC, cbData: (wide.len() * 2) as u32, lpData: wide.as_ptr() as *mut _ };
    unsafe {
        // let it bring its window to the front
        let _ = windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(u32::MAX);
        let mut result = 0usize;
        // it answers 1 once it has taken the request; a timeout (or a hung Stash) is a no,
        // so a browser keeps its own copy of the download
        let sent = SendMessageTimeoutW(hwnd, WM_COPYDATA, WPARAM(0), LPARAM(&cds as *const _ as isize), SMTO_ABORTIFHUNG, 5000, Some(&mut result));
        sent.0 != 0 && result == 1
    }
}

/// Whether the running Stash can take this request (answered before it's handled).
pub fn takes(r: &Request) -> bool {
    match r {
        Request::Add(list) => crate::dl::engine_opt().is_some() && list.iter().any(|d| crate::dl::is_source(&d.url)),
        _ => true,
    }
}

/// Read a WM_COPYDATA from `forward`.
pub fn receive(lparam: LPARAM) -> Option<Request> {
    let cds = unsafe { &*(lparam.0 as *const COPYDATASTRUCT) };
    if cds.dwData != MAGIC || cds.lpData.is_null() {
        return None;
    }
    let data = unsafe { std::slice::from_raw_parts(cds.lpData as *const u16, cds.cbData as usize / 2) };
    Some(decode(&String::from_utf16_lossy(data)))
}

// ---------- native messaging host (for the browser extensions) ----------

/// Chrome starts the host with the extension's origin; Firefox with the manifest path
/// and the extension's id.
pub fn is_native_host(args: &[String]) -> bool {
    args.iter().any(|a| a.starts_with("chrome-extension://") || a == "--native-host" || a.ends_with(".native.json") || a.contains("@stash.app"))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HostMessage {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    download: Option<BrowserDownload>,
}

/// Speak the native messaging protocol on stdin/stdout: 4-byte length, then JSON.
pub fn run_native_host() {
    let mut stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout().lock();
    loop {
        let mut len = [0u8; 4];
        if stdin.read_exact(&mut len).is_err() {
            return; // the browser closed the port
        }
        let len = u32::from_le_bytes(len) as usize;
        if len > 1 << 20 {
            return;
        }
        let mut buf = vec![0u8; len];
        if stdin.read_exact(&mut buf).is_err() {
            return;
        }
        let reply = match serde_json::from_slice::<HostMessage>(&buf) {
            Ok(m) if m.kind == "ping" => serde_json::json!({ "ok": true, "version": env!("CARGO_PKG_VERSION") }),
            Ok(m) if m.kind == "download" && m.download.is_some() => {
                // a Stash that's only just started may need a moment before it can take it
                let req = Request::Add(vec![m.download.unwrap()]);
                let ok = ensure_running()
                    && (0..15).any(|i| {
                        if i > 0 {
                            std::thread::sleep(std::time::Duration::from_millis(200));
                        }
                        forward(&req)
                    });
                serde_json::json!({ "ok": ok })
            }
            Ok(m) if m.kind == "show" => serde_json::json!({ "ok": ensure_running() && forward(&Request::Show) }),
            _ => serde_json::json!({ "ok": false, "error": "unknown message" }),
        };
        let out = reply.to_string();
        let _ = stdout.write_all(&(out.len() as u32).to_le_bytes());
        let _ = stdout.write_all(out.as_bytes());
        let _ = stdout.flush();
    }
}

/// The Chrome extension ids the host answers: the unpacked one from `browser/` (fixed by
/// the key in its manifest), plus the Chrome Web Store listing's id when the build sets
/// `STASH_CHROME_STORE_ID` (the release workflow does, from the `CHROME_EXTENSION_ID` secret).
const CHROME_IDS: &[&str] = match option_env!("STASH_CHROME_STORE_ID") {
    Some(id) if !id.is_empty() => &["nophahmheeiolmgdgedfjemnmcpdglad", id],
    _ => &["nophahmheeiolmgdgedfjemnmcpdglad"],
};
const FIREFOX_ID: &str = "stash-downloads@frstycodes.github.io";
const HOST_NAME: &str = "app.stash.downloads";

/// Tell Chrome, Edge, Brave and Firefox where the native messaging host is (this exe),
/// for the current user. Cheap; done at every start so a moved exe keeps working.
pub fn register_native_host() {
    let Ok(exe) = std::env::current_exe() else { return };
    let dir = crate::dl::data_dir();
    let _ = std::fs::create_dir_all(&dir);
    let origins: Vec<String> = CHROME_IDS.iter().map(|id| format!("chrome-extension://{id}/")).collect();
    let chromium = serde_json::json!({
        "name": HOST_NAME,
        "description": "Stash download manager",
        "path": exe,
        "type": "stdio",
        "allowed_origins": origins,
    });
    let firefox = serde_json::json!({
        "name": HOST_NAME,
        "description": "Stash download manager",
        "path": exe,
        "type": "stdio",
        "allowed_extensions": [FIREFOX_ID],
    });
    let chromium_file = dir.join("stash.native.json");
    let firefox_file = dir.join("stash.firefox.native.json");
    let _ = std::fs::write(&chromium_file, chromium.to_string());
    let _ = std::fs::write(&firefox_file, firefox.to_string());
    let reg = windows_registry::CURRENT_USER;
    for browser in [r"Software\Google\Chrome", r"Software\Microsoft\Edge", r"Software\BraveSoftware\Brave-Browser", r"Software\Chromium"] {
        if let Ok(k) = reg.create(format!(r"{browser}\NativeMessagingHosts\{HOST_NAME}")) {
            let _ = k.set_string("", chromium_file.display().to_string());
        }
    }
    if let Ok(k) = reg.create(format!(r"Software\Mozilla\NativeMessagingHosts\{HOST_NAME}")) {
        let _ = k.set_string("", firefox_file.display().to_string());
    }
}

/// Start Stash if it isn't running, and wait (briefly) for it to be ready.
fn ensure_running() -> bool {
    if running().is_some() {
        return true;
    }
    let Ok(exe) = std::env::current_exe() else { return false };
    if std::process::Command::new(exe).arg("--background").spawn().is_err() {
        return false;
    }
    for _ in 0..50 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if running().is_some() {
            return true;
        }
    }
    false
}
