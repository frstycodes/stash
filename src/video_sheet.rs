//! The New Download sheet's video part: when the pasted link is a page with a video,
//! the page is read in the background (yt-dlp) and the sheet shows the video (picture,
//! title, length) with a quality picker, and for a playlist "this video / the whole list".
//!
//! The sheet (manager.rs) keeps one `State`, tells it when the link changes, paints it
//! into the space `extra_h` asks for, and asks it for the downloads to add.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use windows::Win32::Graphics::Direct2D::{D2D1_BITMAP_BRUSH_PROPERTIES1, D2D1_EXTEND_MODE_CLAMP, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC};
use windows_numerics::Matrix3x2;

use crate::dl::video::{self, Info, Quality, Spec};
use crate::dl::{AddRequest, Priority};
use crate::gfx::{Gfx, Pixels};
use crate::paint::{self, MEDIUM, P, REGULAR};
use crate::settings_page::Rect;

/// Where the video block starts (under the link field and its message line).
pub const TOP: f32 = 128.0;
const CARD_H: f32 = 84.0;
const ROW_GAP: f32 = 46.0;
const THUMB_W: f32 = 112.0;
const THUMB_H: f32 = 63.0;

pub enum Phase {
    Idle,
    Looking,
    Found(Box<Found>),
    Failed(String),
}

pub struct Found {
    pub info: Info,
    pub picture: Option<Pixels>,
}

pub struct State {
    /// The link read (or being read).
    pub link: String,
    pub phase: Phase,
    pub quality: Quality,
    pub whole_list: bool,
}

impl Default for State {
    fn default() -> Self {
        State { link: String::new(), phase: Phase::Idle, quality: Quality::Best, whole_list: false }
    }
}

/// Bumps for every new link: an answer for an older one is dropped.
static GEN: AtomicU64 = AtomicU64::new(0);
static DONE: Mutex<Option<(u64, String, Result<Found, String>)>> = Mutex::new(None);

/// The sheet's clickable parts in the video block.
#[derive(Clone, Debug, PartialEq)]
pub enum Hit {
    Quality(Quality),
    List(bool),
}

impl State {
    /// The field changed: read the page if it's one link to a page that may have a video.
    /// `wake` runs (on another thread) when the answer is in.
    pub fn link_changed(&mut self, text: &str, wake: impl Fn() + Send + 'static) {
        let words: Vec<&str> = text.split_whitespace().collect();
        let link = match words.as_slice() {
            [one] if video::worth_probing(one) => one.to_string(),
            _ => {
                if !self.link.is_empty() {
                    GEN.fetch_add(1, Ordering::Relaxed);
                }
                self.link.clear();
                self.phase = Phase::Idle;
                return;
            }
        };
        if link == self.link {
            return;
        }
        self.link = link.clone();
        self.phase = Phase::Looking;
        let generation = GEN.fetch_add(1, Ordering::Relaxed) + 1;
        crate::tools::runtime().spawn(async move {
            // wait for the typing (or a second paste) to settle
            tokio::time::sleep(Duration::from_millis(350)).await;
            if GEN.load(Ordering::Relaxed) != generation {
                return;
            }
            let result = match video::probe(&link).await {
                Ok(info) => {
                    // picture and sound get joined by ffmpeg: fetch it now, while you choose
                    if !crate::tools::has_ffmpeg() && !info.is_live {
                        crate::tools::runtime().spawn(async { crate::tools::ffmpeg().await });
                    }
                    let picture = match &info.thumbnail {
                        Some(url) => picture(url).await,
                        None => None,
                    };
                    Ok(Found { info, picture })
                }
                Err(e) => Err(e),
            };
            *DONE.lock().unwrap() = Some((generation, link, result));
            wake();
        });
    }

