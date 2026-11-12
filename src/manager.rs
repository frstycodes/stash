//! The downloads window, on one Mica surface: a sidebar of filters, the list of
//! downloads and their history (grouped by day, searchable), a dock at the bottom with
//! the live speed and the actions, the Settings page, and the New Download and
//! Remove sheets.
//!
//! Same approach as the stack: the Windows compositor shows retained visuals,
//! Direct2D paints them only when something changes, and anything that moves
//! (progress bars, the selection, the dock's actions) moves on the compositor.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use windows::Foundation::TimeSpan;
use windows::UI::Color;
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{AnimationIterationBehavior, CompositionBrush, CompositionLinearGradientBrush, CompositionStretch, CompositionSurfaceBrush, ContainerVisual, CubicBezierEasingFunction, SpriteVisual, Visual};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_COLOR_F};
use windows::Win32::Graphics::Direct2D::{D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC};
use windows::Win32::Graphics::Dwm::{DWMWINDOWATTRIBUTE, DwmExtendFrameIntoClientArea, DwmSetWindowAttribute};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Ole::{IDropTarget, RegisterDragDrop};
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, Interface, PCWSTR, Result, w};
use windows_numerics::{Vector2, Vector3};

use crate::blur;
use crate::dl::{self, AddRequest, JobView, Kind, Priority, State};
use crate::gfx::{Gfx, Pixels};
use crate::icons;
use crate::paint::{self, MEDIUM, P, REGULAR, SEMIBOLD};
use crate::popup;
use crate::search::{self, Completion, When};
use crate::settings::Settings;
use crate::settings_page::{self, SettingsPage, Values};
use crate::shell;

mod forms;
pub use forms::{apply_engine_command, queue_finished, start_countdown};

// ---------- geometry (logical pixels) ----------

const SIDEBAR: f32 = 212.0;
const HEAD: f32 = 66.0;
const ROW: f32 = 66.0;
const GROUP: f32 = 38.0;
const CAPTION_BUTTONS: f32 = 150.0;
// our own minimize / maximize / close (Windows' own misplace themselves when maximized)
const CAP_W: f32 = 46.0;
const CAP_H: f32 = 32.0;
const WM_NCMOUSELEAVE: u32 = 0x02A2;
const UPDATE_CARD: f32 = 100.0;
const DOCK_H: f32 = 44.0;
const DEFAULT_W: i32 = 920;
const DEFAULT_H: i32 = 600;
const MIN_W: i32 = 720;
const MIN_H: i32 = 440;
const SHEET_W: f32 = 500.0;
const ADD_H: f32 = 262.0;
const REMOVE_H: f32 = 268.0;
const FIELD_TEXT: f32 = 13.5;
const OPT_H: f32 = 50.0;

const WM_MOUSELEAVE: u32 = 0x02A3;
const T_TOAST: usize = 2;
const T_DOCK: usize = 3;
const T_TIP: usize = 4;
const T_VIEW: usize = 5; // STASH_VIEW's menu or tooltip, once the window is up

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Filter {
    All,
    Downloading,
    Completed,
}

const FILTERS: [(Filter, &str, &str); 3] =
    [(Filter::All, "All Downloads", icons::LIST), (Filter::Downloading, "Downloading", icons::DOWNLOAD), (Filter::Completed, "Completed", icons::CHECK)];

