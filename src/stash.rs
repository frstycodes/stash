//! The stack tile and its fan, built on the Windows compositor.
//!
//! Animation runs inside the compositor (DWM): a single `Progress` value is
//! animated with an easing curve, and every fan item's position on the arc, tilt,
//! scale and opacity are expressions of it. Our thread is idle while it plays.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::mpsc::Sender;
use std::time::SystemTime;

use windows::Foundation::TimeSpan;
use windows::UI::Color;
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{
    CompositionDrawingSurface, CompositionDropShadowSourcePolicy, CompositionPropertySet,
    CompositionStretch, CompositionSurfaceBrush, ContainerVisual, CubicBezierEasingFunction,
    SpriteVisual, Visual,
};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{CombineRgn, CreatePolygonRgn, CreateRectRgn, DeleteObject, HGDIOBJ, RGN_OR, SetWindowRgn, WINDING};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Ole::{IDropTarget, RegisterDragDrop};
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, Interface, PCWSTR, Result, w};
use windows_numerics::{Vector2, Vector3};

use crate::droptarget::{self, DropTarget};
use crate::files::{self, Item};
use crate::gfx::{Gfx, Pixels};
use crate::menu::{self, ids};
use crate::settings::{self, Corner, Display, Settings};
use crate::shell;
use crate::thumbs::{self, Job};

// ---------- geometry (logical pixels; multiplied by the DPI scale `k`) ----------

const TILE: f32 = 88.0;
const STACK: f32 = 64.0;
const ICON: f32 = 52.0;
const OPEN_ICON: f32 = 40.0;
const STEP: f32 = 58.0;
const FIRST: f32 = 74.0;
const RADIUS: f32 = 1000.0;
const MAX_TILT: f32 = 0.33; // radians (~19°): long fans curve more gently instead of tilting further
const FAN_W: f32 = 480.0;
const FAN_MAX_H: f32 = 860.0;
const MARGIN: f32 = 8.0;
const MAX_ITEMS: usize = 12;
const HIT_PAD: f32 = 10.0;

// ---------- timing (ms) ----------

// Springs (run by the compositor). Opening is underdamped for a slight overshoot;
// closing is critically damped so items never swing past the stack.
const OPEN_DAMPING: f32 = 0.68;
const OPEN_PERIOD_MS: i64 = 64;
const CLOSE_DAMPING: f32 = 1.0;
const CLOSE_PERIOD_MS: i64 = 48;
const CLOSE_DELAY_MS: i64 = 30;
const CLOSE_SETTLE_MS: i64 = 340; // when the closed fan can be hidden
const LABEL_IN_DELAY_MS: i64 = 150;
const LABEL_IN_MS: i64 = 130;
const LABEL_OUT_MS: i64 = 65;
const HOVER_OPEN_MS: u32 = 90;
const POLL_MS: u32 = 40;
const CLOSE_AFTER_OUTSIDE_MS: u32 = 240;

// ---------- pile (front to back): scale, offset, tilt, blur, brightness, saturation ----------

struct Layer {
    scale: f32,
    dx: f32,
    dy: f32,
    rotate: f32,
    blur: f32,
    brightness: f32,
    saturation: f32,
}

// Every layer is the same size, so the pile reads as a stack: the back two peek out
// on either side through their offset and tilt, not by being smaller.
const PILE: [Layer; 3] = [
    Layer { scale: 0.8, dx: 0.0, dy: 2.0, rotate: 0.0, blur: 0.0, brightness: 1.0, saturation: 1.0 },
    Layer { scale: 0.8, dx: -5.0, dy: -4.0, rotate: -10.0, blur: 0.4, brightness: 0.86, saturation: 0.92 },
    Layer { scale: 0.8, dx: 5.0, dy: -8.0, rotate: 9.0, blur: 0.8, brightness: 0.74, saturation: 0.85 },
];

// ---------- messages & timers ----------

#[allow(non_upper_case_globals)]
const WM_MOUSELEAVE: u32 = 0x02A3;
const WM_APP_THUMBS: u32 = WM_APP + 1;
const WM_APP_MENU: u32 = WM_APP + 2;
const WM_APP_FOLDER: u32 = WM_APP + 3;
const WM_APP_OUTSIDE: u32 = WM_APP + 4; // a mouse button went down somewhere; lparam = screen point
const TRAY_CLICK: &str = "tray-click";
const T_HOVER: usize = 1;
const T_POLL: usize = 2;
const T_HIDE_FAN: usize = 3;
const T_RELOAD: usize = 4;
const T_SURFACE: usize = 5; // raise a faded-out tile above the windows covering it
const T_UNFADE: usize = 6;  // restore opacity once the tile is back beneath the windows
const T_BADGE: usize = 7;   // start fading the folder badge out
const T_BADGE_HIDE: usize = 8;
const T_BOUNCE: usize = 9;  // a file arrived in another watched folder (debounced)
const T_OUTGOING: usize = 10; // drop the old pile once it has slid out

// Switching folders with the wheel
const WHEEL_STEP: i32 = 120;     // one notch; touchpads send smaller deltas that add up
const SWITCH_COOLDOWN_MS: u128 = 260; // one flick of a touchpad = one folder
const SLIDE: f32 = 26.0;         // how far the piles slide when switching
const SLIDE_MS: i64 = 240;
const FAN_FADE_MS: i64 = 160;     // the previous folder's fan fading out
const BADGE_MS: u32 = 1100;      // how long the folder badge stays up

// Fading the stack in and out when the tray brings it over (and returns it under) windows.
const SURFACE_DELAY_MS: u32 = 20; // lets the transparent frame land before the raise
const FADE_IN_MS: i64 = 180;
const FADE_OUT_MS: i64 = 220;
const FADE_OUT_DELAY_MS: i64 = 110;

static TILE_HWND: AtomicIsize = AtomicIsize::new(0);
static BADGE_HWND: AtomicIsize = AtomicIsize::new(0);
/// File-system changes from the watcher thread: (path, whether something arrived there).
static FOLDER_EVENTS: Mutex<Vec<(PathBuf, bool)>> = Mutex::new(Vec::new());
static MOUSE_HOOK: AtomicIsize = AtomicIsize::new(0);
static THUMB_QUEUE: Mutex<Vec<thumbs::Done>> = Mutex::new(Vec::new());
static MENU_QUEUE: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// STASH_LOG=1 appends diagnostics to %TEMP%\stash.log.
pub fn log(line: impl AsRef<str>) {
    if std::env::var_os("STASH_LOG").is_none() {
        return;
    }
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(std::env::temp_dir().join("stash.log")) {
        let _ = writeln!(f, "{:?} {}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis(), line.as_ref());
    }
}

fn post(msg: u32) {
    post_with(msg, LPARAM(0));
}

fn post_with(msg: u32, lparam: LPARAM) {
    let hwnd = HWND(TILE_HWND.load(Ordering::Relaxed) as *mut _);
    unsafe {
        let _ = PostMessageW(Some(hwnd), msg, WPARAM(0), lparam);
    }
}

/// Animation durations; STASH_SLOW=<factor> stretches them (for inspecting motion).
fn ms(v: i64) -> TimeSpan {
    let slow = std::env::var("STASH_SLOW").ok().and_then(|s| s.parse::<f64>().ok()).unwrap_or(1.0);
    TimeSpan { Duration: (v as f64 * slow * 10_000.0) as i64 }
}

fn arc(s: f32, radius: f32, right: bool) -> (f32, f32) {
    let t = s / radius;
    let dir = if right { -1.0 } else { 1.0 };
    (dir * radius * (1.0 - t.cos()), -radius * t.sin())
}

struct Row {
    label: SpriteVisual,
    label_normal: CompositionSurfaceBrush,
    label_hover: CompositionSurfaceBrush,
    center: (f32, f32),          // icon center when fanned out, fan-window pixels
    angle: f32,                  // tilt when fanned out, radians
    extent: (f32, f32, f32, f32), // icon + label box around the center, before tilt
    path: Option<PathBuf>,       // None = "Open in Explorer"
}

impl Row {
    /// Undo the row's tilt, then test against its untilted box.
    fn contains(&self, x: f32, y: f32, pad: f32) -> bool {
        let (dx, dy) = (x - self.center.0, y - self.center.1);
        let (sin, cos) = self.angle.sin_cos();
        let (lx, ly) = (dx * cos + dy * sin, -dx * sin + dy * cos);
        let (l, t, r, b) = self.extent;
        lx >= l - pad && lx <= r + pad && ly >= t - pad && ly <= b + pad
    }

    /// The tilted box's corners, for the window region.
    fn corners(&self, pad: f32) -> [POINT; 4] {
        let (l, t, r, b) = self.extent;
        let (sin, cos) = self.angle.sin_cos();
        [(l - pad, t - pad), (r + pad, t - pad), (r + pad, b + pad), (l - pad, b + pad)].map(|(x, y)| POINT {
            x: (self.center.0 + x * cos - y * sin).round() as i32,
            y: (self.center.1 + x * sin + y * cos).round() as i32,
        })
    }
}

/// Work that runs a nested message loop; done after the state borrow is released.
enum Blocking {
    Drag(PathBuf),
    Menu { hwnd: HWND, item: bool },
    PickFolder,
}

pub struct Stash {
    gfx: Gfx,
    tile: HWND,
    fan: HWND,
    badge: HWND, // the folder name + dots shown while switching
    _targets: (DesktopWindowTarget, DesktopWindowTarget, DesktopWindowTarget),
    _drop_target: IDropTarget,
    tile_root: ContainerVisual,
    stack: ContainerVisual,
    glow: SpriteVisual,
    fan_root: ContainerVisual,
    fan_layer: ContainerVisual, // the rows on show; a folder switch swaps in a new one
    fan_outgoing: Vec<(ContainerVisual, Vec<[POINT; 4]>, std::time::Instant)>, // earlier folders' fans fading out, with their region
    fan_shown: bool,            // the fan window is up (open, or still collapsing)
    badge_root: ContainerVisual,
    outgoing: Vec<(ContainerVisual, std::time::Instant)>, // earlier folders' piles, sliding out
    props: CompositionPropertySet,
    ease_out: CubicBezierEasingFunction,
    ease_in: CubicBezierEasingFunction,
    rows: Vec<Row>,

    k: f32,  // DPI scale
    ik: f32, // DPI scale times the icon size setting: everything icon-shaped
    fan_size: (f32, f32),
    work_area: RECT,

    dir: PathBuf,       // the folder on show
    downloads: PathBuf, // always the first folder
    settings: Settings,
    items: Vec<Item>,
    thumbs: HashMap<PathBuf, (SystemTime, Option<Pixels>)>,
    pending: HashSet<PathBuf>,
    folder_icons: HashMap<PathBuf, Option<Pixels>>,
    jobs: Sender<Job>,

    open: bool,
    tile_hover: bool,
    outside_ms: u32,
    dragging: bool,
    menu_open: bool,
    menu_target: Option<PathBuf>,
    press: Option<(usize, POINT)>,
    hover_row: Option<usize>,
    tracking_fan_leave: bool,
    ready: bool,
    pinned: bool,    // opened from the tray: stays open until clicked away
    topmost: bool,   // raised above all apps (while a tray-opened fan is showing)
    pinned_fg: HWND, // the foreground window when pinned; switching apps closes the fan
    wheel: i32,                     // wheel delta toward the next switch
    last_switch: std::time::Instant,

    tray: Option<tray_icon::TrayIcon>,
    watcher: Option<notify::RecommendedWatcher>,
}

thread_local! {
    static APP: RefCell<Option<Stash>> = const { RefCell::new(None) };
}

/// Run `f` against the app state, unless it's already borrowed (re-entrancy from a
/// nested message loop), in which case the event is dropped.
fn with<R>(f: impl FnOnce(&mut Stash) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok()?.as_mut().map(f))
}

