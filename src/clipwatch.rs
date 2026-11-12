//! Opt-in (Settings > Shortcuts): when you copy a link to a file, a small card rises above
//! the stack offering to download it. It never takes focus, leaves after a few seconds,
//! and ignores links Stash itself put on the clipboard.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Mutex;

use windows::UI::Color;
use windows::UI::Composition::{CompositionDropShadowSourcePolicy, CompositionStretch, ContainerVisual, SpriteVisual};
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::System::DataExchange::{AddClipboardFormatListener, RemoveClipboardFormatListener};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, w};
use windows_numerics::{Vector2, Vector3};

use crate::gfx::{Gfx, color};
use crate::paint::{self, P};

/// The last text Stash put on the clipboard itself (Copy Download Link…).
static OWN: Mutex<String> = Mutex::new(String::new());

const W: f32 = 344.0;
const H: f32 = 60.0;
const MARGIN: f32 = 18.0; // room for the shadow
const STAY_MS: u32 = 8000;
const T_LEAVE: usize = 1;
const T_HIDE: usize = 2;

/// Called when Stash copies text, so the watcher doesn't offer it back.
pub fn note_own(text: &str) {
    *OWN.lock().unwrap() = text.trim().to_string();
}

pub fn listen(hwnd: HWND, on: bool) {
    unsafe {
        let _ = RemoveClipboardFormatListener(hwnd);
        if on {
            let _ = AddClipboardFormatListener(hwnd);
        }
    }
}

const KINDS: &[&str] = &[
    "zip", "7z", "rar", "tar", "gz", "tgz", "bz2", "xz", "zst", "exe", "msi", "msix", "msixbundle", "appx", "appinstaller", "iso", "img", "dmg", "apk", "deb",
    "rpm", "appimage", "pdf", "epub", "mobi", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "csv", "mp3", "flac", "wav", "m4a", "ogg", "opus", "aac", "mp4",
    "mkv", "mov", "avi", "webm", "m4v", "wmv", "jpg", "jpeg", "png", "gif", "webp", "heic", "tif", "tiff", "psd", "svg", "torrent", "bin", "jar", "whl",
    "nupkg", "vsix", "ttf", "otf", "woff2", "srt", "json", "xml", "safetensors", "gguf", "ckpt", "pt", "onnx",
];

/// The file a copied link points at: a magnet, or an http(s) link whose path ends in a
/// known file type. Returns (link, file name). Pages and anything else aren't offered.
pub fn downloadable(text: &str) -> Option<(String, String)> {
    let t = text.trim().trim_start_matches('\u{feff}').trim();
    if t.is_empty() || t.len() > 4096 || t.chars().any(char::is_whitespace) {
        return None;
    }
    let lower = t.to_ascii_lowercase();
    if lower.starts_with("magnet:?") && lower.contains("xt=urn:bt") {
        let name = t
            .split(['?', '&'])
            .find_map(|p| p.strip_prefix("dn="))
            .map(|n| percent_encoding::percent_decode_str(&n.replace('+', " ")).decode_utf8_lossy().into_owned())
            .unwrap_or_else(|| "Torrent".into());
        return Some((t.to_string(), name));
    }
    let rest = lower.strip_prefix("https://").or_else(|| lower.strip_prefix("http://"))?;
    let path = rest.split(['?', '#']).next()?;
    let (_host, path) = path.split_once('/')?;
    let last = path.rsplit('/').next().filter(|s| !s.is_empty())?;
    let ext = last.rsplit_once('.')?.1;
    if !KINDS.contains(&ext) {
        return None;
    }
    // the name as written (not lowercased), decoded
    let raw = t.split(['?', '#']).next()?.rsplit('/').next()?;
    let name = percent_encoding::percent_decode_str(raw).decode_utf8_lossy().into_owned();
    crate::dl::is_source(t).then(|| (t.to_string(), name))
}

/// The clipboard changed: a downloadable link someone else copied, not seen before.
pub fn check() -> Option<(String, String)> {
    // copied in one of our own windows
    let mut pid = 0;
    unsafe {
        GetWindowThreadProcessId(crate::shell::foreground(), Some(&mut pid));
    }
    if pid == std::process::id() {
        return None;
    }
    let text = crate::shell::clipboard_text()?;
    let found = downloadable(&text)?;
    let own = OWN.lock().unwrap().clone();
    if found.0 == own {
        return None;
    }
    PROMPT.with(|p| {
        let mut p = p.borrow_mut();
        if p.last.as_deref() == Some(found.0.as_str()) {
            return None;
        }
        p.last = Some(found.0.clone());
        Some(found)
    })
}

