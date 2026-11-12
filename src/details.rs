//! The "Where from" popover for a download: its link, the page it came from, where
//! it's saved, its size and dates, and its SHA-256 (worked out on request), each with
//! a copy button. Also the small glyphs the downloads window's newer tools use, drawn
//! on Reicon's 24px grid and 1.5px stroke.

use std::path::PathBuf;

use crate::dl::JobView;
use crate::paint::{self, MEDIUM, P, REGULAR, SEMIBOLD};
use crate::settings_page::Rect;

// ---------- glyphs (Reicon outline style) ----------

pub const COPY: &str = "<g fill=\"none\" stroke=\"currentColor\" stroke-linecap=\"round\" stroke-linejoin=\"round\" stroke-width=\"1.5\"><rect x=\"8.75\" y=\"8.75\" width=\"12.5\" height=\"12.5\" rx=\"3.25\"/><path d=\"M15.25 8.75V6a3.25 3.25 0 0 0-3.25-3.25H6A3.25 3.25 0 0 0 2.75 6v6A3.25 3.25 0 0 0 6 15.25h2.75\"/></g>";
pub const SORT: &str = "<g fill=\"none\" stroke=\"currentColor\" stroke-linecap=\"round\" stroke-linejoin=\"round\" stroke-width=\"1.5\"><path d=\"M7 4.75v14.5m0 0l-3.25-3.25M7 19.25l3.25-3.25\"/><path d=\"M17 19.25V4.75m0 0l-3.25 3.25M17 4.75l3.25 3.25\"/></g>";
pub const INFO: &str = "<g fill=\"none\" stroke=\"currentColor\" stroke-linecap=\"round\" stroke-linejoin=\"round\" stroke-width=\"1.5\"><circle cx=\"12\" cy=\"12\" r=\"9.25\"/><path d=\"M12 11v5.5\"/><path d=\"M12 7.6v.05\" stroke-width=\"2\"/></g>";
pub const EYE: &str = "<g fill=\"none\" stroke=\"currentColor\" stroke-linecap=\"round\" stroke-linejoin=\"round\" stroke-width=\"1.5\"><path d=\"M2.75 12c1.9-4.2 5.25-6.75 9.25-6.75s7.35 2.55 9.25 6.75c-1.9 4.2-5.25 6.75-9.25 6.75S4.65 16.2 2.75 12Z\"/><circle cx=\"12\" cy=\"12\" r=\"3.25\"/></g>";
pub const PEN: &str = "<g fill=\"none\" stroke=\"currentColor\" stroke-linecap=\"round\" stroke-linejoin=\"round\" stroke-width=\"1.5\"><path d=\"M14.5 5.5l4 4\"/><path d=\"M4.75 19.25l.9-4.1L16.1 4.7a2 2 0 0 1 2.83 0l.37.37a2 2 0 0 1 0 2.83L8.85 18.35l-4.1.9Z\"/></g>";
pub const FILE: &str = "<g fill=\"none\" stroke=\"currentColor\" stroke-linecap=\"round\" stroke-linejoin=\"round\" stroke-width=\"1.5\"><path d=\"M13.25 2.75H8A3.25 3.25 0 0 0 4.75 6v12A3.25 3.25 0 0 0 8 21.25h8A3.25 3.25 0 0 0 19.25 18V8.75l-6-6Z\"/><path d=\"M13.25 2.75V7a1.75 1.75 0 0 0 1.75 1.75h4.25\"/></g>";

// ---------- dates ----------

/// "12 Nov 2026, 14:32" (local time) for a unix time, given the local offset.
pub fn date_time(ts: u64, offset: i64) -> String {
    let t = ts as i64 + offset;
    let days = t.div_euclid(86_400);
    let secs = t.rem_euclid(86_400);
    // days since 1970-01-01 to a civil date (Howard Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    format!("{d} {} {y}, {:02}:{:02}", MONTHS[(m - 1) as usize], secs / 3600, secs % 3600 / 60)
}