fn create_window(class: PCWSTR, owner: Option<HWND>) -> Result<HWND> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class,
            w!("Stash"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            owner,
            None,
            Some(instance.into()),
            None,
        )
    }
}

impl Stash {
    pub fn start() -> Result<()> {
        let gfx = Gfx::new()?;
        // Owned by the desktop: app windows always cover it, Win+D leaves it visible.
        let desktop = unsafe { FindWindowW(w!("Progman"), PCWSTR::null()).ok() };
        let tile = create_window(w!("StashTile"), desktop)?;
        let fan = create_window(w!("StashFan"), desktop)?;
        let badge = create_window(w!("StashBadge"), desktop)?;
        TILE_HWND.store(tile.0 as isize, Ordering::Relaxed);
        BADGE_HWND.store(badge.0 as isize, Ordering::Relaxed);

        let c = &gfx.compositor;
        let tile_target = gfx.target(tile)?;
        let fan_target = gfx.target(fan)?;
        let badge_target = gfx.target(badge)?;
        let tile_root = c.CreateContainerVisual()?;
        let fan_root = c.CreateContainerVisual()?;
        let fan_layer = c.CreateContainerVisual()?;
        fan_root.Children()?.InsertAtTop(&fan_layer)?;
        let badge_root = c.CreateContainerVisual()?;
        tile_target.SetRoot(&tile_root)?;
        fan_target.SetRoot(&fan_root)?;
        badge_target.SetRoot(&badge_root)?;

        let glow = c.CreateSpriteVisual()?;
        let stack = c.CreateContainerVisual()?;
        tile_root.Children()?.InsertAtTop(&glow)?;
        tile_root.Children()?.InsertAtTop(&stack)?;

        let props = c.CreatePropertySet()?;
        props.InsertScalar(&HSTRING::from("Progress"), 0.0)?;
        props.InsertScalar(&HSTRING::from("Label"), 0.0)?;
        let ease_out = c.CreateCubicBezierEasingFunction(Vector2 { X: 0.22, Y: 1.0 }, Vector2 { X: 0.36, Y: 1.0 })?;
        let ease_in = c.CreateCubicBezierEasingFunction(Vector2 { X: 0.55, Y: 0.0 }, Vector2 { X: 0.75, Y: 0.25 })?;

        let drop_target = DropTarget::new(tile, |e| {
            with(|s| s.on_drop_event(e));
        });
        unsafe {
            RegisterDragDrop(tile, &drop_target)?;
        }

        let jobs = thumbs::spawn_worker(|done| {
            THUMB_QUEUE.lock().unwrap().push(done);
            post(WM_APP_THUMBS);
        });

        tray_icon::menu::MenuEvent::set_event_handler(Some(|e: tray_icon::menu::MenuEvent| {
            MENU_QUEUE.lock().unwrap().push(e.id.0);
            post(WM_APP_MENU);
        }));
        tray_icon::TrayIconEvent::set_event_handler(Some(|e: tray_icon::TrayIconEvent| {
            if let tray_icon::TrayIconEvent::Click {
                button: tray_icon::MouseButton::Left,
                button_state: tray_icon::MouseButtonState::Up,
                ..
            } = e
            {
                MENU_QUEUE.lock().unwrap().push(TRAY_CLICK.into());
                post(WM_APP_MENU);
            }
        }));

        let downloads = shell::downloads_dir();
        let mut settings = settings::load();
        settings.folders.retain(|f| f != &downloads);
        // the folder on show last time, if it's still on the list
        let dir = settings.current.clone().filter(|c| settings.folders.contains(c)).unwrap_or_else(|| downloads.clone());

        // Folder watcher: event-driven (ReadDirectoryChangesW), no polling. Every
        // watched folder, not just the one on show, so arrivals elsewhere bounce the stack.
        let watcher = notify::recommended_watcher(|res: notify::Result<notify::Event>| {
            use notify::EventKind;
            use notify::event::{ModifyKind, RenameMode};
            let Ok(e) = res else { return };
            let arrived = matches!(
                e.kind,
                EventKind::Create(_) | EventKind::Modify(ModifyKind::Name(RenameMode::To | RenameMode::Both | RenameMode::Any))
            );
            FOLDER_EVENTS.lock().unwrap().extend(e.paths.into_iter().map(|p| (p, arrived)));
            post(WM_APP_FOLDER);
        })
        .ok()
        .map(|mut w| {
            use notify::Watcher;
            for f in std::iter::once(&downloads).chain(&settings.folders) {
                let _ = w.watch(f, notify::RecursiveMode::NonRecursive);
            }
            w
        });

        let state = Stash {
            gfx,
            tile,
            fan,
            badge,
            _targets: (tile_target, fan_target, badge_target),
            _drop_target: drop_target,
            tile_root,
            stack,
            glow,
            fan_root,
            fan_layer,
            fan_outgoing: Vec::new(),
            fan_shown: false,
            badge_root,
            outgoing: Vec::new(),
            props,
            ease_out,
            ease_in,
            rows: Vec::new(),
            k: 1.0,
            ik: 1.0,
            fan_size: (FAN_W, FAN_MAX_H),
            work_area: RECT::default(),
            dir,
            downloads,
            settings,
            items: Vec::new(),
            thumbs: HashMap::new(),
            pending: HashSet::new(),
            folder_icons: HashMap::new(),
            jobs,
            open: false,
            tile_hover: false,
            outside_ms: 0,
            dragging: false,
            menu_open: false,
            menu_target: None,
            press: None,
            hover_row: None,
            tracking_fan_leave: false,
            ready: false,
            pinned: false,
            topmost: false,
            pinned_fg: HWND::default(),
            wheel: 0,
            last_switch: std::time::Instant::now(),
            tray: None,
            watcher,
        };
        APP.with(|a| *a.borrow_mut() = Some(state));
        with(|s| {
            s.tray = build_tray(s.stack_menu());
            s.layout();
            s.request_folder_icon();
            s.reload(true);
            s.prefetch();
            s.ready = true;
        });
        log("started");
        Ok(())
    }

    fn right(&self) -> bool {
        self.settings.corner == Corner::BottomRight
    }

    // ---------- placement ----------

    /// Size and place both windows in the screen corner (physical pixels) and
    /// rebuild everything that depends on the DPI scale.
    fn layout(&mut self) {
        let wa = shell::work_area();
        self.work_area = wa;
        let k = shell::scale_factor(self.tile);
        self.k = k;
        self.ik = k * self.settings.icon_size.scale();
        let ik = self.ik;
        let right = self.right();
        // bigger icons need a wider, taller fan (labels keep their size)
        let grow = self.settings.icon_size.scale().max(1.0);
        let fan_h = (FAN_MAX_H * k * grow).min((wa.bottom - wa.top) as f32 - MARGIN * 2.0 * k);
        self.fan_size = (FAN_W * k * grow, fan_h);
        let m = (MARGIN * k).round() as i32;
        let t = (TILE * ik).round() as i32;
        let (fw, fh) = (self.fan_size.0.round() as i32, fan_h.round() as i32);
        self.fan_size = (fw as f32, fh as f32); // match the window exactly
        let tx = if right { wa.right - m - t } else { wa.left + m };
        let fx = if right { wa.right - m - fw } else { wa.left + m };
        shell::place(self.tile, None, tx, wa.bottom - m - t, t, t, true);
        // the fan sits directly beneath the tile, so items emerge from behind it
        shell::place(self.fan, Some(self.tile), fx, wa.bottom - m - fh, fw, fh, false);

        self.drop_outgoing(true);
        let _ = self.setup_stack(&self.stack);
        let _ = self.build_glow();
        let _ = self.build_tile();
        let _ = self.build_fan();
    }

