//! Small form sheets for the download engine's features: More Options for a new
//! download (mirrors, checksum, speed limit), a download's own speed limit, a new link
//! for an expired one, a site's sign-in, "already downloaded", the countdown when the
//! queue is done, and the Settings page's download hours, proxy and sort folders.
//!
//! One generic sheet (`Form`): a title, a line of text, labelled text fields, an
//! optional segmented choice and switch, and buttons. The window hosts it as
//! `SheetKind::Form`.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Instant;

use super::*;
use crate::dl::prefs::{self, Prefs, ProxyMode, WhenDone};

pub(super) const T_FORM: usize = 41;
const W: f32 = 500.0;
const TOP: f32 = 84.0;
const ROW_STEP: f32 = 46.0;
const FIELD_X: f32 = 150.0;
const FIELD_H: f32 = 34.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum FHit {
    Field(usize),
    Seg(usize),
    Switch,
    Button(usize),
    /// the New Download sheet's own links
    MoreOptions,
    ImportList,
}

pub(super) struct Input {
    label: &'static str,
    text: Vec<char>,
    caret: usize,
    secret: bool,
    hint: &'static str,
}

impl Input {
    fn new(label: &'static str, value: &str, hint: &'static str) -> Self {
        let text: Vec<char> = value.chars().collect();
        Input { label, caret: text.len(), text, secret: false, hint }
    }
    fn secret(mut self) -> Self {
        self.secret = true;
        self
    }
    fn value(&self) -> String {
        self.text.iter().collect::<String>().trim().to_string()
    }
    fn shown(&self) -> String {
        if self.secret { "\u{2022}".repeat(self.text.len()) } else { self.text.iter().collect() }
    }
}

#[derive(Clone)]
pub(super) enum Purpose {
    More { dir: PathBuf, priority: Priority },
    Limit(Vec<u64>),
    Refresh(u64),
    SignIn(u64),
    Duplicate { links: Vec<String>, existing: PathBuf, req: AddRequest },
    Countdown { action: WhenDone, ends: Instant },
    Schedule,
    Proxy,
    Folders,
}

pub(super) struct Form {
    title: String,
    desc: String,
    inputs: Vec<Input>,
    focus: usize,
    seg: Option<(&'static str, Vec<&'static str>, usize)>,
    switch: Option<(&'static str, bool)>,
    /// (label, kind); laid out from the right, the last one rightmost
    buttons: Vec<(String, crate::ui::Kind)>,
    error: Option<String>,
    purpose: Purpose,
}

impl Form {
    fn new(title: impl Into<String>, desc: impl Into<String>, purpose: Purpose) -> Self {
        Form { title: title.into(), desc: desc.into(), inputs: Vec::new(), focus: 0, seg: None, switch: None, buttons: Vec::new(), error: None, purpose }
    }

    fn ok_cancel(mut self, ok: &str) -> Self {
        self.buttons = vec![("Cancel".into(), crate::ui::Kind::Secondary), (ok.into(), crate::ui::Kind::Primary)];
        self
    }

    fn rows_top(&self) -> f32 {
        TOP + self.inputs.len() as f32 * ROW_STEP
    }

    pub(super) fn size(&self) -> (f32, f32) {
        let mut h = self.rows_top();
        if self.seg.is_some() {
            h += ROW_STEP;
        }
        if self.switch.is_some() {
            h += 44.0;
        }
        h += 24.0 + crate::ui::BUTTON_H + 22.0;
        (W, h)
    }

    fn field_rect(&self, i: usize) -> settings_page::Rect {
        settings_page::Rect::new(FIELD_X, TOP + i as f32 * ROW_STEP, W - FIELD_X - 24.0, FIELD_H)
    }

    fn seg_geom(&self, measure: impl Fn(&str) -> f32) -> Option<(settings_page::Rect, Vec<settings_page::Rect>)> {
        let (_, opts, _) = self.seg.as_ref()?;
        let widths = crate::ui::seg_widths(opts, |t| measure(t) + 14.0);
        Some(crate::ui::seg_layout(FIELD_X, self.rows_top() + FIELD_H / 2.0, &widths))
    }

    fn switch_rect(&self) -> settings_page::Rect {
        let y = self.rows_top() + if self.seg.is_some() { ROW_STEP } else { 0.0 } + 6.0;
        settings_page::Rect::new(24.0, y, settings_page::SWITCH.0, settings_page::SWITCH.1)
    }

    fn button_rects(&self, measure: impl Fn(&str) -> f32) -> Vec<settings_page::Rect> {
        let (_, h) = self.size();
        let bh = crate::ui::BUTTON_H;
        let y = h - 22.0 - bh;
        let mut x = W - 24.0;
        let mut out = vec![settings_page::Rect::new(0.0, 0.0, 0.0, 0.0); self.buttons.len()];
        for (i, (label, _)) in self.buttons.iter().enumerate().rev() {
            let bw = (measure(label) + 40.0).max(92.0);
            x -= bw;
            out[i] = settings_page::Rect::new(x, y, bw, bh);
            x -= 10.0;
        }
        out
    }

