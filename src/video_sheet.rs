//! The New Download sheet's video part: when the pasted link is a page with a video,
//! the page is read in the background (yt-dlp) and the sheet shows the video (picture,
//! title, length) with a quality picker, for a playlist "this video / the whole list",
//! and a clip range: drag the start and end handles to download only that part.
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
/// The clip row: a segmented-style well with the chosen part as its selection.
const CLIP_H: f32 = ROW_GAP;
const WELL_X: f32 = 104.0;
/// How far the grips sit inside the selection's edges, and how near one a press (or the
/// wheel) takes it.
const GRIP_IN: f32 = 8.0;
const GRAB: f32 = 14.0;
/// The narrowest the selection is drawn, so a short clip keeps both grips.
const MIN_SEL: f32 = 30.0;
/// The frame preview over a handle while it's dragged.
const PREVIEW_W: f32 = 176.0;
const PREVIEW_H: f32 = 99.0;

pub enum Phase {
    Idle,
    Looking,
    Found(Box<Found>),
    Failed(String),
}

pub struct Found {
    pub info: Info,
    pub picture: Option<Pixels>,
    /// The storyboard's sheets, as they arrive (after the card shows).
    pub frames: Vec<Option<Pixels>>,
}

pub struct State {
    /// The link read (or being read).
    pub link: String,
    pub phase: Phase,
    pub quality: Quality,
    pub whole_list: bool,
    /// Only this part of the video (start and end, in seconds); None: all of it.
    pub clip: Option<(f64, f64)>,
    /// The handle being dragged.
    drag: Option<Edge>,
    /// The grip the wheel last moved: its frame shows while the mouse stays on it.
    peek: Option<Edge>,
    /// Wheel movement not yet a whole notch (touchpads send it in small bits).
    wheel: i32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Edge {
    Start,
    End,
}

impl Default for State {
    fn default() -> Self {
        State { link: String::new(), phase: Phase::Idle, quality: Quality::Best, whole_list: false, clip: None, drag: None, peek: None, wheel: 0 }
    }
}

/// Bumps for every new link: an answer for an older one is dropped.
static GEN: AtomicU64 = AtomicU64::new(0);
static DONE: Mutex<Option<(u64, String, Result<Found, String>)>> = Mutex::new(None);
/// The storyboard sheets for a page read earlier (same generation).
static FRAMES: Mutex<Option<(u64, Vec<Option<Pixels>>)>> = Mutex::new(None);

/// The sheet's clickable parts in the video block.
#[derive(Clone, Debug, PartialEq)]
pub enum Hit {
    Quality(Quality),
    List(bool),
    ClipStart,
    ClipEnd,
    /// The track between or beside the handles: the nearer handle jumps there.
    ClipTrack,
    ClipReset,
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
                    Ok(Found { info, picture, frames: Vec::new() })
                }
                Err(e) => Err(e),
            };
            let board = result.as_ref().ok().and_then(|f| f.info.storyboard.clone());
            *DONE.lock().unwrap() = Some((generation, link, result));
            wake();
            // then the frames for the clip previews, while you look at the card
            if let Some(b) = board {
                let frames = futures_util::future::join_all(b.sheets.iter().enumerate().map(|(i, url)| sheet(url, i, b.width * b.cols))).await;
                if GEN.load(Ordering::Relaxed) == generation {
                    *FRAMES.lock().unwrap() = Some((generation, frames));
                    wake();
                }
            }
        });
    }

    /// Take the background answer, if it's for the current link. True if anything changed.
    pub fn take_answer(&mut self) -> bool {
        let current = GEN.load(Ordering::Relaxed);
        let mut changed = false;
        if let Some((generation, link, result)) = DONE.lock().unwrap().take() {
            if generation == current && link == self.link {
                self.take_result(result);
                changed = true;
            }
        }
        if let Some((generation, frames)) = FRAMES.lock().unwrap().take() {
            if let (true, Phase::Found(f)) = (generation == current, &mut self.phase) {
                f.frames = frames;
                changed = true;
            }
        }
        changed
    }

    fn take_result(&mut self, result: Result<Found, String>) {
        self.phase = match result {
            Ok(f) => {
                self.quality = f.info.default_quality();
                self.whole_list = f.info.only_playlist;
                self.clip = None;
                self.drag = None;
                self.peek = None;
                Phase::Found(Box::new(f))
            }
            // not a video page after all: an ordinary download
            Err(e) if e.starts_with("There's no video") => Phase::Idle,
            Err(e) => Phase::Failed(e),
        };
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
        CARD_H + 12.0 + ROW_GAP + if self.has_list() { ROW_GAP } else { 0.0 } + if self.duration().is_some() { CLIP_H } else { 0.0 } - 4.0
    }

    /// The video's length, when a part of it can be picked: one video (not the whole
    /// playlist, not a live stream) at least a few seconds long.
    fn duration(&self) -> Option<f64> {
        let f = self.found()?;
        if f.info.is_live || self.whole_list || f.info.only_playlist {
            return None;
        }
        f.info.duration.filter(|d| *d >= 3.0)
    }

    /// The part to download, if it's less than the whole video.
    fn active_clip(&self) -> Option<(f64, f64)> {
        self.duration().and(self.clip)
    }

    /// The clip row's centre line (where the track runs).
    fn clip_cy(&self) -> f32 {
        TOP + CARD_H + 12.0 + ROW_GAP * if self.has_list() { 2.0 } else { 1.0 } + crate::ui::SEG_H / 2.0
    }

    /// The well, on a sheet `w` wide.
    fn well(&self, w: f32) -> Rect {
        Rect::new(WELL_X, self.clip_cy() - crate::ui::SEG_H / 2.0, w - 24.0 - WELL_X, crate::ui::SEG_H)
    }

    /// Where times run, inside the well: the start of the video to its end.
    fn track(w: f32) -> (f32, f32) {
        (WELL_X + crate::ui::SEG_PAD, w - 24.0 - crate::ui::SEG_PAD)
    }

    /// The selection's edges as drawn (never narrower than `MIN_SEL`).
    fn sel_edges(&self, w: f32) -> (f32, f32) {
        let (x0, x1) = Self::track(w);
        let (a, b) = self.range();
        let (xa, xb) = (self.x_of(a, w), self.x_of(b, w));
        if xb - xa >= MIN_SEL {
            return (xa, xb);
        }
        let mid = ((xa + xb) / 2.0).clamp(x0 + MIN_SEL / 2.0, x1 - MIN_SEL / 2.0);
        (mid - MIN_SEL / 2.0, mid + MIN_SEL / 2.0)
    }

    /// The handles' times: the clip, or the whole video.
    fn range(&self) -> (f64, f64) {
        let d = self.duration().unwrap_or(0.0);
        self.active_clip().unwrap_or((0.0, d))
    }

    fn x_of(&self, t: f64, w: f32) -> f32 {
        let (x0, x1) = Self::track(w);
        let d = self.duration().unwrap_or(1.0).max(0.001);
        x0 + (x1 - x0) * (t / d).clamp(0.0, 1.0) as f32
    }

    /// The time under `x` on the track, to the second (a tenth of one for short videos).
    fn t_at(&self, x: f32, w: f32) -> f64 {
        let (x0, x1) = Self::track(w);
        let d = self.duration().unwrap_or(0.0);
        let t = ((x - x0) / (x1 - x0)).clamp(0.0, 1.0) as f64 * d;
        if d < 60.0 { (t * 10.0).round() / 10.0 } else { t.round() }.min(d)
    }

    /// Move a handle to `t`, keeping the clip at least a second long; a clip that's the
    /// whole video again is no clip.
    fn set_edge(&mut self, edge: Edge, t: f64) {
        let Some(d) = self.duration() else { return };
        let gap = 1.0_f64.min(d / 2.0);
        let (mut a, mut b) = self.range();
        match edge {
            Edge::Start => a = t.clamp(0.0, b - gap),
            Edge::End => b = t.clamp(a + gap, d),
        }
        self.clip = (a > 0.0 || b < d).then_some((a, b));
    }

    /// A press in the clip row at `x` (sheet coordinates): grab a handle, or bring the
    /// nearer one there. Then `drag_to` follows the mouse until `end_drag`.
    pub fn press(&mut self, h: &Hit, x: f32, w: f32) {
        self.peek = None;
        let (a, b) = self.range();
        let edge = match h {
            Hit::ClipStart => Edge::Start,
            Hit::ClipEnd => Edge::End,
            Hit::ClipTrack => {
                let t = self.t_at(x, w);
                let e = if (t - a).abs() <= (t - b).abs() { Edge::Start } else { Edge::End };
                self.set_edge(e, t);
                e
            }
            _ => return,
        };
        self.drag = Some(edge);
    }

    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// The mouse moved while a handle is held. If the clip changed: whether the handle
    /// went forward (later), for the tick.
    pub fn drag_to(&mut self, x: f32, w: f32) -> Option<bool> {
        let e = self.drag?;
        let before = self.edge_time(e);
        let t = self.t_at(x, w);
        self.set_edge(e, t);
        let after = self.edge_time(e);
        (after != before).then_some(after > before)
    }

    fn edge_time(&self, e: Edge) -> f64 {
        let (a, b) = self.range();
        if e == Edge::Start { a } else { b }
    }

    /// The wheel turned over a grip: step it a second a notch (10 with `big`, for Shift).
    /// If it moved: whether forward, for the tick.
    pub fn nudge(&mut self, h: &Hit, delta: i32, big: bool) -> Option<bool> {
        let e = match h {
            Hit::ClipStart => Edge::Start,
            Hit::ClipEnd => Edge::End,
            _ => return None,
        };
        if self.peek != Some(e) {
            self.wheel = 0;
        }
        self.peek = Some(e);
        self.wheel += delta;
        let notches = self.wheel / 120;
        self.wheel %= 120;
        if notches == 0 {
            return None;
        }
        let before = self.edge_time(e);
        // from a whole second, so nudges land on whole seconds
        let step = if big { 10.0 } else { 1.0 };
        self.set_edge(e, before.round() + notches as f64 * step);
        let after = self.edge_time(e);
        (after != before).then_some(after > before)
    }

    pub fn end_drag(&mut self) {
        self.drag = None;
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
        if self.active_clip().is_some() {
            return Some(if self.quality.audio() { "Download Audio Clip" } else { "Download Clip" }.into());
        }
        let what = match (self.quality.audio(), n == 1) {
            (true, true) => "Audio",
            (true, false) => "Audio Files",
            (false, true) => "Video",
            (false, false) => "Videos",
        };
        Some(if n > 1 { format!("Download {n} {what}") } else { format!("Download {what}") })
    }

    fn quality_seg(&self, measure: &dyn Fn(&str) -> f32) -> (Rect, Vec<Rect>, Vec<Quality>) {
        let qs = self.choices();
        let labels: Vec<&str> = qs.iter().map(|q| short(*q)).collect();
        let widths = crate::ui::seg_widths(&labels, measure);
        let (r, parts) = crate::ui::seg_layout(104.0, TOP + CARD_H + 12.0 + crate::ui::SEG_H / 2.0, &widths);
        (r, parts, qs)
    }

    fn list_seg(&self, measure: &dyn Fn(&str) -> f32) -> Option<(Rect, Vec<Rect>, [String; 2])> {
        let f = self.found()?;
        let p = f.info.playlist.as_ref().filter(|p| p.entries.len() > 1)?;
        let labels = ["This video".to_string(), format!("Whole playlist · {}", p.entries.len())];
        let widths = crate::ui::seg_widths(&[labels[0].as_str(), labels[1].as_str()], measure);
        let (r, parts) = crate::ui::seg_layout(104.0, TOP + CARD_H + 12.0 + ROW_GAP + crate::ui::SEG_H / 2.0, &widths);
        Some((r, parts, labels))
    }

    pub fn hit(&self, x: f32, y: f32, w: f32, measure: &dyn Fn(&str) -> f32) -> Option<Hit> {
        self.found()?;
        if self.duration().is_some() {
            let cy = self.clip_cy();
            if self.active_clip().is_some() && x >= 24.0 && x < 24.0 + measure("Reset") + 4.0 && (y - (cy + 9.0)).abs() <= 9.0 {
                return Some(Hit::ClipReset);
            }
            let well = self.well(w);
            if well.contains(x, y) {
                // take the nearer grip when the press is close to one
                let (xa, xb) = self.sel_edges(w);
                let (da, db) = ((x - (xa + GRIP_IN)).abs(), (x - (xb - GRIP_IN)).abs());
                if da <= GRAB || db <= GRAB {
                    return Some(if da < db || (da == db && x < xa) { Hit::ClipStart } else { Hit::ClipEnd });
                }
                return Some(Hit::ClipTrack);
            }
        }
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
            Hit::ClipReset => self.clip = None,
            // the clip handles are pressed and dragged (see `press`)
            Hit::ClipStart | Hit::ClipEnd | Hit::ClipTrack => {}
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
        if let Some(d) = info.duration.filter(|_| !info.is_live && !self.whole_list) {
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
        let ext = self.quality.ext().to_uppercase();
        let size = match (self.whole_list, self.active_clip(), info.size(self.quality)) {
            (true, ..) => format!("Each as {ext}, in a folder named after the playlist"),
            (false, Some((a, b)), Some(s)) => format!("{ext} · {} clip · about {}", video::duration_text(b - a), paint::bytes(clip_size(s, a, b, info.duration))),
            (false, Some((a, b)), None) => format!("{ext} · {} clip", video::duration_text(b - a)),
            (false, None, Some(s)) => format!("{ext} · about {}", paint::bytes(s)),
            (false, None, None) => ext,
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
        if self.duration().is_some() {
            self.paint_clip(p, w, hover);
        }
    }

    /// The clip row, drawn like the segmented controls: a well, with the part that
    /// downloads as its selection, a grip on each edge and the times inside. While a grip
    /// is dragged, that moment's frame shows above it.
    fn paint_clip(&self, p: &P, w: f32, hover: Option<&Hit>) {
        let cy = self.clip_cy();
        let clipped = self.active_clip().is_some();
        if clipped {
            p.text_mid("Clip", 24.0, cy - 9.0, 13.0, REGULAR, paint::dim(), 80.0);
            crate::ui::link(p, 24.0, cy + 9.0, "Reset", None, hover == Some(&Hit::ClipReset));
        } else {
            p.text_mid("Clip", 24.0, cy, 13.0, REGULAR, paint::dim(), 80.0);
        }
        let well = self.well(w);
        p.round(well.x, well.y, well.w, well.h, crate::ui::SEG_R, paint::well(0.25));
        let (xa, xb) = self.sel_edges(w);
        let (sy, sh) = (well.y + crate::ui::SEG_PAD, well.h - 2.0 * crate::ui::SEG_PAD);
        let lit = hover == Some(&Hit::ClipTrack) || self.drag.is_some();
        p.round(xa, sy, xb - xa, sh, crate::ui::SEG_IN_R, paint::ink(if lit { 0.16 } else { 0.13 }));
        // the grips: brighter when hovered or held
        for (edge, gx, hit) in [(Edge::Start, xa + GRIP_IN, Hit::ClipStart), (Edge::End, xb - GRIP_IN, Hit::ClipEnd)] {
            let hot = self.drag == Some(edge) || hover == Some(&hit);
            let c = if hot { paint::accent() } else { paint::ink(0.4) };
            p.capsule(gx - 1.5, cy - 7.0, 3.0, 14.0, c);
        }
        // the times: inside the selection when they fit, else beside it
        let (a, b) = self.range();
        let (ta, tb) = (video::duration_text(a), video::duration_text(b));
        let (wa, wb) = (p.measure(&ta, 12.5, MEDIUM), p.measure(&tb, 12.5, MEDIUM));
        let ink = |e: Edge| if self.drag == Some(e) { paint::text() } else { paint::dim() };
        if xb - xa >= wa + wb + 48.0 {
            p.text_mid(&ta, xa + 16.0, cy, 12.5, MEDIUM, ink(Edge::Start), wa + 2.0);
            p.text_mid(&tb, xb - 16.0 - wb, cy, 12.5, MEDIUM, ink(Edge::End), wb + 2.0);
        } else {
            if xa - 10.0 - wa >= well.x + 8.0 {
                p.text_mid(&ta, xa - 10.0 - wa, cy, 12.5, MEDIUM, ink(Edge::Start), wa + 2.0);
            }
            if xb + 10.0 + wb <= well.x + well.w - 8.0 {
                p.text_mid(&tb, xb + 10.0, cy, 12.5, MEDIUM, ink(Edge::End), wb + 2.0);
            }
        }
        // the frame under the grip being dragged, or the one the wheel is turning (while
        // the mouse stays on it)
        let on_grip = |e: Edge| hover == Some(if e == Edge::Start { &Hit::ClipStart } else { &Hit::ClipEnd });
        if let Some(e) = self.drag.or(self.peek.filter(|e| on_grip(*e))) {
            let (t, x) = if e == Edge::Start { (a, xa) } else { (b, xb) };
            self.paint_preview(p, w, x, well.y, t);
        }
    }

    /// The frame at `t`, in a card above the well at `x` (when the storyboard has come).
    fn paint_preview(&self, p: &P, w: f32, x: f32, well_top: f32, t: f64) {
        let Some(px) = self.frame_at(t) else { return };
        let (fx, fy) = ((x - PREVIEW_W / 2.0).clamp(24.0, w - 24.0 - PREVIEW_W), well_top - 10.0 - PREVIEW_H);
        p.round(fx - 4.0, fy - 4.0, PREVIEW_W + 8.0, PREVIEW_H + 8.0, 16.0, paint::raised(0.98));
        draw_cover(p, &px, fx, fy, PREVIEW_W, PREVIEW_H, 12.0);
        let label = video::duration_text(t);
        let lw = p.measure(&label, 12.0, MEDIUM) + 12.0;
        let (bx, by) = (fx + (PREVIEW_W - lw) / 2.0, fy + PREVIEW_H - 24.0);
        p.round(bx, by, lw, 18.0, 6.0, paint::color(0, 0, 0, 0.72));
        p.text_mid(&label, bx + 6.0, by + 9.0, 12.0, MEDIUM, paint::color(255, 255, 255, 1.0), lw);
    }

    /// The storyboard's frame for the moment `t`, cut out of its sheet.
    fn frame_at(&self, t: f64) -> Option<Pixels> {
        let f = self.found()?;
        let b = f.info.storyboard.as_ref()?;
        let (sheet, col, row) = b.cell(t)?;
        let px = f.frames.get(sheet)?.as_ref()?;
        // sheets aren't always exactly cols × rows cells (447 tall for 5 rows of 90):
        // cut on fractional edges so lower rows don't drift
        let (cw, ch) = (px.width as f32 / b.cols as f32, px.height as f32 / b.rows as f32);
        let (x, y) = ((col as f32 * cw).round() as u32, (row as f32 * ch).round() as u32);
        let (x2, y2) = ((((col + 1) as f32) * cw).round() as u32, (((row + 1) as f32) * ch).round() as u32);
        crop(px, x, y, x2.min(px.width) - x, y2.min(px.height) - y)
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
                        video: Some(Spec { quality: q, title: e.title.clone(), expected: None, thumbnail: None, clip: None }),
                        ..Default::default()
                    })
                    .collect(),
            );
        }
        let url = if info.url.is_empty() { self.link.clone() } else { info.url.clone() };
        let clip = self.active_clip();
        let expected = match clip {
            Some((a, b)) => info.size(q).map(|s| clip_size(s, a, b, info.duration)),
            None => info.size(q),
        };
        Some(vec![AddRequest {
            source: url,
            dir: dir.to_path_buf(),
            priority,
            video: Some(Spec { quality: q, title: info.title.clone(), expected, thumbnail: info.thumbnail.clone(), clip }),
            ..Default::default()
        }])
    }
}