    /// The tile window's size: whole pixels, so its center matches the fan's origin.
    fn tile_px(&self) -> f32 {
        (TILE * self.ik).round()
    }

    /// The pile sits centered in the tile.
    fn stack_offset(&self) -> Vector3 {
        let m = (self.tile_px() - STACK * self.ik) / 2.0;
        Vector3 { X: m, Y: m, Z: 0.0 }
    }

    /// A pile layer's icon size, in whole pixels. The fan scales its rows to exactly
    /// this at rest, so the pile and the fan line up pixel for pixel.
    fn pile_px(&self, l: &Layer) -> f32 {
        (STACK * self.ik * l.scale).round()
    }

    /// Thumbnail resolution: enough for the largest place an icon is drawn.
    fn thumb_px(&self) -> i32 {
        (96.0 * self.ik.max(self.k)).round() as i32
    }

    /// Size and place a pile container. Its scale follows its "Grow" property, so it
    /// can be sprung as a scalar (it swells when files are dragged over it).
    fn setup_stack(&self, stack: &ContainerVisual) -> Result<()> {
        let ik = self.ik;
        stack.SetSize(Vector2 { X: STACK * ik, Y: STACK * ik })?;
        stack.SetOffset(self.stack_offset())?;
        stack.SetCenterPoint(Vector3 { X: STACK * ik / 2.0, Y: STACK * ik / 2.0, Z: 0.0 })?;
        let props = stack.Properties()?;
        props.InsertScalar(&HSTRING::from("Grow"), 1.0)?;
        let a = self.gfx.compositor.CreateExpressionAnimationWithExpression(&HSTRING::from("Vector3(s.Grow, s.Grow, 1)"))?;
        a.SetReferenceParameter(&HSTRING::from("s"), &props)?;
        stack.StartAnimation(&HSTRING::from("Scale"), &a)
    }

    fn relayout_if_needed(&mut self) {
        let wa = shell::work_area();
        if wa != self.work_area || shell::scale_factor(self.tile) != self.k {
            self.close();
            self.layout();
        }
    }

    // ---------- folder contents & thumbnails ----------

    fn reload(&mut self, force: bool) {
        let new = files::list(&self.dir, self.settings.sort);
        if !force && !files::changed(&self.items, &new) {
            return;
        }
        let before: HashSet<&Path> = self.items.iter().map(|i| i.path.as_path()).collect();
        let fresh = new.iter().any(|i| !before.contains(i.path.as_path()) && !files::is_partial(&i.name));
        self.items = new;
        self.request_thumbs();
        let _ = self.build_tile();
        let _ = self.build_fan();
        if self.ready && fresh {
            let _ = self.bounce();
        }
    }

    fn visible(&self) -> &[Item] {
        &self.items[..self.items.len().min(self.settings.limit.min(MAX_ITEMS))]
    }

    fn request_thumbs(&mut self) {
        let wanted: Vec<(PathBuf, SystemTime)> = self.items.iter().take(MAX_ITEMS).map(|i| (i.path.clone(), i.modified)).collect();
        self.fetch_thumbs(&wanted);
        // Keep memory flat: drop this folder's stale thumbnails. Other folders' stay,
        // so switching to them shows their pile at once (they're pruned when shown).
        let keep: HashSet<&PathBuf> = wanted.iter().map(|(p, _)| p).collect();
        let (dir, folders) = (self.dir.clone(), self.folders());
        self.thumbs.retain(|p, _| {
            keep.contains(p) || p.parent().is_some_and(|parent| parent != dir && folders.iter().any(|f| f == parent))
        });
    }

    fn fetch_thumbs(&mut self, wanted: &[(PathBuf, SystemTime)]) {
        let size = self.thumb_px();
        for (path, modified) in wanted {
            let fresh = self.thumbs.get(path).is_some_and(|(m, _)| m == modified);
            if !fresh && self.pending.insert(path.clone()) {
                let _ = self.jobs.send(Job { path: path.clone(), modified: *modified, size });
            }
        }
    }

    /// Load the other folders' newest thumbnails (and icons) in the background, so
    /// switching to them never waits.
    fn prefetch(&mut self) {
        for f in self.folders() {
            if f == self.dir {
                continue;
            }
            let wanted: Vec<(PathBuf, SystemTime)> =
                files::list(&f, self.settings.sort).into_iter().take(MAX_ITEMS).map(|i| (i.path, i.modified)).collect();
            self.fetch_thumbs(&wanted);
            if !self.folder_icons.contains_key(&f) {
                let _ = self.jobs.send(Job { path: f.clone(), modified: SystemTime::UNIX_EPOCH, size: self.thumb_px() });
            }
        }
    }

    fn on_thumbs(&mut self) {
        let done: Vec<thumbs::Done> = std::mem::take(&mut *THUMB_QUEUE.lock().unwrap());
        // whether any of these is on show (this folder's files or its icon)
        let shown = done.iter().any(|d| d.path == self.dir || d.path.parent() == Some(self.dir.as_path()));
        for d in done {
            // a watched folder can also be an item in another one, so check both
            if d.modified == SystemTime::UNIX_EPOCH && self.folders().contains(&d.path) {
                self.folder_icons.insert(d.path.clone(), d.pixels.clone());
            }
            if self.pending.remove(&d.path) {
                self.thumbs.insert(d.path, (d.modified, d.pixels));
            }
        }
        if !shown {
            return;
        }
        let _ = self.build_tile();
        let _ = self.build_fan();
    }

    fn thumb(&self, path: &Path) -> Option<&Pixels> {
        self.thumbs.get(path).and_then(|(_, p)| p.as_ref())
    }

    // ---------- visuals ----------

    fn sprite(&self, surface: &CompositionDrawingSurface, w: f32, h: f32) -> Result<SpriteVisual> {
        let c = &self.gfx.compositor;
        let brush = c.CreateSurfaceBrushWithSurface(surface)?;
        brush.SetStretch(CompositionStretch::Fill)?;
        let v = c.CreateSpriteVisual()?;
        v.SetBrush(&brush)?;
        v.SetSize(Vector2 { X: w, Y: h })?;
        Ok(v)
    }

    /// Soft shadow that follows the visual's own alpha (icon shapes, pills).
    fn shadow(&self, v: &SpriteVisual, blur: f32, y: f32, opacity: f32) -> Result<()> {
        let s = self.gfx.compositor.CreateDropShadow()?;
        s.SetBlurRadius(blur * self.k)?;
        s.SetOffset(Vector3 { X: 0.0, Y: y * self.k, Z: 0.0 })?;
        s.SetOpacity(opacity)?;
        s.SetColor(Color { A: 255, R: 0, G: 0, B: 0 })?;
        s.SetSourcePolicy(CompositionDropShadowSourcePolicy::InheritFromVisualContent)?;
        v.SetShadow(&s)
    }

    fn build_glow(&self) -> Result<()> {
        let c = &self.gfx.compositor;
        let brush = c.CreateRadialGradientBrush()?;
        let stops = brush.ColorStops()?;
        stops.Append(&c.CreateColorGradientStopWithOffsetAndColor(0.0, Color { A: 150, R: 10, G: 132, B: 255 })?)?;
        stops.Append(&c.CreateColorGradientStopWithOffsetAndColor(1.0, Color { A: 0, R: 10, G: 132, B: 255 })?)?;
        let size = TILE * self.ik;
        self.glow.SetBrush(&brush)?;
        self.glow.SetSize(Vector2 { X: size, Y: size })?;
        self.glow.SetOpacity(0.0)
    }

    /// The icon in the corner: a pile of the newest downloads, or the folder icon.
    fn build_tile(&self) -> Result<()> {
        let k = self.k;
        let children = self.stack.Children()?;
        children.RemoveAll()?;
        let stack_px = STACK * self.ik;
        let pile: Vec<(&Pixels, &Layer)> = if self.settings.display == Display::Stack {
            self.items.iter().take(3).zip(PILE.iter()).filter_map(|(it, l)| Some((self.thumb(&it.path)?, l))).collect()
        } else {
            Vec::new()
        };
        if pile.is_empty() {
            if let Some(px) = self.folder_icons.get(&self.dir).and_then(|p| p.as_ref()) {
                let surface = self.gfx.image_surface(px, stack_px, 0.0, 1.0, 1.0)?;
                let v = self.sprite(&surface, stack_px, stack_px)?;
                self.shadow(&v, 5.0, 3.0, 0.35)?;
                children.InsertAtTop(&v)?;
            }
            return Ok(());
        }
        // back to front
        for (px, l) in pile.into_iter().rev() {
            let size = self.pile_px(l);
            let surface = self.gfx.image_surface(px, size, l.blur * k, l.brightness, l.saturation)?;
            let v = self.sprite(&surface, size, size)?;
            v.SetOffset(Vector3 { X: (stack_px - size) / 2.0 + l.dx * self.ik, Y: (stack_px - size) / 2.0 + l.dy * self.ik, Z: 0.0 })?;
            v.SetCenterPoint(Vector3 { X: size / 2.0, Y: size / 2.0, Z: 0.0 })?;
            v.SetRotationAngleInDegrees(l.rotate)?;
            if l.blur == 0.0 {
                self.shadow(&v, 6.0, 3.0, 0.45)?;
            } else {
                self.shadow(&v, 4.0, 2.0, 0.3)?;
            }
            children.InsertAtTop(&v)?;
        }
        Ok(())
    }