// ---------- the prompt card ----------

#[derive(Clone, Copy, PartialEq)]
enum Spot {
    None,
    Download,
    Close,
}

#[derive(Default)]
struct Prompt {
    last: Option<String>,
    win: Option<Card>,
}

struct Card {
    hwnd: HWND,
    gfx: Gfx,
    _target: DesktopWindowTarget,
    root: ContainerVisual,
    face: SpriteVisual,
    k: f32,
    link: String,
    name: String,
    dir: PathBuf,
    icon: Option<crate::gfx::Pixels>,
    hover: Spot,
    tracking: bool,
}

thread_local! {
    static PROMPT: RefCell<Prompt> = RefCell::new(Prompt::default());
}

fn button_rect() -> (f32, f32, f32, f32) {
    (W - 12.0 - 26.0 - 6.0 - 88.0, (H - 30.0) / 2.0, 88.0, 30.0)
}

fn close_rect() -> (f32, f32, f32, f32) {
    (W - 12.0 - 26.0, (H - 26.0) / 2.0, 26.0, 26.0)
}

fn inside(r: (f32, f32, f32, f32), x: f32, y: f32) -> bool {
    x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3
}

impl Card {
    fn paint(&self) -> windows::core::Result<()> {
        let k = self.k;
        let (link_name, hover) = (self.name.clone(), self.hover);
        let host = self
            .link
            .strip_prefix("https://")
            .or_else(|| self.link.strip_prefix("http://"))
            .and_then(|r| r.split('/').next())
            .map(|h| h.trim_start_matches("www.").to_string())
            .unwrap_or_else(|| "Magnet link".into());
        let dw = self.gfx.dwrite.clone();
        let icon = self.icon.clone();
        let gfx = self.gfx.clone();
        let surface = self.gfx.surface(W * k, H * k, |dc| {
            let p = P { dc, dw: &dw, k };
            p.round(0.0, 0.0, W, H, 16.0, color(28, 28, 30, 0.96));
            p.round_stroke(0.5, 0.5, W - 1.0, H - 1.0, 15.5, color(255, 255, 255, 0.09), 1.0);
            if let Some(px) = &icon {
                if let Ok(bmp) = Gfx::bitmap(dc, px) {
                    let s = 32.0;
                    let r = windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F {
                        left: 14.0 * k,
                        top: (H - s) / 2.0 * k,
                        right: (14.0 + s) * k,
                        bottom: ((H + s) / 2.0) * k,
                    };
                    unsafe {
                        dc.DrawBitmap(&bmp, Some(&r), 1.0, windows::Win32::Graphics::Direct2D::D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None);
                    }
                }
            }
            let _ = &gfx;
            let (bx, by, bw, bh) = button_rect();
            let tx = 56.0;
            let max = bx - tx - 10.0;
            p.text(&link_name, tx, 12.0, 13.0, paint::MEDIUM, color(255, 255, 255, 1.0), max);
            p.text(&host, tx, 31.0, 11.5, paint::REGULAR, color(255, 255, 255, 0.55), max);
            // tonal orange action: a tint of the accent, its text in the accent
            let a = paint::ORANGE;
            p.round(bx, by, bw, bh, 9.0, paint::rgb(a, if hover == Spot::Download { 0.34 } else { 0.22 }));
            let label = "Download";
            let lw = p.measure(label, 12.5, paint::SEMIBOLD);
            p.text_mid(label, bx + (bw - lw) / 2.0, by + bh / 2.0, 12.5, paint::SEMIBOLD, paint::rgb((255, 178, 120), 1.0), bw);
            let (cx, cy, cw, ch) = close_rect();
            if hover == Spot::Close {
                p.round(cx, cy, cw, ch, 8.0, color(255, 255, 255, 0.1));
            }
            p.glyph(crate::icons::CANCEL, cx + cw / 2.0, cy + ch / 2.0, 12.0, color(255, 255, 255, if hover == Spot::Close { 0.9 } else { 0.55 }));
        })?;
        let brush = self.gfx.compositor.CreateSurfaceBrushWithSurface(&surface)?;
        brush.SetStretch(CompositionStretch::Fill)?;
        self.face.SetBrush(&brush)
    }