    fn hit(&self, x: f32, y: f32, measure: &dyn Fn(&str) -> f32) -> Option<FHit> {
        for i in 0..self.inputs.len() {
            if self.field_rect(i).contains(x, y) {
                return Some(FHit::Field(i));
            }
        }
        if let Some((_, parts)) = self.seg_geom(measure) {
            if let Some(i) = parts.iter().position(|r| r.contains(x, y)) {
                return Some(FHit::Seg(i));
            }
        }
        if self.switch.is_some() {
            let r = self.switch_rect();
            if settings_page::Rect::new(r.x - 6.0, r.y - 6.0, W - 48.0, r.h + 12.0).contains(x, y) {
                return Some(FHit::Switch);
            }
        }
        self.button_rects(measure).iter().position(|r| r.contains(x, y)).map(FHit::Button)
    }

    fn key(&mut self, vk: u16, ctrl: bool, shift: bool) -> bool {
        if vk == VK_TAB.0 {
            let n = self.inputs.len().max(1);
            self.focus = if shift { (self.focus + n - 1) % n } else { (self.focus + 1) % n };
            return true;
        }
        let Some(f) = self.inputs.get_mut(self.focus) else { return false };
        let n = f.text.len();
        match vk {
            v if v == VK_BACK.0 && f.caret > 0 => {
                f.caret -= 1;
                f.text.remove(f.caret);
            }
            v if v == VK_DELETE.0 && f.caret < n => {
                f.text.remove(f.caret);
            }
            v if v == VK_LEFT.0 => f.caret = f.caret.saturating_sub(1),
            v if v == VK_RIGHT.0 => f.caret = (f.caret + 1).min(n),
            v if v == VK_HOME.0 => f.caret = 0,
            v if v == VK_END.0 => f.caret = n,
            v if ctrl && v == b'V' as u16 => {
                if let Some(t) = shell::clipboard_text() {
                    for ch in t.split_whitespace().collect::<Vec<_>>().join(" ").chars() {
                        f.text.insert(f.caret, ch);
                        f.caret += 1;
                    }
                }
            }
            v if ctrl && v == b'C' as u16 && !f.secret => shell::set_clipboard_text(&f.value()),
            v if ctrl && v == b'A' as u16 => {
                // select all: here, simply clear on the next key is too surprising; go to the end
                f.caret = n;
            }
            v if v == VK_BACK.0 || v == VK_DELETE.0 => {}
            _ => return false,
        }
        self.error = None;
        true
    }

    fn char(&mut self, ch: char) -> bool {
        let Some(f) = self.inputs.get_mut(self.focus) else { return false };
        if ch.is_control() {
            return true;
        }
        f.text.insert(f.caret, ch);
        f.caret += 1;
        self.error = None;
        true
    }