    /// Where the tile's center sits inside the fan window.
    fn fan_origin(&self) -> (f32, f32) {
        let (w, h) = self.fan_size;
        let half = self.tile_px() / 2.0;
        (if self.right() { w - half } else { half }, h - half)
    }

    /// Rebuild the fan rows. Each row's placement is an expression of the shared
    /// `Progress` value, so rebuilding mid-animation continues seamlessly.
    fn build_fan(&mut self) -> Result<()> {
        let k = self.k;
        let ik = self.ik;
        let c = self.gfx.compositor.clone();
        self.fan_layer.Children()?.RemoveAll()?;
        self.rows.clear();
        self.hover_row = None;

        let right = self.right();
        let (ox, oy) = self.fan_origin();
        let (_, fan_h) = self.fan_size;
        let fits = ((fan_h - TILE * ik / 2.0 - FIRST * ik - ICON * ik) / (STEP * ik)).floor().max(0.0) as usize + 1;
        let visible: Vec<Item> = self.visible().to_vec();
        let n = visible.len().min(fits);
        let hidden = self.items.len() - n;
        let icon = ICON * ik;
        let h = icon / 2.0;
        let dir = if right { -1.0 } else { 1.0 };
        let mut rows = Vec::new();

        let expr = |e: String, prop: &str, target: &Visual| -> Result<()> {
            let a = c.CreateExpressionAnimationWithExpression(&HSTRING::from(e))?;
            a.SetReferenceParameter(&HSTRING::from("p"), &self.props)?;
            target.StartAnimation(&HSTRING::from(prop), &a)
        };
        // the name pill beside an icon, toward the screen center, with its hover look
        let make_label = |text: &str| -> Result<(SpriteVisual, CompositionSurfaceBrush, CompositionSurfaceBrush, f32, f32)> {
            let (normal, lw, lh) = self.gfx.label(text, k, false)?;
            let (hover, _, _) = self.gfx.label(text, k, true)?;
            let label = self.sprite(&normal, lw, lh)?;
            let label_normal = label.Brush()?.cast::<CompositionSurfaceBrush>()?;
            let label_hover = c.CreateSurfaceBrushWithSurface(&hover)?;
            label_hover.SetStretch(CompositionStretch::Fill)?;
            self.shadow(&label, 8.0, 2.0, 0.3)?;
            expr("p.Label".into(), "Opacity", &label.cast()?)?;
            Ok((label, label_normal, label_hover, lw, lh))
        };

        // "Open <folder>" takes the stack's place when it opens, so the folder's name
        // always sits in the same spot, at the bottom, easy to check while switching.
        {
            let open = format!("Open \u{201c}{}\u{201d}", shell::folder_name(&self.dir));
            let text = if hidden > 0 { format!("{open}  ·  {hidden} more") } else { open };
            let row = c.CreateContainerVisual()?;
            row.SetSize(Vector2 { X: icon, Y: icon })?;
            row.SetOffset(Vector3 { X: ox - h, Y: oy - h, Z: 0.0 })?;
            // In folder mode the tile keeps showing the folder icon, so the row is just
            // its label. In stack mode the pile flies out, and the folder icon (or the
            // open button) grows in where it was.
            let half = if self.settings.display == Display::Stack {
                let (v, size) = match self.folder_icons.get(&self.dir).and_then(|p| p.as_ref()) {
                    Some(px) => {
                        let size = self.pile_px(&PILE[0]);
                        (self.sprite(&self.gfx.image_surface(px, size, 0.0, 1.0, 1.0)?, size, size)?, size)
                    }
                    None => {
                        let size = OPEN_ICON * ik;
                        (self.sprite(&self.gfx.open_button(size, k)?, size, size)?, size)
                    }
                };
                v.SetOffset(Vector3 { X: (icon - size) / 2.0, Y: (icon - size) / 2.0, Z: 0.0 })?;
                v.SetCenterPoint(Vector3 { X: size / 2.0, Y: size / 2.0, Z: 0.0 })?;
                self.shadow(&v, 6.0, 3.0, 0.4)?;
                expr("Clamp((p.Progress - 0.15) / 0.45, 0, 1)".into(), "Opacity", &v.cast()?)?;
                expr("Vector3(0.7 + 0.3 * Clamp(p.Progress, 0, 1), 0.7 + 0.3 * Clamp(p.Progress, 0, 1), 1)".into(), "Scale", &v.cast()?)?;
                row.Children()?.InsertAtTop(&v)?;
                size / 2.0
            } else {
                STACK * ik / 2.0
            };
            let (label, label_normal, label_hover, lw, lh) = make_label(&text)?;
            let gap = half + 10.0 * k;
            label.SetOffset(Vector3 { X: if right { h - gap - lw } else { h + gap }, Y: (icon - lh) / 2.0, Z: 0.0 })?;
            row.Children()?.InsertAtTop(&label)?;
            self.fan_layer.Children()?.InsertAtTop(&row)?; // above the files leaving the pile
            let reach = gap + lw;
            rows.push(Row {
                label,
                label_normal,
                label_hover,
                center: (ox, oy),
                angle: 0.0,
                extent: if right { (-reach, -half, half, half) } else { (-half, -half, reach, half) },
                path: None,
            });
        }

        // The arc's radius grows with the list, so the top row tilts at most MAX_TILT.
        let last_slot = n.saturating_sub(1) as f32;
        let r = (RADIUS * ik).max((FIRST + last_slot * STEP) * ik / MAX_TILT);
        for (i, item) in visible[..n].iter().enumerate() {
            let s = (FIRST + i as f32 * STEP) * ik;
            let row = c.CreateContainerVisual()?;
            row.SetSize(Vector2 { X: icon, Y: icon })?;
            row.SetCenterPoint(Vector3 { X: icon / 2.0, Y: icon / 2.0, Z: 0.0 })?;

            let thumb = self.thumb(&item.path);
            let pile_pose = (i < 3 && self.settings.display == Display::Stack && thumb.is_some()).then(|| &PILE[i]);

            let icon_visual = match thumb {
                Some(px) => {
                    let v = self.sprite(&self.gfx.image_surface(px, icon, 0.0, 1.0, 1.0)?, icon, icon)?;
                    self.shadow(&v, 6.0, 3.0, 0.45)?;
                    row.Children()?.InsertAtTop(&v)?;
                    Some(v)
                }
                None => None,
            };
            // Back pile layers are blurred and dimmed in the pile. Keep a copy in that
            // look beneath the sharp icon and cross-fade with Progress, so the blur and
            // darkening build up gradually as the item collapses (and clear as it opens).
            let soft_icon = match (pile_pose, thumb) {
                (Some(l), Some(px)) if l.blur > 0.0 => {
                    let s0 = self.pile_px(l) / icon; // the row is scaled by this in the pile
                    let v = self.sprite(&self.gfx.image_surface(px, icon, l.blur * k / s0, l.brightness, l.saturation)?, icon, icon)?;
                    self.shadow(&v, 4.0 / s0, 2.0 / s0, 0.3)?;
                    row.Children()?.InsertAtBottom(&v)?;
                    Some(v)
                }
                _ => None,
            };

            let (label, label_normal, label_hover, lw, lh) = make_label(&item.name)?;
            let lx = if right { -(lw + 10.0 * k) } else { icon + 10.0 * k };
            label.SetOffset(Vector3 { X: lx, Y: (icon - lh) / 2.0, Z: 0.0 })?;
            row.Children()?.InsertAtTop(&label)?;

            // Drive placement from Progress. The top items start exactly where their
            // layer sits in the pile (offset, tilt, size), so the stack itself opens up;
            // the rest emerge small from behind it.
            let (pdx, pdy, prot, s0, fade) = match pile_pose {
                Some(l) => (l.dx * ik, l.dy * ik, l.rotate, self.pile_px(l) / icon, false),
                None => (0.0, 0.0, 0.0, 0.4, true),
            };
            let rv: Visual = row.cast()?;
            expr(
                format!("Vector3({ox} + {pdx}*(1 - p.Progress) + {dir}*{r}*(1 - Cos({s}*p.Progress/{r})) - {h}, {oy} + {pdy}*(1 - p.Progress) - {r}*Sin({s}*p.Progress/{r}) - {h}, 0)"),
                "Offset",
                &rv,
            )?;
            let grow = 1.0 - s0;
            expr(format!("Vector3({s0} + {grow}*p.Progress, {s0} + {grow}*p.Progress, 1)"), "Scale", &rv)?;
            expr(format!("{prot}*(1 - p.Progress) + {dir} * {s} * p.Progress / {r} * 57.29578"), "RotationAngleInDegrees", &rv)?;
            if fade {
                expr("Clamp(p.Progress / 0.2, 0, 1)".into(), "Opacity", &rv)?;
            }
            if let (Some(soft), Some(sharp)) = (&soft_icon, &icon_visual) {
                expr("Clamp(p.Progress, 0, 1)".into(), "Opacity", &sharp.cast()?)?;
                expr("Clamp((1 - p.Progress) / 0.4, 0, 1)".into(), "Opacity", &soft.cast()?)?;
            }
            // pile order: each later row tucks behind the one before
            self.fan_layer.Children()?.InsertAtBottom(&row)?;

            // final pose, for hit-testing and the click-through region
            let (dx, dy) = arc(s, r, right);
            let reach = h + 10.0 * k + lw;
            rows.push(Row {
                label,
                label_normal,
                label_hover,
                center: (ox + dx, oy + dy),
                angle: dir * s / r,
                extent: if right { (-reach, -h, h, h) } else { (-h, -h, reach, h) },
                path: Some(item.path.clone()),
            });
        }
        self.rows = rows;
        self.update_fan_region();
        Ok(())
    }