    fn animate(&self, show: bool) -> windows::core::Result<()> {
        let c = &self.gfx.compositor;
        let ease = c.CreateCubicBezierEasingFunction(Vector2 { X: 0.23, Y: 1.0 }, Vector2 { X: 0.32, Y: 1.0 })?;
        let dur = windows::Foundation::TimeSpan { Duration: if show { 220 } else { 140 } * 10_000 };
        let o = c.CreateScalarKeyFrameAnimation()?;
        o.InsertKeyFrameWithEasingFunction(1.0, if show { 1.0 } else { 0.0 }, &ease)?;
        o.SetDuration(dur)?;
        self.root.StartAnimation(&HSTRING::from("Opacity"), &o)?;
        let m = MARGIN * self.k;
        let off = c.CreateVector3KeyFrameAnimation()?;
        off.InsertKeyFrameWithEasingFunction(1.0, Vector3 { X: m, Y: m + if show { 0.0 } else { 6.0 * self.k }, Z: 0.0 }, &ease)?;
        off.SetDuration(dur)?;
        self.face.StartAnimation(&HSTRING::from("Offset"), &off)
    }
}

/// Offer `link` in a card above `anchor` (the stack's tile, in screen pixels).
pub fn prompt(gfx: &Gfx, link: String, name: String, dir: PathBuf, anchor: RECT, right: bool) {
    PROMPT.with(|p| {
        let mut p = p.borrow_mut();
        if p.win.is_none() {
            match create(gfx) {
                Ok(card) => p.win = Some(card),
                Err(e) => {
                    crate::stash::log(format!("clipboard prompt failed: {e}"));
                    return;
                }
            }
        }
        let Some(card) = p.win.as_mut() else { return };
        card.k = crate::shell::scale_factor(card.hwnd).max(crate::shell::scale_factor(HWND::default()));
        let k = card.k;
        card.icon = crate::shell::type_icon(&name, false, (32.0 * k * 1.5) as i32);
        card.link = link;
        card.name = name;
        card.dir = dir;
        card.hover = Spot::None;
        let (ww, wh) = (((W + MARGIN * 2.0) * k).round() as i32, ((H + MARGIN * 2.0) * k).round() as i32);
        let m = (MARGIN * k).round() as i32;
        let wa = crate::shell::work_area();
        let x = if right { anchor.right - ww + m } else { anchor.left - m };
        let x = x.clamp(wa.left, wa.right - ww);
        let y = (anchor.top - wh + m - (6.0 * k) as i32).max(wa.top);
        let _ = card.face.SetSize(Vector2 { X: W * k, Y: H * k });
        let _ = card.face.SetOffset(Vector3 { X: m as f32, Y: m as f32 + 8.0 * k, Z: 0.0 });
        let _ = card.root.SetOpacity(0.0);
        let _ = card.paint();
        unsafe {
            let _ = SetWindowPos(card.hwnd, Some(HWND_TOPMOST), x, y, ww, wh, SWP_NOACTIVATE | SWP_SHOWWINDOW);
            let _ = KillTimer(Some(card.hwnd), T_HIDE);
            let _ = SetTimer(Some(card.hwnd), T_LEAVE, STAY_MS, None);
        }
        let _ = card.animate(true);
    });
}