    /// Take the background answer, if it's for the current link. True if anything changed.
    pub fn take_answer(&mut self) -> bool {
        let Some((generation, link, result)) = DONE.lock().unwrap().take() else { return false };
        if generation != GEN.load(Ordering::Relaxed) || link != self.link {
            return false;
        }
        self.phase = match result {
            Ok(f) => {
                self.quality = f.info.default_quality();
                self.whole_list = f.info.only_playlist;
                Phase::Found(Box::new(f))
            }
            // not a video page after all: an ordinary download
            Err(e) if e.starts_with("There's no video") => Phase::Idle,
            Err(e) => Phase::Failed(e),
        };
        true
    }

    pub fn reset(&mut self) {
        GEN.fetch_add(1, Ordering::Relaxed);
        *self = State::default();
    }

    pub fn looking(&self) -> bool {
        matches!(self.phase, Phase::Looking)
    }

    pub fn found(&self) -> Option<&Found> {
        match &self.phase {
            Phase::Found(f) => Some(f),
            _ => None,
        }
    }

    /// The qualities offered for this video.
    pub fn choices(&self) -> Vec<Quality> {
        let Some(f) = self.found() else { return Vec::new() };
        Quality::ALL.into_iter().filter(|q| f.info.only_playlist || f.info.offers(*q)).collect()
    }

    fn has_list(&self) -> bool {
        self.found().is_some_and(|f| f.info.playlist.as_ref().is_some_and(|p| p.entries.len() > 1))
    }

    /// The extra height the sheet makes room for (0 when there's no video).
    pub fn extra_h(&self) -> f32 {
        if self.found().is_none() {
            return 0.0;
        }
        CARD_H + 12.0 + ROW_GAP + if self.has_list() { ROW_GAP } else { 0.0 } - 4.0
    }

    /// One line for the message slot under the field while there's no card.
    pub fn note(&self) -> Option<(String, bool)> {
        match &self.phase {
            Phase::Looking => {
                let s = crate::tools::status();
                Some((
                    match s.fetching {
                        Some(f) => match f.total {
                            Some(t) => format!("Getting video tools… {} of {}", paint::bytes(f.done), paint::bytes(t)),
                            None => format!("Getting video tools… {}", paint::bytes(f.done)),
                        },
                        None => "Looking for a video on this page…".into(),
                    },
                    false,
                ))
            }
            Phase::Failed(e) => Some((e.clone(), true)),
            _ => None,
        }
    }

    /// The Download button's label when a video was found.
    pub fn ok_label(&self) -> Option<String> {
        let f = self.found()?;
        let n = if self.whole_list { f.info.playlist.as_ref().map(|p| p.entries.len()).unwrap_or(1) } else { 1 };
        let what = if self.quality.audio() { "Audio" } else if n == 1 { "Video" } else { "Videos" };
        Some(if n > 1 { format!("Download {n} {what}") } else { format!("Download {what}") })
    }

    fn quality_seg(&self, measure: &dyn Fn(&str) -> f32) -> (Rect, Vec<Rect>, Vec<Quality>) {
        let qs = self.choices();
        let labels: Vec<&str> = qs.iter().map(|q| short(*q)).collect();
        let widths = crate::ui::seg_widths(&labels, |t| measure(t) + 14.0);
        let (r, parts) = crate::ui::seg_layout(104.0, TOP + CARD_H + 12.0 + crate::ui::SEG_H / 2.0, &widths);
        (r, parts, qs)
    }

    fn list_seg(&self, measure: &dyn Fn(&str) -> f32) -> Option<(Rect, Vec<Rect>, [String; 2])> {
        let f = self.found()?;
        let p = f.info.playlist.as_ref().filter(|p| p.entries.len() > 1)?;
        let labels = ["This video".to_string(), format!("Whole playlist · {}", p.entries.len())];
        let widths = crate::ui::seg_widths(&[labels[0].as_str(), labels[1].as_str()], |t| measure(t) + 14.0);
        let (r, parts) = crate::ui::seg_layout(104.0, TOP + CARD_H + 12.0 + ROW_GAP + crate::ui::SEG_H / 2.0, &widths);
        Some((r, parts, labels))
    }