impl Filter {
    fn keeps(self, j: &JobView) -> bool {
        match self {
            Filter::All => true,
            Filter::Downloading => !j.state.finished(),
            Filter::Completed => j.state.finished(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Page {
    List(Filter),
    Settings,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RowBtn {
    Toggle,
    Cancel,
    Again,
    Reveal,
}

#[derive(Clone, PartialEq, Debug)]
enum Hit {
    None,
    Caption,
    CapMin,
    CapMax,
    CapClose,
    UpdateCard,
    Filter(usize),
    Settings,
    Search,
    SearchClear,
    Chip(usize),
    Option(usize),
    Row(u64),
    RowBtn(u64, RowBtn),
    Dock,
    DockTool(usize),
    DockAdd,
    EmptyCta,
    Page(settings_page::Hit),
    // the right-click menu, and anywhere outside it while it's open
    Menu(popup::MHit),
    MenuOff,
    // sheets
    Sheet,
    Backdrop,
    Field,
    Folder,
    Torrent,
    Prio(Priority),
    Cancel,
    Ok,
    AlsoDelete,
    /// the download engine's form sheets (and the New Download sheet's extra links)
    Form(forms::FHit),
    /// The Choose Files sheet (see torrent_sheet).
    Files(crate::torrent_sheet::FHit),
}

/// A single-line text field: the search box and the New Download link field.
#[derive(Default)]
struct Field {
    text: Vec<char>,
    caret: usize,
    all: bool,
    scroll_x: f32,
}

impl Field {
    fn string(&self) -> String {
        self.text.iter().collect()
    }

    fn set(&mut self, s: &str) {
        self.text = s.chars().collect();
        self.caret = self.text.len();
        self.all = false;
    }

    fn insert(&mut self, s: &str) {
        if self.all {
            self.text.clear();
            self.caret = 0;
            self.all = false;
        }
        for ch in s.chars() {
            self.text.insert(self.caret, ch);
            self.caret += 1;
        }
    }

    /// The word under the caret (for completions) and where it starts.
    fn token(&self) -> (usize, String) {
        let mut start = self.caret;
        while start > 0 && !self.text[start - 1].is_whitespace() {
            start -= 1;
        }
        (start, self.text[start..self.caret].iter().collect())
    }

    /// Editing keys. True if the text or caret changed.
    fn key(&mut self, vk: u16, ctrl: bool) -> bool {
        let n = self.text.len();
        match vk {
            v if v == VK_BACK.0 => {
                if self.all {
                    self.text.clear();
                    self.caret = 0;
                } else if ctrl {
                    let mut i = self.caret;
                    while i > 0 && self.text[i - 1] == ' ' {
                        i -= 1;
                    }
                    while i > 0 && !matches!(self.text[i - 1], ' ' | '/' | '.' | '?' | '&' | '=' | ':') {
                        i -= 1;
                    }
                    self.text.drain(i..self.caret);
                    self.caret = i;
                } else if self.caret > 0 {
                    self.caret -= 1;
                    self.text.remove(self.caret);
                }
                self.all = false;
            }
            v if v == VK_DELETE.0 => {
                if self.all {
                    self.text.clear();
                    self.caret = 0;
                } else if self.caret < n {
                    self.text.remove(self.caret);
                }
                self.all = false;
            }
            v if v == VK_LEFT.0 => {
                self.caret = if self.all { 0 } else { self.caret.saturating_sub(1) };
                self.all = false;
            }
            v if v == VK_RIGHT.0 => {
                self.caret = if self.all { n } else { (self.caret + 1).min(n) };
                self.all = false;
            }
            v if v == VK_HOME.0 => {
                self.caret = 0;
                self.all = false;
            }
            v if v == VK_END.0 => {
                self.caret = n;
                self.all = false;
            }
            v if ctrl && v == b'A' as u16 => self.all = n > 0,
            v if ctrl && (v == b'C' as u16 || v == b'X' as u16) => {
                if self.all || v == b'C' as u16 {
                    shell::set_clipboard_text(&self.string());
                }
                if self.all && v == b'X' as u16 {
                    self.text.clear();
                    self.caret = 0;
                    self.all = false;
                }
            }
            v if ctrl && v == b'V' as u16 => {
                if let Some(t) = shell::clipboard_text() {
                    self.insert(&t.split_whitespace().collect::<Vec<_>>().join(" "));
                }
            }
            _ => return false,
        }
        true
    }
}

/// One row on the list, and its visuals.
struct RowVis {
    id: u64,
    v: ContainerVisual,
    sprite: SpriteVisual,
    fill: SpriteVisual,
    rest: SpriteVisual,
    key: String,
    frac: f32,
    bar_w: f32, // the bar's width when it was laid out; a resize lays it out again
    y: f32,
    icon_v: SpriteVisual, // the file's icon, on its own so a new one can ease in
    icon_id: String,      // which icon it shows (the file's own, or its type's)
    icon_alpha: f32,
}

/// What sits in the list: group labels and downloads, in order.
#[derive(Clone)]
enum Line {
    Group(String),
    Job(JobView),
}

enum SheetKind {
    Add { field: Field, dir: PathBuf, priority: Priority, error: Option<String> },
    /// `name` and `path` describe the first; `ids.len() > 1` asks about all of them.
    /// `permanent`: Shift+Delete, which deletes the files for good after asking.
    /// `cancel`: stopping several unfinished downloads (their partial files go too).
    Remove { ids: Vec<u64>, name: String, path: String, delete: bool, permanent: bool, cancel: bool },
    /// The download engine's small forms (sign-in, speed limit, settings...).
    Form(Box<forms::Form>),
    /// Which of a torrent's files to download.
    Files(crate::torrent_sheet::FilesSheet),
}

/// The right-click menu while it's open: for `ids` (the selection), at (x, y) in DIPs.
struct MenuState {
    model: popup::Menu,
    x: f32,
    y: f32,
    hover: Option<popup::MHit>,
    ids: Vec<u64>,
}

struct Sheet {
    kind: SheetKind,
    root: ContainerVisual,
    card: SpriteVisual,
    caret: SpriteVisual,
    switch: Option<(SpriteVisual, SpriteVisual)>, // the Remove sheet's switch (live, like Settings')
    slider: Option<(SpriteVisual, Priority)>,      // the New Download sheet's priority selection
}

/// Work that runs a nested message loop, done with the state borrow released.
enum Blocking {
    PickFolder,
    PickTorrent,
    ImportList,
}

pub struct Manager {
    gfx: Gfx,
    hwnd: HWND,
    _target: DesktopWindowTarget,
    _drop: Option<IDropTarget>,
    root: ContainerVisual,
    backdrop: SpriteVisual,
    sidebar: SpriteVisual,
    sel: SpriteVisual,
    header: SpriteVisual,
    search_caret: SpriteVisual,
    chips: SpriteVisual,
    list: SpriteVisual,      // shows `list_src`, fading out under the header and the dock
    list_src: ContainerVisual,
    blurs: Vec<SpriteVisual>, // blurred copies of the list under the header and the dock
    page_view: SpriteVisual,      // the settings page, through the same mask
    page_blurs: Vec<SpriteVisual>,
    inner: ContainerVisual,  // scrolls
    empty: SpriteVisual,
    dock: ContainerVisual,
    dock_status: SpriteVisual,
    dock_tools: Vec<SpriteVisual>,
    dock_add: SpriteVisual,
    caption: SpriteVisual,
    cap_press: Option<u32>,
    dropdown: SpriteVisual,
    toast: SpriteVisual,
    page: SettingsPage,

    k: f32,
    w: f32,
    h: f32,
    mica: bool,
    view: Page,
    jobs: Vec<JobView>,
    version: u64,
    lines: Vec<Line>,
    marks: HashMap<u64, Vec<usize>>,
    missing: HashMap<u64, bool>,
    selected: Option<u64>, // the focused row (arrow keys move it)
    picked: Vec<u64>,      // every selected row: Ctrl+click toggles, Shift+click extends
    anchor: Option<u64>,   // where Shift+click ranges from
    menu: Option<MenuState>,
    menu_v: SpriteVisual,
    tip: SpriteVisual,
    tip_on: bool,
    view_later: Option<String>,
    tip_off_at: std::time::Instant, // when a tooltip last went away: the next one shows at once
    hover: Hit,
    scroll: f32,
    rows: Vec<RowVis>,
    dying: Vec<(ContainerVisual, std::time::Instant)>, // rows fading out after leaving the list
    entering: bool, // new rows rise in (off while scrolling brings rows into view)
    groups: Vec<SpriteVisual>,
    icons: HashMap<String, Option<Pixels>>,
    file_icons: HashMap<PathBuf, Option<Pixels>>, // finished files' own icons and thumbnails
    icon_pending: std::collections::HashSet<PathBuf>,
    thumbs: std::sync::mpsc::Sender<crate::thumbs::Job>,
    search: Field,
    other_search: Field,
    search_focus: bool,
    options: Vec<Completion>,
    pick: usize,
    sheet: Option<Sheet>,
    tracking: bool,
    dock_open: bool,
    speeds: Vec<f32>,
    settings: Settings,
    toast_text: String,
}

thread_local! {
    static MGR: RefCell<Option<Manager>> = const { RefCell::new(None) };
}
/// The window, for the icon thread to wake, and what it has delivered.
static MGR_HWND: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
static THUMBS_DONE: std::sync::Mutex<Vec<crate::thumbs::Done>> = std::sync::Mutex::new(Vec::new());
const WM_APP_THUMBS: u32 = WM_APP + 40;

fn with<R>(f: impl FnOnce(&mut Manager) -> R) -> Option<R> {
    MGR.with(|m| m.try_borrow_mut().ok()?.as_mut().map(f))
}

fn ms(v: i64) -> TimeSpan {
    TimeSpan { Duration: v * 10_000 }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

/// Seconds to add to UTC for local time (so "today" is the user's today).
fn local_offset() -> i64 {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::Storage::FileSystem::FileTimeToLocalFileTime;
    let t = (now() + 11_644_473_600) * 10_000_000;
    let utc = FILETIME { dwLowDateTime: t as u32, dwHighDateTime: (t >> 32) as u32 };
    let mut local = FILETIME::default();
    unsafe {
        if FileTimeToLocalFileTime(&utc, &mut local).is_err() {
            return 0;
        }
    }
    let l = ((local.dwHighDateTime as u64) << 32) | local.dwLowDateTime as u64;
    (l as i64 - t as i64) / 10_000_000
}

fn age_days(ts: u64) -> i64 {
    let off = local_offset();
    let day = |t: i64| (t + off).div_euclid(86_400);
    day(now() as i64) - day(ts as i64)
}

// ---------- the public face (called from the stack) ----------

/// Show the window (creating it the first time). `links` opens the New Download sheet
/// with them filled in.
pub fn show(gfx: &Gfx, settings: &Settings, links: Option<Vec<String>>) {
    let exists = MGR.with(|m| m.borrow().is_some());
    if !exists {
        match Manager::create(gfx.clone(), settings.clone()) {
            Ok(m) => MGR.with(|cell| *cell.borrow_mut() = Some(m)),
            Err(e) => {
                crate::stash::log(format!("downloads window failed: {e}"));
                return;
            }
        }
        with(|m| m.layout());
    }
    // show it with the state released: restoring sends WM_SIZE, which needs it
    let Some(hwnd) = with(|m| m.hwnd) else { return };
    unsafe {
        let _ = ShowWindow(hwnd, if IsIconic(hwnd).as_bool() { SW_RESTORE } else { SW_SHOW });
        // a process's first ShowWindow takes its launcher's show state; insist
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let _ = SetForegroundWindow(hwnd);
    }
    with(|m| {
        let _ = m.rise_in();
        m.missing.clear(); // files may have moved since
        m.on_snapshot(true);
        if let Some(links) = links {
            m.open_add(Some(links.join(" ")));
        }
        // STASH_VIEW=settings, or STASH_VIEW=search:<text>, opens the window there (for screenshots)
        if let Ok(view) = std::env::var("STASH_VIEW") {
            if let Some(rest) = view.strip_prefix("settings") {
                m.set_view(Page::Settings);
                // "settings/speed" searches the settings
                if let Some((_, q)) = rest.split_once('/') {
                    m.focus_search(true);
                    m.search.set(q);
                    m.on_query_changed();
                }
                // "settings:400" scrolls the page down
                if let Some(px) = rest.strip_prefix(':').and_then(|v| v.parse::<f32>().ok()) {
                    m.page.scroll_by(px);
                }
            } else if let Some(rest) = view.strip_prefix("list:") {
                // "list:<scroll>:<rows selected>:<menu|tip|>" for checking the list's states
                let parts: Vec<&str> = rest.split(':').collect();
                m.scroll = parts.first().and_then(|v| v.parse().ok()).unwrap_or(0.0);
                m.clamp_scroll(false);
                let n: usize = parts.get(1).and_then(|v| v.parse().ok()).unwrap_or(0);
                let ids = m.list_ids();
                m.picked = ids.iter().take(n).copied().collect();
                m.selected = m.picked.first().copied();
                m.anchor = m.selected;
                m.on_selection();
                // the menu and tooltip wait until the window has settled in front
                if let Some(later) = parts.get(2).filter(|v| !v.is_empty()) {
                    m.view_later = Some(later.to_string());
                    unsafe {
                        let _ = SetTimer(Some(m.hwnd), T_VIEW, 3000, None);
                    }
                }
            } else if let Some(q) = view.strip_prefix("search:") {
                m.focus_search(true);
                m.search.set(q);
                m.on_query_changed();
            }
        }
    });
}

impl Manager {
    /// STASH_VIEW's menu or tooltip (see `show`).
    fn view_later(&mut self) {
        let Some(later) = self.view_later.take() else { return };
        match later.as_str() {
            "menu" => {
                let i = self.lines.iter().position(|l| matches!(l, Line::Job(_))).unwrap_or(0);
                let y = self.list_top() + self.line_y(i) - self.scroll + 30.0;
                self.open_menu(SIDEBAR + 260.0, y);
            }
            "delete" => {
                // the Shift+Delete sheet for the first finished download whose file is there
                let id = self.jobs.iter().find(|j| j.state.finished() && j.path.as_ref().is_some_and(|p| p.exists())).map(|j| j.id);
                if let Some(id) = id {
                    self.ask_delete(vec![id]);
                }
            }
            "add" => self.open_add(Some("https://example.com/files/big-archive.zip".into())),
            // the download engine's forms ("form-proxy", "form-signin"...)
            other if other.starts_with("form-") => self.debug_form(&other[5..]),
            "cancel" => {
                // the Cancel Downloads sheet, as for two downloads stopped part-way
                let ids: Vec<u64> = self.jobs.iter().take(2).map(|j| j.id).collect();
                let name = format!("{}  and 1 more", self.jobs.first().map(|j| j.name.clone()).unwrap_or_default());
                self.open_sheet(SheetKind::Remove { ids, name, path: "412 MB downloaded so far".into(), delete: false, permanent: false, cancel: true });
            }
            "remove" => {
                // the Remove sheet for the first finished download whose file is there
                let id = self.jobs.iter().find(|j| j.state.finished() && j.path.as_ref().is_some_and(|p| p.exists())).map(|j| j.id);
                if let Some(id) = id {
                    self.ask_remove(vec![id]);
                }
            }
            "tip" => {
                self.hover = Hit::DockTool(1);
                self.set_dock_open(true);
                self.show_tip();
            }
            _ => {}
        }
    }
}

/// The update status changed: the sidebar card and the About section follow.
pub fn refresh_update() {
    with(|m| {
        let _ = m.paint_sidebar();
        if m.view == Page::Settings {
            m.build_page(false);
        }
        if let crate::update::Status::Ready { version, .. } = crate::update::status() {
            m.show_toast(&format!("Stash {version} is ready. Restart to update."));
        }
    });
}

/// What the sidebar's update card says: title, detail, and whether it can install now.
fn update_card() -> Option<(String, String, bool)> {
    match crate::update::status() {
        crate::update::Status::Downloading(v) => Some(("Updating".into(), format!("Downloading Stash {v}…"), false)),
        crate::update::Status::Ready { version, .. } => Some(("Update ready".into(), format!("Stash {version} is downloaded."), true)),
        _ => None,
    }
}

/// A torrent is waiting for its files to be chosen: bring up the window and ask.
/// (Not over another sheet: it asks again with the next news.)
pub fn ask_files(gfx: &Gfx, settings: &Settings, id: u64) {
    show(gfx, settings, None);
    let shown = with(|m| {
        if m.sheet.is_some() {
            return false;
        }
        m.on_snapshot(false);
        m.open_files(id, true)
    });
    if shown != Some(true) {
        crate::torrent_sheet::unask(id);
    }
}

/// The engine has news.
pub fn refresh() {
    with(|m| {
        if unsafe { IsWindowVisible(m.hwnd).as_bool() && !IsIconic(m.hwnd).as_bool() } {
            m.on_snapshot(false);
        }
    });
}

pub fn set_settings(s: &Settings) {
    with(|m| {
        m.settings = s.clone();
        m.apply_theme();
        paint::set_units(s.downloads.bits, s.downloads.binary);
        if m.view == Page::Settings {
            m.build_page(true);
        }
        let _ = m.paint_sidebar();
        m.force_repaint();
    });
}

/// Where new downloads go: the Downloads folder in the stack (which can be moved).
pub fn default_dir(s: &Settings) -> PathBuf {
    s.downloads.folder.clone().filter(|f| f.is_dir()).unwrap_or_else(shell::downloads_dir)
}

/// A "dl:<action>:<id>" menu command (a row's context menu, or the stack's).
pub fn command(cmd: &str) {
    let mut parts = cmd.splitn(3, ':');
    let (_, action, id) = (parts.next(), parts.next().unwrap_or(""), parts.next().and_then(|i| i.parse::<u64>().ok()));
    let Some(id) = id else { return };
    let e = dl::engine();
    let snap = e.snapshot();
    let job = snap.jobs.iter().find(|j| j.id == id);
    match action {
        "open" => {
            if let Some(j) = job {
                match (&j.path, j.state.finished()) {
                    (Some(p), true) if p.exists() => shell::open(p),
                    _ => shell::open(&j.dir),
                }
            }
        }
        "reveal" => {
            if let Some(j) = job {
                match &j.path {
                    Some(p) if p.exists() => shell::reveal(p),
                    _ => shell::open(&j.dir),
                }
            }
        }
        "pause" => e.pause(id),
        "resume" => e.resume(id),
        "now" => e.start_now(id),
        "again" => e.again(id),
        "prio-0" => e.set_priority(id, Priority::Low),
        "prio-1" => e.set_priority(id, Priority::Normal),
        "prio-2" => e.set_priority(id, Priority::High),
        "copy" => {
            if let Some(j) = job {
                shell::set_clipboard_text(&j.source);
                with(|m| m.show_toast("Download link copied"));
            }
        }
        "source" => {
            if let Some(j) = job {
                let page = j.referrer.clone().unwrap_or_else(|| format!("https://{}", search::site(&j.source)));
                if page.starts_with("http") {
                    shell::open(std::path::Path::new(&page));
                }
            }
        }
        "remove" => {
            // a finished download whose file is still there: ask about the file first
            let ask = job.is_some_and(|j| j.state.finished() && j.path.as_ref().is_some_and(|p| p.exists()));
            if ask {
                with(|m| m.open_remove(id));
            } else {
                e.remove(id, !job.is_some_and(|j| j.state.finished()));
            }
        }
        "delete" => e.remove(id, true),
        _ => {}
    }
}

impl Manager {
    fn create(gfx: Gfx, settings: Settings) -> Result<Self> {
        paint::set_units(settings.downloads.bits, settings.downloads.binary);
        let hwnd = unsafe {
            let instance = GetModuleHandleW(None)?;
            let icon = LoadImageW(Some(instance.into()), PCWSTR(1 as _), IMAGE_ICON, 0, 0, LR_DEFAULTSIZE | LR_SHARED).ok().map(|h| HICON(h.0));
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                style: CS_DBLCLKS,
                lpfnWndProc: Some(wndproc),
                hInstance: instance.into(),
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                hIcon: icon.unwrap_or_default(),
                lpszClassName: w!("StashDownloads"),
                ..Default::default()
            };
            RegisterClassExW(&wc);
            let wa = shell::work_area();
            let k = windows::Win32::UI::HiDpi::GetDpiForSystem() as f32 / 96.0;
            let (lw, lh) = settings.downloads.window.unwrap_or((DEFAULT_W, DEFAULT_H));
            let (pw, ph) = (((lw as f32 * k) as i32).min(wa.right - wa.left), ((lh as f32 * k) as i32).min(wa.bottom - wa.top));
            CreateWindowExW(
                WS_EX_NOREDIRECTIONBITMAP | WS_EX_APPWINDOW,
                w!("StashDownloads"),
                w!("Stash"),
                WINDOW_STYLE(WS_OVERLAPPEDWINDOW.0 & !WS_SYSMENU.0),
                wa.left + (wa.right - wa.left - pw) / 2,
                wa.top + (wa.bottom - wa.top - ph) / 2,
                pw,
                ph,
                None,
                None,
                Some(instance.into()),
                None,
            )?
        };
        MGR_HWND.store(hwnd.0 as isize, std::sync::atomic::Ordering::Relaxed);
        // Dark frame, Mica over the whole window (Windows 11), the caption folded into it.
        let mica = unsafe {
            let dark: u32 = (!settings.theme.is_light()) as u32;
            let _ = DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(20), &dark as *const _ as _, 4);
            let backdrop: u32 = 2; // DWMSBT_MAINWINDOW
            let ok = DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(38), &backdrop as *const _ as _, 4).is_ok();
            let _ = DwmExtendFrameIntoClientArea(hwnd, &MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 });
            let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER);
            ok
        };
        let c = &gfx.compositor;
        let target = gfx.target(hwnd)?;
        let root = c.CreateContainerVisual()?;
        target.SetRoot(&root)?;
        let sprite = || c.CreateSpriteVisual();
        let (backdrop, sidebar, sel, header, search_caret, chips, list, empty) = (sprite()?, sprite()?, sprite()?, sprite()?, sprite()?, sprite()?, sprite()?, sprite()?);
        let (dock_status, dock_add, dropdown, toast) = (sprite()?, sprite()?, sprite()?, sprite()?);
        let caption = sprite()?;
        let dock_tools = vec![sprite()?, sprite()?, sprite()?];
        let list_src = c.CreateContainerVisual()?;
        let inner = c.CreateContainerVisual()?;
        let dock = c.CreateContainerVisual()?;
        list_src.Children()?.InsertAtTop(&inner)?;
        list_src.Children()?.InsertAtTop(&empty)?;
        for v in [&dock_status, &dock_add] {
            dock.Children()?.InsertAtTop(v)?;
        }
        for t in &dock_tools {
            dock.Children()?.InsertAtTop(t)?;
            t.SetOpacity(0.0)?;
        }
        let blurs = vec![sprite()?, sprite()?, sprite()?];
        let (page_view, page_blurs) = (sprite()?, vec![sprite()?, sprite()?, sprite()?]);
        let (menu_v, tip) = (sprite()?, sprite()?);
        let page = SettingsPage::new(&gfx)?;
        // one material: Mica edge to edge (Windows 10 gets one solid color)
        paint::set_light(settings.theme.is_light());
        backdrop.SetBrush(&c.CreateColorBrushWithColor(backdrop_color(mica))?)?;
        for v in [
            backdrop.cast::<Visual>()?,
            sel.cast()?,
            sidebar.cast()?,
            list.cast()?,
            blurs[0].cast()?,
            blurs[1].cast()?,
            blurs[2].cast()?,
            page_view.cast()?,
            page_blurs[0].cast()?,
            page_blurs[1].cast()?,
            page_blurs[2].cast()?,
            header.cast()?,
            search_caret.cast()?,
            chips.cast()?,
            dock.cast()?,
            dropdown.cast()?,
            toast.cast()?,
            caption.cast()?,
            menu_v.cast()?,
            tip.cast()?,
        ] {
            root.Children()?.InsertAtTop(&v)?;
        }
        menu_v.SetOpacity(0.0)?;
        tip.SetOpacity(0.0)?;
        let k0 = unsafe { windows::Win32::UI::HiDpi::GetDpiForSystem() } as f32 / 96.0;
        for (v, small) in [(&menu_v, false), (&dropdown, false), (&toast, false), (&tip, true)] {
            v.SetShadow(&popover_shadow(&gfx, k0, small)?)?;
        }
        page_view.SetIsVisible(false)?;
        for v in &page_blurs {
            v.SetIsVisible(false)?;
        }
        dropdown.SetOpacity(0.0)?;
        toast.SetOpacity(0.0)?;
        // the caret blinks on the compositor
        search_caret.SetBrush(&c.CreateColorBrushWithColor(Color { A: 255, R: 242, G: 125, B: 46 })?)?;
        search_caret.StartAnimation(&HSTRING::from("Opacity"), &blink(&gfx)?)?;
        search_caret.SetIsVisible(false)?;

        let drop = crate::droptarget::DropTarget::new(hwnd, |e| {
            if let crate::droptarget::Event::Drop { paths, .. } = e {
                with(|m| m.on_drop(paths));
            }
        });
        unsafe {
            let _ = RegisterDragDrop(hwnd, &drop);
        }

        Ok(Manager {
            gfx,
            hwnd,
            _target: target,
            _drop: Some(drop),
            root,
            backdrop,
            sidebar,
            sel,
            header,
            search_caret,
            chips,
            list,
            list_src,
            blurs,
            page_view,
            page_blurs,
            inner,
            empty,
            dock,
            dock_status,
            dock_tools,
            dock_add,
            caption,
            cap_press: None,
            dropdown,
            toast,
            page,
            k: 1.0,
            w: 0.0,
            h: 0.0,
            mica,
            view: Page::List(Filter::All),
            jobs: Vec::new(),
            version: u64::MAX,
            lines: Vec::new(),
            marks: HashMap::new(),
            missing: HashMap::new(),
            selected: None,
            picked: Vec::new(),
            anchor: None,
            menu: None,
            menu_v,
            tip,
            tip_on: false,
            view_later: None,
            tip_off_at: std::time::Instant::now() - std::time::Duration::from_secs(10),
            hover: Hit::None,
            scroll: 0.0,
            rows: Vec::new(),
            dying: Vec::new(),
            entering: true,
            groups: Vec::new(),
            icons: HashMap::new(),
            file_icons: HashMap::new(),
            icon_pending: Default::default(),
            thumbs: crate::thumbs::spawn_worker(move |d| {
                THUMBS_DONE.lock().unwrap().push(d);
                unsafe {
                    let _ = PostMessageW(Some(HWND(MGR_HWND.load(std::sync::atomic::Ordering::Relaxed) as *mut _)), WM_APP_THUMBS, WPARAM(0), LPARAM(0));
                }
            }),
            search: Field::default(),
            other_search: Field::default(),
            search_focus: false,
            options: Vec::new(),
            pick: 0,
            sheet: None,
            tracking: false,
            dock_open: false,
            speeds: vec![0.0; 40],
            settings,
            toast_text: String::new(),
        })
    }

    /// Light or dark, from the setting (or Windows'): Mica and the frame switch with it,
    /// and everything repaints.
    fn apply_theme(&mut self) {
        let light = self.settings.theme.is_light();
        if light == paint::light() {
            return;
        }
        paint::set_light(light);
        unsafe {
            let dark: u32 = (!light) as u32;
            let _ = DwmSetWindowAttribute(self.hwnd, DWMWINDOWATTRIBUTE(20), &dark as *const _ as _, 4);
        }
        let _ = self.gfx.compositor.CreateColorBrushWithColor(backdrop_color(self.mica)).and_then(|b| self.backdrop.SetBrush(&b));
        self.layout();
        self.force_repaint();
        if self.sheet.is_some() {
            let _ = self.paint_sheet();
        }
        let _ = self.paint_menu();
    }

    fn back(&self) -> Result<CubicBezierEasingFunction> {
        self.gfx.compositor.CreateCubicBezierEasingFunction(Vector2 { X: 0.3, Y: 1.18 }, Vector2 { X: 0.6, Y: 1.0 })
    }

    fn ease_out(&self) -> Result<CubicBezierEasingFunction> {
        self.gfx.compositor.CreateCubicBezierEasingFunction(Vector2 { X: 0.23, Y: 1.0 }, Vector2 { X: 0.32, Y: 1.0 })
    }

    fn sprite_from(&self, v: &SpriteVisual, w: f32, h: f32, draw: impl FnOnce(&P)) -> Result<()> {
        let k = self.k;
        let dw = self.gfx.dwrite.clone();
        let surface = self.gfx.surface(w * k, h * k, |dc| unsafe {
            dc.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            draw(&P { dc, dw: &dw, k });
        })?;
        let brush = self.gfx.compositor.CreateSurfaceBrushWithSurface(&surface)?;
        brush.SetStretch(CompositionStretch::Fill)?;
        v.SetBrush(&brush)?;
        v.SetSize(Vector2 { X: (w * k).ceil(), Y: (h * k).ceil() })
    }

    fn cw(&self) -> f32 {
        self.w - SIDEBAR
    }

    fn list_top(&self) -> f32 {
        HEAD + if search::parse(&self.search.string()).chips.is_empty() { 0.0 } else { 36.0 }
    }

    fn search_rect(&self) -> (f32, f32, f32, f32) {
        let w = (self.cw() * 0.42).clamp(200.0, 340.0);
        (self.w - CAPTION_BUTTONS - w, 15.0, w, 36.0)
    }

    fn dock_y(&self) -> f32 {
        self.h - 16.0 - DOCK_H
    }

    // ---------- layout ----------

    fn layout(&mut self) {
        let mut r = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut r);
        }
        if r.right - r.left < 1 || r.bottom - r.top < 1 {
            return; // minimized
        }
        self.k = shell::scale_factor(self.hwnd);
        self.w = (r.right - r.left) as f32 / self.k;
        self.h = (r.bottom - r.top) as f32 / self.k;
        let k = self.k;
        let _ = self.backdrop.SetSize(Vector2 { X: self.w * k, Y: self.h * k });
        let _ = self.header.SetOffset(Vector3 { X: SIDEBAR * k, Y: 0.0, Z: 0.0 });
        let _ = self.place_page();
        let _ = self.dock.SetOffset(Vector3 { X: SIDEBAR * k, Y: self.dock_y() * k, Z: 0.0 });
        let _ = self.place_list();
        for r in &mut self.rows {
            r.key.clear();
        }
        let _ = self.paint_sidebar();
        let _ = self.move_sel(false);
        let _ = self.paint_caption();
        let _ = self.paint_header();
        let _ = self.paint_dock();
        if self.view == Page::Settings {
            self.build_page(false);
        } else {
            self.rebuild_list();
        }
        if self.sheet.is_some() {
            let _ = self.place_sheet();
            let _ = self.paint_sheet();
        }
    }

    /// The list fills the whole column, so rows scroll on under the header and the dock.
    /// There the list fades out while blurred copies of it fade in, blurring more toward
    /// the edge, so what's underneath stays faintly visible behind the title, the search
    /// and the dock. (Windows 10's compositor without visual surfaces gets a plain clip.)
    fn place_list(&self) -> Result<()> {
        let k = self.k;
        let c = &self.gfx.compositor;
        let top = self.list_top();
        let (w, h) = (self.cw() * k, self.h * k);
        self.list.SetOffset(Vector3 { X: SIDEBAR * k, Y: 0.0, Z: 0.0 })?;
        self.list.SetSize(Vector2 { X: w, Y: h })?;
        self.empty.SetOffset(Vector3 { X: 0.0, Y: top * k, Z: 0.0 })?;
        let made = scroll_mask(&self.gfx, &self.list_src, &self.list, &self.blurs, k, self.cw(), self.h, top, self.dock_y());
        if made.is_err() {
            // fall back: show the list directly, clipped below the header
            for v in &self.blurs {
                v.SetBrush(None)?;
            }
            self.list.SetBrush(None)?;
            if let Ok(ch) = self.root.Children() {
                let _ = ch.InsertAbove(&self.list_src, &self.list);
            }
            self.list_src.SetOffset(Vector3 { X: SIDEBAR * k, Y: 0.0, Z: 0.0 })?;
            self.list_src.SetSize(Vector2 { X: w, Y: h })?;
            let clip = c.CreateInsetClip()?;
            clip.SetTopInset(top * k)?;
            self.list_src.SetClip(&clip)?;
        }
        Ok(())
    }

    /// The settings page scrolls the same way, under the header and off the window's foot.
    fn place_page(&self) -> Result<()> {
        let k = self.k;
        let c = &self.gfx.compositor;
        self.page.root.SetSize(Vector2 { X: self.cw() * k, Y: self.h * k })?;
        if scroll_mask(&self.gfx, &self.page.root, &self.page_view, &self.page_blurs, k, self.cw(), self.h, HEAD, self.h).is_err() {
            for v in &self.page_blurs {
                v.SetBrush(None)?;
            }
            self.page_view.SetBrush(None)?;
            if let Ok(ch) = self.root.Children() {
                let _ = ch.InsertAbove(&self.page.root, &self.page_view);
            }
            self.page.root.SetOffset(Vector3 { X: SIDEBAR * k, Y: 0.0, Z: 0.0 })?;
            let clip = c.CreateInsetClip()?;
            clip.SetTopInset(HEAD * k)?;
            self.page.root.SetClip(&clip)?;
        }
        Ok(())
    }

    fn rise_in(&self) -> Result<()> {
        let c = &self.gfx.compositor;
        let o = c.CreateScalarKeyFrameAnimation()?;
        o.InsertKeyFrame(0.0, 0.0)?;
        o.InsertKeyFrame(1.0, 1.0)?;
        o.SetDuration(ms(200))?;
        self.root.StartAnimation(&HSTRING::from("Opacity"), &o)?;
        let a = c.CreateVector3KeyFrameAnimation()?;
        a.InsertKeyFrame(0.0, Vector3 { X: 0.0, Y: 14.0 * self.k, Z: 0.0 })?;
        a.InsertKeyFrameWithEasingFunction(1.0, Vector3 { X: 0.0, Y: 0.0, Z: 0.0 }, &self.back()?)?;
        a.SetDuration(ms(420))?;
        self.root.StartAnimation(&HSTRING::from("Offset"), &a)
    }

    // ---------- data ----------

    fn on_snapshot(&mut self, force: bool) {
        let snap = dl::engine().snapshot();
        self.check_prompts(&snap);
        let changed = snap.version != self.version;
        if !changed && !force {
            return;
        }
        let shape = |v: &[JobView]| v.iter().map(|j| (j.id, j.state, j.name.clone())).collect::<Vec<_>>();
        let structural = force || shape(&snap.jobs) != shape(&self.jobs);
        self.version = snap.version;
        self.jobs = snap.jobs;
        // the sparkline: when speed starts from nothing, start the line level instead of a cliff
        if self.speeds.iter().all(|v| *v == 0.0) && snap.down > 0 {
            self.speeds.iter_mut().for_each(|v| *v = snap.down as f32);
        }
        self.speeds.remove(0);
        self.speeds.push(snap.down as f32);
        // which finished files are gone (checked when the set changes, not every tick)
        for j in &self.jobs {
            if j.state.finished() && !self.missing.contains_key(&j.id) {
                self.missing.insert(j.id, j.path.as_ref().is_some_and(|p| !p.exists()));
            }
        }
        if self.selected.is_some_and(|id| !self.jobs.iter().any(|j| j.id == id)) {
            self.selected = None;
        }
        let jobs = &self.jobs;
        self.picked.retain(|id| jobs.iter().any(|j| j.id == *id));
        let _ = self.paint_dock();
        if structural {
            let _ = self.paint_sidebar();
            if self.view != Page::Settings {
                self.rebuild_list();
            }
        } else if self.view != Page::Settings {
            // same rows, fresh numbers
            let fresh: HashMap<u64, &JobView> = self.jobs.iter().map(|j| (j.id, j)).collect();
            for l in &mut self.lines {
                if let Line::Job(j) = l {
                    if let Some(f) = fresh.get(&j.id) {
                        *j = (*f).clone();
                    }
                }
            }
            let _ = self.sync_rows();
        }
    }

    fn is_missing(&self, j: &JobView) -> bool {
        self.missing.get(&j.id).copied().unwrap_or(false)
    }

    fn item<'a>(&self, j: &'a JobView, site: &'a str) -> search::Item<'a> {
        let ts = j.finished_at.unwrap_or(j.added);
        search::Item {
            name: &j.name,
            site,
            url: &j.source,
            kind: search::file_type(&j.name),
            size: j.total,
            when: When::from_age_days(age_days(ts)),
            missing: self.is_missing(j),
            failed: j.state == State::Failed,
            paused: matches!(j.state, State::Paused | State::Queued),
            downloading: j.state == State::Active,
            done: j.state.finished(),
            torrent: j.kind == Kind::Torrent,
        }
    }

    /// The downloads on show, grouped: in progress first, then finished ones by day.
    fn compute_lines(&mut self) {
        let Page::List(filter) = self.view else { return };
        let q = search::parse(&self.search.string());
        self.marks.clear();
        let mut kept: Vec<(f32, JobView)> = Vec::new();
        for j in &self.jobs {
            if !filter.keeps(j) {
                continue;
            }
            let site = search::site(j.referrer.as_deref().filter(|_| j.source.starts_with("magnet:")).unwrap_or(&j.source));
            if let Some(m) = search::matches(&self.item(j, &site), &q) {
                self.marks.insert(j.id, m.pos);
                kept.push((m.score, j.clone()));
            }
        }
        let searching = !q.words.is_empty();
        kept.sort_by(|(sa, a), (sb, b)| {
            if searching {
                sb.partial_cmp(sa).unwrap_or(std::cmp::Ordering::Equal)
            } else {
                let ta = a.finished_at.unwrap_or(a.added);
                let tb = b.finished_at.unwrap_or(b.added);
                tb.cmp(&ta).then(b.id.cmp(&a.id))
            }
        });
        let mut lines = Vec::new();
        let active: Vec<&JobView> = kept.iter().map(|(_, j)| j).filter(|j| !j.state.finished()).collect();
        if !active.is_empty() {
            lines.push(Line::Group(format!("In progress  ·  {}", active.len())));
            lines.extend(active.into_iter().cloned().map(Line::Job));
        }
        for when in [When::Today, When::Yesterday, When::Week, When::Month, When::Older] {
            let g: Vec<&JobView> = kept
                .iter()
                .map(|(_, j)| j)
                .filter(|j| j.state.finished() && When::from_age_days(age_days(j.finished_at.unwrap_or(j.added))) == when)
                .collect();
            if g.is_empty() {
                continue;
            }
            lines.push(Line::Group(format!("{}  ·  {}", when.label(), g.len())));
            lines.extend(g.into_iter().cloned().map(Line::Job));
        }
        self.lines = lines;
    }

    // ---------- caption buttons ----------

    fn paint_caption(&self) -> Result<()> {
        let k = self.k;
        let zoomed = unsafe { IsZoomed(self.hwnd).as_bool() };
        let active = unsafe { GetForegroundWindow() == self.hwnd };
        let (hover, press) = (self.hover.clone(), self.cap_press);
        self.sprite_from(&self.caption, CAP_W * 3.0, CAP_H, |p| {
            let ink = if active { paint::ink(0.9) } else { paint::ink(0.45) };
            for (i, (hit, code, glyph)) in [
                (Hit::CapMin, HTMINBUTTON, '\u{E921}'),
                (Hit::CapMax, HTMAXBUTTON, if zoomed { '\u{E923}' } else { '\u{E922}' }),
                (Hit::CapClose, HTCLOSE, '\u{E8BB}'),
            ]
            .into_iter()
            .enumerate()
            {
                let x = i as f32 * CAP_W;
                let on = hover == hit;
                let down = press == Some(code) && on;
                if hit == Hit::CapClose && on {
                    p.fill(x, 0.0, CAP_W, CAP_H, if down { paint::color(148, 18, 14, 1.0) } else { paint::color(196, 43, 28, 1.0) });
                } else if on {
                    p.fill(x, 0.0, CAP_W, CAP_H, paint::ink(if down { 0.06 } else { 0.09 }));
                }
                let c = if hit == Hit::CapClose && on { paint::color(255, 255, 255, 1.0) } else { ink };
                p.caption_glyph(glyph, x + CAP_W / 2.0, CAP_H / 2.0, 10.0, c);
            }
        })?;
        self.caption.SetOffset(Vector3 { X: (self.w - CAP_W * 3.0) * k, Y: 0.0, Z: 0.0 })
    }

    fn caption_hit(&self, x: f32, y: f32) -> Option<Hit> {
        if y >= CAP_H || x < self.w - CAP_W * 3.0 {
            return None;
        }
        Some(match ((x - (self.w - CAP_W * 3.0)) / CAP_W) as usize {
            0 => Hit::CapMin,
            1 => Hit::CapMax,
            _ => Hit::CapClose,
        })
    }

    // ---------- sidebar ----------

    fn paint_sidebar(&self) -> Result<()> {
        let counts: Vec<usize> = FILTERS.iter().map(|(f, _, _)| self.jobs.iter().filter(|j| f.keeps(j)).count()).collect();
        let (view, hover, h, mica) = (self.view, self.hover.clone(), self.h, self.mica);
        let card = update_card();
        self.sprite_from(&self.sidebar, SIDEBAR, h, |p| {
            let _ = mica;
            p.logo(30.0, 32.0, 24.0);
            p.text_mid("Stash", 50.0, 32.0, 16.0, SEMIBOLD, paint::text(), 140.0);
            for (i, (f, label, g)) in FILTERS.iter().enumerate() {
                let y = 64.0 + i as f32 * 38.0;
                let on = view == Page::List(*f);
                if !on && hover == Hit::Filter(i) {
                    p.round(10.0, y, SIDEBAR - 20.0, 34.0, 13.0, paint::ink(0.04));
                }
                p.glyph(g, 30.0, y + 17.0, 13.0, if on { paint::text() } else { paint::dim() });
                let (weight, ink) = if on { (MEDIUM, paint::text()) } else { (REGULAR, paint::ink(0.78)) };
                p.text_mid(label, 48.0, y + 17.0, 13.5, weight, ink, 110.0);
                if counts[i] > 0 {
                    p.text_right(&counts[i].to_string(), SIDEBAR - 22.0, y + 17.0, 12.0, REGULAR, paint::faint());
                }
            }
            // an update on its way or ready: a card above Settings
            if let Some((title, sub, ready)) = &card {
                let cy = h - 52.0 - 8.0 - UPDATE_CARD;
                p.round(10.0, cy, SIDEBAR - 20.0, UPDATE_CARD, 16.0, paint::ink(0.06));
                p.glyph(icons::DOWNLOAD, 30.0, cy + 22.0, 12.0, paint::accent());
                p.text_mid(title, 46.0, cy + 22.0, 13.0, SEMIBOLD, paint::text(), SIDEBAR - 66.0);
                p.text_mid(sub, 22.0, cy + 44.0, 12.0, REGULAR, paint::dim(), SIDEBAR - 44.0);
                if *ready {
                    let hot = hover == Hit::UpdateCard;
                    p.round(20.0, cy + 60.0, SIDEBAR - 40.0, 30.0, 15.0, if hot { paint::accent_hot() } else { paint::accent() });
                    let label = "Restart to Update";
                    let lw = p.measure(label, 12.5, SEMIBOLD);
                    p.text_mid(label, 20.0 + (SIDEBAR - 40.0 - lw) / 2.0, cy + 75.0, 12.5, SEMIBOLD, paint::color(255, 255, 255, 1.0), SIDEBAR - 40.0);
                }
            }
            let y = h - 52.0;
            let on = view == Page::Settings;
            if !on && hover == Hit::Settings {
                p.round(10.0, y, SIDEBAR - 20.0, 34.0, 13.0, paint::ink(0.04));
            }
            p.glyph(icons::SETTINGS, 30.0, y + 17.0, 13.0, if on { paint::text() } else { paint::dim() });
            p.text_mid("Settings", 48.0, y + 17.0, 13.5, if on { MEDIUM } else { REGULAR }, paint::text(), 100.0);
        })
    }

    /// The selection squircle glides to the current item.
    fn move_sel(&self, animate: bool) -> Result<()> {
        let k = self.k;
        let y = match self.view {
            Page::List(f) => 64.0 + FILTERS.iter().position(|x| x.0 == f).unwrap_or(0) as f32 * 38.0,
            Page::Settings => self.h - 52.0,
        };
        self.sel.SetBrush(&self.gfx.shape_brush(paint::ink(0.075), 34.0, 13.0, k)?)?;
        self.sel.SetSize(Vector2 { X: (SIDEBAR - 20.0) * k, Y: 34.0 * k })?;
        let to = Vector3 { X: 10.0 * k, Y: y * k, Z: 0.0 };
        if !animate {
            self.sel.StopAnimation(&HSTRING::from("Offset"))?;
            return self.sel.SetOffset(to);
        }
        let a = self.gfx.compositor.CreateVector3KeyFrameAnimation()?;
        a.InsertExpressionKeyFrame(0.0, &HSTRING::from("this.StartingValue"))?;
        a.InsertKeyFrameWithEasingFunction(1.0, to, &self.back()?)?;
        a.SetDuration(ms(420))?;
        self.sel.StartAnimation(&HSTRING::from("Offset"), &a)
    }

    // ---------- header: title and search ----------

    fn paint_header(&mut self) -> Result<()> {
        let cw = self.cw();
        let title = match self.view {
            Page::List(f) => FILTERS.iter().find(|x| x.0 == f).map_or("", |x| x.1),
            Page::Settings => "Settings",
        };
        let searching = true;
        let placeholder = if self.view == Page::Settings { "Search settings" } else { "Search downloads" };
        let (sx, sy, sw, sh) = self.search_rect();
        let sx = sx - SIDEBAR;
        let k = self.k;
        let dw = self.gfx.dwrite.clone();
        // keep the caret in view
        let text = self.search.string();
        let before: String = self.search.text[..self.search.caret].iter().collect();
        let caret_x = paint::advance(&dw, &before, FIELD_TEXT * k) / k;
        let field_w = sw - 34.0 - 30.0;
        if caret_x - self.search.scroll_x > field_w {
            self.search.scroll_x = caret_x - field_w;
        } else if caret_x < self.search.scroll_x {
            self.search.scroll_x = (caret_x - 20.0).max(0.0);
        }
        let (scroll_x, all, focus) = (self.search.scroll_x, self.search.all, self.search_focus);
        let hover = self.hover.clone();
        let full_w = paint::advance(&dw, &text, FIELD_TEXT * k) / k;
        self.sprite_from(&self.header.clone(), cw, HEAD, |p| {
            p.text_mid(title, 24.0, 33.0, 21.0, SEMIBOLD, paint::text(), (sx - 40.0).max(80.0));
            if !searching {
                return;
            }
            let bg = if focus { 0.09 } else if hover == Hit::Search { 0.075 } else { 0.06 };
            p.round(sx, sy, sw, sh, 14.0, paint::ink(bg));
            if focus {
                p.round_stroke(sx, sy, sw, sh, 14.0, paint::rgb(paint::ORANGE, 0.55), 1.0);
            }
            p.glyph(icons::SEARCH, sx + 18.0, sy + sh / 2.0, 12.0, paint::faint());
            unsafe {
                p.dc.PushAxisAlignedClip(&D2D_RECT_F { left: (sx + 32.0) * k, top: sy * k, right: (sx + sw - 30.0) * k, bottom: (sy + sh) * k }, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            }
            if text.is_empty() {
                p.text_mid(placeholder, sx + 34.0, sy + sh / 2.0, FIELD_TEXT, REGULAR, paint::faint(), sw - 90.0);
            } else {
                if all {
                    p.round(sx + 34.0 - scroll_x, sy + 8.0, full_w.min(field_w + scroll_x), 20.0, 4.0, paint::rgb(paint::ORANGE, 0.4));
                }
                p.text_mid(&text, sx + 34.0 - scroll_x, sy + sh / 2.0, FIELD_TEXT, REGULAR, paint::text(), 100_000.0);
            }
            unsafe {
                p.dc.PopAxisAlignedClip();
            }
            if text.is_empty() {
                // the shortcut, as a keycap
                let kw = p.measure("Ctrl F", 11.0, SEMIBOLD) + 12.0;
                p.round(sx + sw - kw - 8.0, sy + 9.0, kw, 18.0, 6.0, paint::ink(0.08));
                p.text_mid("Ctrl F", sx + sw - kw - 2.0, sy + 18.0, 11.0, SEMIBOLD, paint::dim(), kw);
            } else {
                let hot = hover == Hit::SearchClear;
                if hot {
                    p.round(sx + sw - 30.0, sy + 6.0, 24.0, 24.0, 8.0, paint::ink(0.1));
                }
                p.glyph(icons::CANCEL, sx + sw - 18.0, sy + sh / 2.0, 9.5, if hot { paint::text() } else { paint::faint() });
            }
        })?;
        let cx = (sx + 34.0 + caret_x - scroll_x).min(sx + sw - 30.0);
        self.search_caret.SetSize(Vector2 { X: (1.5 * k).max(1.0), Y: 18.0 * k })?;
        self.search_caret.SetOffset(Vector3 { X: (SIDEBAR + cx) * k, Y: (sy + 9.0) * k, Z: 0.0 })?;
        self.search_caret.SetIsVisible(focus && searching && !all)?;
        self.paint_chips()
    }

    /// The parsed filters, as removable chips under the header.
    fn paint_chips(&self) -> Result<()> {
        let chips = search::parse(&self.search.string()).chips;
        let k = self.k;
        self.chips.SetOffset(Vector3 { X: SIDEBAR * k, Y: (HEAD - 4.0) * k, Z: 0.0 })?;
        if chips.is_empty() || self.view == Page::Settings {
            return self.chips.SetIsVisible(false);
        }
        self.chips.SetIsVisible(true)?;
        let hover = self.hover.clone();
        self.sprite_from(&self.chips, self.cw(), 34.0, |p| {
            let mut x = 24.0;
            for (i, c) in chips.iter().enumerate() {
                let kw = p.measure(c.key, 12.5, MEDIUM);
                let lw = p.measure(&c.label, 12.5, MEDIUM);
                let w = 10.0 + kw + 5.0 + lw + 26.0;
                p.round(x, 4.0, w, 26.0, 10.0, paint::rgb(paint::ORANGE, 0.14));
                p.text_mid(c.key, x + 10.0, 17.0, 12.5, MEDIUM, paint::dim(), kw + 2.0);
                p.text_mid(&c.label, x + 15.0 + kw, 17.0, 12.5, MEDIUM, paint::accent_text(), lw + 2.0);
                let hot = hover == Hit::Chip(i);
                if hot {
                    p.round(x + w - 23.0, 7.0, 20.0, 20.0, 7.0, paint::ink(0.12));
                }
                p.glyph(icons::CANCEL, x + w - 13.0, 17.0, 8.0, paint::accent_text());
                x += w + 6.0;
            }
        })
    }

    fn chip_rects(&self) -> Vec<(f32, f32)> {
        // x and width of each chip, matching paint_chips
        let chips = search::parse(&self.search.string()).chips;
        let dw = self.gfx.dwrite.clone();
        let m = |s: &str| {
            let fmt = paint::format(&dw, 12.5 * self.k, MEDIUM);
            fmt.ok().map_or(0.0, |_| paint::advance(&dw, s, 12.5 * self.k) / self.k)
        };
        let mut x = 24.0;
        chips
            .iter()
            .map(|c| {
                let w = 10.0 + m(c.key) + 5.0 + m(&c.label) + 26.0;
                let r = (x, w);
                x += w + 6.0;
                r
            })
            .collect()
    }

    // ---------- search: typing and completions ----------

    fn focus_search(&mut self, on: bool) {
        self.search_focus = on;
        if !on {
            self.options.clear();
        }
        self.update_options();
        let _ = self.paint_header();
    }

    fn update_options(&mut self) {
        if !self.search_focus || self.view == Page::Settings {
            self.options.clear();
        } else {
            let (_, token) = self.search.token();
            let mut sites: HashMap<String, usize> = HashMap::new();
            for j in &self.jobs {
                let s = search::site(&j.source);
                if !s.is_empty() {
                    *sites.entry(s).or_default() += 1;
                }
            }
            let mut sites: Vec<(String, usize)> = sites.into_iter().collect();
            sites.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            let jobs = &self.jobs;
            self.options = search::complete(&token, |t| jobs.iter().filter(|j| search::file_type(&j.name) == t).count(), &sites);
            // only show the full menu on an empty word when the box is empty
            if token.is_empty() && !self.search.text.is_empty() {
                self.options.clear();
            }
        }
        self.pick = 0;
        let _ = self.paint_dropdown();
    }

    fn accept_option(&mut self, i: usize) {
        let Some(o) = self.options.get(i).cloned() else { return };
        let (start, _) = self.search.token();
        let end = self.search.caret;
        let mut text: Vec<char> = self.search.text[..start].to_vec();
        text.extend(o.text.chars());
        if o.done {
            text.push(' ');
        }
        let caret = text.len();
        text.extend(self.search.text[end..].iter().skip_while(|c| !c.is_whitespace()));
        self.search.text = text;
        self.search.caret = caret;
        self.search.all = false;
        self.on_query_changed();
    }

    fn on_query_changed(&mut self) {
        self.scroll = 0.0;
        self.update_options();
        let _ = self.paint_header();
        if self.view == Page::Settings {
            // only the settings that match show
            self.page.query = self.search.string();
            self.page.scroll = 0.0;
            self.build_page(false);
            return;
        }
        let _ = self.place_list();
        self.rebuild_list();
    }

    fn paint_dropdown(&self) -> Result<()> {
        let k = self.k;
        let c = &self.gfx.compositor;
        let visible = self.search_focus && !self.options.is_empty() && self.view != Page::Settings;
        let a = c.CreateScalarKeyFrameAnimation()?;
        a.InsertExpressionKeyFrame(0.0, &HSTRING::from("this.StartingValue"))?;
        a.InsertKeyFrame(1.0, if visible { 1.0 } else { 0.0 })?;
        a.SetDuration(ms(140))?;
        self.dropdown.StartAnimation(&HSTRING::from("Opacity"), &a)?;
        if !visible {
            return Ok(());
        }
        let (sx, sy, sw, sh) = self.search_rect();
        let (_, token) = self.search.token();
        let shown = self.options.len().min(6);
        let first = self.pick.saturating_sub(shown - 1);
        let hint = if token.is_empty() {
            "Typos are fine. Narrow it down with:".to_string()
        } else if token.contains(':') || token.starts_with(['<', '>']) {
            "Pick one".to_string()
        } else {
            format!("Filters that start with \u{201c}{token}\u{201d}")
        };
        let h = 36.0 + shown as f32 * OPT_H + 34.0;
        let (opts, pick, hover) = (self.options.clone(), self.pick, self.hover.clone());
        self.sprite_from(&self.dropdown, sw, h, |p| {
            p.popover(0.0, 0.0, sw, h, popup::RADIUS);
            p.text(&hint, 14.0, 12.0, 11.5, MEDIUM, paint::faint(), sw - 28.0);
            for (n, o) in opts.iter().enumerate().skip(first).take(shown) {
                let y = 34.0 + (n - first) as f32 * OPT_H;
                if n == pick || hover == Hit::Option(n) {
                    p.round(popup::PAD, y, sw - 2.0 * popup::PAD, OPT_H - 2.0, popup::RADIUS - popup::PAD, paint::ink(if n == pick { 0.09 } else { 0.05 }));
                }
                // the tag (typed part plain, the completion brighter), its description under it
                let tw = p.measure(&o.text, 12.5, SEMIBOLD) + 16.0;
                p.round(16.0, y + 7.0, tw, 20.0, 7.0, paint::rgb(paint::ORANGE, 0.14));
                let typed: String = o.text.chars().take(token.chars().count()).collect();
                let rest: String = o.text.chars().skip(token.chars().count()).collect();
                let w1 = p.text_mid(&typed, 24.0, y + 17.0, 12.5, SEMIBOLD, paint::accent_text(), tw);
                p.text_mid(&rest, 24.0 + w1, y + 17.0, 12.5, SEMIBOLD, paint::accent_strong(), tw);
                p.text(&o.desc, 16.0, y + 29.0, 12.0, REGULAR, paint::dim(), sw - 32.0);
            }
            let fy = h - 26.0;
            p.fill(12.0, fy - 8.0, sw - 24.0, 1.0, paint::ink(0.06));
            p.text("↑ ↓  choose     Tab  complete     Esc  close", 16.0, fy, 11.0, REGULAR, paint::faint(), sw - 32.0);
        })?;
        self.dropdown.SetOffset(Vector3 { X: sx * k, Y: (sy + sh + 6.0) * k, Z: 0.0 })
    }

    fn option_at(&self, x: f32, y: f32) -> Option<usize> {
        if !self.search_focus || self.options.is_empty() {
            return None;
        }
        let (sx, sy, sw, sh) = self.search_rect();
        let (dx, dy) = (x - sx, y - (sy + sh + 6.0));
        if dx < 0.0 || dx > sw || dy < 34.0 {
            return None;
        }
        let shown = self.options.len().min(6);
        let first = self.pick.saturating_sub(shown - 1);
        let n = ((dy - 34.0) / OPT_H) as usize;
        (n < shown).then_some(first + n)
    }

    // ---------- the list ----------

    fn rebuild_list(&mut self) {
        self.compute_lines();
        let _ = self.build_groups();
        let _ = self.sync_rows();
        let empty = self.lines.is_empty();
        let _ = self.paint_empty(empty);
        self.clamp_scroll(false);
    }

    fn line_y(&self, i: usize) -> f32 {
        let mut y = 4.0;
        for l in &self.lines[..i] {
            y += match l {
                Line::Group(_) => GROUP,
                Line::Job(_) => ROW,
            };
        }
        y
    }

    fn content_h(&self) -> f32 {
        self.line_y(self.lines.len()) + 130.0
    }

    /// Group labels: small floating pills that stick to the top while their group scrolls
    /// past (an expression on the compositor follows the scroll).
    fn build_groups(&mut self) -> Result<()> {
        let c = self.gfx.compositor.clone();
        let children = self.inner.Children()?;
        for g in self.groups.drain(..) {
            let _ = children.Remove(&g);
        }
        let k = self.k;
        let ys: Vec<(usize, f32)> = self.lines.iter().enumerate().filter(|(_, l)| matches!(l, Line::Group(_))).map(|(i, _)| (i, self.line_y(i))).collect();
        for (n, (i, y)) in ys.iter().enumerate() {
            let Line::Group(label) = &self.lines[*i] else { continue };
            let v = c.CreateSpriteVisual()?;
            let dw = self.gfx.dwrite.clone();
            let lw = paint::advance(&dw, label, 12.5 * k) / k + 24.0;
            let label = label.clone();
            self.sprite_from(&v, lw + 8.0, GROUP, |p| {
                p.round(4.0, 6.0, lw, 26.0, 10.0, paint::raised(0.94));
                p.text_mid(&label, 16.0, 19.0, 12.5, SEMIBOLD, paint::ink(0.78), lw);
            })?;
            children.InsertAtTop(&v)?;
            // sticky: max(own position, scroll), pushed up by the next group's label
            let next = ys.get(n + 1).map_or(1.0e7, |(_, y)| *y);
            let e = c.CreateExpressionAnimationWithExpression(&HSTRING::from(format!(
                "Vector3({x}, Min(Max({y}, -inner.Offset.Y + {pad}), {next} - {gh}), 0)",
                x = 10.0 * k,
                y = y * k,
                pad = (self.list_top() + 2.0) * k,
                next = next * k,
                gh = GROUP * k
            )))?;
            e.SetReferenceParameter(&HSTRING::from("inner"), &self.inner)?;
            v.StartAnimation(&HSTRING::from("Offset"), &e)?;
            self.groups.push(v);
        }
        Ok(())
    }

    fn row_w(&self) -> f32 {
        self.cw() - 24.0
    }

    fn icon(&mut self, j: &JobView) -> Option<Pixels> {
        let folder = j.kind == Kind::Torrent && std::path::Path::new(&j.name).extension().is_none();
        let ext = if folder { "<folder>".to_string() } else { std::path::Path::new(&j.name).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default() };
        let size = (34.0 * self.k * 1.5) as i32;
        self.icons.entry(ext.clone()).or_insert_with(|| shell::type_icon(&format!("file.{ext}"), folder, size.max(48))).clone()
    }

    /// The rows on screen (and a screen's worth either side): the list is virtual, so a
    /// long history costs only the rows you can see. Rows that stay glide to their new
    /// places; new downloads (and rows a list switch brings in) rise in; rows that come
    /// into view by scrolling are simply there; rows that leave the list fade out.
    fn sync_rows(&mut self) -> Result<()> {
        let view = self.h - self.list_top();
        let (lo, hi) = (self.scroll - view, self.scroll + 2.0 * view);
        let mut all = std::collections::HashSet::new();
        let mut jobs: Vec<(f32, JobView)> = Vec::new();
        let mut y = 4.0;
        for l in &self.lines {
            match l {
                Line::Group(_) => y += GROUP,
                Line::Job(j) => {
                    all.insert(j.id);
                    if y + ROW >= lo && y <= hi {
                        jobs.push((y, j.clone()));
                    }
                    y += ROW;
                }
            }
        }
        let k = self.k;
        let c = self.gfx.compositor.clone();
        let children = self.inner.Children()?;
        let keep: std::collections::HashSet<u64> = jobs.iter().map(|(_, j)| j.id).collect();
        let fade = self.ease_out()?;
        let mut gone = Vec::new();
        self.rows.retain(|r| {
            if keep.contains(&r.id) {
                return true;
            }
            if all.contains(&r.id) {
                let _ = children.Remove(&r.v); // scrolled out of reach
            } else {
                // left the list: fade out, then go
                let _ = (|| -> Result<()> {
                    let o = c.CreateScalarKeyFrameAnimation()?;
                    o.InsertExpressionKeyFrame(0.0, &HSTRING::from("this.StartingValue"))?;
                    o.InsertKeyFrameWithEasingFunction(1.0, 0.0, &fade)?;
                    o.SetDuration(ms(160))?;
                    r.v.StartAnimation(&HSTRING::from("Opacity"), &o)?;
                    let sc = c.CreateVector3KeyFrameAnimation()?;
                    sc.InsertKeyFrameWithEasingFunction(1.0, Vector3 { X: 0.97, Y: 0.97, Z: 1.0 }, &fade)?;
                    sc.SetDuration(ms(160))?;
                    r.v.StartAnimation(&HSTRING::from("Scale"), &sc)
                })();
                gone.push((r.v.clone(), std::time::Instant::now()));
            }
            false
        });
        // what faded out a moment ago can go now
        self.dying.retain(|(v, t)| {
            let done = t.elapsed().as_millis() > 400;
            if done {
                let _ = children.Remove(v);
            }
            !done
        });
        self.dying.extend(gone);
        let rw = self.row_w();
        let back = self.back()?;
        let mut n = 0;
        for (y, j) in jobs.iter() {
            let y = *y;
            let mut entered = false;
            let idx = match self.rows.iter().position(|r| r.id == j.id) {
                Some(idx) => idx,
                None => {
                    entered = self.entering;
                    let v = c.CreateContainerVisual()?;
                    let (sprite, fill, rest) = (c.CreateSpriteVisual()?, c.CreateSpriteVisual()?, c.CreateSpriteVisual()?);
                    let icon_v = c.CreateSpriteVisual()?;
                    // a soft shadow under the icon's own shape, so pale icons don't melt into the surface
                    let shadow = c.CreateDropShadow()?;
                    shadow.SetBlurRadius(8.0 * k)?;
                    shadow.SetOffset(Vector3 { X: 0.0, Y: 1.5 * k, Z: 0.0 })?;
                    shadow.SetOpacity(if paint::light() { 0.14 } else { 0.22 })?;
                    shadow.SetSourcePolicy(windows::UI::Composition::CompositionDropShadowSourcePolicy::InheritFromVisualContent)?;
                    icon_v.SetShadow(&shadow)?;
                    v.Children()?.InsertAtTop(&sprite)?;
                    v.Children()?.InsertAtTop(&icon_v)?;
                    v.Children()?.InsertAtTop(&rest)?;
                    v.Children()?.InsertAtTop(&fill)?;
                    children.InsertAtBottom(&v)?;
                    v.SetOffset(Vector3 { X: 12.0 * k, Y: y * k, Z: 0.0 })?;
                    v.SetCenterPoint(Vector3 { X: rw * k / 2.0, Y: ROW * k / 2.0, Z: 0.0 })?;
                    // in with a small rise (not when it's just scrolled into view); later moves glide
                    if entered {
                        let delay = ms((n.min(10) as i64) * 24);
                        let o = c.CreateScalarKeyFrameAnimation()?;
                        o.InsertKeyFrame(0.0, 0.0)?;
                        o.InsertKeyFrame(1.0, 1.0)?;
                        o.SetDuration(ms(200))?;
                        o.SetDelayTime(delay)?;
                        o.SetDelayBehavior(windows::UI::Composition::AnimationDelayBehavior::SetInitialValueBeforeDelay)?;
                        v.StartAnimation(&HSTRING::from("Opacity"), &o)?;
                        let sc = c.CreateVector3KeyFrameAnimation()?;
                        sc.InsertKeyFrame(0.0, Vector3 { X: 0.97, Y: 0.97, Z: 1.0 })?;
                        sc.InsertKeyFrameWithEasingFunction(1.0, Vector3 { X: 1.0, Y: 1.0, Z: 1.0 }, &back)?;
                        sc.SetDuration(ms(320))?;
                        sc.SetDelayTime(delay)?;
                        sc.SetDelayBehavior(windows::UI::Composition::AnimationDelayBehavior::SetInitialValueBeforeDelay)?;
                        v.StartAnimation(&HSTRING::from("Scale"), &sc)?;
                    }
                    let glide = c.CreateVector3KeyFrameAnimation()?;
                    glide.SetTarget(&HSTRING::from("Offset"))?;
                    glide.InsertExpressionKeyFrameWithEasingFunction(1.0, &HSTRING::from("this.FinalValue"), &back)?;
                    glide.SetDuration(ms(340))?;
                    let implicit = c.CreateImplicitAnimationCollection()?;
                    implicit.Insert(&HSTRING::from("Offset"), &glide)?;
                    v.SetImplicitAnimations(&implicit)?;
                    self.rows.push(RowVis { id: j.id, v, sprite, fill, rest, key: String::new(), frac: -1.0, bar_w: 0.0, y, icon_v, icon_id: String::new(), icon_alpha: -1.0 });
                    self.rows.len() - 1
                }
            };
            if (self.rows[idx].y - y).abs() > 0.1 {
                self.rows[idx].v.SetOffset(Vector3 { X: 12.0 * k, Y: y * k, Z: 0.0 })?;
                self.rows[idx].y = y;
            }
            let selected = self.picked.contains(&j.id);
            let hover_btn = match &self.hover {
                Hit::RowBtn(id, b) if *id == j.id => Some(*b),
                _ => None,
            };
            let hovered = matches!(&self.hover, Hit::Row(id) | Hit::RowBtn(id, _) if *id == j.id);
            let missing = self.is_missing(j);
            let marks = self.marks.get(&j.id).cloned().unwrap_or_default();
            let site = search::site(&j.source);
            let key = format!(
                "{}|{:?}|{:?}|{}|{}|{}|{selected}|{hovered}|{hover_btn:?}|{missing}|{marks:?}|{rw}|{k}|{:?}|{}",
                j.name,
                j.state,
                j.phase,
                j.done / 200_000,
                j.speed / 50_000,
                j.peers.unwrap_or(j.connections),
                j.priority,
                paint::speed(1_000_000)
            );
            if self.rows[idx].key != key {
                let sprite = self.rows[idx].sprite.clone();
                let j2 = j.clone();
                self.sprite_from(&sprite, rw, ROW, |p| paint_row(p, &j2, rw, selected, hovered, hover_btn, missing, None, &marks, &site))?;
                self.rows[idx].key = key;
            }
            self.update_row_icon(idx, j, missing, n, entered)?;
            if entered {
                n += 1;
            }
            self.update_bar(idx, j)?;
        }
        Ok(())
    }

    /// The row's icon: the file's own (its thumbnail, or an app's icon) once it's on
    /// disk, else its type's. A new or changed icon eases in instead of popping.
    fn update_row_icon(&mut self, idx: usize, j: &JobView, missing: bool, n: usize, entered: bool) -> Result<()> {
        let k = self.k;
        // finished: the file itself; an app still downloading: its icon, read early
        let source = if j.state.finished() {
            j.path.clone().filter(|_| !missing)
        } else {
            dl::early_picture(j.id).filter(|_| j.kind == Kind::Http)
        };
        if let Some(p) = &source {
            if !self.file_icons.contains_key(p) && self.icon_pending.insert(p.clone()) {
                let size = (32.0 * k * 1.5).round() as i32;
                let _ = self.thumbs.send(crate::thumbs::Job { path: p.clone(), modified: SystemTime::UNIX_EPOCH, size: size.max(48) });
            }
        }
        let own = source.as_ref().is_some_and(|p| matches!(self.file_icons.get(p), Some(Some(_))));
        let ext = std::path::Path::new(&j.name).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        let id = if own { format!("file:{}", source.as_ref().map(|p| p.display().to_string()).unwrap_or_default()) } else { format!("type:{ext}:{}", j.kind == Kind::Torrent) };
        let alpha = if missing { 0.45 } else if j.state.finished() || j.state == State::Active { 1.0 } else { 0.6 };
        let row = &self.rows[idx];
        let changed = row.icon_id != id;
        let first = row.icon_id.is_empty();
        let v = row.icon_v.clone();
        if changed {
            let px = if own { source.as_ref().and_then(|p| self.file_icons.get(p).cloned().flatten()) } else { self.icon(j) };
            let size = 32.0;
            if let Some(px) = px {
                let surface = self.gfx.image_surface(&px, (size * k).round(), 0.0, 1.0, 1.0)?;
                let brush = self.gfx.compositor.CreateSurfaceBrushWithSurface(&surface)?;
                brush.SetStretch(CompositionStretch::Uniform)?;
                v.SetBrush(&brush)?;
            }
            v.SetSize(Vector2 { X: (size * k).round(), Y: (size * k).round() })?;
            v.SetOffset(Vector3 { X: ((34.0 - size / 2.0) * k).round(), Y: (((ROW - size) / 2.0) * k).round(), Z: 0.0 })?;
            v.SetCenterPoint(Vector3 { X: size * k / 2.0, Y: size * k / 2.0, Z: 0.0 })?;
            // ease in: a brand-new row's icon after the row itself, a swapped one at once;
            // a row that only scrolled into view shows its icon as it is
            let c = &self.gfx.compositor;
            if first && !entered {
                v.StopAnimation(&HSTRING::from("Opacity"))?;
                v.StopAnimation(&HSTRING::from("Scale"))?;
                v.SetOpacity(alpha)?;
                v.SetScale(Vector3 { X: 1.0, Y: 1.0, Z: 1.0 })?;
                let row = &mut self.rows[idx];
                row.icon_id = id;
                row.icon_alpha = alpha;
                return Ok(());
            }
            let delay = if first { ms((n.min(10) as i64) * 24 + 60) } else { ms(0) };
            let o = c.CreateScalarKeyFrameAnimation()?;
            o.InsertKeyFrame(0.0, 0.0)?;
            o.InsertKeyFrameWithEasingFunction(1.0, alpha, &self.ease_out()?)?;
            o.SetDuration(ms(260))?;
            o.SetDelayTime(delay)?;
            o.SetDelayBehavior(windows::UI::Composition::AnimationDelayBehavior::SetInitialValueBeforeDelay)?;
            v.StartAnimation(&HSTRING::from("Opacity"), &o)?;
            let sc = c.CreateVector3KeyFrameAnimation()?;
            sc.InsertKeyFrame(0.0, Vector3 { X: 0.7, Y: 0.7, Z: 1.0 })?;
            sc.InsertKeyFrameWithEasingFunction(1.0, Vector3 { X: 1.0, Y: 1.0, Z: 1.0 }, &self.back()?)?;
            sc.SetDuration(ms(380))?;
            sc.SetDelayTime(delay)?;
            sc.SetDelayBehavior(windows::UI::Composition::AnimationDelayBehavior::SetInitialValueBeforeDelay)?;
            v.StartAnimation(&HSTRING::from("Scale"), &sc)?;
        } else if (self.rows[idx].icon_alpha - alpha).abs() > 0.01 {
            let o = self.gfx.compositor.CreateScalarKeyFrameAnimation()?;
            o.InsertExpressionKeyFrame(0.0, &HSTRING::from("this.StartingValue"))?;
            o.InsertKeyFrameWithEasingFunction(1.0, alpha, &self.ease_out()?)?;
            o.SetDuration(ms(200))?;
            v.StartAnimation(&HSTRING::from("Opacity"), &o)?;
        }
        let row = &mut self.rows[idx];
        row.icon_id = id;
        row.icon_alpha = alpha;
        Ok(())
    }

    /// Icons fetched in the background have arrived: rows that show them ease them in.
    fn on_thumbs(&mut self) {
        let done: Vec<crate::thumbs::Done> = std::mem::take(&mut *THUMBS_DONE.lock().unwrap());
        for d in done {
            self.icon_pending.remove(&d.path);
            self.file_icons.insert(d.path, d.pixels);
        }
        if self.view != Page::Settings {
            let _ = self.sync_rows();
        }
    }

    fn bar_geom(&self) -> (f32, f32, f32) {
        (68.0, 35.0, (self.row_w() - 68.0 - 96.0).max(40.0))
    }

    /// The progress bar: the filled part and the rest of the track are two pills with
    /// a gap between them. The fill's width glides on the compositor, and the rest
    /// follows it through an expression.
    fn update_bar(&mut self, idx: usize, j: &JobView) -> Result<()> {
        const GAP: f32 = 4.0;
        let k = self.k;
        let (bx, by, bw) = self.bar_geom();
        let c = self.gfx.compositor.clone();
        let row = &self.rows[idx];
        if j.state.finished() {
            row.fill.SetIsVisible(false)?;
            row.rest.SetIsVisible(false)?;
            return Ok(());
        }
        row.fill.SetIsVisible(true)?;
        row.rest.SetIsVisible(true)?;
        let h = (4.0 * k).round().max(3.0);
        let color = match j.state {
            State::Active => paint::accent(),
            State::Failed => paint::red(),
            _ => paint::ink(0.42), // paused or waiting: a quiet grey in either theme
        };
        let frac = j.fraction().unwrap_or(0.0);
        if row.frac < 0.0 || (row.bar_w - bw).abs() > 0.5 {
            row.fill.SetBrush(&self.gfx.pill_brush(color, h)?)?;
            row.rest.SetBrush(&self.gfx.pill_brush(paint::ink(0.10), h)?)?;
            row.fill.SetOffset(Vector3 { X: bx * k, Y: by * k, Z: 0.0 })?;
            row.fill.SetSize(Vector2 { X: ((bw - GAP) * frac * k).max(h), Y: h })?;
            let e = c.CreateExpressionAnimationWithExpression(&HSTRING::from(format!(
                "Vector3(fill.Offset.X + fill.Size.X + {gap}, {y}, 0)",
                gap = GAP * k,
                y = by * k
            )))?;
            e.SetReferenceParameter(&HSTRING::from("fill"), &row.fill)?;
            row.rest.StartAnimation(&HSTRING::from("Offset"), &e)?;
            let s = c.CreateExpressionAnimationWithExpression(&HSTRING::from(format!("Vector2(Max({end} - fill.Size.X - {gap} - fill.Offset.X, {h}), {h})", end = (bx + bw) * k, gap = GAP * k, h = h)))?;
            s.SetReferenceParameter(&HSTRING::from("fill"), &row.fill)?;
            row.rest.StartAnimation(&HSTRING::from("Size"), &s)?;
        } else if (frac - row.frac).abs() > 0.0001 {
            let a = c.CreateVector2KeyFrameAnimation()?;
            a.InsertExpressionKeyFrame(0.0, &HSTRING::from("this.StartingValue"))?;
            a.InsertKeyFrameWithEasingFunction(1.0, Vector2 { X: ((bw - GAP) * frac * k).max(h), Y: h }, &c.CreateLinearEasingFunction()?)?;
            a.SetDuration(ms(520))?;
            row.fill.StartAnimation(&HSTRING::from("Size"), &a)?;
            row.fill.SetBrush(&self.gfx.pill_brush(color, h)?)?;
        }
        self.rows[idx].frac = frac;
        self.rows[idx].bar_w = bw;
        Ok(())
    }

    fn force_repaint(&mut self) {
        for r in &mut self.rows {
            r.key.clear();
            r.frac = -1.0;
        }
        if self.view != Page::Settings {
            let _ = self.sync_rows();
        }
        let _ = self.paint_dock();
        let _ = self.paint_header();
    }

    // ---------- empty states ----------

    fn empty_copy(&self) -> (String, String, Option<&'static str>) {
        let q = self.search.string();
        if !q.trim().is_empty() {
            return (format!("No downloads match \u{201c}{}\u{201d}", q.trim()), "Try fewer words, or a filter like type:video or site:github.com.".into(), Some("Clear search"));
        }
        match self.view {
            Page::List(Filter::Downloading) => ("All caught up".into(), "Nothing is downloading right now.".into(), Some("New Download")),
            Page::List(Filter::Completed) => ("Nothing finished yet".into(), "Downloads land here the moment they're done.".into(), None),
            _ => ("Your stash is empty".into(), "Copy a link and paste it here, or drop a .torrent file.".into(), Some("New Download")),
        }
    }

    fn empty_button(&self) -> (f32, f32, f32, f32) {
        let (w, h) = (self.cw(), self.h - self.list_top());
        let cy = (h / 2.0 - 80.0).max(60.0);
        (w / 2.0 - 78.0, cy + 120.0, 156.0, crate::ui::BUTTON_H)
    }

    fn paint_empty(&self, empty: bool) -> Result<()> {
        if !empty {
            return self.empty.SetOpacity(0.0);
        }
        let (w, h) = (self.cw(), self.h - self.list_top());
        let cy = (h / 2.0 - 80.0).max(60.0);
        let (title, sub, cta) = self.empty_copy();
        let (bx, by, bw, bh) = self.empty_button();
        let hot = self.hover == Hit::EmptyCta;
        let searching = !self.search.string().trim().is_empty();
        self.sprite_from(&self.empty, w, h, |p| {
            p.logo_imprint(w / 2.0, cy - 6.0, 76.0);
            let tw = p.measure(&title, 20.0, SEMIBOLD).min(w - 48.0);
            p.text(&title, (w - tw) / 2.0, cy + 50.0, 20.0, SEMIBOLD, paint::text(), w - 48.0);
            let sw = p.measure(&sub, 13.0, REGULAR).min(w - 48.0);
            p.text(&sub, (w - sw) / 2.0, cy + 80.0, 13.0, REGULAR, paint::dim(), w - 48.0);
            let Some(cta) = cta else { return };
            let r = settings_page::Rect::new(bx, by, bw, bh);
            if searching {
                crate::ui::button(p, r, cta, None, crate::ui::Kind::Secondary, hot, true);
                return;
            }
            crate::ui::button(p, r, cta, Some(icons::ADD), crate::ui::Kind::Primary, hot, true);
            let y = by + 66.0;
            let parts = [("or press", false), ("Ctrl", true), ("V", true)];
            let widths: Vec<f32> = parts.iter().map(|(t, cap)| p.measure(t, 11.5, if *cap { MEDIUM } else { REGULAR }) + if *cap { 14.0 } else { 0.0 }).collect();
            let total: f32 = widths.iter().sum::<f32>() + 12.0;
            let mut x = w / 2.0 - total / 2.0;
            for ((t, cap), tw) in parts.iter().zip(widths) {
                if *cap {
                    p.round(x, y - 10.0, tw, 21.0, 6.0, paint::ink(0.07));
                    p.round(x, y - 10.0, tw, 19.0, 6.0, paint::ink(0.10));
                    p.text_mid(t, x + 7.0, y - 0.5, 11.5, MEDIUM, paint::text(), tw);
                } else {
                    p.text_mid(t, x, y, 11.5, REGULAR, paint::faint(), tw + 2.0);
                }
                x += tw + 6.0;
            }
        })?;
        self.empty.SetOpacity(1.0)
    }

    // ---------- the dock ----------

    /// The live speed (a sparkline), what's going on, and the actions: one round +, with
    /// Pause all, Resume all and Clear finished fanning out of it on hover.
    fn paint_dock(&self) -> Result<()> {
        let k = self.k;
        let on_list = self.view != Page::Settings;
        self.dock.SetIsVisible(on_list)?;
        if !on_list {
            return Ok(());
        }
        let cw = self.cw();
        let active = self.jobs.iter().filter(|j| j.state == State::Active).count();
        let queued = self.jobs.iter().filter(|j| j.state == State::Queued).count();
        let down = self.speeds.last().copied().unwrap_or(0.0) as u64;
        let status = match (active, queued) {
            (0, 0) => {
                let n = self.jobs.len();
                if n == 0 { "Nothing downloading".to_string() } else { format!("Nothing downloading  ·  {n} in history") }
            }
            (a, 0) => format!("{a} downloading"),
            (a, q) => format!("{a} downloading, {q} queued"),
        };
        let speed = if active > 0 { Some(paint::speed(down)) } else { None };
        let status = if self.picked.len() > 1 { format!("{} selected  \u{b7}  Esc to clear", self.picked.len()) } else { status };
        // gaming mode holds downloads back: say why they're slow
        let status = if active > 0 && self.picked.len() <= 1 && dl::gaming::active() { format!("{}  \u{b7}  {status}", dl::gaming::NOTE) } else { status };
        let values = self.speeds.clone();
        let status_w = cw - 24.0 - 22.0 - 4.0 * 48.0;
        self.sprite_from(&self.dock_status, status_w, DOCK_H, |p| {
            let cy = DOCK_H / 2.0 + 5.0; // a touch lower than the button's middle
            // the chart only while something downloads
            let mut x = 0.0;
            if active > 0 {
                p.spark(0.0, cy - 10.0, 56.0, 20.0, &values, paint::accent());
                x = 68.0;
            }
            if let Some(s) = &speed {
                x += p.text_mid(s, x, cy, 13.5, SEMIBOLD, paint::text(), 120.0) + 6.0;
                x += p.text_mid("·", x, cy, 13.5, REGULAR, paint::faint(), 10.0) + 6.0;
            }
            p.text_mid(&status, x, cy, 13.0, REGULAR, paint::dim(), status_w - x);
        })?;
        self.dock_status.SetOffset(Vector3 { X: 24.0 * k, Y: 0.0, Z: 0.0 })?;
        // the + : a flat orange circle
        let hot = self.hover == Hit::DockAdd;
        self.sprite_from(&self.dock_add, 44.0, 44.0, |p| {
            p.round(0.0, 0.0, 44.0, 44.0, 22.0, if hot { paint::accent_hot() } else { paint::accent() });
            p.glyph(icons::ADD, 22.0, 22.0, 14.0, paint::color(255, 255, 255, 1.0));
        })?;
        self.dock_add.SetOffset(Vector3 { X: (cw - 22.0 - 44.0) * k, Y: 0.0, Z: 0.0 })?;
        // the ghost actions
        let multi = self.picked.len() > 1;
        for (i, (icon, _)) in DOCK_TOOLS.iter().enumerate() {
            let icon = if multi && i == 2 { &icons::CANCEL } else { icon };
            let hot = self.hover == Hit::DockTool(i);
            self.sprite_from(&self.dock_tools[i], 40.0, 40.0, |p| {
                crate::ui::button(p, settings_page::Rect::new(0.0, 0.0, 40.0, 40.0), "", Some(icon), crate::ui::Kind::Ghost, hot, true);
            })?;
            if !self.dock_open {
                self.dock_tools[i].SetOffset(Vector3 { X: (self.tool_x(i) + 22.0) * k, Y: 2.0 * k, Z: 0.0 })?;
            }
        }
        Ok(())
    }

    fn tool_x(&self, i: usize) -> f32 {
        // nearest the + first
        self.cw() - 22.0 - 44.0 - 8.0 - 40.0 - i as f32 * 44.0
    }

    /// Hovering the dock fans the actions out of the +, nearest first; leaving folds them back.
    fn set_dock_open(&mut self, open: bool) {
        if open == self.dock_open {
            return;
        }
        self.dock_open = open;
        let k = self.k;
        let c = &self.gfx.compositor;
        let _ = (|| -> Result<()> {
            let back = self.back()?;
            let out = self.ease_out()?;
            for (i, v) in self.dock_tools.iter().enumerate() {
                let delay = ms(if open { i as i64 * 45 } else { (2 - i as i64) * 30 });
                let x = self.tool_x(i) + if open { 0.0 } else { 22.0 };
                let a = c.CreateVector3KeyFrameAnimation()?;
                a.InsertExpressionKeyFrame(0.0, &HSTRING::from("this.StartingValue"))?;
                a.InsertKeyFrameWithEasingFunction(1.0, Vector3 { X: x * k, Y: 2.0 * k, Z: 0.0 }, if open { &back } else { &out })?;
                a.SetDuration(ms(if open { 320 } else { 200 }))?;
                a.SetDelayTime(delay)?;
                v.StartAnimation(&HSTRING::from("Offset"), &a)?;
                let o = c.CreateScalarKeyFrameAnimation()?;
                o.InsertExpressionKeyFrame(0.0, &HSTRING::from("this.StartingValue"))?;
                o.InsertKeyFrame(1.0, if open { 1.0 } else { 0.0 })?;
                o.SetDuration(ms(180))?;
                o.SetDelayTime(delay)?;
                v.StartAnimation(&HSTRING::from("Opacity"), &o)?;
            }
            Ok(())
        })();
    }

    // ---------- scrolling ----------

    fn clamp_scroll(&mut self, animate: bool) {
        let view_h = self.h - self.list_top();
        let max = (self.content_h() - view_h).max(0.0);
        let before = self.scroll;
        self.scroll = self.scroll.clamp(0.0, max);
        if (self.scroll - before).abs() > 0.5 || animate {
            // rows coming into reach are just there
            self.entering = false;
            let _ = self.sync_rows();
            self.entering = true;
        }
        let to = Vector3 { X: 0.0, Y: (self.list_top() - self.scroll) * self.k, Z: 0.0 };
        let _ = (|| -> Result<()> {
            if animate {
                let a = self.gfx.compositor.CreateVector3KeyFrameAnimation()?;
                a.InsertExpressionKeyFrame(0.0, &HSTRING::from("this.StartingValue"))?;
                a.InsertKeyFrameWithEasingFunction(1.0, to, &self.ease_out()?)?;
                a.SetDuration(ms(180))?;
                self.inner.StartAnimation(&HSTRING::from("Offset"), &a)
            } else {
                self.inner.StopAnimation(&HSTRING::from("Offset"))?;
                self.inner.SetOffset(to)
            }
        })();
    }

    // ---------- the settings page ----------

    fn values(&self) -> Values {
        Values {
            settings: self.settings.clone(),
            downloads: default_dir(&self.settings),
            login: shell::start_at_login(),
            magnets: shell::handles_magnets(),
            packaged: shell::is_packaged(),
            update: crate::update::status(),
            install: crate::update::install(),
        }
    }

    fn build_page(&mut self, animate: bool) {
        let v = self.values();
        self.page.top = HEAD;
        let _ = self.page.build(&v, self.k, self.cw(), self.h - HEAD, animate);
    }

    fn set_view(&mut self, view: Page) {
        if view == self.view {
            return;
        }
        let was_settings = self.view == Page::Settings;
        self.view = view;
        self.scroll = 0.0;
        self.selected = None;
        self.picked.clear();
        self.close_menu();
        self.hide_tip();
        self.focus_search(false);
        let settings = view == Page::Settings;
        if was_settings != settings {
            std::mem::swap(&mut self.search, &mut self.other_search);
            self.page.query = if settings { self.search.string() } else { self.other_search.string() };
        }
        let _ = self.page_view.SetIsVisible(settings);
        for v in &self.page_blurs {
            let _ = v.SetIsVisible(settings);
        }
        let _ = self.list.SetIsVisible(!settings);
        for v in &self.blurs {
            let _ = v.SetIsVisible(!settings);
        }
        let _ = self.move_sel(true);
        let _ = self.paint_sidebar();
        let _ = self.paint_header();
        let _ = self.paint_dock();
        if settings {
            self.build_page(false);
        } else {
            let _ = self.place_list();
            // rows in both lists glide to their new places; only new ones come in
            self.rebuild_list();
        }
    }

    // ---------- hit testing ----------

    fn hit(&self, x: f32, y: f32) -> Hit {
        if let Some(h) = self.caption_hit(x, y) {
            return h;
        }
        if let Some(s) = &self.sheet {
            return self.sheet_hit(s, x, y);
        }
        if let Some(m) = &self.menu {
            return m.model.hit(x - m.x, y - m.y).map_or(Hit::MenuOff, Hit::Menu);
        }
        if let Some(i) = self.option_at(x, y) {
            return Hit::Option(i);
        }
        if x < SIDEBAR {
            for i in 0..FILTERS.len() {
                let ty = 64.0 + i as f32 * 38.0;
                if (10.0..SIDEBAR - 10.0).contains(&x) && (ty..ty + 34.0).contains(&y) {
                    return Hit::Filter(i);
                }
            }
            if let Some((_, _, true)) = update_card() {
                let cy = self.h - 52.0 - 8.0 - UPDATE_CARD;
                if (20.0..SIDEBAR - 20.0).contains(&x) && y >= cy + 60.0 && y < cy + 90.0 {
                    return Hit::UpdateCard;
                }
            }
            if y >= self.h - 52.0 && y < self.h - 18.0 && x >= 10.0 {
                return Hit::Settings;
            }
            return if y < 56.0 { Hit::Caption } else { Hit::None };
        }
        let cx = x - SIDEBAR;
        if y < HEAD {
            if self.view != Page::Settings {
                let (sx, sy, sw, sh) = self.search_rect();
                if x >= sx && x < sx + sw && y >= sy && y < sy + sh {
                    if !self.search.text.is_empty() && x >= sx + sw - 32.0 {
                        return Hit::SearchClear;
                    }
                    return Hit::Search;
                }
            }
            return Hit::Caption;
        }
        if self.view == Page::Settings {
            return Hit::Page(self.page.hit(cx, y - HEAD + self.page.scroll));
        }
        // chips
        if !search::parse(&self.search.string()).chips.is_empty() && y < HEAD + 34.0 {
            for (i, (chx, chw)) in self.chip_rects().into_iter().enumerate() {
                if cx >= chx + chw - 26.0 && cx < chx + chw && y >= HEAD && y < HEAD + 32.0 {
                    return Hit::Chip(i);
                }
            }
            return Hit::None;
        }
        // the dock
        let dy = self.dock_y();
        if y >= dy - 6.0 && y < dy + DOCK_H + 6.0 && cx >= 16.0 {
            let ax = self.cw() - 22.0 - 44.0;
            if cx >= ax && cx < ax + 44.0 {
                return Hit::DockAdd;
            }
            if self.dock_open {
                for i in 0..DOCK_TOOLS.len() {
                    let tx = self.tool_x(i);
                    if cx >= tx && cx < tx + 40.0 {
                        return Hit::DockTool(i);
                    }
                }
            }
            return Hit::Dock;
        }
        let top = self.list_top();
        if self.lines.is_empty() {
            let (bx, by, bw, bh) = self.empty_button();
            if self.empty_copy().2.is_some() && cx >= bx && cx < bx + bw && y - top >= by && y - top < by + bh {
                return Hit::EmptyCta;
            }
            return Hit::None;
        }
        let ly = y - top + self.scroll;
        let mut yy = 4.0;
        for l in &self.lines {
            let lh = if matches!(l, Line::Group(_)) { GROUP } else { ROW };
            if ly >= yy && ly < yy + lh {
                let Line::Job(j) = l else { return Hit::None };
                let rx = cx - 12.0;
                let ry = ly - yy;
                let rw = self.row_w();
                let near = |bx: f32| (rx - bx).abs() <= 15.0 && (ry - ROW / 2.0).abs() <= 15.0;
                let missing = self.is_missing(j);
                if j.state.finished() && missing && !j.expired && rx >= rw - 140.0 && (ry - ROW / 2.0).abs() <= 16.0 {
                    return Hit::RowBtn(j.id, RowBtn::Again);
                }
                if near(rw - 26.0) {
                    return Hit::RowBtn(j.id, if j.state.finished() { RowBtn::Reveal } else { RowBtn::Cancel });
                }
                if near(rw - 62.0) {
                    if !j.state.finished() {
                        return Hit::RowBtn(j.id, RowBtn::Toggle);
                    }
                    if !j.expired && !missing {
                        return Hit::RowBtn(j.id, RowBtn::Again);
                    }
                }
                return Hit::Row(j.id);
            }
            yy += lh;
        }
        Hit::None
    }

    fn set_hover(&mut self, h: Hit) {
        if h == self.hover {
            return;
        }
        let old = std::mem::replace(&mut self.hover, h.clone());
        let any = |f: fn(&Hit) -> bool| f(&old) || f(&h);
        if self.sheet.is_some() {
            let _ = self.paint_sheet();
            let _ = self.paint_caption();
            return;
        }
        if let (Hit::Page(_), _) | (_, Hit::Page(_)) = (&old, &h) {
            let ph = if let Hit::Page(p) = &h { p.clone() } else { settings_page::Hit::None };
            self.page.set_hover(ph);
        }
        if any(|x| matches!(x, Hit::CapMin | Hit::CapMax | Hit::CapClose)) {
            let _ = self.paint_caption();
        }
        if any(|x| matches!(x, Hit::Filter(_) | Hit::Settings | Hit::UpdateCard)) {
            let _ = self.paint_sidebar();
        }
        if any(|x| matches!(x, Hit::Search | Hit::SearchClear)) {
            let _ = self.paint_header();
        }
        if any(|x| matches!(x, Hit::Chip(_))) {
            let _ = self.paint_chips();
        }
        if any(|x| matches!(x, Hit::Option(_))) {
            let _ = self.paint_dropdown();
        }
        if self.menu.is_some() {
            let mh = if let Hit::Menu(m) = &h { Some(*m) } else { None };
            if let Some(m) = &mut self.menu {
                if m.hover != mh {
                    m.hover = mh;
                    let _ = self.paint_menu();
                }
            }
        }
        self.update_tip();
        let in_dock = matches!(h, Hit::Dock | Hit::DockAdd | Hit::DockTool(_));
        self.set_dock_open(in_dock || self.picked.len() > 1);
        if any(|x| matches!(x, Hit::DockAdd | Hit::DockTool(_))) {
            let _ = self.paint_dock();
        }
        if any(|x| matches!(x, Hit::EmptyCta)) {
            let _ = self.paint_empty(self.lines.is_empty());
        }
        if any(|x| matches!(x, Hit::Row(_) | Hit::RowBtn(..))) {
            let _ = self.sync_rows();
        }
    }

    // ---------- actions ----------

    fn job(&self, id: u64) -> Option<&JobView> {
        self.jobs.iter().find(|j| j.id == id)
    }

    fn toggle(&self, id: u64) {
        let Some(j) = self.job(id) else { return };
        let e = dl::engine();
        match j.state {
            State::Active | State::Queued | State::Seeding => e.pause(id),
            State::Paused | State::Failed => e.resume(id),
            State::Done => {}
        }
    }

    fn open(&self, id: u64) {
        command(&format!("dl:open:{id}"));
    }

    fn click(&mut self, hit: Hit, double: bool) -> Option<Blocking> {
        self.hide_tip();
        if !matches!(hit, Hit::Search | Hit::SearchClear | Hit::Option(_) | Hit::Chip(_)) && self.search_focus {
            self.focus_search(false);
        }
        match hit {
            Hit::Menu(popup::MHit::Item(i)) => {
                let Some(m) = self.menu.take() else { return None };
                self.close_menu();
                if let Some(popup::Item::Action { id, .. }) = m.model.items.get(i) {
                    self.act(id, &m.ids);
                }
            }
            Hit::Menu(popup::MHit::Prio(pr)) => {
                let Some(m) = self.menu.take() else { return None };
                self.close_menu();
                for id in &m.ids {
                    if self.job(*id).is_some_and(|j| !j.state.finished()) {
                        dl::engine().set_priority(*id, pr);
                    }
                }
            }
            Hit::Menu(popup::MHit::Card) => {}
            Hit::MenuOff => self.close_menu(),
            Hit::Filter(i) => self.set_view(Page::List(FILTERS[i].0)),
            Hit::Settings => self.set_view(Page::Settings),
            Hit::Search => {
                if !self.search_focus {
                    self.focus_search(true);
                }
            }
            Hit::SearchClear => {
                self.search = Field::default();
                self.search_focus = true;
                self.on_query_changed();
            }
            Hit::Option(i) => self.accept_option(i),
            Hit::Chip(i) => {
                let chips = search::parse(&self.search.string()).chips;
                if let Some(c) = chips.get(i) {
                    let q: Vec<String> = self.search.string().split_whitespace().filter(|t| *t != c.raw).map(String::from).collect();
                    self.search.set(&q.join(" "));
                    self.on_query_changed();
                }
            }
            Hit::DockAdd => self.open_add(None),
            // with several rows selected, the dock's actions are for them
            Hit::DockTool(i) if self.picked.len() > 1 => {
                let ids = self.sel_in_order();
                self.act(["pause", "resume", "remove"][i.min(2)], &ids);
            }
            Hit::DockTool(0) => dl::engine().pause_all(),
            Hit::DockTool(1) => dl::engine().resume_all(),
            Hit::DockTool(_) => dl::engine().clear_finished(),
            Hit::EmptyCta => {
                if self.search.text.is_empty() {
                    self.open_add(None);
                } else {
                    self.search = Field::default();
                    self.on_query_changed();
                }
            }
            Hit::Row(id) => {
                let (ctrl, shift) = unsafe { (GetKeyState(VK_CONTROL.0 as i32) < 0, GetKeyState(VK_SHIFT.0 as i32) < 0) };
                if ctrl {
                    if let Some(i) = self.picked.iter().position(|s| *s == id) {
                        self.picked.remove(i);
                    } else {
                        self.picked.push(id);
                    }
                    self.selected = Some(id);
                    self.anchor = Some(id);
                } else if shift && self.anchor.is_some() {
                    self.select_range(id);
                } else {
                    self.select_only(id);
                    if double {
                        self.open(id);
                    }
                }
                self.on_selection();
            }
            Hit::RowBtn(id, RowBtn::Toggle) => self.toggle(id),
            Hit::RowBtn(id, RowBtn::Cancel) => dl::engine().remove(id, true),
            Hit::RowBtn(id, RowBtn::Again) => {
                dl::engine().again(id);
                self.missing.remove(&id);
            }
            Hit::RowBtn(id, RowBtn::Reveal) => command(&format!("dl:reveal:{id}")),
            Hit::Page(h) => {
                if let settings_page::Hit::Switch(id) = &h {
                    self.page.press(Some(id));
                }
                let v = self.values();
                if let Some(settings_page::Action::Command(cmd)) = self.page.click(&h, &v) {
                    if cmd == "noop" {
                        self.build_page(true);
                    } else if cmd == "update:check" {
                        crate::update::check_now();
                    } else if cmd == "update:store" {
                        shell::open(std::path::Path::new(crate::update::STORE));
                    } else {
                        crate::stash::queue(cmd);
                    }
                }
            }
            Hit::Backdrop | Hit::Cancel => self.close_sheet(),
            Hit::Ok => self.submit_sheet(),
            Hit::Folder => return Some(Blocking::PickFolder),
            Hit::Torrent => return Some(Blocking::PickTorrent),
            Hit::Prio(p) => {
                if let Some(Sheet { kind: SheetKind::Add { priority, .. }, .. }) = &mut self.sheet {
                    *priority = p;
                }
                let _ = self.paint_sheet();
            }
            Hit::Files(h) => {
                if let Some(Sheet { kind: SheetKind::Files(f), .. }) = &mut self.sheet {
                    if f.click(h) {
                        let _ = self.paint_sheet();
                    }
                }
            }
            Hit::AlsoDelete => {
                if let Some(Sheet { kind: SheetKind::Remove { delete, .. }, .. }) = &mut self.sheet {
                    *delete = !*delete;
                }
                let _ = self.paint_sheet();
            }
            Hit::None => {
                if self.selected.take().is_some() || !self.picked.is_empty() {
                    self.picked.clear();
                    self.on_selection();
                }
            }
            Hit::UpdateCard => crate::stash::queue("update:apply".into()),
            Hit::Form(h) => return self.form_click(h),
            Hit::Field | Hit::Sheet | Hit::Caption | Hit::Dock | Hit::CapMin | Hit::CapMax | Hit::CapClose => {}
        }
        None
    }

    // ---------- selection ----------

    fn select_only(&mut self, id: u64) {
        self.picked = vec![id];
        self.selected = Some(id);
        self.anchor = Some(id);
    }

    /// The downloads on the list, top to bottom.
    fn list_ids(&self) -> Vec<u64> {
        self.lines.iter().filter_map(|l| if let Line::Job(j) = l { Some(j.id) } else { None }).collect()
    }

    /// Shift+click: everything from the anchor to `id`.
    fn select_range(&mut self, id: u64) {
        let ids = self.list_ids();
        let a = self.anchor.and_then(|a| ids.iter().position(|x| *x == a));
        let b = ids.iter().position(|x| *x == id);
        if let (Some(a), Some(b)) = (a, b) {
            self.picked = ids[a.min(b)..=a.max(b)].to_vec();
        } else {
            self.picked = vec![id];
        }
        self.selected = Some(id);
    }

    fn sel_in_order(&self) -> Vec<u64> {
        self.list_ids().into_iter().filter(|id| self.picked.contains(id)).collect()
    }

    /// The rows, and the dock (its actions follow a multiple selection), catch up.
    fn on_selection(&mut self) {
        let _ = self.sync_rows();
        let in_dock = matches!(self.hover, Hit::Dock | Hit::DockAdd | Hit::DockTool(_));
        self.set_dock_open(in_dock || self.picked.len() > 1);
        let _ = self.paint_dock();
    }

    /// Run a menu or keyboard action on these downloads.
    fn act(&mut self, action: &str, ids: &[u64]) {
        let e = dl::engine();
        match action {
            "copy" => {
                let links: Vec<String> = ids.iter().filter_map(|id| self.job(*id).map(|j| j.source.clone())).collect();
                if links.is_empty() {
                    return;
                }
                shell::set_clipboard_text(&links.join("\r\n"));
                let note = if links.len() == 1 { "Download link copied".to_string() } else { format!("{} download links copied", links.len()) };
                self.show_toast(&note);
            }
            "remove" => self.ask_remove(ids.to_vec()),
            "files" => {
                if let Some(id) = ids.first().copied() {
                    let first = self.job(id).is_some_and(|j| j.choose_files);
                    self.open_files(id, first);
                }
            }
            "pause" => {
                for id in ids {
                    if self.job(*id).is_some_and(|j| matches!(j.state, State::Active | State::Queued | State::Seeding)) {
                        e.pause(*id);
                    }
                }
            }
            "resume" => {
                for id in ids {
                    if self.job(*id).is_some_and(|j| matches!(j.state, State::Paused | State::Failed)) {
                        e.resume(*id);
                    }
                }
            }
            "limit" => self.open_limit(ids.to_vec()),
            "refresh" => {
                if let Some(id) = ids.first() {
                    self.open_refresh(*id);
                }
            }
            "again" => {
                for id in ids {
                    if self.job(*id).is_some_and(|j| !j.expired && (j.state.finished() || j.state == State::Failed)) {
                        e.again(*id);
                        self.missing.remove(id);
                    }
                }
            }
            _ => {
                for id in ids {
                    command(&format!("dl:{action}:{id}"));
                }
            }
        }
    }

    /// Remove downloads from the list. Unfinished ones are cancelled (their partial
    /// file goes); when finished files are still on the PC, ask whether they go too.
    fn ask_remove(&mut self, ids: Vec<u64>) {
        self.ask_remove_with(ids, false);
    }

    /// Shift+Delete, as in Explorer: delete the files for good (after asking), and take
    /// the downloads off the list. With no file on disk, it's a plain remove.
    fn ask_delete(&mut self, ids: Vec<u64>) {
        self.ask_remove_with(ids, true);
    }

    fn ask_remove_with(&mut self, ids: Vec<u64>, permanent: bool) {
        let has_file = |j: &JobView| j.state.finished() && j.path.as_ref().is_some_and(|p| p.exists());
        let jobs: Vec<JobView> = ids.iter().filter_map(|id| self.job(*id).cloned()).collect();
        let Some(first) = jobs.iter().find(|j| has_file(j)).cloned() else {
            // stopping several downloads part-way throws their progress away: ask first
            let started = jobs.iter().filter(|j| !j.state.finished() && j.done > 0).count();
            if started > 1 {
                let more = jobs.len() - 1;
                let done: u64 = jobs.iter().filter(|j| !j.state.finished()).map(|j| j.done).sum();
                let name = format!("{}  and {more} more", jobs[0].name);
                let path = format!("{} downloaded so far", paint::bytes(done));
                self.open_sheet(SheetKind::Remove { ids: jobs.iter().map(|j| j.id).collect(), name, path, delete: false, permanent: false, cancel: true });
                return;
            }
            for j in &jobs {
                dl::engine().remove(j.id, !j.state.finished());
            }
            if jobs.len() > 1 {
                self.show_toast(&format!("Removed {} downloads", jobs.len()));
            }
            self.picked.clear();
            return;
        };
        let files = jobs.iter().filter(|j| has_file(j)).count();
        let (name, path) = if jobs.len() == 1 {
            let path = first.path.as_ref().map(|p| p.parent().unwrap_or(p).display().to_string()).unwrap_or_default();
            (first.name.clone(), format!("{path}  \u{b7}  {}", first.total.map(paint::bytes).unwrap_or_default()))
        } else {
            let more = jobs.len() - 1;
            let total: u64 = jobs.iter().filter(|j| has_file(j)).filter_map(|j| j.total).sum();
            (format!("{}  and {more} more", first.name), format!("{files} of {} files still on your PC  \u{b7}  {}", jobs.len(), paint::bytes(total)))
        };
        self.open_sheet(SheetKind::Remove { ids: jobs.iter().map(|j| j.id).collect(), name, path, delete: permanent, permanent, cancel: false });
    }

    // ---------- the right-click menu ----------

    fn menu_items(&self, ids: &[u64]) -> Vec<popup::Item> {
        use popup::Item;
        // each action's icon, by its command
        let icon = |id: &str| -> Option<&'static str> {
            Some(match id {
                "open" => icons::OPEN,
                "reveal" => icons::FOLDER,
                "pause" => icons::PAUSE,
                "resume" => icons::PLAY,
                "retry" => icons::RETRY,
                "now" => icons::BOLT,
                "copy" => icons::LINK,
                "source" => icons::GLOBE,
                "again" => icons::DOWNLOAD,
                "remove" => icons::TRASH,
                "cancel" => icons::CANCEL,
                "files" => icons::LIST,
                _ => return None,
            })
        };
        // removing is the one destructive action: always in the danger colour
        let action = |id: &str, label: String, keys: Option<&'static str>, enabled: bool| Item::Action { id: id.into(), label, icon: icon(id), keys, enabled, danger: id == "remove" };
        let jobs: Vec<&JobView> = ids.iter().filter_map(|id| self.job(*id)).collect();
        if let [j] = jobs[..] {
            let done = j.state.finished();
            let missing = self.is_missing(j);
            let mut items = vec![
                action("open", "Open".into(), Some("Enter"), done && !missing),
                action("reveal", "Show in Folder".into(), None, !missing),
                Item::Sep,
            ];
            if !done {
                let (id, label, glyph) = match j.state {
                    State::Active | State::Queued | State::Seeding => ("pause", "Pause", "pause"),
                    State::Failed => ("resume", "Try Again", "retry"),
                    _ => ("resume", "Resume", "resume"),
                };
                items.push(Item::Action { id: id.into(), label: label.into(), icon: icon(glyph), keys: Some("Space"), enabled: true, danger: false });
                items.push(action("now", "Start Now".into(), None, j.state == State::Queued));
                if j.kind == Kind::Http {
                    let limit = if j.limit_bps > 0 { format!("Speed Limit ({} KB/s)\u{2026}", j.limit_bps / 1024) } else { "Speed Limit\u{2026}".to_string() };
                    items.push(Item::Action { id: "limit".into(), label: limit, icon: None, keys: None, enabled: true, danger: false });
                    if matches!(j.state, State::Failed | State::Paused) || j.expired {
                        items.push(Item::Action { id: "refresh".into(), label: "Refresh Link\u{2026}".into(), icon: icon("copy"), keys: None, enabled: true, danger: false });
                    }
                }
                items.push(Item::Priority(Some(j.priority)));
                items.push(Item::Sep);
            }
            if j.kind == Kind::Torrent {
                let many = dl::torrent_files(j.id).is_some_and(|l| l.files.len() > 1);
                items.push(action("files", "Choose Files\u{2026}".into(), None, many));
            }
            let copy = if j.source.starts_with("magnet:") { "Copy Magnet Link" } else { "Copy Download Link" };
            items.push(action("copy", copy.into(), Some("Ctrl+C"), true));
            items.push(action("source", "Open Source Page".into(), None, j.referrer.is_some() || j.source.starts_with("http")));
            let again = if j.expired { "Download Again (link expired)" } else { "Download Again" };
            items.push(action("again", again.into(), None, !j.expired && (done || j.state == State::Failed)));
            items.push(Item::Sep);
            items.push(if done {
                action("remove", "Remove from List\u{2026}".into(), Some("Del"), true)
            } else {
                Item::Action { id: "remove".into(), label: "Cancel Download".into(), icon: icon("cancel"), keys: Some("Del"), enabled: true, danger: true }
            });
            return items;
        }
        let n = jobs.len();
        let count = |f: &dyn Fn(&JobView) -> bool| jobs.iter().filter(|j| f(j)).count();
        let pausable = count(&|j| matches!(j.state, State::Active | State::Queued | State::Seeding));
        let resumable = count(&|j| matches!(j.state, State::Paused | State::Failed));
        let again = count(&|j| !j.expired && (j.state.finished() || j.state == State::Failed));
        let unfinished: Vec<&&JobView> = jobs.iter().filter(|j| !j.state.finished()).collect();
        // the count each action applies to (none: the plain, disabled verb)
        let label = |n: usize, verb: &str, one: &str, many: &str| match n {
            0 => verb.to_string(),
            1 => format!("{verb} {one}"),
            n => format!("{verb} {n} {many}"),
        };
        let mut items = vec![
            action("pause", label(pausable, "Pause", "Download", "Downloads"), Some("Space"), pausable > 0),
            action("resume", label(resumable, "Resume", "Download", "Downloads"), None, resumable > 0),
            action(
                "again",
                match again {
                    0 => "Download Again".to_string(),
                    1 => "Download 1 File Again".to_string(),
                    n => format!("Download {n} Files Again"),
                },
                None,
                again > 0,
            ),
        ];
        if !unfinished.is_empty() {
            let first = unfinished[0].priority;
            items.push(Item::Priority(unfinished.iter().all(|j| j.priority == first).then_some(first)));
        }
        items.push(Item::Sep);
        items.push(action("copy", format!("Copy {n} Links"), Some("Ctrl+C"), true));
        items.push(Item::Sep);
        items.push(action("remove", format!("Remove {n} from List\u{2026}"), Some("Del"), true));
        items
    }

    /// Open the menu for the selection at (x, y): it grows from the pointer, flipping
    /// to stay inside the window.
    fn open_menu(&mut self, x: f32, y: f32) {
        self.hide_tip();
        let ids = self.sel_in_order();
        if ids.is_empty() {
            return;
        }
        let k = self.k;
        let dw = self.gfx.dwrite.clone();
        let model = popup::Menu::new(self.menu_items(&ids), |t, size, _| paint::advance(&dw, t, size * k) / k);
        let (w, h) = (model.w, model.h);
        let right = x + w > self.w - 8.0;
        let up = y + h > self.h - 8.0;
        let mx = (if right { x - w } else { x }).clamp(8.0, (self.w - w - 8.0).max(8.0));
        let my = (if up { y - h } else { y }).clamp(8.0, (self.h - h - 8.0).max(8.0));
        self.menu = Some(MenuState { model, x: mx, y: my, hover: None, ids });
        let _ = (|| -> Result<()> {
            self.paint_menu()?;
            let v = &self.menu_v;
            v.SetOffset(Vector3 { X: mx * k, Y: my * k, Z: 0.0 })?;
            v.SetCenterPoint(Vector3 { X: if right { w * k } else { 0.0 }, Y: if up { h * k } else { 0.0 }, Z: 0.0 })?;
            let c = &self.gfx.compositor;
            let o = c.CreateScalarKeyFrameAnimation()?;
            o.InsertKeyFrame(0.0, 0.0)?;
            o.InsertKeyFrameWithEasingFunction(1.0, 1.0, &self.ease_out()?)?;
            o.SetDuration(ms(110))?;
            v.StartAnimation(&HSTRING::from("Opacity"), &o)?;
            let sc = c.CreateVector3KeyFrameAnimation()?;
            sc.InsertKeyFrame(0.0, Vector3 { X: 0.95, Y: 0.95, Z: 1.0 })?;
            sc.InsertKeyFrameWithEasingFunction(1.0, Vector3 { X: 1.0, Y: 1.0, Z: 1.0 }, &self.ease_out()?)?;
            sc.SetDuration(ms(170))?;
            v.StartAnimation(&HSTRING::from("Scale"), &sc)
        })();
    }

    fn paint_menu(&self) -> Result<()> {
        let Some(m) = &self.menu else { return Ok(()) };
        self.sprite_from(&self.menu_v, m.model.w, m.model.h, |p| m.model.paint(p, m.hover))
    }

    fn close_menu(&mut self) {
        if self.menu.take().is_none() && !self.menu_v.Opacity().is_ok_and(|o| o > 0.0) {
            return;
        }
        let _ = (|| -> Result<()> {
            let o = self.gfx.compositor.CreateScalarKeyFrameAnimation()?;
            o.InsertExpressionKeyFrame(0.0, &HSTRING::from("this.StartingValue"))?;
            o.InsertKeyFrame(1.0, 0.0)?;
            o.SetDuration(ms(90))?;
            self.menu_v.StartAnimation(&HSTRING::from("Opacity"), &o)
        })();
    }

    /// Arrow keys move through the menu, Enter runs the item, Esc closes it.
    fn menu_key(&mut self, vk: u16) -> bool {
        let Some(m) = &mut self.menu else { return false };
        let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
        let items = m.model.actionable();
        let at = match m.hover {
            Some(popup::MHit::Item(i)) => items.iter().position(|x| *x == i),
            _ => None,
        };
        let next = vk == VK_DOWN.0 || (ctrl && vk == b'N' as u16);
        let prev = vk == VK_UP.0 || (ctrl && vk == b'P' as u16);
        if (next || prev) && !items.is_empty() {
            let n = items.len();
            let i = match (at, next) {
                (None, true) => 0,
                (None, false) => n - 1,
                (Some(i), true) => (i + 1) % n,
                (Some(i), false) => (i + n - 1) % n,
            };
            m.hover = Some(popup::MHit::Item(items[i]));
            let _ = self.paint_menu();
        } else if vk == VK_RETURN.0 || vk == VK_SPACE.0 {
            if let Some(h) = m.hover {
                self.click(Hit::Menu(h), false);
            }
        } else {
            self.close_menu();
        }
        true
    }

    // ---------- tooltips ----------

    fn tip_text(&self, h: &Hit) -> Option<String> {
        let multi = self.picked.len() > 1;
        Some(match h {
            Hit::DockAdd => "New download  (Ctrl+N)".into(),
            Hit::DockTool(i) if multi => ["Pause selected", "Resume selected", "Remove selected"][(*i).min(2)].into(),
            Hit::DockTool(i) => DOCK_TOOLS.get(*i)?.1.into(),
            Hit::RowBtn(id, b) => {
                let j = self.job(*id)?;
                match b {
                    RowBtn::Toggle => match j.state {
                        State::Active | State::Queued | State::Seeding => "Pause",
                        State::Failed => "Try again",
                        _ => "Resume",
                    },
                    RowBtn::Cancel => "Cancel download",
                    RowBtn::Reveal => "Show in folder",
                    RowBtn::Again if self.is_missing(j) => return None, // the button says it
                    RowBtn::Again => "Download again",
                }
                .into()
            }
            _ => return None,
        })
    }

    /// Where a tooltip points: the button's center x and its top and bottom (DIPs).
    fn tip_anchor(&self, h: &Hit) -> Option<(f32, f32, f32)> {
        match h {
            Hit::DockAdd => Some((self.w - 22.0 - 22.0, self.dock_y(), self.dock_y() + 44.0)),
            Hit::DockTool(i) => Some((SIDEBAR + self.tool_x(*i) + 20.0, self.dock_y() + 2.0, self.dock_y() + 42.0)),
            Hit::RowBtn(id, b) => {
                let i = self.lines.iter().position(|l| matches!(l, Line::Job(j) if j.id == *id))?;
                let top = self.list_top() + self.line_y(i) - self.scroll + ROW / 2.0 - 15.0;
                let bx = match b {
                    RowBtn::Cancel | RowBtn::Reveal => self.row_w() - 26.0,
                    _ => self.row_w() - 62.0,
                };
                Some((SIDEBAR + 12.0 + bx, top, top + 30.0))
            }
            _ => None,
        }
    }

    /// After a hover change: the hovered button's tooltip shows after a short rest
    /// (at once if another one was just up), and goes when the pointer leaves.
    fn update_tip(&mut self) {
        if self.tip_text(&self.hover).is_none() || self.menu.is_some() {
            self.hide_tip();
            return;
        }
        if self.tip_on || self.tip_off_at.elapsed().as_millis() < 400 {
            self.show_tip();
        } else {
            unsafe {
                let _ = SetTimer(Some(self.hwnd), T_TIP, 550, None);
            }
        }
    }

    fn show_tip(&mut self) {
        let hover = self.hover.clone();
        let (Some(text), Some((cx, top, bottom))) = (self.tip_text(&hover), self.tip_anchor(&hover)) else { return };
        let k = self.k;
        let dw = self.gfx.dwrite.clone();
        let w = popup::tip_w(&text, |t, size, _| paint::advance(&dw, t, size * k) / k);
        let mut y = top - 6.0 - popup::TIP_H;
        if matches!(hover, Hit::RowBtn(..)) && y < self.list_top() {
            y = bottom + 6.0;
        }
        let x = (cx - w / 2.0).clamp(SIDEBAR + 4.0, self.w - w - 4.0);
        let was_on = self.tip_on;
        self.tip_on = true;
        let _ = (|| -> Result<()> {
            self.sprite_from(&self.tip, w, popup::TIP_H, |p| popup::paint_tip(p, &text, w))?;
            self.tip.SetOffset(Vector3 { X: (x * k).round(), Y: (y * k).round(), Z: 0.0 })?;
            if !was_on {
                let o = self.gfx.compositor.CreateScalarKeyFrameAnimation()?;
                o.InsertExpressionKeyFrame(0.0, &HSTRING::from("this.StartingValue"))?;
                o.InsertKeyFrameWithEasingFunction(1.0, 1.0, &self.ease_out()?)?;
                o.SetDuration(ms(140))?;
                self.tip.StartAnimation(&HSTRING::from("Opacity"), &o)?;
            }
            Ok(())
        })();
    }

    fn hide_tip(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), T_TIP);
        }
        if !self.tip_on {
            return;
        }
        self.tip_on = false;
        self.tip_off_at = std::time::Instant::now();
        let _ = (|| -> Result<()> {
            let o = self.gfx.compositor.CreateScalarKeyFrameAnimation()?;
            o.InsertExpressionKeyFrame(0.0, &HSTRING::from("this.StartingValue"))?;
            o.InsertKeyFrame(1.0, 0.0)?;
            o.SetDuration(ms(90))?;
            self.tip.StartAnimation(&HSTRING::from("Opacity"), &o)
        })();
    }

    fn on_drop(&mut self, paths: Vec<PathBuf>) {
        let dir = default_dir(&self.settings);
        for p in paths {
            if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("torrent")) {
                dl::engine().add(AddRequest { source: p.display().to_string(), dir: dir.clone(), ..Default::default() });
            }
        }
    }

    fn on_key(&mut self, vk: u16) -> bool {
        let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
        if self.sheet.is_some() {
            return self.sheet_key(vk, ctrl);
        }
        if self.menu.is_some() {
            return self.menu_key(vk);
        }
        self.hide_tip();
        if ctrl && vk == b'F' as u16 {
            self.focus_search(true);
            self.search.all = !self.search.text.is_empty();
            let _ = self.paint_header();
            return true;
        }
        if self.search_focus {
            let open = !self.options.is_empty();
            let next = vk == VK_DOWN.0 || (ctrl && vk == b'N' as u16);
            let prev = vk == VK_UP.0 || (ctrl && vk == b'P' as u16);
            if open && (next || prev) {
                let n = self.options.len();
                self.pick = if next { (self.pick + 1) % n } else { (self.pick + n - 1) % n };
                let _ = self.paint_dropdown();
                return true;
            }
            if open && (vk == VK_TAB.0 || vk == VK_RETURN.0) {
                self.accept_option(self.pick);
                return true;
            }
            if vk == VK_ESCAPE.0 {
                if open {
                    self.options.clear();
                    let _ = self.paint_dropdown();
                } else if !self.search.text.is_empty() {
                    self.search = Field::default();
                    self.on_query_changed();
                } else {
                    self.focus_search(false);
                }
                return true;
            }
            if vk == VK_RETURN.0 || vk == VK_DOWN.0 {
                // into the results
                self.focus_search(false);
                if let Some(id) = self.list_ids().first().copied() {
                    self.select_only(id);
                    self.on_selection();
                }
                return true;
            }
            if self.search.key(vk, ctrl) {
                self.on_query_changed();
                return true;
            }
            return false;
        }
        // Settings hides the list: its keys (Space, Delete, Ctrl+A...) mustn't reach rows you can't see
        if self.view == Page::Settings && !(ctrl && (vk == b'N' as u16 || vk == b'V' as u16)) {
            return false;
        }
        let ids: Vec<u64> = self.lines.iter().filter_map(|l| if let Line::Job(j) = l { Some(j.id) } else { None }).collect();
        let pos = self.selected.and_then(|s| ids.iter().position(|&id| id == s));
        match vk {
            v if ctrl && (v == b'N' as u16 || v == b'V' as u16) => {
                let clip = shell::clipboard_text().filter(|t| t.split_whitespace().any(dl::is_source));
                if v == b'N' as u16 || clip.is_some() {
                    self.open_add(clip);
                }
            }
            v if v == VK_DOWN.0 || v == VK_UP.0 => {
                let next = match (pos, v == VK_DOWN.0) {
                    (None, _) => 0,
                    (Some(p), true) => (p + 1).min(ids.len().saturating_sub(1)),
                    (Some(p), false) => p.saturating_sub(1),
                };
                if let Some(&id) = ids.get(next) {
                    // Shift extends the selection from the anchor; plain arrows move it
                    if unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0 && self.anchor.is_some() {
                        self.select_range(id);
                    } else {
                        self.select_only(id);
                    }
                    if let Some(i) = self.lines.iter().position(|l| matches!(l, Line::Job(j) if j.id == id)) {
                        let top = self.line_y(i);
                        let view = self.h - self.list_top() - 80.0;
                        if top - GROUP < self.scroll {
                            self.scroll = top - GROUP;
                        } else if top + ROW > self.scroll + view {
                            self.scroll = top + ROW - view;
                        }
                        self.clamp_scroll(true);
                    }
                    self.on_selection();
                }
            }
            v if ctrl && v == b'A' as u16 => {
                self.picked = ids.clone();
                self.anchor = ids.first().copied();
                self.on_selection();
            }
            v if v == VK_ESCAPE.0 && !self.picked.is_empty() => {
                self.picked.clear();
                self.selected = None;
                self.on_selection();
            }
            v if v == VK_SPACE.0 => {
                // one row pauses or resumes; several all pause, unless all are paused
                let ids = self.sel_in_order();
                if let [id] = ids[..] {
                    self.toggle(id);
                } else if !ids.is_empty() {
                    let running = ids.iter().any(|id| self.job(*id).is_some_and(|j| matches!(j.state, State::Active | State::Queued | State::Seeding)));
                    self.act(if running { "pause" } else { "resume" }, &ids);
                }
            }
            v if v == VK_RETURN.0 => {
                if let Some(id) = self.selected {
                    self.open(id);
                }
            }
            v if v == VK_DELETE.0 => {
                let ids = self.sel_in_order();
                if !ids.is_empty() {
                    // Shift+Delete skips the Recycle Bin, after asking, as in Explorer
                    if unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0 {
                        self.ask_delete(ids);
                    } else {
                        self.act("remove", &ids);
                    }
                }
            }
            v if ctrl && v == b'C' as u16 => {
                let ids = self.sel_in_order();
                self.act("copy", &ids);
            }
            // Shift+F10 or the menu key: the menu for the selection, under the focused row
            v if v == VK_APPS.0 || (v == VK_F10.0 && unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0) => {
                let at = self.selected.and_then(|id| self.lines.iter().position(|l| matches!(l, Line::Job(j) if j.id == id)));
                if let Some(i) = at {
                    let y = self.list_top() + self.line_y(i) - self.scroll + ROW - 8.0;
                    self.open_menu(SIDEBAR + 80.0, y);
                }
            }
            _ => return false,
        }
        true
    }

    fn on_char(&mut self, ch: char) {
        if self.form_char(ch) {
            return;
        }
        if let Some(Sheet { kind: SheetKind::Add { field, error, .. }, .. }) = &mut self.sheet {
            field.insert(&ch.to_string());
            *error = None;
            let _ = self.paint_sheet();
            return;
        }
        if self.sheet.is_some() {
            return;
        }
        if !self.search_focus && !ch.is_whitespace() {
            // typing anywhere searches
            self.focus_search(true);
        }
        if self.search_focus {
            self.search.insert(&ch.to_string());
            self.on_query_changed();
        }
    }

    fn show_toast(&mut self, text: &str) {
        self.toast_text = text.to_string();
        let k = self.k;
        let c = &self.gfx.compositor;
        let _ = (|| -> Result<()> {
            let dw = self.gfx.dwrite.clone();
            let tw = paint::advance(&dw, text, 13.0 * k) / k + 46.0;
            let t = text.to_string();
            self.sprite_from(&self.toast, tw, 38.0, |p| {
                p.popover(0.0, 0.0, tw, 38.0, popup::RADIUS);
                p.glyph(icons::CHECK, 18.0, 19.0, 11.0, paint::green());
                p.text_mid(&t, 32.0, 19.0, 13.0, MEDIUM, paint::text(), tw);
            })?;
            self.toast.SetOffset(Vector3 { X: (SIDEBAR + (self.cw() - tw) / 2.0) * k, Y: (self.dock_y() - 52.0) * k, Z: 0.0 })?;
            let o = c.CreateScalarKeyFrameAnimation()?;
            o.InsertKeyFrame(0.0, 0.0)?;
            o.InsertKeyFrame(0.12, 1.0)?;
            o.InsertKeyFrame(0.85, 1.0)?;
            o.InsertKeyFrame(1.0, 0.0)?;
            o.SetDuration(ms(2000))?;
            self.toast.StartAnimation(&HSTRING::from("Opacity"), &o)
        })();
    }

    // ---------- sheets: New Download and Remove ----------

    fn sheet_size(&self, kind: &SheetKind) -> (f32, f32) {
        match kind {
            SheetKind::Add { .. } => (SHEET_W, ADD_H),
            // no switch to show when it's a permanent delete
            SheetKind::Remove { permanent: true, .. } | SheetKind::Remove { cancel: true, .. } => (420.0, REMOVE_H - 46.0),
            SheetKind::Remove { .. } => (420.0, REMOVE_H),
            SheetKind::Form(f) => f.size(),
            SheetKind::Files(f) => (crate::torrent_sheet::W, f.height()),
        }
    }

    /// Open the Choose Files sheet for a torrent; false if its files aren't known yet.
    fn open_files(&mut self, id: u64, first: bool) -> bool {
        let Some(f) = crate::torrent_sheet::FilesSheet::new(id, first) else { return false };
        self.open_sheet(SheetKind::Files(f));
        true
    }

    fn sheet_origin(&self, kind: &SheetKind) -> (f32, f32) {
        let (w, h) = self.sheet_size(kind);
        ((self.w - w) / 2.0, ((self.h - h) / 2.0 - 20.0).max(10.0))
    }

    fn open_sheet(&mut self, kind: SheetKind) {
        self.close_sheet();
        self.close_menu();
        self.hide_tip();
        self.focus_search(false);
        let _ = (|| -> Result<()> {
            let c = self.gfx.compositor.clone();
            let root = c.CreateContainerVisual()?;
            let dim = c.CreateSpriteVisual()?;
            dim.SetBrush(&c.CreateColorBrushWithColor(Color { A: if paint::light() { 56 } else { 120 }, R: 0, G: 0, B: 0 })?)?;
            dim.SetRelativeSizeAdjustment(Vector2 { X: 1.0, Y: 1.0 })?;
            let card = c.CreateSpriteVisual()?;
            let caret = c.CreateSpriteVisual()?;
            caret.SetBrush(&c.CreateColorBrushWithColor(Color { A: 255, R: 242, G: 125, B: 46 })?)?;
            caret.StartAnimation(&HSTRING::from("Opacity"), &blink(&self.gfx)?)?;
            card.Children()?.InsertAtTop(&caret)?;
            root.Children()?.InsertAtTop(&dim)?;
            root.Children()?.InsertAtTop(&card)?;
            root.SetSize(Vector2 { X: self.w * self.k, Y: self.h * self.k })?;
            self.root.Children()?.InsertAtTop(&root)?;
            card.SetShadow(&popover_shadow(&self.gfx, self.k, false)?)?;
            let switch = if matches!(kind, SheetKind::Remove { .. }) { Some(settings_page::new_switch(&self.gfx, &card.Children()?, self.k)?) } else { None };
            let slider = if matches!(kind, SheetKind::Add { .. }) {
                let v = c.CreateSpriteVisual()?;
                card.Children()?.InsertAtBottom(&v)?;
                Some((v, Priority::Normal))
            } else {
                None
            };
            self.sheet = Some(Sheet { kind, root: root.clone(), card: card.clone(), caret, switch, slider });
            self.place_sheet()?;
            self.paint_sheet()?;
            let fade = c.CreateScalarKeyFrameAnimation()?;
            fade.InsertKeyFrame(0.0, 0.0)?;
            fade.InsertKeyFrame(1.0, 1.0)?;
            fade.SetDuration(ms(160))?;
            root.StartAnimation(&HSTRING::from("Opacity"), &fade)?;
            card.SetScale(Vector3 { X: 0.95, Y: 0.95, Z: 1.0 })?;
            let s = c.CreateVector3KeyFrameAnimation()?;
            s.InsertKeyFrameWithEasingFunction(1.0, Vector3 { X: 1.0, Y: 1.0, Z: 1.0 }, &self.back()?)?;
            s.SetDuration(ms(300))?;
            card.StartAnimation(&HSTRING::from("Scale"), &s)?;
            Ok(())
        })();
        self.hover = Hit::None;
    }

    fn open_add(&mut self, text: Option<String>) {
        if let Some(Sheet { kind: SheetKind::Add { field, .. }, .. }) = &mut self.sheet {
            if let Some(t) = text {
                field.set(&t);
            }
            let _ = self.paint_sheet();
            return;
        }
        // a copied link fills itself in
        let text = text
            .filter(|t| !t.is_empty())
            .or_else(|| shell::clipboard_text().map(|t| t.split_whitespace().filter(|s| dl::is_source(s)).collect::<Vec<_>>().join(" ")).filter(|t| !t.is_empty()))
            .unwrap_or_default();
        let mut field = Field::default();
        field.set(&text);
        field.all = !text.is_empty();
        self.open_sheet(SheetKind::Add { field, dir: default_dir(&self.settings), priority: Priority::Normal, error: None });
    }

    fn open_remove(&mut self, id: u64) {
        self.ask_remove(vec![id]);
    }

    fn place_sheet(&self) -> Result<()> {
        let Some(s) = &self.sheet else { return Ok(()) };
        let (x, y) = self.sheet_origin(&s.kind);
        let (w, h) = self.sheet_size(&s.kind);
        let k = self.k;
        s.root.SetSize(Vector2 { X: self.w * k, Y: self.h * k })?;
        s.card.SetOffset(Vector3 { X: x * k, Y: y * k, Z: 0.0 })?;
        s.card.SetCenterPoint(Vector3 { X: w * k / 2.0, Y: h * k / 2.0, Z: 0.0 })
    }

    fn close_sheet(&mut self) {
        if let Some(s) = self.sheet.take() {
            let _ = self.root.Children().and_then(|c| c.Remove(&s.root));
        }
        self.hover = Hit::None;
    }

    fn sheet_hit(&self, s: &Sheet, x: f32, y: f32) -> Hit {
        let (ox, oy) = self.sheet_origin(&s.kind);
        let (w, h) = self.sheet_size(&s.kind);
        let (x, y) = (x - ox, y - oy);
        if !(0.0..w).contains(&x) || !(0.0..h).contains(&y) {
            return Hit::Backdrop;
        }
        let inside = |l: f32, t: f32, rw: f32, rh: f32| x >= l && x < l + rw && y >= t && y < t + rh;
        match &s.kind {
            SheetKind::Form(_) => return self.form_hit(x, y).map(Hit::Form).unwrap_or(Hit::Sheet),
            SheetKind::Add { .. } => {
                if let Some(h) = self.add_sheet_links(x, y) {
                    return Hit::Form(h);
                }
                if inside(24.0, 76.0, w - 48.0, 36.0) {
                    return Hit::Field;
                }
                if inside(w - 100.0, 136.0, 80.0, 26.0) {
                    return Hit::Folder;
                }
                let (_, parts) = self.prio_seg();
                for (r, p) in parts.iter().zip([Priority::Low, Priority::Normal, Priority::High]) {
                    if r.contains(x, y) {
                        return Hit::Prio(p);
                    }
                }
                if inside(18.0, 214.0, 150.0, 34.0) {
                    return Hit::Torrent;
                }
                if inside(w - 24.0 - 120.0 - 10.0 - 92.0, 214.0, 92.0, 36.0) {
                    return Hit::Cancel;
                }
                if inside(w - 24.0 - 120.0, 214.0, 120.0, 36.0) {
                    return Hit::Ok;
                }
            }
            SheetKind::Remove { permanent, cancel, .. } => {
                if !permanent && !cancel && inside(22.0, 152.0, w - 44.0, 34.0) {
                    return Hit::AlsoDelete;
                }
                let ok_w = 178.0;
                let bh = crate::ui::BUTTON_H;
                if inside(w - 22.0 - ok_w, h - 22.0 - bh, ok_w, bh) {
                    return Hit::Ok;
                }
                if inside(w - 22.0 - ok_w - 10.0 - 92.0, h - 22.0 - bh, 92.0, bh) {
                    return Hit::Cancel;
                }
            }
            SheetKind::Files(f) => {
                use crate::torrent_sheet::FHit;
                return match f.hit(x, y) {
                    Some(FHit::Ok) => Hit::Ok,
                    Some(FHit::Cancel) => Hit::Cancel,
                    Some(h) => Hit::Files(h),
                    None => Hit::Sheet,
                };
            }
        }
        Hit::Sheet
    }

    /// The New Download sheet's priority control: the app's segmented control.
    fn prio_seg(&self) -> (settings_page::Rect, Vec<settings_page::Rect>) {
        let dw = self.gfx.dwrite.clone();
        let k = self.k;
        let widths = crate::ui::seg_widths(&["Low", "Normal", "High"], |t| paint::advance(&dw, t, 13.0 * k) / k + 14.0);
        crate::ui::seg_layout(104.0, 187.0, &widths)
    }

    fn paint_sheet(&mut self) -> Result<()> {
        if self.is_form() {
            return self.paint_form();
        }
        let prio = self.prio_seg();
        let k = self.k;
        let hover = self.hover.clone();
        let dw = self.gfx.dwrite.clone();
        let Some(s) = &mut self.sheet else { return Ok(()) };
        let (card, caret_v, live_switch) = (s.card.clone(), s.caret.clone(), s.switch.clone());
        match &mut s.kind {
            SheetKind::Add { field, dir, priority, error } => {
                let w = SHEET_W;
                let text = field.string();
                let field_w = w - 48.0 - 20.0;
                let before: String = field.text[..field.caret].iter().collect();
                let caret_x = paint::advance(&dw, &before, FIELD_TEXT * k) / k;
                if caret_x - field.scroll_x > field_w {
                    field.scroll_x = caret_x - field_w;
                } else if caret_x < field.scroll_x {
                    field.scroll_x = (caret_x - 20.0).max(0.0);
                }
                let (scroll_x, all, priority) = (field.scroll_x, field.all, *priority);
                let error = error.clone();
                let folder = dir.display().to_string();
                let count: usize = text.split_whitespace().filter(|t| dl::is_source(t)).map(dl::pattern_count).sum();
                let ok_label = if count > 1 { format!("Download {count}") } else { "Download".to_string() };
                let full_w = paint::advance(&dw, &text, FIELD_TEXT * k) / k;
                let draw = |p: &P| {
                    p.popover(0.0, 0.0, w, ADD_H, 26.0);
                    p.text("New Download", 24.0, 19.0, 17.0, SEMIBOLD, paint::text(), 300.0);
                    p.text("Paste one or more links: web addresses or magnet links.", 24.0, 46.0, 12.5, REGULAR, paint::dim(), w - 48.0);
                    p.round(24.0, 76.0, w - 48.0, 36.0, 13.0, paint::well(0.25));
                    p.round_stroke(24.0, 76.0, w - 48.0, 36.0, 13.0, paint::rgb(paint::ORANGE, 0.75), 1.5);
                    unsafe {
                        p.dc.PushAxisAlignedClip(&D2D_RECT_F { left: 34.0 * k, top: 78.0 * k, right: (w - 34.0) * k, bottom: 110.0 * k }, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
                    }
                    if text.is_empty() {
                        p.text_mid("https://example.com/file.zip   or   magnet:?xt=…", 34.0, 94.0, FIELD_TEXT, REGULAR, paint::faint(), field_w);
                    } else {
                        if all {
                            p.round(34.0 - scroll_x, 84.0, full_w.min(field_w + scroll_x), 20.0, 4.0, paint::rgb(paint::ORANGE, 0.4));
                        }
                        p.text_mid(&text, 34.0 - scroll_x, 94.0, FIELD_TEXT, REGULAR, paint::text(), 100_000.0);
                    }
                    unsafe {
                        p.dc.PopAxisAlignedClip();
                    }
                    if let Some(e) = &error {
                        p.text(e, 26.0, 116.0, 11.5, REGULAR, paint::red(), w - 48.0);
                    }
                    p.text_mid("Save to", 24.0, 149.0, 13.0, REGULAR, paint::dim(), 80.0);
                    p.glyph(icons::FOLDER, 112.0, 149.0, 12.0, paint::dim());
                    p.text_mid(&folder, 126.0, 149.0, 13.0, REGULAR, paint::text(), w - 126.0 - 110.0);
                    let change_w = p.measure("Change…", 13.0, MEDIUM);
                    crate::ui::link(p, w - 26.0 - change_w, 149.0, "Change…", None, hover == Hit::Folder);
                    p.text_mid("Priority", 24.0, 187.0, 13.0, REGULAR, paint::dim(), 80.0);
                    // the app's segmented control; its selection is a sprite that slides
                    let (rect, parts) = &prio;
                    let sel = [Priority::Low, Priority::Normal, Priority::High].iter().position(|x| *x == priority);
                    let hot = [Priority::Low, Priority::Normal, Priority::High].iter().position(|x| hover == Hit::Prio(*x));
                    crate::ui::paint_seg(p, *rect, parts, &["Low", "Normal", "High"], sel, hot, false);
                    crate::ui::link(p, 26.0, 232.0, "Open Torrent File…", Some(icons::TORRENT), hover == Hit::Torrent);
                    Manager::paint_add_sheet_links(p, &hover);
                    let ok_x = w - 24.0 - 120.0;
                    let cancel_x = ok_x - 10.0 - 92.0;
                    let bh = crate::ui::BUTTON_H;
                    crate::ui::button(p, settings_page::Rect::new(cancel_x, 214.0, 92.0, bh), "Cancel", None, crate::ui::Kind::Secondary, hover == Hit::Cancel, true);
                    crate::ui::button(p, settings_page::Rect::new(ok_x, 214.0, 120.0, bh), &ok_label, None, crate::ui::Kind::Primary, hover == Hit::Ok, count > 0);
                };
                let k2 = self.k;
                let surface = self.gfx.surface(w * k2, ADD_H * k2, |dc| unsafe {
                    dc.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
                    draw(&P { dc, dw: &dw, k: k2 });
                })?;
                set_surface(&self.gfx, &card, &surface, w * k2, ADD_H * k2)?;
                // slide the selection to the priority (set in place when the sheet opens)
                if let Some(Sheet { slider: Some((v, shown)), .. }) = &mut self.sheet {
                    let i = [Priority::Low, Priority::Normal, Priority::High].iter().position(|x| *x == priority).unwrap_or(1);
                    let animate = *shown != priority && v.Size().is_ok_and(|s| s.X > 0.0);
                    crate::ui::place_seg_slider(&self.gfx, v, prio.1[i], k2, animate)?;
                    *shown = priority;
                }
                let cx = (34.0 + caret_x - scroll_x).min(w - 34.0);
                caret_v.SetSize(Vector2 { X: (1.5 * k).max(1.0), Y: 18.0 * k })?;
                caret_v.SetOffset(Vector3 { X: cx * k, Y: 85.0 * k, Z: 0.0 })?;
                caret_v.SetIsVisible(!all)?;
            }
            SheetKind::Remove { ids, name, path, delete, permanent, cancel } => {
                let (permanent, cancel) = (*permanent, *cancel);
                // no switch to show: a permanent delete, or cancelling downloads
                let bare = permanent || cancel;
                let (w, h) = (420.0, if bare { REMOVE_H - 46.0 } else { REMOVE_H });
                let (name, path, delete) = (name.clone(), path.clone(), *delete || cancel);
                let many = ids.len() > 1;
                let n = ids.len();
                let (title, desc, switch, keep, gone) = if cancel {
                    (
                        format!("Cancel {n} downloads?"),
                        "What they've downloaded so far is deleted.",
                        "",
                        String::new(),
                        "Cancel Downloads".to_string(),
                    )
                } else if permanent {
                    (
                        if many { format!("Permanently delete {n} downloads?") } else { "Permanently delete this file?".to_string() },
                        if many { "Their files are deleted from your PC, not moved to the Recycle Bin." } else { "It's deleted from your PC, not moved to the Recycle Bin." },
                        "",
                        String::new(),
                        "Delete Permanently".to_string(),
                    )
                } else if many {
                    (
                        format!("Remove {n} downloads from the list?"),
                        "Some of their files are still on your PC.",
                        "Also move their files to the Recycle Bin",
                        format!("Remove {n} from List"),
                        "Remove and Delete Files".to_string(),
                    )
                } else {
                    (
                        "Remove from your downloads?".to_string(),
                        "The file is still on your PC.",
                        "Also move the file to the Recycle Bin",
                        "Remove from List".to_string(),
                        "Remove and Delete File".to_string(),
                    )
                };
                let icon = shell::type_icon(&name, false, (32.0 * k) as i32);
                let draw = |p: &P| {
                    p.popover(0.0, 0.0, w, h, 26.0);
                    p.text(&title, 22.0, 20.0, 16.5, SEMIBOLD, paint::text(), w - 44.0);
                    p.text(desc, 22.0, 46.0, 12.5, REGULAR, paint::dim(), w - 44.0);
                    p.round(22.0, 78.0, w - 44.0, 56.0, 18.0, paint::ink(0.05));
                    if let Some(px) = &icon {
                        if let Ok(b) = Gfx::bitmap(p.dc, px) {
                            let dest = D2D_RECT_F { left: 34.0 * k, top: 90.0 * k, right: 66.0 * k, bottom: 122.0 * k };
                            unsafe { p.dc.DrawBitmap(&b, Some(&dest), 1.0, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None) };
                        }
                    }
                    p.text(&name, 76.0, 87.0, 13.5, MEDIUM, paint::text(), w - 44.0 - 70.0);
                    p.text(&path, 76.0, 107.0, 12.0, REGULAR, paint::dim(), w - 44.0 - 70.0);
                    // the switch (its live parts sit on this track), then its label; a
                    // permanent delete has no choice to make
                    let sx = 24.0;
                    if !bare {
                        settings_page::paint_switch_track(p, settings_page::Rect::new(sx, 158.0, settings_page::SWITCH.0, settings_page::SWITCH.1), true);
                    }
                    p.text_mid(switch, sx + 52.0, 169.0, 13.0, REGULAR, paint::text(), w - 100.0);
                    let ok_w = 178.0;
                    let bh = crate::ui::BUTTON_H;
                    let (ok_x, by) = (w - 22.0 - ok_w, h - 22.0 - bh);
                    let cancel_x = ok_x - 10.0 - 92.0;
                    // Cancel is Secondary; the action Primary, or Danger once it deletes the file
                    // ("Keep" when the action itself is a cancel, so the two don't read alike)
                    let dismiss = if cancel { "Keep" } else { "Cancel" };
                    crate::ui::button(p, settings_page::Rect::new(cancel_x, by, 92.0, bh), dismiss, None, crate::ui::Kind::Secondary, hover == Hit::Cancel, true);
                    let (label, kind) = if delete { (gone.as_str(), crate::ui::Kind::Danger) } else { (keep.as_str(), crate::ui::Kind::Primary) };
                    crate::ui::button(p, settings_page::Rect::new(ok_x, by, ok_w, bh), label, None, kind, hover == Hit::Ok, true);
                };
                let surface = self.gfx.surface(w * k, h * k, |dc| unsafe {
                    dc.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
                    draw(&P { dc, dw: &dw, k });
                })?;
                set_surface(&self.gfx, &card, &surface, w * k, h * k)?;
                caret_v.SetIsVisible(false)?;
                if let Some((track, knob)) = &live_switch {
                    let _ = track.SetIsVisible(!bare);
                    let _ = knob.SetIsVisible(!bare);
                    let r = settings_page::Rect::new(24.0, 158.0, settings_page::SWITCH.0, settings_page::SWITCH.1);
                    // slides when flipped; set in place (no motion) the first time it's drawn
                    let placed = knob.Size().is_ok_and(|s| s.X > 0.0);
                    let flip = placed && track.Opacity().is_ok_and(|o| (o > 0.5) != delete);
                    settings_page::place_switch(&self.gfx, track, knob, r, delete, true, k, flip)?;
                }
            }
            SheetKind::Form(_) => {}
            SheetKind::Files(f) => {
                use crate::torrent_sheet::FHit;
                let fh = match hover {
                    Hit::Files(h) => Some(h),
                    Hit::Ok => Some(FHit::Ok),
                    Hit::Cancel => Some(FHit::Cancel),
                    _ => None,
                };
                let (w, h) = (crate::torrent_sheet::W, f.height());
                let surface = self.gfx.surface(w * k, h * k, |dc| unsafe {
                    dc.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
                    f.paint(&P { dc, dw: &dw, k }, fh);
                })?;
                set_surface(&self.gfx, &card, &surface, w * k, h * k)?;
                caret_v.SetIsVisible(false)?;
            }
        }
        Ok(())
    }

    fn sheet_key(&mut self, vk: u16, ctrl: bool) -> bool {
        if vk == VK_ESCAPE.0 {
            self.close_sheet();
            return true;
        }
        if vk == VK_RETURN.0 {
            // a permanent delete can't be undone: it takes a click, not a stray Enter
            if !matches!(self.sheet, Some(Sheet { kind: SheetKind::Remove { permanent: true, .. }, .. })) {
                self.submit_sheet();
            }
            return true;
        }
        if self.is_form() {
            return self.form_key(vk, ctrl);
        }
        let Some(Sheet { kind: SheetKind::Add { field, error, .. }, .. }) = &mut self.sheet else { return false };
        if !field.key(vk, ctrl) {
            return false;
        }
        *error = None;
        let _ = self.paint_sheet();
        true
    }

    fn submit_sheet(&mut self) {
        if self.is_form() {
            return self.submit_form(None);
        }
        let Some(s) = &mut self.sheet else { return };
        match &mut s.kind {
            SheetKind::Add { field, dir, priority, error } => {
                let text = field.string();
                let links: Vec<&str> = text.split_whitespace().collect();
                if links.is_empty() {
                    *error = Some("Paste a link to download.".into());
                    let _ = self.paint_sheet();
                    return;
                }
                if let Some(bad) = links.iter().find(|l| !dl::is_source(l)) {
                    *error = Some(format!("\u{201c}{}\u{201d} isn't a web address or a magnet link.", bad.chars().take(48).collect::<String>()));
                    let _ = self.paint_sheet();
                    return;
                }
                let links: Vec<String> = links.iter().map(|l| l.to_string()).collect();
                let template = AddRequest { dir: dir.clone(), priority: *priority, ..Default::default() };
                if self.view == Page::List(Filter::Completed) || self.view == Page::Settings {
                    self.set_view(Page::List(Filter::All));
                }
                self.scroll = 0.0;
                self.close_sheet();
                self.clamp_scroll(true);
                // patterns expanded, and a question first for what's already here
                self.add_links(links, template);
            }
            SheetKind::Remove { ids, delete, permanent, cancel, .. } => {
                let (ids, delete, permanent, cancel) = (ids.clone(), *delete, *permanent, *cancel);
                for id in &ids {
                    // an unfinished one is cancelled, its partial file with it
                    let unfinished = self.job(*id).is_some_and(|j| !j.state.finished());
                    if permanent && !unfinished {
                        dl::engine().remove_permanently(*id);
                    } else {
                        dl::engine().remove(*id, delete || unfinished);
                    }
                }
                self.picked.clear();
                self.close_sheet();
                let n = ids.len();
                let note = match (n, delete) {
                    _ if cancel => format!("Cancelled {n} downloads"),
                    _ if permanent && n == 1 => "Deleted permanently".to_string(),
                    _ if permanent => format!("{n} downloads deleted permanently"),
                    (1, true) => "Removed, and the file moved to the Recycle Bin".to_string(),
                    (1, false) => "Removed from your downloads".to_string(),
                    (_, true) => format!("Removed {n} downloads, and their files moved to the Recycle Bin"),
                    (_, false) => format!("Removed {n} downloads from the list"),
                };
                self.show_toast(&note);
            }
            SheetKind::Form(_) => {}
            SheetKind::Files(f) => {
                if !f.can_submit() {
                    return;
                }
                dl::engine().choose_files(f.id, f.chosen());
                self.close_sheet();
            }
        }
    }

    // ---------- the frame ----------

    fn nc_hit(&self, x: i32, y: i32) -> u32 {
        let r = shell::window_rect(self.hwnd);
        let (x, y) = (x - r.left, y - r.top);
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        let b = (6.0 * self.k) as i32;
        if unsafe { !IsZoomed(self.hwnd).as_bool() } {
            match (x < b, y < b, x >= w - b, y >= h - b) {
                (true, true, _, _) => return HTTOPLEFT,
                (_, true, true, _) => return HTTOPRIGHT,
                (true, _, _, true) => return HTBOTTOMLEFT,
                (_, _, true, true) => return HTBOTTOMRIGHT,
                (true, ..) => return HTLEFT,
                (_, true, ..) => return HTTOP,
                (_, _, true, _) => return HTRIGHT,
                (.., true) => return HTBOTTOM,
                _ => {}
            }
        }
        match self.hit(x as f32 / self.k, y as f32 / self.k) {
            Hit::Caption => HTCAPTION,
            Hit::CapMin => HTMINBUTTON,
            Hit::CapMax => HTMAXBUTTON, // also what shows Windows 11's snap layouts on hover
            Hit::CapClose => HTCLOSE,
            _ => HTCLIENT,
        }
    }
}