    /// Clip the fan window to its rows (plus the tile) so clicks in the gaps reach
    /// the desktop underneath.
    fn update_fan_region(&self) {
        let pad = 14.0 * self.k;
        let (w, h) = self.fan_size;
        let t = self.tile_px();
        let tile_left = if self.right() { w - t } else { 0.0 };
        unsafe {
            let region = CreateRectRgn(tile_left as i32, (h - t) as i32, (tile_left + t) as i32, h as i32);
            let current = self.rows.iter().map(|row| row.corners(pad));
            let leaving = self.fan_outgoing.iter().flat_map(|(_, corners, _)| corners.iter().copied());
            for corners in current.chain(leaving) {
                let r = CreatePolygonRgn(&corners, WINDING);
                CombineRgn(Some(region), Some(region), Some(r), RGN_OR);
                let _ = DeleteObject(HGDIOBJ(r.0));
            }
            SetWindowRgn(self.fan, Some(region), true);
        }
    }

    // ---------- animation ----------

    fn animate_scalar(&self, prop: &str, to: f32, duration: i64, delay: i64, ease: &CubicBezierEasingFunction) -> Result<()> {
        let a = self.gfx.compositor.CreateScalarKeyFrameAnimation()?;
        a.InsertExpressionKeyFrame(0.0, &HSTRING::from("this.StartingValue"))?;
        a.InsertKeyFrameWithEasingFunction(1.0, to, ease)?;
        a.SetDuration(ms(duration))?;
        if delay > 0 {
            a.SetDelayTime(ms(delay))?;
        }
        self.props.StartAnimation(&HSTRING::from(prop), &a)
    }

    fn spring_scalar(&self, prop: &str, to: f32, damping: f32, period: i64, delay: i64) -> Result<()> {
        self.spring_on(&self.props, prop, to, damping, period, delay)
    }

    fn spring_on(&self, target: &CompositionPropertySet, prop: &str, to: f32, damping: f32, period: i64, delay: i64) -> Result<()> {
        use windows::Foundation::{IReference, PropertyValue};
        let a = self.gfx.compositor.CreateSpringScalarAnimation()?;
        a.SetFinalValue(&PropertyValue::CreateSingle(to)?.cast::<IReference<f32>>()?)?;
        a.SetDampingRatio(damping)?;
        a.SetPeriod(ms(period))?;
        if delay > 0 {
            a.SetDelayTime(ms(delay))?;
        }
        target.StartAnimation(&HSTRING::from(prop), &a)
    }

    fn animate_opacity(&self, v: &Visual, to: f32, duration: i64) -> Result<()> {
        let a = self.gfx.compositor.CreateScalarKeyFrameAnimation()?;
        a.InsertExpressionKeyFrame(0.0, &HSTRING::from("this.StartingValue"))?;
        a.InsertKeyFrameWithEasingFunction(1.0, to, &self.ease_out)?;
        a.SetDuration(ms(duration))?;
        v.StartAnimation(&HSTRING::from("Opacity"), &a)
    }

    /// Fade the whole stack (tile and fan) together.
    fn fade(&self, to: f32, duration: i64, delay: i64) {
        let ease = if to > 0.0 { &self.ease_out } else { &self.ease_in };
        for v in [&self.tile_root, &self.fan_root] {
            let _ = (|| -> Result<()> {
                let a = self.gfx.compositor.CreateScalarKeyFrameAnimation()?;
                a.InsertExpressionKeyFrame(0.0, &HSTRING::from("this.StartingValue"))?;
                a.InsertKeyFrameWithEasingFunction(1.0, to, ease)?;
                a.SetDuration(ms(duration))?;
                if delay > 0 {
                    a.SetDelayTime(ms(delay))?;
                }
                v.StartAnimation(&HSTRING::from("Opacity"), &a)
            })();
        }
    }

    fn set_opacity(&self, to: f32) {
        for v in [&self.tile_root, &self.fan_root] {
            let _ = v.StopAnimation(&HSTRING::from("Opacity"));
            let _ = v.SetOpacity(to);
        }
    }

    /// Single hop when a download finishes, like the Dock.
    fn bounce(&self) -> Result<()> {
        let c = &self.gfx.compositor;
        let base = self.stack_offset();
        let up = Vector3 { Y: base.Y - 18.0 * self.ik, ..base };
        let a = c.CreateVector3KeyFrameAnimation()?;
        a.InsertKeyFrame(0.0, base)?;
        a.InsertKeyFrameWithEasingFunction(0.45, up, &c.CreateCubicBezierEasingFunction(Vector2 { X: 0.2, Y: 0.7 }, Vector2 { X: 0.4, Y: 1.0 })?)?;
        a.InsertKeyFrameWithEasingFunction(1.0, base, &c.CreateCubicBezierEasingFunction(Vector2 { X: 0.6, Y: 0.0 }, Vector2 { X: 0.8, Y: 0.3 })?)?;
        a.SetDuration(ms(550))?;
        self.stack.StartAnimation(&HSTRING::from("Offset"), &a)
    }

    // ---------- open / close ----------

    fn open(&mut self) {
        log(format!("open (already open: {})", self.open));
        if self.open {
            return;
        }
        self.open = true;
        self.outside_ms = 0;
        unsafe {
            let _ = KillTimer(Some(self.tile), T_HIDE_FAN);
            let _ = ShowWindow(self.fan, SW_SHOWNOACTIVATE);
            let _ = SetTimer(Some(self.tile), T_POLL, POLL_MS, None);
        }
        self.fan_shown = true;
        shell::restack(self.fan, Some(self.tile));
        let _ = self.spring_scalar("Progress", 1.0, OPEN_DAMPING, OPEN_PERIOD_MS, 0);
        let _ = self.animate_scalar("Label", 1.0, LABEL_IN_MS, LABEL_IN_DELAY_MS, &self.ease_out);
        self.show_pile(false);
    }

    fn close(&mut self) {
        log(format!("close (open: {})", self.open));
        if !self.open {
            return;
        }
        self.open = false;
        self.press = None;
        self.set_hover_row(None);
        unsafe {
            let _ = KillTimer(Some(self.tile), T_POLL);
            let _ = SetTimer(Some(self.tile), T_HIDE_FAN, (CLOSE_DELAY_MS + CLOSE_SETTLE_MS) as u32, None);
        }
        let _ = self.animate_scalar("Label", 0.0, LABEL_OUT_MS, 0, &self.ease_out);
        let _ = self.spring_scalar("Progress", 0.0, CLOSE_DAMPING, CLOSE_PERIOD_MS, CLOSE_DELAY_MS);
        if self.topmost && shell::covered(self.tile) {
            // it's about to drop back under a window: fade out while it collapses
            self.fade(0.0, FADE_OUT_MS, FADE_OUT_DELAY_MS);
        }
        self.unpin();
    }

    /// Tray click: open the fan on top of every app, or close it if it's open.
    fn toggle_from_tray(&mut self) {
        if self.open && self.pinned {
            self.close();
            return;
        }
        self.pinned = true;
        self.pinned_fg = shell::foreground();
        install_mouse_hook();
        unsafe {
            let _ = KillTimer(Some(self.tile), T_UNFADE);
        }
        if !self.topmost && !self.open && shell::covered(self.tile) {
            // Hidden behind a window: go transparent first, then raise and fade in,
            // so it doesn't pop over the window.
            self.set_opacity(0.0);
            self.topmost = true;
            unsafe {
                let _ = SetTimer(Some(self.tile), T_SURFACE, SURFACE_DELAY_MS, None);
            }
            return;
        }
        self.topmost = true;
        self.restack();
        self.fade(1.0, FADE_IN_MS, 0); // in case it was fading out from a moment ago
        self.open();
    }

    fn unpin(&mut self) {
        self.pinned = false;
        uninstall_mouse_hook();
    }

    /// Put both windows at their z-order: above every app while raised, otherwise
    /// just above the desktop. The fan always sits directly beneath the tile.
    fn restack(&self) {
        if self.topmost {
            shell::raise_topmost(self.badge);
        } else {
            shell::restack(self.badge, None);
        }
        shell::restack(self.tile, Some(self.badge));
        shell::restack(self.fan, Some(self.tile));
    }

    /// A tray-opened fan closes on any click outside it (the tray icon toggles it itself).
    fn on_outside_click(&mut self, p: POINT) {
        if !self.pinned || !self.open || self.menu_open || self.dragging || self.point_over(p) {
            return;
        }
        let on_tray = self.tray.as_ref().and_then(|t| t.rect()).is_some_and(|r| {
            let (x, y) = (p.x as f64, p.y as f64);
            x >= r.position.x && x < r.position.x + r.size.width as f64 && y >= r.position.y && y < r.position.y + r.size.height as f64
        });
        if !on_tray {
            self.close();
        }
    }