    pub fn hit(&self, x: f32, y: f32, measure: &dyn Fn(&str) -> f32) -> Option<Hit> {
        self.found()?;
        let (_, parts, qs) = self.quality_seg(measure);
        if let Some(i) = parts.iter().position(|r| r.contains(x, y)) {
            return Some(Hit::Quality(qs[i]));
        }
        if let Some((_, parts, _)) = self.list_seg(measure) {
            if let Some(i) = parts.iter().position(|r| r.contains(x, y)) {
                if !self.found().is_some_and(|f| f.info.only_playlist) {
                    return Some(Hit::List(i == 1));
                }
            }
        }
        None
    }

    pub fn click(&mut self, h: &Hit) {
        match h {
            Hit::Quality(q) => self.quality = *q,
            Hit::List(on) => self.whole_list = *on,
        }
    }

    /// Paint the block (card, quality, playlist) on a sheet `w` wide.
    pub fn paint(&self, p: &P, w: f32, hover: Option<&Hit>) {
        let Some(f) = self.found() else { return };
        let info = &f.info;
        let (x, y) = (24.0, TOP);
        p.round(x, y, w - 48.0, CARD_H, 18.0, paint::ink(0.05));
        // the picture, cropped to 16:9 on a squircle, with its length on it
        let (tx, ty) = (x + 10.0, y + (CARD_H - THUMB_H) / 2.0);
        p.round(tx, ty, THUMB_W, THUMB_H, 10.0, paint::ink(0.08));
        if let Some(px) = &f.picture {
            draw_cover(p, px, tx, ty, THUMB_W, THUMB_H, 10.0);
        } else {
            p.glyph(crate::icons::PLAY, tx + THUMB_W / 2.0, ty + THUMB_H / 2.0, 22.0, paint::dim());
        }
        if let Some(d) = info.duration.filter(|_| !info.is_live) {
            let t = video::duration_text(d);
            let tw = p.measure(&t, 11.0, MEDIUM) + 10.0;
            let (bx, by) = (tx + THUMB_W - tw - 5.0, ty + THUMB_H - 20.0);
            p.round(bx, by, tw, 15.0, 5.0, paint::color(0, 0, 0, 0.72));
            p.text_mid(&t, bx + 5.0, by + 7.5, 11.0, MEDIUM, paint::color(255, 255, 255, 1.0), tw);
        }
        let mx = tx + THUMB_W + 14.0;
        let mw = w - 24.0 - 14.0 - mx;
        let list = info.playlist.as_ref();
        let title = if self.whole_list { list.map(|l| l.title.as_str()).unwrap_or(&info.title) } else { info.title.as_str() };
        p.text(title, mx, y + 14.0, 13.5, MEDIUM, paint::text(), mw);
        let mut meta: Vec<String> = Vec::new();
        if let Some(u) = &info.uploader {
            meta.push(u.clone());
        }
        if info.is_live {
            meta.push("Live now".into());
        } else if self.whole_list {
            if let Some(l) = list {
                meta.push(format!("{} videos", l.entries.len()));
            }
        } else if let Some(d) = info.duration {
            meta.push(video::duration_text(d));
        }
        p.text(&meta.join(" · "), mx, y + 34.0, 12.0, REGULAR, paint::dim(), mw);
        let size = match (self.whole_list, info.size(self.quality)) {
            (true, _) => format!("{} each, into a folder of its own", self.quality.ext().to_uppercase()),
            (false, Some(s)) => format!("{} · about {}", self.quality.ext().to_uppercase(), paint::bytes(s)),
            (false, None) => self.quality.ext().to_uppercase(),
        };
        p.text(&size, mx, y + 53.0, 12.0, REGULAR, paint::dim(), mw);

        let measure = |t: &str| p.measure(t, 13.0, MEDIUM);
        let (rect, parts, qs) = self.quality_seg(&measure);
        p.text_mid("Quality", 24.0, rect.y + rect.h / 2.0, 13.0, REGULAR, paint::dim(), 80.0);
        let labels: Vec<&str> = qs.iter().map(|q| short(*q)).collect();
        let sel = qs.iter().position(|q| *q == self.quality);
        let hot = qs.iter().position(|q| hover == Some(&Hit::Quality(*q)));
        crate::ui::paint_seg(p, rect, &parts, &labels, sel, hot, true);
        if let Some((rect, parts, labels)) = self.list_seg(&measure) {
            p.text_mid("Playlist", 24.0, rect.y + rect.h / 2.0, 13.0, REGULAR, paint::dim(), 80.0);
            let sel = Some(usize::from(self.whole_list));
            let hot = [false, true].iter().position(|b| hover == Some(&Hit::List(*b)));
            crate::ui::paint_seg(p, rect, &parts, &[labels[0].as_str(), labels[1].as_str()], sel, hot, true);
        }
    }