const DOCK_TOOLS: [(&str, &str); 3] = [(icons::PAUSE, "Pause all"), (icons::PLAY, "Resume all"), (icons::CLEAR, "Clear finished")];

/// How every scrolling area in the window meets its chrome: `src` (not in the tree)
/// fills the column from the window's top, and `view` shows it through an alpha mask that
/// thins it out under the header (the top `top` DIPs) and from `bottom` down (a dock's
/// top, or the window's foot), while the `blurs` show blurred copies there, the more
/// blurred the more transparent, so it all fades to nothing at the edges and nothing is
/// laid over the Mica. `w` and `h` are the column's size, in DIPs.
#[allow(clippy::too_many_arguments)]
fn scroll_mask(gfx: &Gfx, src: &ContainerVisual, view: &SpriteVisual, blurs: &[SpriteVisual], k: f32, w: f32, h: f32, top: f32, bottom: f32) -> Result<()> {
    let c = &gfx.compositor;
    let wpx = w * k;
    view.SetOffset(Vector3 { X: SIDEBAR * k, Y: 0.0, Z: 0.0 })?;
    view.SetSize(Vector2 { X: wpx, Y: h * k })?;
    let white = |a: f32| Color { A: (a.clamp(0.0, 1.0) * 255.0) as u8, R: 255, G: 255, B: 255 };
    // a vertical alpha ramp: (y in DIPs from the visual's top, alpha)
    let ramp = |stops: &[(f32, f32)], height: f32| -> Result<CompositionLinearGradientBrush> {
        let g = c.CreateLinearGradientBrush()?;
        g.SetStartPoint(Vector2 { X: 0.0, Y: 0.0 })?;
        g.SetEndPoint(Vector2 { X: 0.0, Y: 1.0 })?;
        let list = g.ColorStops()?;
        for (y, a) in stops {
            list.Append(&c.CreateColorGradientStopWithOffsetAndColor((y / height).clamp(0.0, 1.0), white(*a))?)?;
        }
        Ok(g)
    };
    // the content's region [from, to) (DIPs), as a brush
    let region = |from: f32, to: f32| -> Result<CompositionSurfaceBrush> {
        let vs = c.CreateVisualSurface()?;
        vs.SetSourceVisual(src)?;
        vs.SetSourceOffset(Vector2 { X: 0.0, Y: from * k })?;
        vs.SetSourceSize(Vector2 { X: wpx, Y: (to - from) * k })?;
        let b = c.CreateSurfaceBrushWithSurface(&vs)?;
        b.SetStretch(CompositionStretch::None)?;
        b.SetHorizontalAlignmentRatio(0.0)?;
        b.SetVerticalAlignmentRatio(0.0)?;
        Ok(b)
    };
    let masked = |v: &SpriteVisual, brush: &CompositionBrush, mask: &CompositionLinearGradientBrush| -> Result<()> {
        let m = c.CreateMaskBrush()?;
        m.SetSource(brush)?;
        m.SetMask(mask)?;
        v.SetBrush(&m)
    };
    let place = |v: &SpriteVisual, y: f32, bh: f32| -> Result<()> {
        v.SetOffset(Vector3 { X: SIDEBAR * k, Y: y * k, Z: 0.0 })?;
        v.SetSize(Vector2 { X: wpx, Y: bh * k })
    };
    // the sharp content: gone near the top, full from the header's foot to the bottom chrome
    let sharp = ramp(&[(0.0, 0.0), (top * 0.7, 0.0), (top * 0.9, 0.55), (top, 1.0), (bottom - 40.0, 1.0), (bottom - 6.0, 0.3), (bottom + 8.0, 0.0), (h, 0.0)], h)?;
    masked(view, &region(0.0, h)?.cast()?, &sharp)?;
    // under the header: a light blur in the middle, a heavier one at the top
    let hb = top + 12.0;
    let light = blur::brush(c, &region(0.0, hb)?.cast()?, 5.0 * k)?;
    masked(&blurs[0], &light.cast()?, &ramp(&[(0.0, 0.0), (top * 0.35, 0.1), (top * 0.7, 0.32), (top * 0.92, 0.2), (top, 0.0), (hb, 0.0)], hb)?)?;
    place(&blurs[0], 0.0, hb)?;
    let heavy = blur::brush(c, &region(0.0, hb)?.cast()?, 14.0 * k)?;
    masked(&blurs[1], &heavy.cast()?, &ramp(&[(0.0, 0.0), (top * 0.4, 0.14), (top * 0.65, 0.06), (top * 0.8, 0.0), (hb, 0.0)], hb)?)?;
    place(&blurs[1], 0.0, hb)?;
    // at the bottom: only the strip behind the chrome, thinning out toward the edge
    let from = (bottom - 24.0).min(h - 24.0);
    let foot = blur::brush(c, &region(from, h)?.cast()?, 9.0 * k)?;
    masked(&blurs[2], &foot.cast()?, &ramp(&[(0.0, 0.0), (20.0, 0.4), (38.0, 0.22), (h - from, 0.0)], h - from)?)?;
    place(&blurs[2], from, h - from)?;
    Ok(())
}