    /// The pile is hidden while open: its items are out in the fan. (The folder
    /// icon has no counterpart in the fan, so it stays.)
    fn show_pile(&self, visible: bool) {
        let hide = !visible && self.settings.display == Display::Stack;
        let _ = self.stack.StopAnimation(&HSTRING::from("Opacity"));
        let _ = self.stack.SetOpacity(if hide { 0.0 } else { 1.0 });
    }

    fn pointer_over(&self) -> bool {
        self.point_over(shell::cursor())
    }

    fn point_over(&self, c: POINT) -> bool {
        let t = shell::window_rect(self.tile);
        if c.x >= t.left && c.x < t.right && c.y >= t.top && c.y < t.bottom {
            return true;
        }
        let f = shell::window_rect(self.fan);
        self.row_at((c.x - f.left) as f32, (c.y - f.top) as f32, HIT_PAD * self.k).is_some()
    }

    fn row_at(&self, x: f32, y: f32, pad: f32) -> Option<usize> {
        self.rows.iter().position(|r| r.contains(x, y, pad))
    }

    fn set_hover_row(&mut self, row: Option<usize>) {
        if row == self.hover_row {
            return;
        }
        if let Some(old) = self.hover_row.and_then(|i| self.rows.get(i)) {
            let _ = old.label.SetBrush(&old.label_normal);
        }
        if let Some(new) = row.and_then(|i| self.rows.get(i)) {
            let _ = new.label.SetBrush(&new.label_hover);
        }
        self.hover_row = row;
    }

    fn on_timer(&mut self, id: usize) {
        match id {
            T_HOVER => {
                unsafe {
                    let _ = KillTimer(Some(self.tile), T_HOVER);
                }
                if self.tile_hover {
                    self.open();
                }
            }
            T_POLL => {
                if self.pinned {
                    // a tray-opened fan ignores the pointer, but closes when you switch apps
                    let fg = shell::foreground();
                    if self.open && !self.menu_open && !self.dragging && fg != self.pinned_fg && !shell::is_own_or_shell(fg) {
                        self.close();
                    }
                    return;
                }
                // STASH_PIN_OPEN=1 keeps the fan open (for inspecting it without a mouse)
                if !self.open || self.dragging || self.menu_open || std::env::var_os("STASH_PIN_OPEN").is_some() {
                    self.outside_ms = 0;
                } else if self.pointer_over() {
                    self.outside_ms = 0;
                } else {
                    self.outside_ms += POLL_MS;
                    if self.outside_ms >= CLOSE_AFTER_OUTSIDE_MS {
                        self.close();
                    }
                }
            }
            T_HIDE_FAN => {
                unsafe {
                    let _ = KillTimer(Some(self.tile), T_HIDE_FAN);
                }
                if !self.open && !self.dragging {
                    // the fan has collapsed back onto the pile's exact pose: swap them
                    self.show_pile(true);
                    unsafe {
                        let _ = ShowWindow(self.fan, SW_HIDE);
                    }
                    self.fan_shown = false;
                    self.drop_outgoing(true);
                    if self.topmost {
                        self.topmost = false;
                        self.restack(); // back beneath every app window
                        // then make it opaque again (if it faded), once it's out of sight
                        unsafe {
                            let _ = SetTimer(Some(self.tile), T_UNFADE, 100, None);
                        }
                    }
                }
            }
            T_RELOAD => {
                unsafe {
                    let _ = KillTimer(Some(self.tile), T_RELOAD);
                }
                self.reload(false);
            }
            T_SURFACE => {
                unsafe {
                    let _ = KillTimer(Some(self.tile), T_SURFACE);
                }
                if self.pinned {
                    self.restack();
                    self.fade(1.0, FADE_IN_MS, 0);
                    self.open();
                }
            }
            T_BADGE => {
                unsafe {
                    let _ = KillTimer(Some(self.tile), T_BADGE);
                    let _ = SetTimer(Some(self.tile), T_BADGE_HIDE, 200, None);
                }
                let _ = self.badge_root.cast().and_then(|v| self.animate_opacity(&v, 0.0, 180));
            }
            T_BADGE_HIDE => {
                unsafe {
                    let _ = KillTimer(Some(self.tile), T_BADGE_HIDE);
                    let _ = ShowWindow(self.badge, SW_HIDE);
                }
            }
            T_OUTGOING => {
                unsafe {
                    let _ = KillTimer(Some(self.tile), T_OUTGOING);
                }
                self.drop_outgoing(false);
            }
            T_BOUNCE => {
                unsafe {
                    let _ = KillTimer(Some(self.tile), T_BOUNCE);
                }
                let _ = self.bounce();
            }
            T_UNFADE => {
                unsafe {
                    let _ = KillTimer(Some(self.tile), T_UNFADE);
                }
                if !self.topmost {
                    self.set_opacity(1.0);
                }
            }
            _ => {}
        }
    }

    // ---------- input ----------

    /// While open, the tile is the "Open <folder>" row: hovering lights its label and a
    /// click opens the folder.
    fn on_tile_mouse(&mut self, msg: u32) -> Option<Blocking> {
        match msg {
            WM_MOUSEMOVE => {
                if !self.tile_hover {
                    self.tile_hover = true;
                    track_leave(self.tile);
                    unsafe {
                        let _ = SetTimer(Some(self.tile), T_HOVER, HOVER_OPEN_MS, None);
                    }
                }
                if self.open {
                    self.set_hover_row(Some(0));
                }
            }
            WM_MOUSELEAVE => {
                self.tile_hover = false;
                unsafe {
                    let _ = KillTimer(Some(self.tile), T_HOVER);
                }
                if self.hover_row == Some(0) {
                    self.set_hover_row(None);
                }
            }
            WM_LBUTTONUP => {
                if self.open {
                    self.activate(None);
                } else {
                    self.open();
                }
            }
            WM_RBUTTONUP => {
                self.menu_target = None;
                return Some(Blocking::Menu { hwnd: self.tile, item: false });
            }
            _ => {}
        }
        None
    }

    fn on_fan_mouse(&mut self, msg: u32, wparam: WPARAM, x: i32, y: i32) -> Option<Blocking> {
        let row = if self.open { self.row_at(x as f32, y as f32, 0.0) } else { None };
        match msg {
            WM_MOUSEMOVE => {
                if !self.tracking_fan_leave {
                    self.tracking_fan_leave = true;
                    track_leave(self.fan);
                }
                if let Some((i, at)) = self.press {
                    let dragged = (x - at.x).abs().max((y - at.y).abs()) as f32 > 6.0 * self.k;
                    if wparam.0 & 0x0001 != 0 /* MK_LBUTTON */ && dragged {
                        self.press = None;
                        if let Some(path) = self.rows.get(i).and_then(|r| r.path.clone()) {
                            return Some(Blocking::Drag(path));
                        }
                    }
                }
                self.set_hover_row(row);
            }
            WM_MOUSELEAVE => {
                self.tracking_fan_leave = false;
                self.set_hover_row(None);
            }
            WM_LBUTTONDOWN => self.press = row.map(|i| (i, POINT { x, y })),
            WM_LBUTTONUP => {
                if let (Some((i, _)), Some(r)) = (self.press.take(), row) {
                    if i == r {
                        let path = self.rows[r].path.clone();
                        self.activate(path);
                    }
                }
            }
            WM_RBUTTONUP => {
                if let Some(path) = row.and_then(|r| self.rows[r].path.clone()) {
                    self.menu_target = Some(path);
                    return Some(Blocking::Menu { hwnd: self.fan, item: true });
                }
            }
            _ => {}
        }
        None
    }

    fn activate(&mut self, path: Option<PathBuf>) {
        shell::open(path.as_deref().unwrap_or(&self.dir));
        self.close();
    }

    fn on_drop_event(&mut self, e: droptarget::Event) {
        match e {
            droptarget::Event::Hover(on) => {
                let s = if on { 1.14 } else { 1.0 };
                let _ = self.stack.Properties().and_then(|p| self.spring_on(&p, "Grow", s, 0.55, 70, 0));
                let _ = self.glow.cast().and_then(|v| self.animate_opacity(&v, if on { 1.0 } else { 0.0 }, 160));
            }
            droptarget::Event::Drop { paths, copy } => {
                for p in &paths {
                    let _ = files::import(p, &self.dir, copy);
                }
                self.reload(false);
            }
        }
    }