    fn paint(&self, p: &P, hover: Option<FHit>, dw: &windows::Win32::Graphics::DirectWrite::IDWriteFactory, k: f32) {
        let (w, h) = self.size();
        p.popover(0.0, 0.0, w, h, 26.0);
        p.text(&self.title, 24.0, 19.0, 17.0, SEMIBOLD, paint::text(), w - 48.0);
        p.text(&self.desc, 24.0, 46.0, 12.5, REGULAR, paint::dim(), w - 48.0);
        for (i, f) in self.inputs.iter().enumerate() {
            let r = self.field_rect(i);
            p.text_mid(f.label, 24.0, r.y + r.h / 2.0, 13.0, REGULAR, paint::dim(), FIELD_X - 30.0);
            p.round(r.x, r.y, r.w, r.h, 12.0, paint::well(0.25));
            if i == self.focus {
                p.round_stroke(r.x, r.y, r.w, r.h, 12.0, paint::rgb(paint::ORANGE, 0.75), 1.5);
            }
            let shown = f.shown();
            let before: String = shown.chars().take(f.caret).collect();
            let inner = r.w - 24.0;
            let cx = paint::advance(dw, &before, FIELD_TEXT * k) / k;
            let scroll = (cx - inner + 4.0).max(0.0);
            unsafe {
                p.dc.PushAxisAlignedClip(&D2D_RECT_F { left: (r.x + 8.0) * k, top: r.y * k, right: (r.x + r.w - 8.0) * k, bottom: (r.y + r.h) * k }, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            }
            if shown.is_empty() {
                p.text_mid(f.hint, r.x + 12.0, r.y + r.h / 2.0, FIELD_TEXT, REGULAR, paint::faint(), inner);
            } else {
                p.text_mid(&shown, r.x + 12.0 - scroll, r.y + r.h / 2.0, FIELD_TEXT, REGULAR, paint::text(), 100_000.0);
            }
            unsafe {
                p.dc.PopAxisAlignedClip();
            }
        }
        let measure = |t: &str| paint::advance(dw, t, 13.0 * k) / k;
        if let (Some((label, opts, sel)), Some((rect, parts))) = (&self.seg, self.seg_geom(measure)) {
            p.text_mid(label, 24.0, rect.y + rect.h / 2.0, 13.0, REGULAR, paint::dim(), FIELD_X - 30.0);
            let hot = match hover {
                Some(FHit::Seg(i)) => Some(i),
                _ => None,
            };
            crate::ui::paint_seg(p, rect, &parts, opts, Some(*sel), hot, true);
        }
        if let Some((label, on)) = self.switch {
            let r = self.switch_rect();
            // painted whole (no live knob): it simply flips
            if on {
                p.capsule(r.x, r.y, r.w, r.h, paint::accent());
            } else {
                settings_page::paint_switch_track(p, r, true);
            }
            let kw = 24.0;
            let kx = if on { r.x + r.w - kw - 2.0 } else { r.x + 2.0 };
            p.capsule(kx, r.y + 2.0, kw, r.h - 4.0, paint::color(255, 255, 255, 1.0));
            p.text_mid(label, r.x + r.w + 12.0, r.y + r.h / 2.0, 13.0, REGULAR, paint::text(), W - 120.0);
        }
        if let Some(e) = &self.error {
            let y = h - 22.0 - crate::ui::BUTTON_H - 22.0;
            p.text(e, 24.0, y, 11.5, REGULAR, paint::red(), W - 48.0);
        }
        let rects = self.button_rects(|t| paint::advance(dw, t, 13.5 * k) / k);
        for (i, ((label, kind), r)) in self.buttons.iter().zip(rects).enumerate() {
            crate::ui::button(p, r, label, None, *kind, hover == Some(FHit::Button(i)), true);
        }
    }

    /// Where the caret goes (in the sheet), if a field has focus.
    fn caret(&self, dw: &windows::Win32::Graphics::DirectWrite::IDWriteFactory, k: f32) -> Option<(f32, f32)> {
        let f = self.inputs.get(self.focus)?;
        let r = self.field_rect(self.focus);
        let before: String = f.shown().chars().take(f.caret).collect();
        let cx = paint::advance(dw, &before, FIELD_TEXT * k) / k;
        let scroll = (cx - (r.w - 24.0) + 4.0).max(0.0);
        Some((r.x + 12.0 + cx - scroll, r.y + 8.0))
    }
}

thread_local! {
    /// Sign-ins already asked for (once per download, until it asks again).
    static ASKED: RefCell<HashSet<u64>> = RefCell::new(HashSet::new());
    static QUEUE_DONE: Cell<u64> = const { Cell::new(0) };
}

/// The engine says everything finished: true when a "when done" action should start.
pub fn queue_finished(snap: &dl::Snapshot, s: &Settings) -> bool {
    let seen = QUEUE_DONE.with(|c| c.replace(snap.queue_done));
    snap.queue_done > seen && s.downloads.engine.when_done != WhenDone::Nothing
}

/// The 30-second countdown before sleeping, shutting down or quitting.
pub fn start_countdown(action: WhenDone) {
    with(|m| m.open_countdown(action));
}

/// A Settings page button for the engine's settings ("dle:edit:schedule").
pub fn open_engine_form(which: &str) {
    with(|m| m.open_engine_form(which));
}

/// Apply a "dle:…" settings command; true if the settings changed.
pub fn apply_engine_command(id: &str, p: &mut Prefs) -> bool {
    let Some(rest) = id.strip_prefix("dle:") else { return false };
    let (verb, arg) = rest.split_once(':').unwrap_or((rest, ""));
    match verb {
        "flip" => match arg {
            "retry" => p.auto_retry = !p.auto_retry,
            "awake" => p.keep_awake = !p.keep_awake,
            "metered" => p.pause_metered = !p.pause_metered,
            "sort" => p.auto_sort = !p.auto_sort,
            _ => return false,
        },
        "seg" => {
            let (what, i) = arg.split_once(':').unwrap_or((arg, "0"));
            let i: usize = i.parse().unwrap_or(0);
            match what {
                "when" => p.when_done = [WhenDone::Nothing, WhenDone::Sleep, WhenDone::ShutDown, WhenDone::Quit][i.min(3)],
                _ => return false,
            }
        }
        "set" => match serde_json::from_str::<Prefs>(arg) {
            Ok(new) => *p = new,
            Err(_) => return false,
        },
        "edit" => {
            open_engine_form(arg);
            return false;
        }
        _ => return false,
    }
    true
}

fn kbps_of(text: &str) -> Option<u64> {
    let t = text.trim().to_ascii_lowercase();
    if t.is_empty() || t == "none" {
        return Some(0);
    }
    let digits: String = t.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    let n: f64 = digits.parse().ok()?;
    let rest = t[digits.len()..].trim();
    let mult = if rest.starts_with('m') { 1024.0 } else { 1.0 };
    Some((n * mult).round() as u64)
}

impl Manager {
    fn form(&self) -> Option<&Form> {
        match &self.sheet {
            Some(Sheet { kind: SheetKind::Form(f), .. }) => Some(f),
            _ => None,
        }
    }