/// Every popover's shadow: soft, wide and faint, so it lifts the card without a dark
/// halo (a tooltip's is smaller, as the tooltip is).
fn popover_shadow(gfx: &Gfx, k: f32, small: bool) -> Result<windows::UI::Composition::DropShadow> {
    let (blur, y, opacity) = if small { (14.0, 3.0, 0.16) } else { (36.0, 10.0, 0.2) };
    let shadow = gfx.compositor.CreateDropShadow()?;
    shadow.SetBlurRadius(blur * k)?;
    shadow.SetOffset(Vector3 { X: 0.0, Y: y * k, Z: 0.0 })?;
    shadow.SetOpacity(if paint::light() { opacity * 0.7 } else { opacity })?;
    shadow.SetSourcePolicy(windows::UI::Composition::CompositionDropShadowSourcePolicy::InheritFromVisualContent)?;
    Ok(shadow)
}

/// Under everything: clear over Mica; one solid colour where there's no Mica (Windows 10).
fn backdrop_color(mica: bool) -> Color {
    match (mica, paint::light()) {
        (true, _) => Color { A: 0, R: 0, G: 0, B: 0 },
        (false, true) => Color { A: 255, R: 243, G: 242, B: 245 },
        (false, false) => Color { A: 255, R: 30, G: 28, B: 33 },
    }
}