    fn on_menu(&mut self) -> Option<Blocking> {
        let queued: Vec<String> = std::mem::take(&mut *MENU_QUEUE.lock().unwrap());
        let mut blocking = None;
        for id in queued {
            let target = self.menu_target.clone();
            if let Some(i) = id.strip_prefix(ids::FOLDER_PREFIX).and_then(|i| i.parse::<usize>().ok()) {
                if let Some(f) = self.folders().get(i).cloned() {
                    let dir = if i >= self.current_index() { 1.0 } else { -1.0 };
                    self.set_folder(f, dir);
                }
                continue;
            }
            match id.as_str() {
                ids::ADD_FOLDER => blocking = Some(Blocking::PickFolder),
                ids::REMOVE_FOLDER => self.remove_folder(),
                TRAY_CLICK => self.toggle_from_tray(),
                ids::OPEN_FOLDER => self.activate(None),
                ids::QUIT => unsafe { PostQuitMessage(0) },
                ids::LOGIN => {
                    shell::set_start_at_login(!shell::start_at_login());
                    self.refresh_tray();
                }
                ids::ITEM_OPEN => self.activate(target),
                ids::ITEM_REVEAL => {
                    if let Some(p) = target {
                        shell::reveal(&p);
                    }
                    self.close();
                }
                ids::ITEM_RECYCLE => {
                    if let Some(p) = target {
                        shell::recycle(&p);
                    }
                    self.reload(false);
                }
                other => {
                    let before = self.settings.clone();
                    if menu::apply_setting(other, &mut self.settings) && self.settings != before {
                        settings::save(&self.settings);
                        self.close();
                        self.refresh_tray();
                        if self.settings.icon_size != before.icon_size {
                            self.layout();
                            // fetch thumbnails again at the new resolution
                            self.thumbs.clear();
                            self.pending.clear();
                            self.folder_icons.clear();
                            self.request_folder_icon();
                        } else if self.settings.corner != before.corner {
                            self.layout();
                        }
                        self.reload(true);
                        self.prefetch(); // sort, count or size changed
                    }
                }
            }
        }
        blocking
    }

    fn refresh_tray(&self) {
        if let Some(tray) = &self.tray {
            tray.set_menu(Some(Box::new(self.stack_menu())));
        }
    }

    fn stack_menu(&self) -> tray_icon::menu::Menu {
        let names: Vec<String> = self.folders().iter().map(|f| shell::folder_name(f)).collect();
        menu::stack_menu(&self.settings, shell::start_at_login(), &names, self.current_index())
    }

    // ---------- folders ----------

    /// Every watched folder, Downloads first.
    fn folders(&self) -> Vec<PathBuf> {
        std::iter::once(self.downloads.clone()).chain(self.settings.folders.iter().cloned()).collect()
    }

    fn current_index(&self) -> usize {
        self.folders().iter().position(|f| f == &self.dir).unwrap_or(0)
    }

    fn request_folder_icon(&mut self) {
        if !self.folder_icons.contains_key(&self.dir) {
            let _ = self.jobs.send(Job { path: self.dir.clone(), modified: SystemTime::UNIX_EPOCH, size: self.thumb_px() });
        }
    }

    /// Show another folder. `dir` is the direction the new pile slides in from
    /// (1 = from the right, as when moving to the next folder).
    fn set_folder(&mut self, path: PathBuf, dir: f32) {
        if path == self.dir {
            return;
        }
        self.dir = path;
        self.settings.current = (self.dir != self.downloads).then(|| self.dir.clone());
        settings::save(&self.settings);
        // hand the old pile (or fan) to a visual of its own, to animate it out
        let leaving = if self.open { None } else { self.detach_pile().ok() };
        if self.fan_shown {
            let _ = self.detach_fan();
        }
        // not a reload: nothing here is new, so no bounce
        self.items = files::list(&self.dir, self.settings.sort);
        self.request_thumbs();
        self.request_folder_icon();
        let _ = self.build_tile();
        let _ = self.build_fan();
        self.refresh_tray();

        if self.open {
            // fan the new folder's files out from the pile (its layer starts collapsed)
            let _ = self.spring_scalar("Progress", 1.0, OPEN_DAMPING, OPEN_PERIOD_MS, 0);
            let _ = self.animate_scalar("Label", 1.0, LABEL_IN_MS, LABEL_IN_DELAY_MS, &self.ease_out);
        } else {
            if let Some(old) = leaving {
                let _ = self.slide_out(&old, dir);
            }
            let _ = self.slide_in(dir);
            let _ = self.show_badge();
        }
    }

    /// Swap in a fresh fan layer, with its own Progress and Label starting collapsed,
    /// and fade the old one out from wherever it is: its rows keep following its own
    /// properties, so nothing snaps.
    fn detach_fan(&mut self) -> Result<()> {
        let c = &self.gfx.compositor;
        let fresh = c.CreateContainerVisual()?;
        self.fan_root.Children()?.InsertAbove(&fresh, &self.fan_layer)?;
        let props = c.CreatePropertySet()?;
        props.InsertScalar(&HSTRING::from("Progress"), 0.0)?;
        props.InsertScalar(&HSTRING::from("Label"), 0.0)?;
        self.props = props;
        let old = std::mem::replace(&mut self.fan_layer, fresh);
        let pad = 14.0 * self.k;
        let corners = self.rows.iter().map(|r| r.corners(pad)).collect();
        self.fan_outgoing.push((old.clone(), corners, std::time::Instant::now()));
        self.animate_opacity(&old.cast()?, 0.0, FAN_FADE_MS)?;
        unsafe {
            let _ = SetTimer(Some(self.tile), T_OUTGOING, (SLIDE_MS + 40) as u32, None);
        }
        Ok(())
    }

    /// Swap in a fresh pile container and hand back the old one, still on screen and
    /// still mid-animation if a switch was already under way, so it can slide out
    /// from wherever it is.
    fn detach_pile(&mut self) -> Result<ContainerVisual> {
        let fresh = self.gfx.compositor.CreateContainerVisual()?;
        self.setup_stack(&fresh)?;
        self.tile_root.Children()?.InsertAbove(&fresh, &self.stack)?;
        let old = std::mem::replace(&mut self.stack, fresh);
        self.outgoing.push((old.clone(), std::time::Instant::now()));
        unsafe {
            let _ = SetTimer(Some(self.tile), T_OUTGOING, (SLIDE_MS + 40) as u32, None);
        }
        Ok(old)
    }

    /// Remove old piles and fans that have finished animating out (or all of them).
    fn drop_outgoing(&mut self, all: bool) {
        let done = std::time::Duration::from_millis((SLIDE_MS + 30) as u64);
        if let Ok(children) = self.tile_root.Children() {
            self.outgoing.retain(|(v, since)| {
                let gone = all || since.elapsed() >= done;
                if gone {
                    let _ = children.Remove(v);
                }
                !gone
            });
        }
        if let Ok(children) = self.fan_root.Children() {
            let before = self.fan_outgoing.len();
            self.fan_outgoing.retain(|(v, _, since)| {
                let gone = all || since.elapsed() >= done;
                if gone {
                    let _ = children.Remove(v);
                }
                !gone
            });
            if self.fan_outgoing.len() != before {
                self.update_fan_region(); // stop reserving space for them
            }
        }
        if !self.outgoing.is_empty() || !self.fan_outgoing.is_empty() {
            unsafe {
                let _ = SetTimer(Some(self.tile), T_OUTGOING, 40, None);
            }
        }
    }

    /// The old pile leaves the way the new one doesn't come from.
    fn slide_out(&self, old: &ContainerVisual, dir: f32) -> Result<()> {
        let c = &self.gfx.compositor;
        let base = self.stack_offset();
        let a = c.CreateVector3KeyFrameAnimation()?;
        a.InsertExpressionKeyFrame(0.0, &HSTRING::from("this.StartingValue"))?;
        a.InsertKeyFrameWithEasingFunction(1.0, Vector3 { X: base.X - dir * SLIDE * self.ik, ..base }, &self.ease_out)?;
        a.SetDuration(ms(SLIDE_MS))?;
        old.StartAnimation(&HSTRING::from("Offset"), &a)?;
        let o = c.CreateScalarKeyFrameAnimation()?;
        o.InsertExpressionKeyFrame(0.0, &HSTRING::from("this.StartingValue"))?;
        o.InsertKeyFrameWithEasingFunction(1.0, 0.0, &self.ease_out)?;
        o.SetDuration(ms(SLIDE_MS * 2 / 3))?;
        old.StartAnimation(&HSTRING::from("Opacity"), &o)
    }

    /// The new pile slides in sideways and fades up.
    fn slide_in(&self, dir: f32) -> Result<()> {
        let c = &self.gfx.compositor;
        let base = self.stack_offset();
        let a = c.CreateVector3KeyFrameAnimation()?;
        a.InsertKeyFrame(0.0, Vector3 { X: base.X + dir * SLIDE * self.ik, ..base })?;
        a.InsertKeyFrameWithEasingFunction(1.0, base, &self.ease_out)?;
        a.SetDuration(ms(SLIDE_MS))?;
        self.stack.StartAnimation(&HSTRING::from("Offset"), &a)?;
        let o = c.CreateScalarKeyFrameAnimation()?;
        o.InsertKeyFrame(0.0, 0.0)?;
        o.InsertKeyFrameWithEasingFunction(1.0, 1.0, &self.ease_out)?;
        o.SetDuration(ms(SLIDE_MS * 2 / 3))?;
        self.stack.StartAnimation(&HSTRING::from("Opacity"), &o)
    }

