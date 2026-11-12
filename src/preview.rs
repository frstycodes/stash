//! Quick Look for a finished download: a large preview over the list (a photo or a
//! video's frame, the start of a text file, or an app's icon with what it is), with
//! Open and Show in Folder. Space or Ctrl+Y opens it; Space or Esc closes it, and the
//! arrow keys move through the list with it open.
//!
//! The picture and text are read on a thread of their own and handed back through
//! `deliver`, so opening it never waits on the disk.

use std::path::{Path, PathBuf};

use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::{D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_EXTEND_MODE_CLAMP, D2D1_BITMAP_BRUSH_PROPERTIES1, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC};
use windows_numerics::Matrix3x2;

use crate::gfx::{Gfx, Pixels};
use crate::paint::{self, MEDIUM, P, REGULAR, SEMIBOLD};
use crate::search::{self, FileType};
use crate::settings_page::Rect;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Look {
    Picture,
    Video,
    Text,
    Other,
}

/// What a click on the preview hits.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum QHit {
    Card,
    Open,
    Reveal,
}

/// What the loader thread found.
pub struct Loaded {
    pub path: PathBuf,
    pub pixels: Option<Pixels>,
    pub text: Option<Vec<String>>,
}

pub struct Preview {
    pub id: u64,
    pub name: String,
    pub path: PathBuf,
    pub look: Look,
    pub size: Option<u64>,
    pub modified: Option<u64>,
    pub pixels: Option<Pixels>,
    pub text: Option<Vec<String>>,
    pub loading: bool,
}

const TEXT_EXT: [&str; 34] = [
    "txt", "md", "markdown", "log", "csv", "tsv", "json", "xml", "yaml", "yml", "ini", "cfg", "conf", "toml", "rs", "py", "js", "ts", "html", "htm", "css", "c", "cpp", "h", "cs", "java", "go", "sh", "bat", "ps1", "srt", "vtt",
    "nfo", "sql",
];

pub fn look_of(name: &str) -> Look {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    if TEXT_EXT.contains(&ext.as_str()) {
        return Look::Text;
    }
    match search::file_type(name) {
        FileType::Image => Look::Picture,
        FileType::Video => Look::Video,
        _ => Look::Other,
    }
}

impl Preview {
    pub fn new(id: u64, name: &str, path: &Path, size: Option<u64>) -> Self {
        let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs());
        Preview { id, name: name.to_string(), path: path.to_path_buf(), look: look_of(name), size, modified, pixels: None, text: None, loading: true }
    }

    /// Read the picture (and the text) on a thread; `deliver` gets it there.
    pub fn load(&self, deliver: impl FnOnce(Loaded) + Send + 'static) {
        let (path, look) = (self.path.clone(), self.look);
        let _ = std::thread::Builder::new().name("stash-preview".into()).spawn(move || {
            crate::shell::init_com_thread();
            let text = if look == Look::Text { read_text(&path) } else { None };
            let size = if matches!(look, Look::Picture | Look::Video) { 1600 } else { 256 };
            let pixels = crate::shell::thumbnail(&path, size);
            deliver(Loaded { path, pixels, text });
        });
    }

    pub fn take(&mut self, l: Loaded) -> bool {
        if l.path != self.path {
            return false;
        }
        self.pixels = l.pixels;
        self.text = l.text;
        self.loading = false;
        true
    }
}

/// The start of a text file: up to 60 lines, tabs as spaces, very long lines cut.
pub fn read_text(path: &Path) -> Option<Vec<String>> {
    use std::io::Read;
    let mut buf = vec![0u8; 32 * 1024];
    let n = std::fs::File::open(path).and_then(|mut f| f.read(&mut buf)).ok()?;
    let bytes = &buf[..n];
    if bytes.contains(&0) {
        return None; // binary, or UTF-16: not worth guessing
    }
    let s = String::from_utf8_lossy(bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes));
    Some(s.lines().take(60).map(|l| l.replace('\t', "    ").chars().take(240).collect()).collect())
}

