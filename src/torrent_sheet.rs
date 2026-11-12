//! The Choose Files sheet: which of a torrent's files to download. It comes up by
//! itself when a torrent's metadata arrives (Settings > Torrents > Choose files), and
//! from a torrent's right-click menu to change the choice later. The downloads window
//! hosts it like its other sheets; the layout, painting and hits live here.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use crate::dl::{self, FileEntry};
use crate::icons;
use crate::paint::{self, MEDIUM, P, REGULAR, SEMIBOLD};
use crate::settings_page::Rect;

pub const W: f32 = 540.0;
const ROW_H: f32 = 34.0;
const VISIBLE: usize = 9;
const LIST_TOP: f32 = 98.0;
const PAD: f32 = 22.0;

/// What a point on the sheet is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FHit {
    /// A file's row (its position in the list).
    Row(usize),
    All,
    Nothing,
    Ok,
    Cancel,
}

pub struct FilesSheet {
    pub id: u64,
    name: String,
    files: Arc<Vec<FileEntry>>,
    chosen: Vec<bool>,
    /// The first row in view.
    scroll: usize,
    /// The question a new torrent asks (not a change to a running one).
    first: bool,
}

impl FilesSheet {
    /// The sheet for a torrent download, if its files are known.
    pub fn new(id: u64, first: bool) -> Option<Self> {
        let list = dl::torrent_files(id)?;
        let chosen = list.files.iter().map(|f| list.wanted(f.index)).collect();
        Some(FilesSheet { id, name: list.name.clone(), files: list.files.clone(), chosen, scroll: 0, first })
    }

    pub fn height(&self) -> f32 {
        LIST_TOP + self.rows_shown() as f32 * ROW_H + 18.0 + crate::ui::BUTTON_H + PAD
    }

    fn rows_shown(&self) -> usize {
        self.files.len().clamp(1, VISIBLE)
    }

    fn buttons(&self) -> (Rect, Rect) {
        let bh = crate::ui::BUTTON_H;
        let y = self.height() - PAD - bh;
        let ok = Rect::new(W - PAD - 132.0, y, 132.0, bh);
        (Rect::new(ok.x - 10.0 - 92.0, y, 92.0, bh), ok)
    }