fn set_surface(gfx: &Gfx, v: &SpriteVisual, surface: &windows::UI::Composition::CompositionDrawingSurface, w: f32, h: f32) -> Result<()> {
    let brush = gfx.compositor.CreateSurfaceBrushWithSurface(surface)?;
    brush.SetStretch(CompositionStretch::Fill)?;
    v.SetBrush(&brush)?;
    v.SetSize(Vector2 { X: w.ceil(), Y: h.ceil() })
}

fn blink(gfx: &Gfx) -> Result<windows::UI::Composition::ScalarKeyFrameAnimation> {
    let b = gfx.compositor.CreateScalarKeyFrameAnimation()?;
    b.InsertKeyFrame(0.0, 1.0)?;
    b.InsertKeyFrame(0.5, 1.0)?;
    b.InsertKeyFrame(0.501, 0.0)?;
    b.InsertKeyFrame(1.0, 0.0)?;
    b.SetDuration(ms(1060))?;
    b.SetIterationBehavior(AnimationIterationBehavior::Forever)?;
    Ok(b)
}

fn has_toggle(j: &JobView) -> bool {
    !j.state.finished()
}

/// One download row, Tiered: the name with the live speed and percent at its right
/// end; the bar (its fill is separate, see update_bar); under it, the size on the left
/// and quiet details on the right. Finished rows show the size and where it came from.
#[allow(clippy::too_many_arguments)]
fn paint_row(p: &P, j: &JobView, rw: f32, selected: bool, hovered: bool, hover_btn: Option<RowBtn>, missing: bool, icon: Option<&Pixels>, marks: &[usize], site: &str) {
    if selected {
        p.round(0.0, 2.0, rw, ROW - 4.0, 16.0, paint::ink(0.075));
    } else if hovered {
        p.round(0.0, 2.0, rw, ROW - 4.0, 16.0, paint::ink(0.04));
    }
    let k = p.k;
    let finished = j.state.finished();
    // (the icon is its own visual, with a soft shadow: see update_row_icon)
    if let Some(px) = icon {
        if let Ok(b) = Gfx::bitmap(p.dc, px) {
            let s = 28.0;
            let dest = D2D_RECT_F { left: (34.0 - s / 2.0) * k, top: (ROW - s) / 2.0 * k, right: (34.0 + s / 2.0) * k, bottom: ((ROW + s) / 2.0) * k };
            let alpha = if missing { 0.45 } else if finished || j.state == State::Active { 1.0 } else { 0.6 };
            unsafe { p.dc.DrawBitmap(&b, Some(&dest), alpha, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None) };
        }
    }
    let right = rw - 96.0; // the buttons take the rest
    let name_y = if finished { 13.0 } else { 10.0 };
    // speed and percent at the end of the name line
    let mut head_w = 0.0;
    if !finished {
        let pct = j.fraction().map(|f| format!("{:.0}%", f * 100.0));
        if let Some(pct) = &pct {
            head_w += p.text_right(pct, right, name_y + 9.0, 12.5, SEMIBOLD, if j.state == State::Active { paint::text() } else { paint::dim() });
        }
        if j.state == State::Active && j.phase.is_none() {
            head_w += 10.0 + p.text_right(&paint::speed(j.speed), right - head_w - 10.0, name_y + 9.0, 12.5, MEDIUM, paint::accent_text());
        }
    }
    let name_ink = if missing { paint::dim() } else { paint::text() };
    let nw = p.text_marked(&j.name, 68.0, name_y, 14.0, MEDIUM, name_ink, (right - 68.0 - head_w - 16.0).max(60.0), marks);
    if j.priority != Priority::Normal && !finished {
        let (label, c, bg) = if j.priority == Priority::High {
            ("HIGH", paint::rgb(paint::GOLD, 1.0), paint::rgb(paint::GOLD, 0.14))
        } else {
            ("LOW", paint::faint(), paint::ink(0.07))
        };
        let lw = p.measure(label, 9.0, SEMIBOLD) + 10.0;
        p.round(68.0 + nw + 8.0, name_y + 2.0, lw, 15.0, 5.0, bg);
        p.text_mid(label, 68.0 + nw + 13.0, name_y + 9.5, 9.0, SEMIBOLD, c, lw);
    }
    let meta_y = if finished { 35.0 } else { 44.0 };
    if finished {
        let size = j.total.map(paint::bytes).unwrap_or_default();
        let from = if site.is_empty() { shell::folder_name(&j.dir) } else { site.to_string() };
        if missing {
            p.glyph(icons::WARNING, 75.0, meta_y + 8.0, 9.5, paint::warn_text());
            let w1 = p.text("Moved or deleted", 86.0, meta_y, 12.5, MEDIUM, paint::warn_text(), 160.0);
            p.text(&format!("  ·  {size}  ·  {from}"), 86.0 + w1, meta_y, 12.5, REGULAR, paint::dim(), right - 86.0 - w1);
        } else if j.verified == Some(false) {
            // the file isn't the one its checksum says
            p.glyph(icons::WARNING, 75.0, meta_y + 8.0, 9.5, paint::red());
            let w1 = p.text("Checksum doesn't match", 86.0, meta_y, 12.5, MEDIUM, paint::red(), 200.0);
            p.text(&format!("  ·  {size}  ·  {from}"), 86.0 + w1, meta_y, 12.5, REGULAR, paint::dim(), right - 86.0 - w1);
        } else if j.verified == Some(true) {
            p.glyph(icons::CHECK, 75.0, meta_y + 8.0, 9.5, paint::green());
            let w1 = p.text("Verified", 86.0, meta_y, 12.5, MEDIUM, paint::green(), 120.0);
            p.text(&format!("  ·  {size}  ·  {from}"), 86.0 + w1, meta_y, 12.5, REGULAR, paint::dim(), right - 86.0 - w1);
        } else {
            p.text(&format!("{size}  ·  {from}"), 68.0, meta_y, 12.5, REGULAR, paint::dim(), right - 68.0);
        }
    } else {
        let size = match j.total {
            Some(t) => format!("{} of {}", paint::bytes(j.done), paint::bytes(t)),
            None => paint::bytes(j.done),
        };
        let left = match j.state {
            // why it waits (no network, download hours, a retry, a sign-in), if it says
            State::Queued | State::Paused if j.phase.is_some() => format!("{}  ·  {size}", j.phase.unwrap_or_default()),
            State::Queued => format!("Waiting  ·  {size}"),
            State::Paused => format!("Paused  ·  {size}"),
            State::Failed => format!("Failed  ·  {}", j.error.clone().unwrap_or_default()),
            _ => match j.phase {
                Some(ph) => ph.to_string(),
                None => size,
            },
        };
        let lc = if j.state == State::Failed { paint::red() } else { paint::dim() };
        // details on the right: time left, then peers or connections, each with an icon
        let mut x = right;
        if j.state == State::Active {
            let who = j.peers.map(|p| (icons::PEOPLE, p)).unwrap_or((icons::CONNECTIONS, j.connections));
            if who.1 > 0 {
                let w = p.text_right(&who.1.to_string(), x, meta_y + 8.0, 12.0, REGULAR, paint::dim());
                p.glyph(who.0, x - w - 9.0, meta_y + 8.0, 9.5, paint::dim());
                x -= w + 24.0;
            }
            if let Some(eta) = j.eta_secs() {
                let w = p.text_right(&paint::eta_short(eta), x, meta_y + 8.0, 12.0, REGULAR, paint::dim());
                p.glyph(icons::CLOCK, x - w - 9.0, meta_y + 8.0, 9.5, paint::dim());
                x -= w + 24.0;
            }
        }
        p.text(&left, 68.0, meta_y, 12.5, REGULAR, lc, x - 68.0 - 8.0);
        // the bar's track is its own visual (the gap between done and to-do moves)
    }

    // buttons on the right
    let cy = ROW / 2.0;
    let button = |cx: f32, g: &str, on: bool, show: bool| {
        if !show {
            return;
        }
        crate::ui::button(p, settings_page::Rect::new(cx - 15.0, cy - 15.0, 30.0, 30.0), "", Some(g), crate::ui::Kind::Secondary, on, true);
    };
    if finished && missing {
        if j.expired {
            p.text_right("Link expired", rw - 14.0, cy, 12.5, MEDIUM, paint::faint());
        } else {
            let hot = hover_btn == Some(RowBtn::Again);
            let label = "Download again";
            let lw = p.measure(label, 12.5, SEMIBOLD);
            let bw = lw + 40.0;
            if hot {
                p.round(rw - 10.0 - bw, cy - 16.0, bw, 32.0, 12.0, paint::rgb(paint::ORANGE, 0.14));
            }
            p.glyph(icons::RETRY, rw - 10.0 - bw + 18.0, cy, 10.5, paint::accent_text());
            p.text_mid(label, rw - 10.0 - bw + 30.0, cy, 12.5, SEMIBOLD, paint::accent_text(), lw + 2.0);
        }
        return;
    }
    let show_secondary = hovered || selected;
    if finished {
        button(rw - 26.0, icons::FOLDER, hover_btn == Some(RowBtn::Reveal), show_secondary);
        if !j.expired {
            button(rw - 62.0, icons::RETRY, hover_btn == Some(RowBtn::Again), show_secondary);
        }
    } else {
        button(rw - 26.0, icons::CANCEL, hover_btn == Some(RowBtn::Cancel), true);
        if has_toggle(j) {
            let g = match j.state {
                State::Active | State::Queued | State::Seeding => icons::PAUSE,
                State::Failed => icons::RETRY,
                _ => icons::PLAY,
            };
            button(rw - 62.0, g, hover_btn == Some(RowBtn::Toggle), true);
        }
    }
    let _: D2D1_COLOR_F = paint::text();
}