    fn form_mut(&mut self) -> Option<&mut Form> {
        match &mut self.sheet {
            Some(Sheet { kind: SheetKind::Form(f), .. }) => Some(f),
            _ => None,
        }
    }

    pub(super) fn is_form(&self) -> bool {
        self.form().is_some()
    }

    fn open_form(&mut self, f: Form) {
        self.open_sheet(SheetKind::Form(Box::new(f)));
    }

    pub(super) fn form_hit(&self, x: f32, y: f32) -> Option<FHit> {
        let dw = self.gfx.dwrite.clone();
        let k = self.k;
        self.form()?.hit(x, y, &|t: &str| paint::advance(&dw, t, 13.0 * k) / k)
    }

    /// The New Download sheet's extra links: "Import List…" by the title, "More Options…"
    /// on the priority row. (x, y) inside the sheet.
    pub(super) fn add_sheet_links(&self, x: f32, y: f32) -> Option<FHit> {
        let dw = self.gfx.dwrite.clone();
        let k = self.k;
        let mw = |t: &str| paint::advance(&dw, t, 13.0 * k) / k;
        let w = SHEET_W;
        let import = mw("Import List\u{2026}");
        if x >= w - 26.0 - import - 4.0 && x < w - 20.0 && (16.0..40.0).contains(&y) {
            return Some(FHit::ImportList);
        }
        let more = mw("More Options\u{2026}");
        // (it sits on the priority row, which a video card pushes down)
        let dy = self.vsheet.extra_h();
        if x >= w - 26.0 - more - 4.0 && x < w - 20.0 && (174.0 + dy..200.0 + dy).contains(&y) {
            return Some(FHit::MoreOptions);
        }
        None
    }

    pub(super) fn paint_add_sheet_links(p: &P, hover: &Hit, dy: f32) {
        let w = SHEET_W;
        let import = p.measure("Import List\u{2026}", 13.0, MEDIUM);
        crate::ui::link(p, w - 26.0 - import, 28.0, "Import List\u{2026}", None, *hover == Hit::Form(FHit::ImportList));
        let more = p.measure("More Options\u{2026}", 13.0, MEDIUM);
        crate::ui::link(p, w - 26.0 - more, 187.0 + dy, "More Options\u{2026}", None, *hover == Hit::Form(FHit::MoreOptions));
    }

    pub(super) fn paint_form(&mut self) -> Result<()> {
        let k = self.k;
        let dw = self.gfx.dwrite.clone();
        let hover = match &self.hover {
            Hit::Form(h) => Some(*h),
            _ => None,
        };
        let Some(Sheet { kind: SheetKind::Form(f), card, caret, .. }) = &self.sheet else { return Ok(()) };
        let (w, h) = f.size();
        let surface = self.gfx.surface(w * k, h * k, |dc| unsafe {
            dc.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            f.paint(&P { dc, dw: &dw, k }, hover, &dw, k);
        })?;
        set_surface(&self.gfx, card, &surface, w * k, h * k)?;
        match f.caret(&dw, k) {
            Some((x, y)) => {
                caret.SetSize(Vector2 { X: (1.5 * k).max(1.0), Y: 18.0 * k })?;
                caret.SetOffset(Vector3 { X: x * k, Y: y * k, Z: 0.0 })?;
                caret.SetIsVisible(true)?;
            }
            None => caret.SetIsVisible(false)?,
        }
        Ok(())
    }

    pub(super) fn form_key(&mut self, vk: u16, ctrl: bool) -> bool {
        let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
        let Some(f) = self.form_mut() else { return false };
        if !f.key(vk, ctrl, shift) {
            return false;
        }
        let _ = self.paint_form();
        true
    }

    pub(super) fn form_char(&mut self, ch: char) -> bool {
        let Some(f) = self.form_mut() else { return false };
        f.char(ch);
        let _ = self.paint_form();
        true
    }