    /// The downloads to add for the sheet's link, if it's a video.
    pub fn requests(&self, text: &str, dir: &Path, priority: Priority) -> Option<Vec<AddRequest>> {
        let f = self.found()?;
        if text.split_whitespace().collect::<Vec<_>>() != [self.link.as_str()] {
            return None;
        }
        let info = &f.info;
        let q = self.quality;
        if self.whole_list {
            let list = info.playlist.as_ref()?;
            // a playlist goes into a folder named after it
            let folder: PathBuf = dir.join(crate::dl::safe_name(&list.title).unwrap_or_else(|| "Playlist".into()));
            return Some(
                list.entries
                    .iter()
                    .map(|e| AddRequest {
                        source: e.url.clone(),
                        dir: folder.clone(),
                        priority,
                        video: Some(Spec { quality: q, title: e.title.clone(), expected: None, thumbnail: None }),
                        ..Default::default()
                    })
                    .collect(),
            );
        }
        let url = if info.url.is_empty() { self.link.clone() } else { info.url.clone() };
        Some(vec![AddRequest {
            source: url,
            dir: dir.to_path_buf(),
            priority,
            video: Some(Spec { quality: q, title: info.title.clone(), expected: info.size(q), thumbnail: info.thumbnail.clone() }),
            ..Default::default()
        }])
    }
}

fn short(q: Quality) -> &'static str {
    match q {
        Quality::AudioM4a => "M4A",
        Quality::AudioMp3 => "MP3",
        q => q.label(),
    }
}

/// The picture, read by the shell (JPEG, WebP), for the sheet.
async fn picture(url: &str) -> Option<Pixels> {
    let path = video::sheet_picture(url).await?;
    tokio::task::spawn_blocking(move || {
        crate::shell::init_com_thread();
        let px = crate::shell::thumbnail(&path, 256);
        let _ = std::fs::remove_file(&path);
        px
    })
    .await
    .ok()
    .flatten()
}

/// Fill a squircle with a picture, cropped to cover it.
fn draw_cover(p: &P, px: &Pixels, x: f32, y: f32, w: f32, h: f32, r: f32) {
    let k = p.k;
    let Ok(bmp) = Gfx::bitmap(p.dc, px) else { return };
    let Some(geom) = p.squircle(x, y, w, h, r) else { return };
    let (pw, ph) = (px.width as f32, px.height as f32);
    if pw < 1.0 || ph < 1.0 {
        return;
    }
    let s = (w * k / pw).max(h * k / ph);
    let (ox, oy) = (x * k + (w * k - pw * s) / 2.0, y * k + (h * k - ph * s) / 2.0);
    unsafe {
        let props = D2D1_BITMAP_BRUSH_PROPERTIES1 { extendModeX: D2D1_EXTEND_MODE_CLAMP, extendModeY: D2D1_EXTEND_MODE_CLAMP, interpolationMode: D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC };
        let Ok(brush) = p.dc.CreateBitmapBrush(&bmp, Some(&props), None) else { return };
        brush.SetTransform(&Matrix3x2 { M11: s, M12: 0.0, M21: 0.0, M22: s, M31: ox, M32: oy });
        p.dc.FillGeometry(&geom, &brush, None);
    }
    // a hairline inside the edge, so a dark picture keeps its shape
    p.round_stroke(x, y, w, h, r, paint::ink(0.08), 1.0);
}