    /// The folder's name and position, above the stack for a moment.
    fn show_badge(&self) -> Result<()> {
        let folders = self.folders();
        let (surface, w, h) = self.gfx.badge(&shell::folder_name(&self.dir), folders.len(), self.current_index(), self.k)?;
        let children = self.badge_root.Children()?;
        children.RemoveAll()?;
        let v = self.sprite(&surface, w, h)?;
        self.shadow(&v, 10.0, 3.0, 0.35)?;
        // leave room around it for the shadow
        let pad = 12.0 * self.k;
        v.SetOffset(Vector3 { X: pad, Y: pad, Z: 0.0 })?;
        children.InsertAtTop(&v)?;

        let t = shell::window_rect(self.tile);
        let (bw, bh) = ((w + pad * 2.0).ceil() as i32, (h + pad * 2.0).ceil() as i32);
        let wa = self.work_area;
        let x = ((t.left + t.right) / 2 - bw / 2).clamp(wa.left, (wa.right - bw).max(wa.left));
        let y = t.top - bh + (pad as i32) - (4.0 * self.k) as i32;
        unsafe {
            let _ = SetWindowPos(self.badge, None, x, y, bw, bh, SWP_NOACTIVATE | SWP_NOZORDER);
            let _ = ShowWindow(self.badge, SW_SHOWNOACTIVATE);
            let _ = KillTimer(Some(self.tile), T_BADGE_HIDE);
            let _ = SetTimer(Some(self.tile), T_BADGE, BADGE_MS, None);
        }
        self.restack();

        // rise into place
        let c = &self.gfx.compositor;
        let o = c.CreateScalarKeyFrameAnimation()?;
        o.InsertExpressionKeyFrame(0.0, &HSTRING::from("this.StartingValue"))?;
        o.InsertKeyFrameWithEasingFunction(1.0, 1.0, &self.ease_out)?;
        o.SetDuration(ms(140))?;
        self.badge_root.StartAnimation(&HSTRING::from("Opacity"), &o)?;
        let a = c.CreateVector3KeyFrameAnimation()?;
        a.InsertKeyFrame(0.0, Vector3 { X: 0.0, Y: 6.0 * self.k, Z: 0.0 })?;
        a.InsertKeyFrameWithEasingFunction(1.0, Vector3 { X: 0.0, Y: 0.0, Z: 0.0 }, &self.ease_out)?;
        a.SetDuration(ms(200))?;
        self.badge_root.StartAnimation(&HSTRING::from("Offset"), &a)
    }

    /// Wheel over the stack: down or right = next folder. Notches and touchpad deltas
    /// add up; after a switch, the rest of that flick is ignored.
    fn on_wheel(&mut self, msg: u32, wparam: WPARAM) {
        let folders = self.folders();
        if folders.len() < 2 || self.dragging || self.menu_open {
            return;
        }
        let delta = (wparam.0 >> 16) as u16 as i16 as i32;
        let delta = if msg == WM_MOUSEHWHEEL { delta } else { -delta };
        if self.last_switch.elapsed().as_millis() < SWITCH_COOLDOWN_MS {
            self.last_switch = std::time::Instant::now(); // still the same flick
            self.wheel = 0;
            return;
        }
        if self.wheel.signum() != delta.signum() {
            self.wheel = 0;
        }
        self.wheel += delta;
        if self.wheel.abs() < WHEEL_STEP {
            return;
        }
        let step = self.wheel.signum();
        self.wheel = 0;
        self.last_switch = std::time::Instant::now();
        let n = folders.len() as i32;
        let next = (self.current_index() as i32 + step).rem_euclid(n) as usize;
        self.set_folder(folders[next].clone(), step as f32);
    }

    fn add_folder(&mut self, path: PathBuf) {
        if !self.folders().contains(&path) {
            self.settings.folders.push(path.clone());
            if let Some(w) = &mut self.watcher {
                use notify::Watcher;
                let _ = w.watch(&path, notify::RecursiveMode::NonRecursive);
            }
        }
        self.set_folder(path.clone(), 1.0);
        settings::save(&self.settings);
        self.refresh_tray();
    }

    fn remove_folder(&mut self) {
        let i = self.current_index();
        if i == 0 {
            return; // Downloads stays
        }
        let gone = self.settings.folders.remove(i - 1);
        if let Some(w) = &mut self.watcher {
            use notify::Watcher;
            let _ = w.unwatch(&gone);
        }
        self.folder_icons.remove(&gone);
        let prev = self.folders()[i - 1].clone();
        self.set_folder(prev, -1.0);
        settings::save(&self.settings);
        self.refresh_tray();
    }

    /// Changes in the folder on show reload it; a new file anywhere else bounces the stack.
    fn on_folder_events(&mut self) {
        let events: Vec<(PathBuf, bool)> = std::mem::take(&mut *FOLDER_EVENTS.lock().unwrap());
        let mut reload = false;
        let mut elsewhere = false;
        for (path, arrived) in events {
            let Some(parent) = path.parent() else { continue };
            if parent == self.dir {
                reload = true;
            } else if arrived && self.folders().iter().any(|f| f == parent) {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                elsewhere |= !files::is_partial(&name) && path.exists();
            }
        }
        unsafe {
            // debounce bursts of file-system events
            if reload {
                let _ = SetTimer(Some(self.tile), T_RELOAD, 300, None);
            }
            if elsewhere {
                let _ = SetTimer(Some(self.tile), T_BOUNCE, 300, None);
            }
        }
    }
}

fn track_leave(hwnd: HWND) {
    let mut t = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
    unsafe {
        let _ = TrackMouseEvent(&mut t);
    }
}

/// While a tray-opened fan is showing, watch for clicks anywhere on screen (our
/// windows never take focus, so there's no deactivation to tell us).
fn install_mouse_hook() {
    if MOUSE_HOOK.load(Ordering::Relaxed) != 0 {
        return;
    }
    unsafe {
        let instance = GetModuleHandleW(None).ok().map(|h| h.into());
        if let Ok(hook) = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), instance, 0) {
            MOUSE_HOOK.store(hook.0 as isize, Ordering::Relaxed);
        }
    }
}

fn uninstall_mouse_hook() {
    let hook = MOUSE_HOOK.swap(0, Ordering::Relaxed);
    if hook != 0 {
        unsafe {
            let _ = UnhookWindowsHookEx(HHOOK(hook as *mut _));
        }
    }
}

extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && matches!(wparam.0 as u32, WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN) {
        let pt = unsafe { (*(lparam.0 as *const MSLLHOOKSTRUCT)).pt };
        // packed like a mouse message's lparam, so wndproc unpacks it the same way
        post_with(WM_APP_OUTSIDE, LPARAM(((pt.y as u16 as isize) << 16) | pt.x as u16 as isize));
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn build_tray(menu: tray_icon::menu::Menu) -> Option<tray_icon::TrayIcon> {
    let png = image::load_from_memory(include_bytes!("../assets/tray@2x.png")).ok()?.into_rgba8();
    let (w, h) = png.dimensions();
    let icon = tray_icon::Icon::from_rgba(png.into_raw(), w, h).ok()?;
    tray_icon::TrayIconBuilder::new()
        .with_tooltip("Stash")
        .with_icon(icon)
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .build()
        .ok()
}

/// Runs a drag or a context menu. These spin their own message loop, so they run
/// with the state borrow released.
fn run_blocking(action: Blocking) {
    match action {
        Blocking::Drag(path) => {
            let fan = with(|s| {
                s.dragging = true;
                s.fan
            });
            if let Some(fan) = fan {
                shell::start_drag(fan, &path);
            }
            with(|s| {
                s.dragging = false;
                if !s.pointer_over() {
                    s.close();
                }
            });
        }
        Blocking::Menu { hwnd, item } => {
            let menu = with(|s| {
                s.menu_open = true;
                if item { menu::item_menu() } else { s.stack_menu() }
            });
            if let Some(menu) = menu {
                use tray_icon::menu::ContextMenu;
                unsafe {
                    let _ = SetForegroundWindow(hwnd); // so the menu closes when clicking elsewhere
                    menu.show_context_menu_for_hwnd(hwnd.0 as isize, None);
                }
            }
            with(|s| {
                s.menu_open = false;
                // the menu may have activated us; restore our z-order
                s.restack();
            });
        }
        Blocking::PickFolder => {
            with(|s| s.menu_open = true);
            let picked = shell::pick_folder();
            with(|s| {
                s.menu_open = false;
                s.restack();
                if let Some(p) = picked {
                    s.add_folder(p);
                }
            });
        }
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let x = (lparam.0 & 0xffff) as i16 as i32;
    let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
    let is_tile = hwnd.0 as isize == TILE_HWND.load(Ordering::Relaxed);
    if hwnd.0 as isize == BADGE_HWND.load(Ordering::Relaxed) {
        // the badge is only a picture: clicks go through to whatever is beneath
        return match msg {
            WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        };
    }
    if msg != WM_MOUSEMOVE && msg != WM_TIMER && msg < WM_APP {
        log(format!("msg {msg:#06x} tile={is_tile}"));
    }
    let blocking = match msg {
        WM_MOUSEMOVE | WM_MOUSELEAVE | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONUP => {
            with(|s| if is_tile { s.on_tile_mouse(msg) } else { s.on_fan_mouse(msg, wparam, x, y) }).flatten()
        }
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
            with(|s| s.on_wheel(msg, wparam));
            None
        }
        WM_TIMER => {
            with(|s| s.on_timer(wparam.0));
            None
        }
        WM_APP_THUMBS => {
            with(|s| s.on_thumbs());
            None
        }
        WM_APP_MENU => with(|s| s.on_menu()).flatten(),
        WM_APP_OUTSIDE => {
            with(|s| s.on_outside_click(POINT { x, y }));
            None
        }
        WM_APP_FOLDER => {
            with(|s| s.on_folder_events());
            None
        }
        WM_DISPLAYCHANGE | WM_SETTINGCHANGE | WM_DPICHANGED if is_tile => {
            with(|s| s.relayout_if_needed());
            None
        }
        _ => return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    };
    if let Some(action) = blocking {
        run_blocking(action);
    }
    LRESULT(0)
}