fn track_leave(hwnd: HWND) {
    let mut t = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
    unsafe {
        let _ = TrackMouseEvent(&mut t);
    }
}

fn run_blocking(_hwnd: HWND, b: Blocking) {
    match b {
        Blocking::PickFolder => {
            if let Some(dir) = shell::pick_folder() {
                with(|m| {
                    if let Some(Sheet { kind: SheetKind::Add { dir: d, .. }, .. }) = &mut m.sheet {
                        *d = dir;
                    }
                    let _ = m.paint_sheet();
                });
            }
        }
        Blocking::ImportList => {
            if let Some(file) = forms::pick_list() {
                with(|m| m.import_list(&file));
            }
        }
        Blocking::PickTorrent => {
            if let Some(file) = shell::pick_torrent() {
                with(|m| {
                    if let Some(Sheet { kind: SheetKind::Add { dir, priority, .. }, .. }) = &m.sheet {
                        dl::engine().add(AddRequest { source: file.display().to_string(), dir: dir.clone(), priority: *priority, ..Default::default() });
                    }
                    m.close_sheet();
                });
            }
        }
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let x = (lparam.0 & 0xffff) as i16 as i32;
    let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
    let mut blocking = None;
    match msg {
        WM_NCCALCSIZE if wparam.0 != 0 => {
            unsafe {
                if IsZoomed(hwnd).as_bool() {
                    let p = &mut *(lparam.0 as *mut NCCALCSIZE_PARAMS);
                    let dpi = windows::Win32::UI::HiDpi::GetDpiForWindow(hwnd);
                    let f = windows::Win32::UI::HiDpi::GetSystemMetricsForDpi(SM_CXFRAME, dpi) + windows::Win32::UI::HiDpi::GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi);
                    p.rgrc[0].left += f;
                    p.rgrc[0].top += f;
                    p.rgrc[0].right -= f;
                    p.rgrc[0].bottom -= f;
                }
            }
            return LRESULT(0);
        }
        WM_NCHITTEST => {
            if let Some(h) = with(|m| m.nc_hit(x, y)) {
                return LRESULT(h as isize);
            }
        }
        WM_ACTIVATE => unsafe {
            let _ = DwmExtendFrameIntoClientArea(hwnd, &MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 });
            if (wparam.0 & 0xffff) == 0 {
                with(|m| {
                    m.close_menu();
                    m.hide_tip();
                });
            }
            if (wparam.0 & 0xffff) != 0 {
                // files may have been moved or deleted while we were away
                with(|m| {
                    m.missing.clear();
                    m.file_icons.retain(|p, _| p.exists());
                    m.on_snapshot(true);
                });
            }
        },
        WM_SIZE => {
            with(|m| {
                m.layout();
                m.on_snapshot(false);
            });
            return LRESULT(0);
        }
        WM_DPICHANGED => unsafe {
            let r = &*(lparam.0 as *const RECT);
            let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
            return LRESULT(0);
        },
        WM_GETMINMAXINFO => unsafe {
            let k = shell::scale_factor(hwnd);
            let info = &mut *(lparam.0 as *mut MINMAXINFO);
            info.ptMinTrackSize = POINT { x: (MIN_W as f32 * k) as i32, y: (MIN_H as f32 * k) as i32 };
            return LRESULT(0);
        },
        WM_MOUSEMOVE => {
            with(|m| {
                if !m.tracking {
                    m.tracking = true;
                    track_leave(hwnd);
                }
                let h = m.hit(x as f32 / m.k, y as f32 / m.k);
                let cursor = match &h {
                    Hit::Search | Hit::Field | Hit::Form(forms::FHit::Field(_)) => IDC_IBEAM,
                    Hit::None | Hit::Caption | Hit::Row(_) | Hit::Sheet | Hit::Backdrop | Hit::Dock | Hit::Menu(_) | Hit::MenuOff => IDC_ARROW,
                    Hit::Page(settings_page::Hit::None) => IDC_ARROW,
                    _ => IDC_HAND,
                };
                m.set_hover(h);
                unsafe {
                    if let Ok(c) = LoadCursorW(None, cursor) {
                        SetCursor(Some(c));
                    }
                }
            });
            return LRESULT(0);
        }
        WM_SETCURSOR if (lparam.0 & 0xffff) as u32 == HTCLIENT => return LRESULT(1),
        // the caption buttons live in the non-client area (so snap layouts work)
        WM_NCMOUSEMOVE if matches!(wparam.0 as u32, HTMINBUTTON | HTMAXBUTTON | HTCLOSE) => {
            with(|m| {
                let mut t = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE | TME_NONCLIENT, hwndTrack: hwnd, dwHoverTime: 0 };
                unsafe {
                    let _ = TrackMouseEvent(&mut t);
                }
                let r = shell::window_rect(hwnd);
                let mut pt = POINT { x, y };
                unsafe {
                    let _ = windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut pt);
                }
                let _ = r;
                let h = m.hit(pt.x as f32 / m.k, pt.y as f32 / m.k);
                m.set_hover(h);
            });
            return LRESULT(0);
        }
        WM_NCMOUSEMOVE => {
            with(|m| {
                if matches!(m.hover, Hit::CapMin | Hit::CapMax | Hit::CapClose) {
                    m.set_hover(Hit::None);
                }
            });
        }
        WM_NCMOUSELEAVE => {
            with(|m| {
                m.cap_press = None;
                if matches!(m.hover, Hit::CapMin | Hit::CapMax | Hit::CapClose) {
                    m.set_hover(Hit::None);
                }
            });
            return LRESULT(0);
        }
        WM_NCLBUTTONDOWN | WM_NCLBUTTONDBLCLK if matches!(wparam.0 as u32, HTMINBUTTON | HTMAXBUTTON | HTCLOSE) => {
            with(|m| {
                m.cap_press = Some(wparam.0 as u32);
                let _ = m.paint_caption();
            });
            return LRESULT(0);
        }
        WM_NCLBUTTONUP if matches!(wparam.0 as u32, HTMINBUTTON | HTMAXBUTTON | HTCLOSE) => {
            let pressed = with(|m| {
                let p = m.cap_press.take();
                let _ = m.paint_caption();
                p
            })
            .flatten();
            if pressed == Some(wparam.0 as u32) {
                let cmd = match wparam.0 as u32 {
                    HTMINBUTTON => SC_MINIMIZE,
                    HTMAXBUTTON => {
                        if unsafe { IsZoomed(hwnd).as_bool() } {
                            SC_RESTORE
                        } else {
                            SC_MAXIMIZE
                        }
                    }
                    _ => SC_CLOSE,
                };
                unsafe {
                    let _ = PostMessageW(Some(hwnd), WM_SYSCOMMAND, WPARAM(cmd as usize), LPARAM(0));
                }
            }
            return LRESULT(0);
        }
        WM_ACTIVATEAPP | WM_NCACTIVATE => {
            with(|m| {
                let _ = m.paint_caption();
            });
        }
        WM_MOUSELEAVE => {
            with(|m| {
                m.tracking = false;
                m.set_hover(Hit::None);
                m.hide_tip();
            });
            return LRESULT(0);
        }
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
            blocking = with(|m| {
                let h = m.hit(x as f32 / m.k, y as f32 / m.k);
                m.click(h, msg == WM_LBUTTONDBLCLK)
            })
            .flatten();
        }
        WM_LBUTTONUP => {
            with(|m| m.page.press(None));
        }
        WM_RBUTTONUP => {
            with(|m| {
                m.close_menu();
                let (fx, fy) = (x as f32 / m.k, y as f32 / m.k);
                if let Hit::Row(id) | Hit::RowBtn(id, _) = m.hit(fx, fy) {
                    // a row outside the selection becomes the selection
                    if !m.picked.contains(&id) {
                        m.select_only(id);
                        m.on_selection();
                    }
                    m.selected = Some(id);
                    m.open_menu(fx, fy);
                }
            });
            return LRESULT(0);
        }
        WM_MOUSEWHEEL => {
            let delta = (wparam.0 >> 16) as u16 as i16 as f32;
            with(|m| {
                if let Some(Sheet { kind: SheetKind::Files(f), .. }) = &mut m.sheet {
                    if f.scroll_by(if delta > 0.0 { -3 } else { 3 }) {
                        let _ = m.paint_sheet();
                    }
                    return;
                }
                if m.sheet.is_some() {
                    return;
                }
                m.close_menu();
                m.hide_tip();
                if m.view == Page::Settings {
                    m.page.scroll_by(-delta / 120.0 * 90.0);
                } else {
                    m.scroll -= delta / 120.0 * ROW * 1.5;
                    m.clamp_scroll(true);
                }
            });
            return LRESULT(0);
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            if with(|m| m.on_key(wparam.0 as u16)) == Some(true) {
                return LRESULT(0);
            }
        }
        WM_CHAR => {
            let ch = wparam.0 as u32;
            if ch >= 0x20 && ch != 0x7f {
                if let Some(c) = char::from_u32(ch) {
                    with(|m| m.on_char(c));
                }
            }
            return LRESULT(0);
        }
        WM_SETTINGCHANGE => {
            let what = if lparam.0 != 0 { unsafe { PCWSTR(lparam.0 as *const u16).to_string().unwrap_or_default() } } else { String::new() };
            if what == "ImmersiveColorSet" {
                with(|m| m.apply_theme());
            }
        }
        WM_APP_THUMBS => {
            with(|m| m.on_thumbs());
            return LRESULT(0);
        }
        WM_TIMER if wparam.0 == T_VIEW => {
            unsafe {
                let _ = KillTimer(Some(hwnd), T_VIEW);
            }
            with(|m| m.view_later());
            return LRESULT(0);
        }
        WM_TIMER if wparam.0 == forms::T_FORM => {
            with(|m| m.form_tick());
            return LRESULT(0);
        }
        WM_TIMER if wparam.0 == T_TIP => {
            unsafe {
                let _ = KillTimer(Some(hwnd), T_TIP);
            }
            with(|m| m.show_tip());
            return LRESULT(0);
        }
        WM_TIMER if wparam.0 == T_TOAST || wparam.0 == T_DOCK => unsafe {
            let _ = KillTimer(Some(hwnd), wparam.0);
            return LRESULT(0);
        },
        WM_CLOSE if with(|m| m.settings.close_quits) == Some(true) => {
            // Settings > General: closing quits (the window's size is kept first)
            with(|m| {
                let r = shell::window_rect(hwnd);
                let k = m.k;
                if unsafe { !IsZoomed(hwnd).as_bool() } {
                    crate::stash::queue(format!("dls:window:{}x{}", ((r.right - r.left) as f32 / k) as i32, ((r.bottom - r.top) as f32 / k) as i32));
                }
            });
            crate::stash::queue(crate::menu::ids::QUIT.into());
            return LRESULT(0);
        }
        WM_CLOSE => {
            // closing hides it; Stash keeps running in the tray
            with(|m| {
                m.close_sheet();
                m.focus_search(false);
                let r = shell::window_rect(hwnd);
                let k = m.k;
                if unsafe { !IsZoomed(hwnd).as_bool() } {
                    crate::stash::queue(format!("dls:window:{}x{}", ((r.right - r.left) as f32 / k) as i32, ((r.bottom - r.top) as f32 / k) as i32));
                }
            });
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            return LRESULT(0);
        }
        _ => {}
    }
    if let Some(b) = blocking {
        run_blocking(hwnd, b);
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}