    /// "Select All" and "Select None", right-aligned above the list.
    fn links(&self) -> (Rect, Rect) {
        let none = Rect::new(W - PAD - 84.0, 62.0, 84.0, 24.0);
        (Rect::new(none.x - 8.0 - 72.0, 62.0, 72.0, 24.0), none)
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<FHit> {
        let (cancel, ok) = self.buttons();
        let (all, nothing) = self.links();
        if ok.contains(x, y) {
            return Some(FHit::Ok);
        }
        if cancel.contains(x, y) {
            return Some(FHit::Cancel);
        }
        if all.contains(x, y) {
            return Some(FHit::All);
        }
        if nothing.contains(x, y) {
            return Some(FHit::Nothing);
        }
        let list = Rect::new(PAD - 6.0, LIST_TOP, W - 2.0 * PAD + 12.0, self.rows_shown() as f32 * ROW_H);
        if list.contains(x, y) {
            let i = self.scroll + ((y - LIST_TOP) / ROW_H) as usize;
            return (i < self.files.len()).then_some(FHit::Row(i));
        }
        None
    }

    /// A click on the sheet; true if it changed.
    pub fn click(&mut self, h: FHit) -> bool {
        match h {
            FHit::Row(i) => {
                if let Some(c) = self.chosen.get_mut(i) {
                    *c = !*c;
                }
            }
            FHit::All => self.chosen.iter_mut().for_each(|c| *c = true),
            FHit::Nothing => self.chosen.iter_mut().for_each(|c| *c = false),
            FHit::Ok | FHit::Cancel => return false,
        }
        true
    }

    /// Scroll by `rows` (negative: up); true if it moved.
    pub fn scroll_by(&mut self, rows: i32) -> bool {
        let max = self.files.len().saturating_sub(VISIBLE) as i32;
        let to = (self.scroll as i32 + rows).clamp(0, max) as usize;
        let moved = to != self.scroll;
        self.scroll = to;
        moved
    }

    pub fn can_submit(&self) -> bool {
        self.chosen.iter().any(|c| *c)
    }

    /// The chosen files' indices in the torrent.
    pub fn chosen(&self) -> Vec<usize> {
        self.files.iter().zip(&self.chosen).filter(|(_, c)| **c).map(|(f, _)| f.index).collect()
    }

    pub fn paint(&self, p: &P, hover: Option<FHit>) {
        let h = self.height();
        p.popover(0.0, 0.0, W, h, 26.0);
        let title = if self.first { "Choose files to download" } else { "Files to download" };
        p.text(title, PAD, 20.0, 16.5, SEMIBOLD, paint::text(), W - 2.0 * PAD);
        let n = self.chosen.iter().filter(|c| **c).count();
        let size: u64 = self.files.iter().zip(&self.chosen).filter(|(_, c)| **c).map(|(f, _)| f.size).sum();
        let total: u64 = self.files.iter().map(|f| f.size).sum();
        p.text(&self.name, PAD, 46.0, 12.5, REGULAR, paint::dim(), W - 2.0 * PAD);
        p.text_mid(
            &format!("{n} of {} files \u{00b7} {} of {}", self.files.len(), paint::bytes(size), paint::bytes(total)),
            PAD,
            74.0,
            12.5,
            MEDIUM,
            paint::text(),
            W - 2.0 * PAD - 170.0,
        );
        let (all, nothing) = self.links();
        crate::ui::link(p, all.x, all.y + all.h / 2.0, "Select All", None, hover == Some(FHit::All));
        crate::ui::link(p, nothing.x, nothing.y + nothing.h / 2.0, "Select None", None, hover == Some(FHit::Nothing));

        // the list, in a well
        let shown = self.rows_shown();
        let list_h = shown as f32 * ROW_H;
        p.round(PAD - 6.0, LIST_TOP - 2.0, W - 2.0 * PAD + 12.0, list_h + 4.0, 14.0, paint::ink(0.04));
        for (row, i) in (self.scroll..self.files.len()).take(shown).enumerate() {
            let f = &self.files[i];
            let y = LIST_TOP + row as f32 * ROW_H;
            let cy = y + ROW_H / 2.0;
            if hover == Some(FHit::Row(i)) {
                p.round(PAD - 2.0, y + 2.0, W - 2.0 * PAD + 4.0, ROW_H - 4.0, 10.0, paint::ink(0.06));
            }
            // the check box: an orange tick when chosen, an empty rounded square when not
            let on = self.chosen[i];
            if on {
                p.round(PAD + 6.0, cy - 9.0, 18.0, 18.0, 5.0, paint::accent());
                p.glyph(TICK, PAD + 15.0, cy, 14.0, paint::color(255, 255, 255, 1.0));
            } else {
                p.round_stroke(PAD + 6.0, cy - 9.0, 18.0, 18.0, 5.0, paint::ink(0.35), 1.3);
            }
            let ink = if on { paint::text() } else { paint::dim() };
            let size = paint::bytes(f.size);
            let size_w = p.measure(&size, 12.5, REGULAR);
            p.text_mid(&f.name, PAD + 36.0, cy, 13.0, REGULAR, ink, W - 2.0 * PAD - 36.0 - size_w - 16.0);
            p.text_right(&size, W - PAD - 6.0, cy, 12.5, REGULAR, paint::dim());
        }
        // more above or below: a quiet hint at the list's edge
        if self.files.len() > VISIBLE {
            let more = format!("{}\u{2013}{} of {} \u{00b7} scroll for more", self.scroll + 1, (self.scroll + VISIBLE).min(self.files.len()), self.files.len());
            p.text_mid(&more, PAD, LIST_TOP + list_h + 11.0, 11.0, REGULAR, paint::faint(), 260.0);
        }
        let (cancel, ok) = self.buttons();
        let dismiss = if self.first { "Later" } else { "Cancel" };
        crate::ui::button(p, cancel, dismiss, None, crate::ui::Kind::Secondary, hover == Some(FHit::Cancel), true);
        crate::ui::button(p, ok, "Download", Some(icons::DOWNLOAD), crate::ui::Kind::Primary, hover == Some(FHit::Ok), self.can_submit());
    }
}

/// A plain tick for the check boxes.
const TICK: &str = "<path fill=\"none\" stroke=\"currentColor\" stroke-linecap=\"round\" stroke-linejoin=\"round\" stroke-width=\"2.4\" d=\"m6 12.5l4 4l8-9\"/>";

/// Torrents already asked about this run: each asks once by itself (Later leaves it
/// waiting; its menu's Choose Files… asks again).
static ASKED: Mutex<Option<HashSet<u64>>> = Mutex::new(None);

/// Torrents in the snapshot waiting for a choice that haven't asked yet. Marks them asked.
pub fn take_new_questions(jobs: &[dl::JobView]) -> Vec<u64> {
    let mut asked = ASKED.lock().unwrap();
    let asked = asked.get_or_insert_with(HashSet::new);
    jobs.iter().filter(|j| j.choose_files && asked.insert(j.id)).map(|j| j.id).collect()
}

/// Ask again later (the window couldn't show it now).
pub fn unask(id: u64) {
    if let Some(a) = ASKED.lock().unwrap().as_mut() {
        a.remove(&id);
    }
}