/// What kind of file it is, in words ("ZIP archive").
pub fn kind_words(name: &str) -> String {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_uppercase()).unwrap_or_default();
    let what = match search::file_type(name) {
        FileType::Video => "video",
        FileType::Audio => "audio",
        FileType::Archive => "archive",
        FileType::Document => "document",
        FileType::Image => "image",
        FileType::App => "app",
        FileType::Other => "file",
    };
    if ext.is_empty() { format!("A {what}") } else { format!("{ext} {what}") }
}

// ---------- geometry ----------

const PAD: f32 = 14.0;
const FOOT: f32 = 72.0;
pub const RADIUS: f32 = 26.0;

/// The card's size for a content column `cw` x `ch`.
pub fn size(cw: f32, ch: f32) -> (f32, f32) {
    ((cw - 64.0).clamp(360.0, 680.0), (ch - 120.0).clamp(300.0, 540.0))
}

fn buttons(w: f32, h: f32) -> (Rect, Rect) {
    let bh = crate::ui::BUTTON_H_SMALL;
    let y = h - FOOT / 2.0 - bh / 2.0;
    let open = Rect::new(w - PAD - 6.0 - 84.0, y, 84.0, bh);
    let reveal = Rect::new(open.x - 8.0 - bh, y, bh, bh);
    (open, reveal)
}

pub fn hit(w: f32, h: f32, x: f32, y: f32) -> QHit {
    let (open, reveal) = buttons(w, h);
    if open.contains(x, y) {
        QHit::Open
    } else if reveal.contains(x, y) {
        QHit::Reveal
    } else {
        QHit::Card
    }
}

/// Fill a squircle with a bitmap scaled to cover (or fit) the box.
fn picture(p: &P, px: &Pixels, r: Rect, radius: f32, cover: bool) {
    let k = p.k;
    let (iw, ih) = (px.width as f32, px.height as f32);
    let scale = if cover { (r.w * k / iw).max(r.h * k / ih) } else { (r.w * k / iw).min(r.h * k / ih) };
    let (dw, dh) = (iw * scale / k, ih * scale / k);
    let (dx, dy) = (r.x + (r.w - dw) / 2.0, r.y + (r.h - dh) / 2.0);
    let Ok(bmp) = Gfx::bitmap(p.dc, px) else { return };
    let Some(shape) = p.squircle(dx.max(r.x), dy.max(r.y), dw.min(r.w), dh.min(r.h), radius) else { return };
    unsafe {
        let props = D2D1_BITMAP_BRUSH_PROPERTIES1 { extendModeX: D2D1_EXTEND_MODE_CLAMP, extendModeY: D2D1_EXTEND_MODE_CLAMP, interpolationMode: D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC };
        let Ok(brush) = p.dc.CreateBitmapBrush(&bmp, Some(&props), None) else { return };
        brush.SetTransform(&Matrix3x2 { M11: scale, M12: 0.0, M21: 0.0, M22: scale, M31: dx * k, M32: dy * k });
        p.dc.FillGeometry(&shape, &brush, None);
    }
}