    pub(super) fn form_click(&mut self, h: FHit) -> Option<Blocking> {
        match h {
            FHit::MoreOptions => self.open_more_options(),
            FHit::ImportList => return Some(Blocking::ImportList),
            FHit::Field(i) => {
                if let Some(f) = self.form_mut() {
                    f.focus = i;
                }
                let _ = self.paint_form();
            }
            FHit::Seg(i) => {
                if let Some(Form { seg: Some((_, _, sel)), .. }) = self.form_mut() {
                    *sel = i;
                }
                let _ = self.paint_form();
            }
            FHit::Switch => {
                if let Some(Form { switch: Some((_, on)), .. }) = self.form_mut() {
                    *on = !*on;
                }
                let _ = self.paint_form();
            }
            FHit::Button(i) => {
                let last = self.form().map(|f| f.buttons.len().saturating_sub(1)).unwrap_or(0);
                // the first button is Cancel, except where it's a choice of its own
                let choice = self.form().is_some_and(|f| matches!(f.purpose, Purpose::Duplicate { .. } | Purpose::Countdown { .. }));
                if i == last {
                    self.submit_form(None);
                } else if choice {
                    self.submit_form(Some(i));
                } else {
                    self.close_form();
                }
            }
        }
        None
    }

    fn close_form(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), T_FORM);
        }
        self.close_sheet();
    }

    // ---------- opening them ----------

    /// More Options: the links typed so far, plus mirrors, a checksum and a speed limit.
    fn open_more_options(&mut self) {
        let Some(Sheet { kind: SheetKind::Add { field, dir, priority, .. }, .. }) = &self.sheet else { return };
        let (links, dir, priority) = (field.string(), dir.clone(), *priority);
        let mut f = Form::new("New Download", "More options for this download.", Purpose::More { dir, priority }).ok_cancel("Download");
        f.inputs = vec![
            Input::new("Link", &links, "https://example.com/file[01-10].zip"),
            Input::new("Mirrors", "", "Other links to the same file (optional)"),
            Input::new("Checksum", "", "SHA-256, SHA-1 or MD5 (optional)"),
            Input::new("Speed limit", "", "KB/s, or 2 MB (optional)"),
        ];
        self.open_form(f);
    }

    /// A text file of links, one or more per line, into the New Download field.
    pub(super) fn import_list(&mut self, path: &std::path::Path) {
        let Ok(bytes) = std::fs::read(path) else { return };
        let links = dl::links_in_text(&String::from_utf8_lossy(&bytes));
        if links.is_empty() {
            self.show_toast("No links in that file");
            return;
        }
        if let Some(Sheet { kind: SheetKind::Add { field, error, .. }, .. }) = &mut self.sheet {
            let mut text = field.string();
            if !text.trim().is_empty() {
                text.push(' ');
            }
            text += &links.join(" ");
            field.set(&text);
            *error = None;
        }
        let _ = self.paint_sheet();
    }

    pub(super) fn open_limit(&mut self, ids: Vec<u64>) {
        let current = ids.first().and_then(|id| self.job(*id)).map(|j| j.limit_bps / 1024).unwrap_or(0);
        let mut f = Form::new("Speed Limit", "Just for this download, on top of the overall limit. Empty for none.", Purpose::Limit(ids)).ok_cancel("Set Limit");
        f.inputs = vec![Input::new("KB/s", &if current > 0 { current.to_string() } else { String::new() }, "No limit")];
        self.open_form(f);
    }

    pub(super) fn open_refresh(&mut self, id: u64) {
        let name = self.job(id).map(|j| j.name.clone()).unwrap_or_default();
        let mut f = Form::new("Refresh Link", format!("A new link for \u{201c}{name}\u{201d}: if it's the same file, it carries on."), Purpose::Refresh(id)).ok_cancel("Continue");
        f.inputs = vec![Input::new("New link", "", "Paste the new download link")];
        self.open_form(f);
    }

    fn open_sign_in(&mut self, id: u64, site: &str) {
        let host = site.rsplit_once(':').map(|(h, port)| if port == "443" || port == "80" { h.to_string() } else { site.to_string() }).unwrap_or(site.to_string());
        let mut f = Form::new(format!("Sign in to {host}"), "This download needs a user name and password.", Purpose::SignIn(id)).ok_cancel("Sign In");
        f.inputs = vec![Input::new("User name", "", ""), Input::new("Password", "", "").secret()];
        f.switch = Some(("Remember in Windows Credential Manager", false));
        self.open_form(f);
    }

    fn open_countdown(&mut self, action: WhenDone) {
        if action == WhenDone::Nothing {
            return;
        }
        let mut f = Form::new("Downloads finished", "", Purpose::Countdown { action, ends: Instant::now() + std::time::Duration::from_secs(30) });
        let now = match action {
            WhenDone::Sleep => "Sleep Now",
            WhenDone::ShutDown => "Shut Down Now",
            _ => "Quit Now",
        };
        f.buttons = vec![("Cancel".into(), crate::ui::Kind::Secondary), (now.into(), crate::ui::Kind::Primary)];
        self.open_form(f);
        self.countdown_text();
        unsafe {
            let _ = SetTimer(Some(self.hwnd), T_FORM, 1000, None);
        }
        let _ = self.paint_form();
    }

    fn countdown_text(&mut self) -> Option<u64> {
        let f = self.form_mut()?;
        let Purpose::Countdown { action, ends } = f.purpose else { return None };
        let left = ends.saturating_duration_since(Instant::now()).as_secs_f32().ceil() as u64;
        f.desc = format!("Stash will {} in {left} s.", action.verb());
        Some(left)
    }

    /// Once a second while the countdown shows.
    pub(super) fn form_tick(&mut self) {
        match self.countdown_text() {
            Some(0) => self.submit_form(None),
            Some(_) => {
                let _ = self.paint_form();
            }
            None => unsafe {
                let _ = KillTimer(Some(self.hwnd), T_FORM);
            },
        }
    }

    fn open_engine_form(&mut self, which: &str) {
        let p = self.settings.downloads.engine.clone();
        let time = |t: Option<u16>| t.map(prefs::format_time).unwrap_or_default();
        let f = match which {
            "schedule" => {
                let mut f = Form::new("Download Hours", "Times are 24-hour (07:30). Leave a box empty to not use it.", Purpose::Schedule).ok_cancel("Save");
                f.inputs = vec![
                    Input::new("Start every day", &time(p.schedule.start_at), "e.g. 01:00"),
                    Input::new("Stop every day", &time(p.schedule.stop_at), "e.g. 07:00"),
                    Input::new("Only from", &time(p.schedule.window.map(|w| w.0)), "e.g. 23:00"),
                    Input::new("Until", &time(p.schedule.window.map(|w| w.1)), "e.g. 07:00"),
                ];
                f
            }
            "proxy" => {
                let mut f = Form::new("Proxy", "How Stash reaches the internet for web and FTP downloads.", Purpose::Proxy).ok_cancel("Save");
                let addr = if p.proxy.host.is_empty() { String::new() } else { format!("{}:{}", p.proxy.host, p.proxy.port) };
                f.inputs = vec![
                    Input::new("Address", &addr, "host:port"),
                    Input::new("User name", &p.proxy.user, "Optional"),
                    Input::new("Password", "", "Unchanged").secret(),
                ];
                let sel = [ProxyMode::System, ProxyMode::Off, ProxyMode::Http, ProxyMode::Socks5].iter().position(|m| *m == p.proxy.mode).unwrap_or(0);
                f.seg = Some(("Use", vec!["Windows", "None", "HTTP", "SOCKS5"], sel));
                f
            }
            "folders" => {
                let mut f = Form::new("Folders by Type", "Empty: a folder of that name where the download is saved.", Purpose::Folders).ok_cancel("Save");
                let get = |c: dl::sort::Category| p.sort_folders.iter().find(|(k, _)| *k == c).map(|(_, d)| d.display().to_string()).unwrap_or_default();
                f.inputs = dl::sort::ALL.iter().map(|c| Input::new(c.name(), &get(*c), "Default")).collect();
                f
            }
            _ => return,
        };
        self.open_form(f);
    }

    /// The window is up to date with the engine: ask for any sign-in a download needs.
    pub(super) fn check_prompts(&mut self, snap: &dl::Snapshot) {
        if self.sheet.is_some() {
            return;
        }
        let ask = snap.jobs.iter().find(|j| j.needs_login.is_some() && ASKED.with(|a| !a.borrow().contains(&j.id)));
        if let Some(j) = ask {
            let (id, site) = (j.id, j.needs_login.clone().unwrap_or_default());
            ASKED.with(|a| a.borrow_mut().insert(id));
            self.open_sign_in(id, &site);
        }
        // a download that went on asks again next time it needs to
        ASKED.with(|a| a.borrow_mut().retain(|id| snap.jobs.iter().any(|j| j.id == *id && j.needs_login.is_some())));
    }

    /// New downloads from the New Download sheet or More Options: patterns expanded, and
    /// a question first for what's already on this PC.
    pub(super) fn add_links(&mut self, links: Vec<String>, template: AddRequest) {
        let exact_dir = template.dir != default_dir(&self.settings);
        let all = dl::expand_all(links.iter().map(String::as_str));
        let snap = dl::engine().snapshot();
        let mut dupes = Vec::new();
        let mut existing = None;
        for l in all {
            if let Some(p) = dl::existing_download(&snap, &l, &template.dir) {
                existing.get_or_insert(p);
                dupes.push(l);
                continue;
            }
            dl::engine().add(AddRequest { source: l, exact_dir, ..template.clone() });
        }
        let Some(existing) = existing else { return };
        let name = existing.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let desc = if dupes.len() == 1 { format!("\u{201c}{name}\u{201d} is already on your PC.") } else { format!("{} of these are already on your PC.", dupes.len()) };
        let mut f = Form::new("Already downloaded", desc, Purpose::Duplicate { links: dupes, existing, req: AddRequest { exact_dir, ..template } });
        f.buttons = vec![("Show Existing".into(), crate::ui::Kind::Secondary), ("Download Again".into(), crate::ui::Kind::Primary)];
        self.open_form(f);
    }

    // ---------- submitting ----------

    /// The last button (or Enter), or another button `which`.
    pub(super) fn submit_form(&mut self, which: Option<usize>) {
        if !self.is_form() {
            return;
        }
        let Some(sheet) = self.sheet.take() else { return };
        let _ = self.root.Children().and_then(|c| c.Remove(&sheet.root));
        let SheetKind::Form(f) = sheet.kind else { return };
        let f = *f;
        // an error puts it back up, saying what's wrong
        let fail = |m: &mut Manager, mut f: Form, e: &str| {
            f.error = Some(e.to_string());
            m.open_form(f);
        };
        let vals: Vec<String> = f.inputs.iter().map(Input::value).collect();
        let raw: Vec<String> = f.inputs.iter().map(|i| i.text.iter().collect()).collect();
        let value = |i: usize| vals.get(i).cloned().unwrap_or_default();
        unsafe {
            let _ = KillTimer(Some(self.hwnd), T_FORM);
        }
        match f.purpose.clone() {
            Purpose::More { dir, priority } => {
                let links: Vec<String> = value(0).split_whitespace().map(String::from).collect();
                if links.is_empty() {
                    return fail(self, f, "Paste a link to download.");
                }
                if let Some(bad) = links.iter().find(|l| !dl::is_source(l)) {
                    let bad = bad.chars().take(40).collect::<String>();
                    return fail(self, f, &format!("\u{201c}{bad}\u{201d} isn't a web, FTP or magnet link."));
                }
                let mirrors: Vec<String> = value(1).split_whitespace().map(String::from).collect();
                if mirrors.iter().any(|m| !dl::is_source(m)) {
                    return fail(self, f, "A mirror isn't a web or FTP link.");
                }
                let sum = value(2);
                if !sum.is_empty() && dl::parse_checksum(&sum).is_none() {
                    return fail(self, f, "That checksum isn't SHA-256, SHA-1 or MD5 (hex).");
                }
                let Some(kbps) = kbps_of(&value(3)) else { return fail(self, f, "The speed limit should be a number of KB/s.") };
                let req = AddRequest {
                    dir,
                    priority,
                    checksum: (!sum.is_empty()).then_some(sum),
                    mirrors,
                    limit_bps: kbps * 1024,
                    ..Default::default()
                };
                self.close_sheet();
                self.add_links(links, req);
            }
            Purpose::Limit(ids) => {
                let Some(kbps) = kbps_of(&value(0)) else { return fail(self, f, "Type a number of KB/s, or leave it empty.") };
                for id in ids {
                    dl::engine().set_limit(id, kbps * 1024);
                }
                self.close_sheet();
                self.show_toast(&if kbps == 0 { "No speed limit for this download".to_string() } else { format!("Limited to {kbps} KB/s") });
            }
            Purpose::Refresh(id) => {
                let url = value(0);
                if !dl::is_source(&url) {
                    return fail(self, f, "Paste a web or FTP link.");
                }
                dl::engine().refresh_link(id, url);
                self.close_sheet();
            }
            Purpose::SignIn(id) => {
                let user = value(0);
                if user.is_empty() {
                    return fail(self, f, "Type your user name.");
                }
                let pass = raw.get(1).cloned().unwrap_or_default();
                let remember = f.switch.is_some_and(|s| s.1);
                dl::engine().sign_in(id, user, pass, remember);
                self.close_sheet();
            }
            Purpose::Duplicate { links, existing, req } => {
                self.close_sheet();
                if which == Some(0) {
                    shell::reveal(&existing);
                } else {
                    for l in links {
                        dl::engine().add(AddRequest { source: l, ..req.clone() });
                    }
                }
            }
            Purpose::Countdown { action, .. } => {
                self.close_sheet();
                if which == Some(0) {
                    return; // cancelled
                }
                match action {
                    WhenDone::Sleep => dl::sleep_now(),
                    WhenDone::ShutDown => dl::shut_down_now(),
                    WhenDone::Quit => crate::stash::queue(crate::menu::ids::QUIT.into()),
                    WhenDone::Nothing => {}
                }
            }
            Purpose::Schedule => {
                let parse = |i: usize| -> std::result::Result<Option<u16>, ()> {
                    let v = value(i);
                    if v.is_empty() { Ok(None) } else { prefs::parse_time(&v).map(Some).ok_or(()) }
                };
                let (Ok(start), Ok(stop), Ok(from), Ok(to)) = (parse(0), parse(1), parse(2), parse(3)) else {
                    return fail(self, f, "Times look like 07:30 (24-hour).");
                };
                if from.is_some() != to.is_some() {
                    return fail(self, f, "Download hours need both a start and an end.");
                }
                let mut p = self.settings.downloads.engine.clone();
                p.schedule = prefs::Schedule { start_at: start, stop_at: stop, window: from.zip(to) };
                self.close_sheet();
                queue_prefs(&p);
            }
            Purpose::Proxy => {
                let mode = [ProxyMode::System, ProxyMode::Off, ProxyMode::Http, ProxyMode::Socks5][f.seg.as_ref().map(|s| s.2).unwrap_or(0)];
                let addr = value(0);
                let mut p = self.settings.downloads.engine.clone();
                if matches!(mode, ProxyMode::Http | ProxyMode::Socks5) {
                    let parsed = addr.trim_start_matches("http://").trim_start_matches("socks5://").rsplit_once(':').and_then(|(h, port)| Some((h.to_string(), port.parse::<u16>().ok()?)));
                    let Some((host, port)) = parsed.filter(|(h, _)| !h.is_empty()) else {
                        return fail(self, f, "Type the proxy's address as host:port.");
                    };
                    p.proxy.host = host;
                    p.proxy.port = port;
                } else if let Some((h, port)) = addr.rsplit_once(':') {
                    p.proxy.host = h.to_string();
                    p.proxy.port = port.parse().unwrap_or(0);
                }
                p.proxy.mode = mode;
                p.proxy.user = value(1);
                let pass = raw.get(2).cloned().unwrap_or_default();
                if !pass.is_empty() || p.proxy.user.is_empty() {
                    dl::set_proxy_password(&p.proxy, &pass);
                }
                self.close_sheet();
                queue_prefs(&p);
            }
            Purpose::Folders => {
                let mut p = self.settings.downloads.engine.clone();
                p.sort_folders = dl::sort::ALL.iter().enumerate().filter(|(i, _)| !value(*i).is_empty()).map(|(i, c)| (*c, PathBuf::from(value(i)))).collect();
                self.close_sheet();
                queue_prefs(&p);
            }
        }
    }
}

