//! Stash's own right-click menu and tooltips for the downloads window, drawn like the
//! rest of the app instead of Windows' menus: a squircle card on the app's surface,
//! text items with their shortcut, and the priority as a small segmented control.

use windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT;

use crate::dl::Priority;
use crate::paint::{self, MEDIUM, P, REGULAR};

pub const PAD: f32 = 6.0;
const ITEM_H: f32 = 30.0;
const SEP_H: f32 = 9.0;
const PRIO_H: f32 = 38.0;
pub const RADIUS: f32 = 14.0;
const ITEM_R: f32 = RADIUS - PAD; // concentric with the card
const SEG_W: f32 = 58.0; // each priority option
const ICON_W: f32 = 26.0; // the icon column before each label
const TEXT_X: f32 = PAD + 12.0 + ICON_W;

pub enum Item {
    /// `id` is the command it runs; `keys` the shortcut shown on the right.
    Action { id: String, label: String, icon: Option<&'static str>, keys: Option<&'static str>, enabled: bool, danger: bool },
    Sep,
    /// The priority of the downloads it's for (None: they differ).
    Priority(Option<Priority>),
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum MHit {
    Item(usize),
    Prio(Priority),
    Card,
}

pub struct Menu {
    pub items: Vec<Item>,
    pub w: f32,
    pub h: f32,
}

const PRIOS: [(Priority, &str); 3] = [(Priority::Low, "Low"), (Priority::Normal, "Normal"), (Priority::High, "High")];

impl Menu {
    /// `measure(text, size, weight)` gives a text's width, to size the card.
    pub fn new(items: Vec<Item>, measure: impl Fn(&str, f32, DWRITE_FONT_WEIGHT) -> f32) -> Self {
        let mut w: f32 = 3.0 * SEG_W + 2.0 * crate::ui::SEG_PAD + 70.0 + 2.0 * PAD + ICON_W;
        let mut h = 2.0 * PAD;
        for it in &items {
            h += match it {
                Item::Action { label, keys, .. } => {
                    let kw = keys.map_or(0.0, |k| measure(k, 12.0, REGULAR) + 28.0);
                    w = w.max(measure(label, 13.0, REGULAR) + kw + 2.0 * PAD + 30.0 + ICON_W);
                    ITEM_H
                }
                Item::Sep => SEP_H,
                Item::Priority(_) => PRIO_H,
            };
        }
        Menu { items, w: w.max(210.0).ceil(), h }
    }

    fn rows(&self) -> impl Iterator<Item = (usize, f32, f32)> + '_ {
        let mut y = PAD;
        self.items.iter().enumerate().map(move |(i, it)| {
            let h = match it {
                Item::Action { .. } => ITEM_H,
                Item::Sep => SEP_H,
                Item::Priority(_) => PRIO_H,
            };
            let top = y;
            y += h;
            (i, top, h)
        })
    }

    /// The priority control (the app's segmented control), its right edge on the shortcuts' column.
    fn prio_seg(&self, cy: f32) -> (crate::settings_page::Rect, Vec<crate::settings_page::Rect>) {
        let widths = [SEG_W; 3];
        let total = 3.0 * SEG_W + 2.0 * crate::ui::SEG_PAD + 4.0;
        crate::ui::seg_layout(self.w - PAD - 10.0 - total, cy, &widths)
    }


    pub fn hit(&self, x: f32, y: f32) -> Option<MHit> {
        if !(0.0..self.w).contains(&x) || !(0.0..self.h).contains(&y) {
            return None;
        }
        for (i, top, h) in self.rows() {
            if y < top || y >= top + h {
                continue;
            }
            return Some(match &self.items[i] {
                Item::Action { enabled: true, .. } => MHit::Item(i),
                Item::Priority(_) => {
                    let cy = top + h / 2.0;
                    let (_, parts) = self.prio_seg(cy);
                    parts.iter().zip(PRIOS).find(|(r, _)| r.contains(x, y)).map_or(MHit::Card, |(_, (p, _))| MHit::Prio(p))
                }
                _ => MHit::Card,
            });
        }
        Some(MHit::Card)
    }

    /// The enabled items, for moving through them with the arrow keys.
    pub fn actionable(&self) -> Vec<usize> {
        self.items.iter().enumerate().filter(|(_, it)| matches!(it, Item::Action { enabled: true, .. })).map(|(i, _)| i).collect()
    }

    pub fn paint(&self, p: &P, hover: Option<MHit>) {
        let (w, h) = (self.w, self.h);
        p.popover(0.0, 0.0, w, h, RADIUS);
        for (i, top, rh) in self.rows() {
            match &self.items[i] {
                Item::Action { label, icon, keys, enabled, danger, .. } => {
                    let hot = hover == Some(MHit::Item(i));
                    if hot {
                        let fill = if *danger { paint::color(229, 72, 77, 0.9) } else { paint::ink(0.09) };
                        p.round(PAD, top, w - 2.0 * PAD, rh, ITEM_R, fill);
                    }
                    let ink = match (*enabled, *danger, hot) {
                        (false, ..) => paint::faint(),
                        (_, true, true) => paint::color(255, 255, 255, 1.0),
                        (_, true, false) => paint::danger_text(),
                        _ => paint::text(),
                    };
                    if let Some(g) = icon {
                        // quieter than the label, except where the colour means something
                        let gink = match (*enabled, *danger, hot) {
                            (false, ..) => paint::faint(),
                            (_, true, _) => ink,
                            _ => paint::dim(),
                        };
                        p.glyph(g, PAD + 12.0 + 8.0, top + rh / 2.0, 11.0, gink);
                    }
                    p.text_mid(label, TEXT_X, top + rh / 2.0, 13.0, REGULAR, ink, w - TEXT_X - PAD - 12.0);
                    if let Some(k) = keys {
                        let kink = if hot && *danger { paint::ink(0.75) } else { paint::faint() };
                        p.text_right(k, w - PAD - 12.0, top + rh / 2.0, 12.0, REGULAR, kink);
                    }
                }
                Item::Sep => {
                    p.round(PAD + 12.0, top + SEP_H / 2.0 - 0.5, w - 2.0 * PAD - 24.0, 1.0, 0.5, paint::ink(0.08));
                }
                Item::Priority(current) => {
                    let cy = top + rh / 2.0;
                    p.text_mid("Priority", TEXT_X, cy, 13.0, REGULAR, paint::text(), 80.0);
                    let (rect, parts) = self.prio_seg(cy);
                    let sel = PRIOS.iter().position(|(pr, _)| *current == Some(*pr));
                    let hot = PRIOS.iter().position(|(pr, _)| hover == Some(MHit::Prio(*pr)));
                    let names: Vec<&str> = PRIOS.iter().map(|(_, n)| *n).collect();
                    crate::ui::paint_seg(p, rect, &parts, &names, sel, hot, true);
                }
            }
        }
    }
}

pub const TIP_H: f32 = 26.0;

/// A tooltip's size for `text`.
pub fn tip_w(text: &str, measure: impl Fn(&str, f32, DWRITE_FONT_WEIGHT) -> f32) -> f32 {
    (measure(text, 12.0, MEDIUM) + 20.0).ceil()
}

pub fn paint_tip(p: &P, text: &str, w: f32) {
    p.popover(0.0, 0.0, w, TIP_H, 9.0);
    p.text_mid(text, 10.0, TIP_H / 2.0, 12.0, MEDIUM, paint::text(), w);
}