/// One storyboard sheet (the `i`th), decoded at its own size (`w` wide).
async fn sheet(url: &str, i: usize, w: u32) -> Option<Pixels> {
    let bytes = reqwest::get(url).await.ok()?.bytes().await.ok()?;
    let path = std::env::temp_dir().join(format!("stash-board-{}-{}-{i}.jpg", std::process::id(), GEN.load(Ordering::Relaxed)));
    std::fs::write(&path, &bytes).ok()?;
    tokio::task::spawn_blocking(move || {
        crate::shell::init_com_thread();
        let px = crate::shell::thumbnail(&path, w.max(1) as i32);
        let _ = std::fs::remove_file(&path);
        px
    })
    .await
    .ok()
    .flatten()
}

/// A rectangle out of a picture.
fn crop(px: &Pixels, x: u32, y: u32, w: u32, h: u32) -> Option<Pixels> {
    if w == 0 || h == 0 || x + w > px.width || y + h > px.height {
        return None;
    }
    let mut bgra = Vec::with_capacity((w * h * 4) as usize);
    for r in y..y + h {
        let start = ((r * px.width + x) * 4) as usize;
        bgra.extend_from_slice(px.bgra.get(start..start + (w * 4) as usize)?);
    }
    Some(Pixels { width: w, height: h, bgra })
}

/// About how big a clip of a video `size` bytes long will be.
fn clip_size(size: u64, a: f64, b: f64, duration: Option<f64>) -> u64 {
    match duration.filter(|d| *d > 0.0) {
        Some(d) => (size as f64 * ((b - a) / d).clamp(0.0, 1.0)) as u64,
        None => size,
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