pub fn paint(p: &P, q: &Preview, w: f32, h: f32, hover: Option<QHit>, offset: i64) {
    p.popover(0.0, 0.0, w, h, RADIUS);
    let media = Rect::new(PAD, PAD, w - 2.0 * PAD, h - PAD - FOOT);
    let inner_r = RADIUS - PAD; // concentric
    match (&q.pixels, &q.text, q.look) {
        (_, Some(lines), Look::Text) => {
            p.round(media.x, media.y, media.w, media.h, inner_r, paint::well(0.3));
            unsafe {
                let k = p.k;
                p.dc.PushAxisAlignedClip(&D2D_RECT_F { left: (media.x + 16.0) * k, top: (media.y + 12.0) * k, right: (media.x + media.w - 16.0) * k, bottom: (media.y + media.h - 10.0) * k }, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
                for (i, line) in lines.iter().enumerate() {
                    let y = media.y + 14.0 + i as f32 * 19.0;
                    if y > media.y + media.h {
                        break;
                    }
                    p.text(line, media.x + 16.0, y, 12.5, REGULAR, paint::ink(0.82), 100_000.0);
                }
                p.dc.PopAxisAlignedClip();
            }
            if lines.is_empty() {
                p.text_mid("This file is empty.", media.x + 16.0, media.y + 26.0, 13.0, REGULAR, paint::dim(), media.w);
            }
        }
        (Some(px), _, Look::Picture | Look::Video) if px.width > 96 => {
            p.round(media.x, media.y, media.w, media.h, inner_r, paint::well(0.3));
            picture(p, px, media, inner_r, false);
            if q.look == Look::Video {
                // a play mark, so a frame reads as a video
                let (cx, cy) = (media.x + media.w / 2.0, media.y + media.h / 2.0);
                p.round(cx - 28.0, cy - 28.0, 56.0, 56.0, 28.0, paint::color(0, 0, 0, 0.45));
                p.glyph(crate::icons::PLAY, cx + 2.0, cy, 20.0, paint::color(255, 255, 255, 0.95));
            }
        }
        (px, _, _) => {
            // the icon, large, with what it is under it
            p.round(media.x, media.y, media.w, media.h, inner_r, paint::well(0.18));
            let cy = media.y + media.h / 2.0 - 18.0;
            if let Some(px) = px {
                let s = 128.0_f32.min(media.h - 80.0);
                picture(p, px, Rect::new(media.x + (media.w - s) / 2.0, cy - s / 2.0, s, s), 0.0, false);
            } else if q.loading {
                p.text_mid("Loading preview\u{2026}", media.x + media.w / 2.0 - 60.0, cy, 13.0, REGULAR, paint::faint(), 200.0);
            }
            let words = kind_words(&q.name);
            let ww = p.measure(&words, 13.0, MEDIUM);
            p.text_mid(&words, media.x + (media.w - ww) / 2.0, cy + 86.0, 13.0, MEDIUM, paint::dim(), ww + 2.0);
        }
    }
    // the name and what it is, Open and Show in Folder
    let (open, reveal) = buttons(w, h);
    let fy = h - FOOT;
    p.text(&q.name, PAD + 6.0, fy + 16.0, 15.0, SEMIBOLD, paint::text(), reveal.x - PAD - 18.0);
    let mut meta = kind_words(&q.name);
    if let Some(s) = q.size {
        meta.push_str(&format!("  \u{b7}  {}", paint::bytes(s)));
    }
    if let Some(t) = q.modified {
        meta.push_str(&format!("  \u{b7}  Modified {}", crate::details::date_time(t, offset)));
    }
    p.text(&meta, PAD + 6.0, fy + 39.0, 12.5, REGULAR, paint::dim(), reveal.x - PAD - 18.0);
    crate::ui::button(p, reveal, "", Some(crate::icons::FOLDER), crate::ui::Kind::Secondary, hover == Some(QHit::Reveal), true);
    crate::ui::button(p, open, "Open", None, crate::ui::Kind::Primary, hover == Some(QHit::Open), true);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks() {
        assert_eq!(look_of("a.PNG"), Look::Picture);
        assert_eq!(look_of("clip.mkv"), Look::Video);
        assert_eq!(look_of("notes.md"), Look::Text);
        assert_eq!(look_of("setup.exe"), Look::Other);
        assert_eq!(kind_words("x.zip"), "ZIP archive");
    }

    #[test]
    fn text_excerpt() {
        let f = std::env::temp_dir().join(format!("stash-preview-{}.txt", std::process::id()));
        std::fs::write(&f, "\u{feff}one\n\ttwo\n").unwrap();
        assert_eq!(read_text(&f).unwrap(), vec!["one".to_string(), "    two".to_string()]);
        std::fs::write(&f, b"a\0b").unwrap();
        assert!(read_text(&f).is_none());
        let _ = std::fs::remove_file(&f);
    }
}