// ---------- the popover ----------

#[derive(Clone, PartialEq, Debug)]
pub enum Sha {
    Idle,
    Working,
    Done(String),
    Failed(String),
}

/// What a click on the popover hits.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DHit {
    Card,
    /// Copy this field ("source", "page", "path", "sha").
    Copy(&'static str),
    Sha,
}

pub struct Details {
    pub name: String,
    pub source: String,
    pub referrer: Option<String>,
    pub path: Option<PathBuf>,
    pub size: Option<u64>,
    pub added: u64,
    pub finished: Option<u64>,
    pub sha: Sha,
}

pub const W: f32 = 460.0;
const PAD: f32 = 20.0;
const HEAD: f32 = 62.0;
const FIELD: f32 = 48.0;
const SHA_H: f32 = 62.0;

struct Field {
    key: &'static str,
    label: &'static str,
    value: String,
    y: f32,
}

impl Details {
    pub fn new(j: &JobView, sha: Sha) -> Self {
        Details {
            name: j.name.clone(),
            source: j.source.clone(),
            referrer: j.referrer.clone().filter(|r| !r.is_empty()),
            path: j.path.clone().filter(|_| j.state.finished()),
            size: j.total,
            added: j.added,
            finished: j.finished_at,
            sha,
        }
    }

    fn fields(&self) -> Vec<Field> {
        let mut out = Vec::new();
        let mut y = HEAD;
        let mut push = |key, label, value: String| {
            out.push(Field { key, label, value, y });
            y += FIELD;
        };
        let link = if self.source.starts_with("magnet:") { "Magnet link" } else { "Download link" };
        push("source", link, self.source.clone());
        if let Some(r) = &self.referrer {
            push("page", "Downloaded from page", r.clone());
        }
        if let Some(p) = &self.path {
            push("path", "Saved to", p.display().to_string());
        }
        out
    }

    /// The facts row (size and dates) and the SHA-256 row, below the fields.
    fn facts_y(&self) -> f32 {
        HEAD + self.fields().len() as f32 * FIELD + 4.0
    }

    fn sha_y(&self) -> f32 {
        self.facts_y() + FIELD + 4.0
    }

    pub fn height(&self) -> f32 {
        self.sha_y() + if self.path.is_some() { SHA_H } else { 0.0 } + PAD - 6.0
    }

    fn copy_rect(y: f32, h: f32) -> Rect {
        Rect::new(W - PAD - 30.0, y + (h - 30.0) / 2.0, 30.0, 30.0)
    }

    fn sha_link(&self) -> Rect {
        Rect::new(PAD - 4.0, self.sha_y() + 22.0, 150.0, 26.0)
    }

    pub fn hit(&self, x: f32, y: f32) -> DHit {
        for f in self.fields() {
            if Self::copy_rect(f.y, FIELD).contains(x, y) {
                return DHit::Copy(f.key);
            }
        }
        if self.path.is_some() {
            match &self.sha {
                Sha::Done(_) if Self::copy_rect(self.sha_y(), SHA_H).contains(x, y) => return DHit::Copy("sha"),
                Sha::Idle | Sha::Failed(_) if self.sha_link().contains(x, y) => return DHit::Sha,
                _ => {}
            }
        }
        DHit::Card
    }

    /// The text a copy button copies.
    pub fn value(&self, key: &str) -> Option<String> {
        match key {
            "sha" => match &self.sha {
                Sha::Done(h) => Some(h.clone()),
                _ => None,
            },
            _ => self.fields().into_iter().find(|f| f.key == key).map(|f| f.value),
        }
    }