fn create(gfx: &Gfx) -> windows::core::Result<Card> {
    let hwnd = unsafe {
        let instance = GetModuleHandleW(None)?;
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: w!("StashClipPrompt"),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
            w!("StashClipPrompt"),
            w!("Stash"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(instance.into()),
            None,
        )?
    };
    let target = gfx.target(hwnd)?;
    let c = &gfx.compositor;
    let root = c.CreateContainerVisual()?;
    target.SetRoot(&root)?;
    let face = c.CreateSpriteVisual()?;
    let shadow = c.CreateDropShadow()?;
    let k = crate::shell::scale_factor(hwnd);
    shadow.SetBlurRadius(16.0 * k)?;
    shadow.SetOffset(Vector3 { X: 0.0, Y: 4.0 * k, Z: 0.0 })?;
    shadow.SetOpacity(0.45)?;
    shadow.SetColor(Color { A: 255, R: 0, G: 0, B: 0 })?;
    shadow.SetSourcePolicy(CompositionDropShadowSourcePolicy::InheritFromVisualContent)?;
    face.SetShadow(&shadow)?;
    root.Children()?.InsertAtTop(&face)?;
    Ok(Card {
        hwnd,
        gfx: gfx.clone(),
        _target: target,
        root,
        face,
        k,
        link: String::new(),
        name: String::new(),
        dir: PathBuf::new(),
        icon: None,
        hover: Spot::None,
        tracking: false,
    })
}

fn dismiss() {
    PROMPT.with(|p| {
        if let Some(card) = p.borrow().win.as_ref() {
            let _ = card.animate(false);
            unsafe {
                let _ = KillTimer(Some(card.hwnd), T_LEAVE);
                let _ = SetTimer(Some(card.hwnd), T_HIDE, 160, None);
            }
        }
    });
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let spot_at = |lparam: LPARAM| -> Spot {
        let k = PROMPT.with(|p| p.borrow().win.as_ref().map_or(1.0, |c| c.k));
        let x = (lparam.0 & 0xffff) as i16 as f32 / k - MARGIN;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32 / k - MARGIN;
        if inside(button_rect(), x, y) {
            Spot::Download
        } else if inside(close_rect(), x, y) {
            Spot::Close
        } else {
            Spot::None
        }
    };
    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_MOUSEMOVE => {
            let spot = spot_at(lparam);
            PROMPT.with(|p| {
                let mut p = p.borrow_mut();
                let Some(card) = p.win.as_mut() else { return };
                if !card.tracking {
                    card.tracking = true;
                    let mut t = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                    unsafe {
                        let _ = TrackMouseEvent(&mut t);
                        // it stays while you're on it
                        let _ = KillTimer(Some(hwnd), T_LEAVE);
                    }
                }
                if spot != card.hover {
                    card.hover = spot;
                    let _ = card.paint();
                }
            });
            LRESULT(0)
        }
        0x02A3 /* WM_MOUSELEAVE */ => {
            PROMPT.with(|p| {
                let mut p = p.borrow_mut();
                let Some(card) = p.win.as_mut() else { return };
                card.tracking = false;
                card.hover = Spot::None;
                let _ = card.paint();
                unsafe {
                    let _ = SetTimer(Some(hwnd), T_LEAVE, 2500, None);
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            match spot_at(lparam) {
                Spot::Download => {
                    let job = PROMPT.with(|p| p.borrow().win.as_ref().map(|c| (c.link.clone(), c.dir.clone())));
                    if let Some((link, dir)) = job {
                        crate::dl::engine().add(crate::dl::AddRequest { source: link, dir, ..Default::default() });
                    }
                    dismiss();
                }
                Spot::Close => dismiss(),
                Spot::None => {}
            }
            LRESULT(0)
        }
        WM_TIMER => {
            unsafe {
                let _ = KillTimer(Some(hwnd), wparam.0);
            }
            match wparam.0 {
                T_LEAVE => dismiss(),
                T_HIDE => unsafe {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                },
                _ => {}
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offers_files_not_pages() {
        assert_eq!(downloadable("https://example.com/files/setup%20v2.exe?x=1").map(|d| d.1), Some("setup v2.exe".into()));
        assert!(downloadable("  https://cdn.example.org/a/b/Movie.MKV  ").is_some());
        assert!(downloadable("magnet:?xt=urn:btih:abc&dn=Ubuntu+24.04").is_some_and(|d| d.1 == "Ubuntu 24.04"));
        assert!(downloadable("https://example.com/").is_none());
        assert!(downloadable("https://example.com/blog/post.html").is_none());
        assert!(downloadable("https://example.com/download").is_none());
        assert!(downloadable("see https://example.com/a.zip").is_none());
        assert!(downloadable("ftp://example.com/a.zip").is_none());
        assert!(downloadable("https://example.zip").is_none());
        assert!(downloadable("\u{feff}https://example.com/a.zip").is_some());
    }
}