/// New engine settings go to the stack, which owns the settings file.
fn queue_prefs(p: &Prefs) {
    if let Ok(json) = serde_json::to_string(p) {
        crate::stash::queue(format!("dle:set:{json}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_commands() {
        let mut p = Prefs::default();
        assert!(apply_engine_command("dle:flip:sort", &mut p) && p.auto_sort);
        assert!(apply_engine_command("dle:seg:when:2", &mut p) && p.when_done == WhenDone::ShutDown);
        let mut q = Prefs::default();
        q.pause_metered = true;
        assert!(apply_engine_command(&format!("dle:set:{}", serde_json::to_string(&q).unwrap()), &mut p));
        assert_eq!(p, q);
        assert!(!apply_engine_command("dle:flip:nothing", &mut p));
        assert_eq!(kbps_of("500"), Some(500));
        assert_eq!(kbps_of("2 MB"), Some(2048));
        assert_eq!(kbps_of(""), Some(0));
        assert_eq!(kbps_of("fast"), None);
    }
}

/// A file picker for lists of links (run with the window's state released).
pub(super) fn pick_list() -> Option<PathBuf> {
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::{FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH};
    unsafe {
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let filters = [COMDLG_FILTERSPEC { pszName: w!("Lists of links"), pszSpec: w!("*.txt;*.lst;*.list;*.csv") }, COMDLG_FILTERSPEC { pszName: w!("All files"), pszSpec: w!("*.*") }];
        let _ = dialog.SetFileTypes(&filters);
        let _ = dialog.SetTitle(w!("Import a list of links"));
        dialog.Show(None).ok()?;
        let name = dialog.GetResult().ok()?.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path = PathBuf::from(name.to_string().unwrap_or_default());
        CoTaskMemFree(Some(name.0 as *const std::ffi::c_void));
        (!path.as_os_str().is_empty()).then_some(path)
    }
}

impl Manager {
    /// STASH_VIEW=list:0:0:form-<name>: one of these forms, for screenshots.
    pub(super) fn debug_form(&mut self, which: &str) {
        let first = self.jobs.first().map(|j| j.id).unwrap_or(0);
        match which {
            "more" => {
                self.open_add(Some("https://example.com/photos/img[001-120].jpg".into()));
                self.open_more_options();
            }
            "add" => self.open_add(Some("https://example.com/photos/img[001-120].jpg https://example.com/a.zip".into())),
            "signin" => self.open_sign_in(first, "files.example.com:443"),
            "limit" => self.open_limit(vec![first]),
            "refresh" => self.open_refresh(first),
            "countdown" => self.open_countdown(WhenDone::Quit),
            "dup" => {
                let mut f = Form::new("Already downloaded", "\u{201c}report.pdf\u{201d} is already on your PC.", Purpose::Duplicate { links: Vec::new(), existing: PathBuf::new(), req: AddRequest::default() });
                f.buttons = vec![("Show Existing".into(), crate::ui::Kind::Secondary), ("Download Again".into(), crate::ui::Kind::Primary)];
                self.open_form(f);
            }
            other => self.open_engine_form(other),
        }
    }
}