    pub fn paint(&self, p: &P, hover: Option<DHit>, copied: Option<&str>, offset: i64) {
        let h = self.height();
        p.popover(0.0, 0.0, W, h, 22.0);
        p.text(&self.name, PAD, 18.0, 15.0, SEMIBOLD, paint::text(), W - 2.0 * PAD);
        let from = crate::search::site(self.referrer.as_deref().unwrap_or(&self.source));
        let sub = if from.is_empty() { "Where it came from".to_string() } else { format!("Where it came from: {from}") };
        p.text(&sub, PAD, 40.0, 12.0, REGULAR, paint::dim(), W - 2.0 * PAD);
        let copy_btn = |key: &'static str, r: Rect| {
            let done = copied == Some(key);
            let hot = hover == Some(DHit::Copy(key));
            crate::ui::button(p, r, "", Some(if done { crate::icons::CHECK } else { COPY }), crate::ui::Kind::Ghost, hot, true);
        };
        for f in self.fields() {
            p.fill(PAD, f.y, W - 2.0 * PAD, 1.0, paint::ink(0.06));
            p.text(f.label, PAD, f.y + 7.0, 11.5, MEDIUM, paint::faint(), W - 2.0 * PAD - 40.0);
            p.text(&f.value, PAD, f.y + 24.0, 13.0, REGULAR, paint::text(), W - 2.0 * PAD - 40.0);
            copy_btn(f.key, Self::copy_rect(f.y, FIELD));
        }
        // size and dates, side by side
        let fy = self.facts_y();
        p.fill(PAD, fy - 4.0, W - 2.0 * PAD, 1.0, paint::ink(0.06));
        let facts = [
            ("Size", self.size.map(paint::bytes).unwrap_or_else(|| "Unknown".into())),
            ("Added", date_time(self.added, offset)),
            ("Finished", self.finished.map(|t| date_time(t, offset)).unwrap_or_else(|| "Not yet".into())),
        ];
        let cols = [PAD, PAD + 110.0, PAD + 270.0];
        for ((label, value), x) in facts.iter().zip(cols) {
            p.text(label, x, fy + 7.0, 11.5, MEDIUM, paint::faint(), 150.0);
            p.text(value, x, fy + 24.0, 13.0, REGULAR, paint::text(), 160.0);
        }
        if self.path.is_none() {
            return;
        }
        let sy = self.sha_y();
        p.fill(PAD, sy - 4.0, W - 2.0 * PAD, 1.0, paint::ink(0.06));
        p.text("SHA-256", PAD, sy + 7.0, 11.5, MEDIUM, paint::faint(), 200.0);
        let link = self.sha_link();
        match &self.sha {
            Sha::Idle => {
                crate::ui::link(p, link.x + 4.0, link.y + 13.0, "Work it out", None, hover == Some(DHit::Sha));
            }
            Sha::Working => {
                p.text_mid("Reading the file\u{2026}", PAD, link.y + 13.0, 13.0, REGULAR, paint::dim(), 300.0);
            }
            Sha::Failed(e) => {
                let w = p.text_mid(e, PAD, link.y + 13.0, 12.5, REGULAR, paint::danger_text(), 260.0);
                crate::ui::link(p, PAD + w + 12.0, link.y + 13.0, "Try again", None, hover == Some(DHit::Sha));
            }
            Sha::Done(hex) => {
                // two lines of 32, so the whole thing is there to compare
                let (a, b) = hex.split_at(hex.len().min(32));
                p.text(a, PAD, sy + 24.0, 12.5, REGULAR, paint::text(), W - 2.0 * PAD - 40.0);
                p.text(b, PAD, sy + 40.0, 12.5, REGULAR, paint::text(), W - 2.0 * PAD - 40.0);
                copy_btn("sha", Self::copy_rect(sy, SHA_H));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(date_time(0, 0), "1 Jan 1970, 00:00");
        // 2026-11-12 14:32 UTC, shown at UTC+1
        assert_eq!(date_time(1_794_493_920, 3600), "12 Nov 2026, 15:32");
        assert_eq!(date_time(951_782_400, 0), "29 Feb 2000, 00:00");
    }
}
