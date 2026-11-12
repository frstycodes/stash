//! The Settings page in the downloads window: titled sections of grouped rows on
//! squircle cards. The page is one painted surface (repainted only when something
//! changes); the segmented controls' selection squircles and the switches' knobs are
//! their own visuals, so they glide on the compositor.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use windows::Foundation::TimeSpan;
use windows::UI::Composition::{CompositionStretch, ContainerVisual, CubicBezierEasingFunction, SpriteVisual};
use windows::core::{HSTRING, Result};
use windows_numerics::{Vector2, Vector3};

use crate::gfx::Gfx;
use crate::icons;
use crate::paint::{self, MEDIUM, P, REGULAR, SEMIBOLD};
use crate::settings::{Corner, Display, IconSize, Settings, Sort, Theme};
use crate::shell;

const PAD: f32 = 24.0;
const ROW_H: f32 = 64.0;
const CARD_R: f32 = 30.0;
const SEG_H: f32 = crate::ui::SEG_H;
// Concentric radii: a nested shape's radius is its container's minus the padding between.
const SEG_PAD: f32 = crate::ui::SEG_PAD;
const SEG_R: f32 = crate::ui::SEG_R;
#[allow(dead_code)]
const SEG_IN_R: f32 = crate::ui::SEG_IN_R;
const SW_W: f32 = 40.0;
const SW_H: f32 = 22.0;
const SW_PAD: f32 = 2.0;
const SW_R: f32 = SW_H / 2.0; // capsule
const KNOB_R: f32 = SW_R - SW_PAD;

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Rect { x, y, w, h }
    }
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
}

/// What a click on the page asks for. The window turns these into commands for the
/// stack (which owns the settings) or into folder pickers.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// A settings command, as the stack's menus use ("dls:max:5", "corner:bottom-left").
    Command(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Hit {
    None,
    Seg(&'static str, usize),
    StepDown(&'static str),
    StepUp(&'static str),
    Switch(&'static str),
    Button(String),
}

struct Seg {
    id: &'static str,
    opts: Vec<String>,
    sel: Option<usize>,
    /// The Custom stepper is showing: its value and unit.
    custom: Option<(u64, &'static str)>,
    rect: Rect,
    parts: Vec<Rect>,
    minus: Rect,
    plus: Rect,
}

struct Switch {
    id: &'static str,
    on: bool,
    enabled: bool,
    rect: Rect,
}

struct Button {
    id: String,
    label: String,
    icon: Option<&'static str>,
    primary: bool,
    rect: Rect,
}

enum Op {
    Section(&'static str, f32),
    Card(f32, f32),
    Line(f32),
    Text { title: String, desc: Option<String>, y: f32 },
    Folder { name: String, path: String, downloads: bool, y: f32 },
    Foot(String, f32),
    About { y: f32, status: String },
    Empty(String),
}

/// The page's live values, read from the settings and the system.
pub struct Values {
    pub settings: Settings,
    pub downloads: PathBuf,
    pub login: bool,
    pub magnets: bool,
    pub packaged: bool,
    pub update: crate::update::Status,
    pub install: crate::update::Install,
}

/// One row's control, placed at the right end of the row.
enum Ctl {
    None,
    Seg { id: &'static str, opts: Vec<String>, sel: Option<usize>, custom: Option<(u64, &'static str)> },
    Switch { id: &'static str, on: bool, enabled: bool },
    /// Buttons from the right: id, label, icon, width, primary.
    Buttons(Vec<(String, String, Option<&'static str>, f32, bool)>),
}

enum Kind {
    Plain,
    Folder { name: String, path: String, downloads: bool },
    AddFolder,
    About { status: String },
}

struct Row {
    title: String,
    desc: Option<String>,
    /// Extra words it should be found by ("startup" for "Start when you sign in").
    keys: &'static str,
    kind: Kind,
    ctl: Ctl,
}

impl Row {
    fn new(title: &str, desc: Option<&str>, keys: &'static str, ctl: Ctl) -> Row {
        Row { title: title.into(), desc: desc.map(String::from), keys, kind: Kind::Plain, ctl }
    }
    fn height(&self) -> f32 {
        match self.kind {
            Kind::AddFolder => 56.0,
            Kind::About { .. } => 76.0,
            _ => ROW_H,
        }
    }
}

struct Section {
    title: &'static str,
    keys: &'static str,
    rows: Vec<Row>,
    foot: Option<String>,
}

pub struct SettingsPage {
    gfx: Gfx,
    pub root: ContainerVisual,
    inner: ContainerVisual,
    surface: SpriteVisual,
    sliders: HashMap<&'static str, SpriteVisual>,
    knobs: HashMap<&'static str, (SpriteVisual, SpriteVisual)>, // (orange track, knob)
    ops: Vec<Op>,
    segs: Vec<Seg>,
    switches: Vec<Switch>,
    buttons: Vec<Button>,
    pub height: f32,
    pub scroll: f32,
    hover: Hit,
    /// Segments switched to Custom by the user (shown even when the value is a preset).
    custom_open: HashSet<&'static str>,
    /// What's typed in the search box: only matching settings show.
    pub query: String,
    k: f32,
    w: f32,
    view_h: f32,
    /// Where the page starts in `root`: under the header (the content scrolls on up behind it).
    pub top: f32,
}

/// A switch's live parts (the orange "on" track and the knob) in `parent`; the off track
/// is painted under them. Every switch in the app is made here, so they all look and move
/// the same.
pub fn new_switch(gfx: &Gfx, parent: &windows::UI::Composition::VisualCollection, k: f32) -> Result<(SpriteVisual, SpriteVisual)> {
    let c = &gfx.compositor;
    let track = c.CreateSpriteVisual()?;
    let knob = c.CreateSpriteVisual()?;
    parent.InsertAtTop(&track)?;
    parent.InsertAtTop(&knob)?;
    let shadow = c.CreateDropShadow()?;
    shadow.SetBlurRadius(4.0 * k)?;
    shadow.SetOffset(Vector3 { X: 0.0, Y: 1.0 * k, Z: 0.0 })?;
    shadow.SetOpacity(0.35)?;
    shadow.SetSourcePolicy(windows::UI::Composition::CompositionDropShadowSourcePolicy::InheritFromVisualContent)?;
    knob.SetShadow(&shadow)?;
    Ok((track, knob))
}

/// The off track a switch's parts sit on (paint it under them).
pub fn paint_switch_track(p: &P, r: Rect, enabled: bool) {
    p.capsule(r.x, r.y, r.w, r.h, paint::ink(if enabled { 0.16 } else { 0.08 }));
}

/// A switch's size.
pub const SWITCH: (f32, f32) = (SW_W, SW_H);

/// Put a switch at `r` (DIPs, in its parent) showing `on`, sliding there if `animate`.
#[allow(clippy::too_many_arguments)]
pub fn place_switch(gfx: &Gfx, track: &SpriteVisual, knob: &SpriteVisual, r: Rect, on: bool, enabled: bool, k: f32, animate: bool) -> Result<()> {
    let c = &gfx.compositor;
    // switches are true capsules (circular ends): track radius 11, knob 11 - 2 = 9
    track.SetBrush(&gfx.pill_brush(paint::accent(), r.h * k)?)?;
    track.SetSize(Vector2 { X: r.w * k, Y: r.h * k })?;
    track.SetOffset(Vector3 { X: r.x * k, Y: r.y * k, Z: 0.0 })?;
    let to = if on { 1.0 } else { 0.0 };
    if animate {
        let a = c.CreateScalarKeyFrameAnimation()?;
        a.InsertExpressionKeyFrame(0.0, &HSTRING::from("this.StartingValue"))?;
        a.InsertKeyFrame(1.0, to)?;
        a.SetDuration(ms(200))?;
        track.StartAnimation(&HSTRING::from("Opacity"), &a)?;
    } else {
        track.StopAnimation(&HSTRING::from("Opacity"))?;
        track.SetOpacity(to)?;
    }
    // the knob: a white capsule, wider than tall, as on current iOS
    let (kw, kh) = (24.0, SW_H - SW_PAD * 2.0);
    knob.SetBrush(&gfx.pill_brush(paint::color(255, 255, 255, if enabled { 1.0 } else { 0.5 }), KNOB_R * 2.0 * k)?)?;
    knob.SetSize(Vector2 { X: kw * k, Y: kh * k })?;
    knob.SetCenterPoint(Vector3 { X: kw * k / 2.0, Y: kh * k / 2.0, Z: 0.0 })?;
    let x = if on { r.x + r.w - kw - SW_PAD } else { r.x + SW_PAD };
    let to = Vector3 { X: x * k, Y: (r.y + SW_PAD) * k, Z: 0.0 };
    if !animate {
        knob.StopAnimation(&HSTRING::from("Offset"))?;
        return knob.SetOffset(to);
    }
    // the same sprung glide as the segmented controls
    let a = c.CreateVector3KeyFrameAnimation()?;
    a.InsertExpressionKeyFrame(0.0, &HSTRING::from("this.StartingValue"))?;
    a.InsertKeyFrameWithEasingFunction(1.0, to, &c.CreateCubicBezierEasingFunction(Vector2 { X: 0.3, Y: 1.18 }, Vector2 { X: 0.6, Y: 1.0 })?)?;
    a.SetDuration(ms(420))?;
    knob.StartAnimation(&HSTRING::from("Offset"), &a)
}

/// Settings > Torrents: the seed ratio and seeding time presets (0 = never stop).
const TORRENT_RATIOS: [f32; 4] = [0.0, 1.0, 2.0, 5.0];
const TORRENT_MINUTES: [u64; 4] = [0, 30, 120, 1440];

fn ms(v: i64) -> TimeSpan {
    TimeSpan { Duration: v * 10_000 }
}

mod engine_rows;

impl SettingsPage {
    pub fn new(gfx: &Gfx) -> Result<Self> {
        let c = &gfx.compositor;
        let root = c.CreateContainerVisual()?;
        let inner = c.CreateContainerVisual()?;
        let surface = c.CreateSpriteVisual()?;
        inner.Children()?.InsertAtTop(&surface)?;
        root.Children()?.InsertAtTop(&inner)?;
        Ok(SettingsPage {
            gfx: gfx.clone(),
            root,
            inner,
            surface,
            sliders: HashMap::new(),
            knobs: HashMap::new(),
            ops: Vec::new(),
            segs: Vec::new(),
            switches: Vec::new(),
            buttons: Vec::new(),
            height: 0.0,
            scroll: 0.0,
            hover: Hit::None,
            custom_open: HashSet::new(),
            query: String::new(),
            k: 1.0,
            w: 0.0,
            view_h: 0.0,
            top: 0.0,
        })
    }

    fn back(&self) -> Result<CubicBezierEasingFunction> {
        self.gfx.compositor.CreateCubicBezierEasingFunction(Vector2 { X: 0.3, Y: 1.18 }, Vector2 { X: 0.6, Y: 1.0 })
    }

    // ---------- layout ----------

    /// Lay the page out for `w` logical pixels of width, then paint it.
    pub fn build(&mut self, v: &Values, k: f32, w: f32, view_h: f32, animate: bool) -> Result<()> {
        self.k = k;
        self.w = w;
        self.view_h = view_h;
        self.layout(v);
        self.paint()?;
        self.place_controls(animate)?;
        self.clamp_scroll(false);
        Ok(())
    }

    fn seg(&mut self, id: &'static str, opts: Vec<String>, sel: Option<usize>, custom: Option<(u64, &'static str)>, right: f32, cy: f32) {
        let measure = |s: &str| -> f32 {
            // DM Sans at 13px: about 7px per character, plus padding
            let dw = self.gfx.dwrite.clone();
            paint::advance(&dw, s, 13.0 * self.k) / self.k
        };
        let stepper_w = custom.map(|(v, unit)| 24.0 + measure(&v.to_string()) + 6.0 + measure(unit) + 30.0);
        let mut widths: Vec<f32> = opts.iter().map(|o| (measure(o) + 22.0).max(38.0)).collect();
        if let (Some(sw), Some(last)) = (stepper_w, widths.last_mut()) {
            *last = sw.max(*last);
        }
        let total: f32 = widths.iter().sum::<f32>() + 6.0 + 2.0 * (widths.len() as f32 - 1.0);
        let rect = Rect::new(right - total, cy - SEG_H / 2.0, total, SEG_H);
        let mut x = rect.x + SEG_PAD;
        let mut parts = Vec::new();
        for w in &widths {
            parts.push(Rect::new(x, rect.y + SEG_PAD, *w, SEG_H - SEG_PAD * 2.0));
            x += w + 2.0;
        }
        let last = *parts.last().unwrap();
        let minus = Rect::new(last.x + 2.0, last.y + 2.0, 24.0, last.h - 4.0);
        let plus = Rect::new(last.x + last.w - 26.0, last.y + 2.0, 24.0, last.h - 4.0);
        self.segs.push(Seg { id, opts, sel, custom, rect, parts, minus, plus });
    }

    /// Every setting, as sections of rows (also what search looks through).
    fn model(&self, v: &Values) -> Vec<Section> {
        let s = &v.settings;
        let d = &s.downloads;
        let pick = |presets: &[u64], value: u64, id: &'static str| -> (Option<usize>, bool) {
            match presets.iter().position(|p| *p == value) {
                Some(i) if !self.custom_open.contains(id) => (Some(i), false),
                _ => (Some(presets.len()), true),
            }
        };
        let with_custom = |presets: &[u64], label: &dyn Fn(u64) -> String| -> Vec<String> {
            let mut o: Vec<String> = presets.iter().map(|p| label(*p)).collect();
            o.push("Custom".into());
            o
        };
        let mut sections = Vec::new();

        // general: how Stash starts, opens and closes
        sections.push(Section {
            title: "General",
            keys: "general app behaviour behavior",
            rows: vec![
                Row::new("Clicking the tray icon", Some("Fans out the stack, or opens this window."), "tray icon click open notification area taskbar", Ctl::Seg {
                    id: "tray",
                    opts: vec!["Shows the stack".into(), "Opens Stash".into()],
                    sel: Some(usize::from(s.tray_opens_window)),
                    custom: None,
                }),
                Row::new("When Stash starts", Some("Waits quietly in the tray, or opens this window."), "start startup launch open minimized tray background window", Ctl::Seg {
                    id: "start",
                    opts: vec!["Stay in the tray".into(), "Open the window".into()],
                    sel: Some(usize::from(s.start_window)),
                    custom: None,
                }),
                Row::new("Closing the window", Some("Stash keeps downloading from the tray, or quits."), "close exit quit tray minimize background x", Ctl::Seg {
                    id: "close",
                    opts: vec!["Keep running".into(), "Quit Stash".into()],
                    sel: Some(usize::from(s.close_quits)),
                    custom: None,
                }),
                Row::new("Show the stack on the desktop", Some("The pile of recent files in the corner of your screen."), "stack desktop corner pile hide show tile", Ctl::Switch { id: "show-stack", on: s.show_stack, enabled: true }),
                Row::new("Show new downloads arriving", Some("The stack comes up over your windows and catches the file."), "animation arrive drop new download stack popup raise", Ctl::Switch { id: "arrivals", on: s.show_arrivals, enabled: s.show_stack }),
                Row::new("Start when you sign in", None, "startup login boot launch autostart windows", Ctl::Switch { id: "login", on: v.login, enabled: true }),
            ],
            foot: None,
        });

        // appearance
        let theme = [Theme::System, Theme::Light, Theme::Dark].iter().position(|t| *t == s.theme);
        sections.push(Section {
            title: "Appearance",
            keys: "appearance look",
            rows: vec![Row::new(
                "Theme",
                Some("System follows your Windows light or dark mode."),
                "theme light dark mode appearance color colour night",
                Ctl::Seg { id: "theme", opts: vec!["System".into(), "Light".into(), "Dark".into()], sel: theme, custom: None },
            )],
            foot: None,
        });

        // folders in the stack
        let mut rows = vec![Row {
            title: "Downloads".into(),
            desc: Some(v.downloads.display().to_string()),
            keys: "folder location save path move default downloads",
            kind: Kind::Folder { name: "Downloads".into(), path: v.downloads.display().to_string(), downloads: true },
            ctl: Ctl::Buttons(vec![("dls:folder:choose".into(), "Move…".into(), None, 76.0, false)]),
        }];
        for (i, f) in s.folders.iter().enumerate() {
            rows.push(Row {
                title: shell::folder_name(f),
                desc: Some(f.display().to_string()),
                keys: "folder stack watch remove change",
                kind: Kind::Folder { name: shell::folder_name(f), path: f.display().to_string(), downloads: false },
                ctl: Ctl::Buttons(vec![
                    (format!("folder:remove:{i}"), String::new(), Some(icons::CANCEL), 32.0, false),
                    (format!("folder:change:{i}"), "Change…".into(), None, 86.0, false),
                ]),
            });
        }
        rows.push(Row { title: "Add Folder…".into(), desc: None, keys: "add folder new watch screenshots desktop", kind: Kind::AddFolder, ctl: Ctl::None });
        sections.push(Section {
            title: "Folders in the stack",
            keys: "folders",
            rows,
            foot: Some("Downloads is also where Stash saves what it downloads. You can move it, but it can't be removed.".into()),
        });

        // downloads
        let (unit, per_unit) = paint::limit_unit();
        let limit_units = ((d.limit_kbps as f64 * 1024.0) / per_unit).round() as u64;
        let (sel, custom) = pick(&[1, 2, 3, 5], d.max_active as u64, "max");
        let max = Ctl::Seg { id: "max", opts: with_custom(&[1, 2, 3, 5], &|p| p.to_string()), sel, custom: custom.then_some((d.max_active as u64, "")) };
        let (sel, custom) = pick(&[1, 4, 8, 16], d.connections as u64, "conn");
        let conn = Ctl::Seg { id: "conn", opts: with_custom(&[1, 4, 8, 16], &|p| p.to_string()), sel, custom: custom.then_some((d.connections as u64, "")) };
        let (sel, custom) = pick(&[0, 1, 5], limit_units, "limit");
        let limit = Ctl::Seg {
            id: "limit",
            opts: with_custom(&[0, 1, 5], &|p| if p == 0 { "None".to_string() } else { format!("{p} {unit}") }),
            sel,
            custom: custom.then_some((limit_units.max(1), unit)),
        };
        sections.push(Section {
            title: "Downloads",
            keys: "",
            rows: vec![
                Row::new("Downloads at once", Some("The rest wait in line, highest priority first."), "simultaneous parallel concurrent queue max", max),
                Row::new("Connections per download", Some("More connections are faster on servers that allow it."), "threads segments parts speed", conn),
                Row::new("Speed limit", Some("Leaves room for everything else on your network."), "bandwidth throttle cap rate slow", limit),
                Row::new("Keep seeding finished torrents", Some("Share pieces back with other people while Stash runs."), "torrent upload share bittorrent", Ctl::Switch { id: "seed", on: d.seed, enabled: true }),
                Row::new(
                    "Gaming mode",
                    Some("While you play online, downloads slow down just enough to keep your ping low."),
                    "game gaming ping latency lag fps online play bufferbloat",
                    Ctl::Switch { id: "gaming", on: d.gaming, enabled: true },
                ),
                Row::new(
                    "Open magnet links and .torrent files",
                    Some(if v.packaged { "Set this in Windows Settings, under Default apps." } else { "Stash asks before it starts one." }),
                    "torrent magnet default associate handler",
                    Ctl::Switch { id: "magnets", on: v.magnets, enabled: !v.packaged },
                ),
            ],
            foot: None,
        });

        sections.push(self.engine_section(v));
        // torrents: their upload, when seeding stops, and how they reach peers
        let t = &d.torrents;
        let up_units = ((t.upload_kbps as f64 * 1024.0) / per_unit).round() as u64;
        let (sel, custom) = pick(&[0, 1, 5], up_units, "t-up");
        let up = Ctl::Seg {
            id: "t-up",
            opts: with_custom(&[0, 1, 5], &|p| if p == 0 { "None".to_string() } else { format!("{p} {unit}") }),
            sel,
            custom: custom.then_some((up_units.max(1), unit)),
        };
        let ratio = TORRENT_RATIOS.iter().position(|r| (*r - t.seed_ratio).abs() < 0.01);
        let minutes = TORRENT_MINUTES.iter().position(|m| *m == t.seed_minutes);
        sections.push(Section {
            title: "Torrents",
            keys: "torrent bittorrent magnet",
            rows: vec![
                Row::new("Upload limit", Some("Separate from the speed limit."), "upload seed bandwidth throttle cap rate", up),
                Row::new("Stop seeding at ratio", Some("Shared back this many times over."), "seed ratio share upload stop", Ctl::Seg {
                    id: "t-ratio",
                    opts: vec!["Never".into(), "1.0".into(), "2.0".into(), "5.0".into()],
                    sel: ratio,
                    custom: None,
                }),
                Row::new("Stop seeding after", Some("However much has been shared by then."), "seed time minutes hours stop", Ctl::Seg {
                    id: "t-minutes",
                    opts: vec!["Never".into(), "30 min".into(), "2 hours".into(), "1 day".into()],
                    sel: minutes,
                    custom: None,
                }),
                Row::new("Choose files before downloading", Some("When a torrent has more than one file."), "select files pick only torrent", Ctl::Switch { id: "t-choose", on: t.choose_files, enabled: true }),
                Row::new("Forward the port on your router", Some("UPnP lets more peers reach you. Applies the next time torrents start."), "upnp port forwarding router nat firewall", Ctl::Switch { id: "t-upnp", on: t.upnp, enabled: true }),
            ],
            foot: None,
        });

        // units
        sections.push(Section {
            title: "Units",
            keys: "units",
            rows: vec![
                Row::new(
                    "Show speeds in",
                    Some("Bytes, like your file sizes, or bits, like your internet plan."),
                    "speed bits bytes mbps mb/s",
                    Ctl::Seg { id: "bits", opts: vec!["MB/s".into(), "Mbps".into()], sel: Some(usize::from(d.bits)), custom: None },
                ),
                Row::new(
                    "Size units",
                    Some("Binary (1024) matches the sizes File Explorer shows."),
                    "size binary mib megabytes 1024 1000",
                    Ctl::Seg { id: "binary", opts: vec!["MB · 1000".into(), "MiB · 1024".into()], sel: Some(usize::from(d.binary)), custom: None },
                ),
            ],
            foot: None,
        });

        // browsers
        sections.push(Section {
            title: "Browsers",
            keys: "",
            rows: vec![Row::new(
                "Browser extension",
                Some("Chrome, Edge, Brave and Firefox hand their downloads to Stash."),
                "browser extension chrome edge brave firefox addon plugin",
                Ctl::Buttons(vec![("dls:browser".into(), "Set up…".into(), None, 82.0, false)]),
            )],
            foot: None,
        });

        // stack
        let sort = [Sort::Added, Sort::Modified, Sort::Name, Sort::Kind].iter().position(|z| *z == s.sort);
        let counts = [5usize, 8, 10, 12];
        let size = [IconSize::Small, IconSize::Medium, IconSize::Large, IconSize::ExtraLarge].iter().position(|z| *z == s.icon_size);
        sections.push(Section {
            title: "Stack",
            keys: "stack desktop",
            rows: vec![
                Row::new("Show as", Some("A pile of the newest files, or the folder's own icon."), "display pile folder", Ctl::Seg {
                    id: "display",
                    opts: vec!["Stack".into(), "Folder".into()],
                    sel: Some(usize::from(s.display == Display::Folder)),
                    custom: None,
                }),
                Row::new("Sort by", Some("Which files are on top."), "order date name kind", Ctl::Seg {
                    id: "sort",
                    opts: vec!["Added".into(), "Modified".into(), "Name".into(), "Kind".into()],
                    sel: sort,
                    custom: None,
                }),
                Row::new("Show most recent", Some("How many files fan out."), "count number items limit", Ctl::Seg {
                    id: "count",
                    opts: counts.iter().map(|n| n.to_string()).collect(),
                    sel: counts.iter().position(|n| *n == s.limit),
                    custom: None,
                }),
                Row::new("Corner", Some("Where the stack sits on your desktop."), "position left right place", Ctl::Seg {
                    id: "corner",
                    opts: vec!["Bottom left".into(), "Bottom right".into()],
                    sel: Some(usize::from(s.corner == Corner::BottomRight)),
                    custom: None,
                }),
                Row::new("Icon size", None, "big small large scale", Ctl::Seg { id: "size", opts: vec!["S".into(), "M".into(), "L".into(), "XL".into()], sel: size, custom: None }),
            ],
            foot: None,
        });

        // about and updates
        use crate::update::{Install, Status};
        let status = match (&v.install, &v.update) {
            (Install::Store, _) => "The Microsoft Store keeps Stash up to date.".to_string(),
            (Install::Dev, _) => "A development build: it doesn't update itself.".to_string(),
            (_, Status::Idle) => "Stash checks for updates once a day.".to_string(),
            (_, Status::Checking) => "Checking for updates…".to_string(),
            (_, Status::UpToDate) => "Stash is up to date.".to_string(),
            (_, Status::Downloading(ver)) => format!("Downloading version {ver}…"),
            (_, Status::Ready { version, .. }) => format!("Version {version} is ready to install."),
            (_, Status::Failed(e)) => format!("Couldn't check for updates: {e}"),
        };
        let button = match (&v.install, &v.update) {
            (Install::Store, _) => ("update:store", "Open Microsoft Store", false, 168.0),
            (_, Status::Ready { .. }) => ("update:apply", "Restart to Update", true, 150.0),
            _ => ("update:check", "Check for Updates", false, 150.0),
        };
        let mut rows = vec![Row {
            title: "Stash".into(),
            desc: None,
            keys: "about version update upgrade new release check",
            kind: Kind::About { status },
            ctl: Ctl::Buttons(vec![(button.0.into(), button.1.into(), None, button.3, button.2)]),
        }];
        if matches!(v.install, Install::Installer | Install::Portable) {
            rows.push(Row::new(
                "Download updates automatically",
                Some("New versions download in the background; you choose when to restart."),
                "update auto automatic background upgrade",
                Ctl::Switch { id: "autoupdate", on: s.auto_update, enabled: true },
            ));
        }
        sections.push(Section { title: "About", keys: "about", rows, foot: None });
        // development builds and test copies (STASH_DATA) only
        if cfg!(debug_assertions) || std::env::var_os("STASH_DATA").is_some() {
            let play = |id: &str, label: &str, w: f32, primary: bool| (id.to_string(), label.to_string(), None, w, primary);
            sections.push(Section {
                title: "Debug",
                keys: "debug test developer animation",
                rows: vec![
                    Row::new(
                        "New download animation",
                        Some("Plays a download arriving in the stack. Behind a window, the stack comes up first."),
                        "debug animation drop stack arrive",
                        Ctl::Buttons(vec![play("debug:drop:slow", "Slow", 64.0, false), play("debug:drop", "Play", 64.0, true)]),
                    ),
                    Row::new(
                        "From behind this window",
                        Some("Waits a second, so you can put another window over the stack first."),
                        "debug animation delay covered",
                        Ctl::Buttons(vec![play("debug:drop:later", "Play in 1 s", 104.0, false)]),
                    ),
                ],
                foot: None,
            });
        }
        sections
    }

    /// Does this row match what's typed? Every word must match its title, description,
    /// search words or section (fuzzily, like download search).
    fn matches(&self, sec: &Section, row: &Row) -> bool {
        let words: Vec<String> = self.query.split_whitespace().map(|w| w.to_lowercase()).collect();
        if words.is_empty() {
            return true;
        }
        let hay = format!("{} {} {} {} {}", row.title, row.desc.as_deref().unwrap_or(""), row.keys, sec.title, sec.keys);
        words.iter().all(|w| crate::search::match_word(w, &hay).is_some())
    }

    fn layout(&mut self, v: &Values) {
        self.ops.clear();
        self.segs.clear();
        self.switches.clear();
        self.buttons.clear();
        let right = self.w - PAD - 20.0;
        let mut y = 8.0;
        let searching = !self.query.trim().is_empty();
        let mut any = false;
        for sec in self.model(v) {
            let rows: Vec<&Row> = sec.rows.iter().filter(|r| self.matches(&sec, r)).collect();
            if rows.is_empty() {
                continue;
            }
            any = true;
            self.ops.push(Op::Section(sec.title, y + 22.0));
            y += 36.0;
            let card_h: f32 = rows.iter().map(|r| r.height()).sum();
            self.ops.push(Op::Card(y, card_h));
            let mut ry = y;
            for (n, row) in rows.iter().enumerate() {
                if n > 0 {
                    self.ops.push(Op::Line(ry));
                }
                let h = row.height();
                let cy = ry + h / 2.0;
                match &row.kind {
                    Kind::Plain => self.ops.push(Op::Text { title: row.title.clone(), desc: row.desc.clone(), y: ry }),
                    Kind::Folder { name, path, downloads } => {
                        self.ops.push(Op::Folder { name: name.clone(), path: path.clone(), downloads: *downloads, y: ry });
                    }
                    Kind::AddFolder => {
                        self.buttons.push(Button { id: "add-folder".into(), label: "Add Folder…".into(), icon: Some(icons::ADD), primary: false, rect: Rect::new(PAD + 16.0, ry + 12.0, 128.0, crate::ui::BUTTON_H_SMALL) });
                        self.ops.push(Op::Text { title: String::new(), desc: Some("Scroll over the stack to switch between folders.".into()), y: ry + 16.0 });
                    }
                    Kind::About { status } => self.ops.push(Op::About { y: ry, status: status.clone() }),
                }
                match &row.ctl {
                    Ctl::None => {}
                    Ctl::Seg { id, opts, sel, custom } => self.seg(id, opts.clone(), *sel, *custom, right, cy),
                    Ctl::Switch { id, on, enabled } => {
                        self.switches.push(Switch { id, on: *on, enabled: *enabled, rect: Rect::new(right - SW_W, cy - SW_H / 2.0, SW_W, SW_H) });
                    }
                    Ctl::Buttons(list) => {
                        let mut x = right;
                        let dw = self.gfx.dwrite.clone();
                        let k = self.k;
                        for (id, label, icon, _, primary) in list {
                            // the app's small button: as wide as its label, a circle if it's only an icon
                            let bh = crate::ui::BUTTON_H_SMALL;
                            let w = if label.is_empty() { bh } else { crate::ui::button_w(|t| paint::advance(&dw, t, 13.5 * k) / k, label, icon.is_some()) };
                            x -= w;
                            self.buttons.push(Button { id: id.clone(), label: label.clone(), icon: *icon, primary: *primary, rect: Rect::new(x, cy - bh / 2.0, w, bh) });
                            x -= 8.0;
                        }
                    }
                }
                ry += h;
            }
            y += card_h + 8.0;
            if let (Some(foot), false) = (&sec.foot, searching) {
                self.ops.push(Op::Foot(foot.clone(), y + 10.0));
                y += 28.0;
            }
        }
        if !any {
            self.ops.push(Op::Empty(self.query.trim().to_string()));
            y = 300.0;
        }
        self.height = y + 20.0;
    }

    // ---------- painting ----------

    fn paint(&self) -> Result<()> {
        let k = self.k;
        let (w, h) = (self.w, self.height);
        let hover = self.hover.clone();
        let dw = self.gfx.dwrite.clone();
        let surface = self.gfx.surface(w * k, h * k, |dc| {
            let p = P { dc, dw: &dw, k };
            for op in &self.ops {
                match op {
                    Op::Section(t, y) => {
                        p.text_mid(t, PAD + 6.0, *y, 13.0, SEMIBOLD, paint::dim(), 300.0);
                    }
                    Op::Card(y, ch) => p.round(PAD, *y, w - PAD * 2.0, *ch, CARD_R, paint::ink(0.045)),
                    Op::Line(y) => p.fill(PAD + 20.0, *y, w - PAD * 2.0 - 40.0, 1.0, paint::ink(0.07)),
                    Op::Text { title, desc, y } => {
                        let max = (w - PAD * 2.0) * 0.5;
                        match (title.is_empty(), desc) {
                            (true, Some(d)) => {
                                p.text_mid(d, PAD + 160.0, *y + 18.0, 12.5, REGULAR, paint::faint(), w - PAD * 2.0 - 180.0);
                            }
                            (false, Some(d)) => {
                                p.text(title, PAD + 20.0, *y + 13.0, 14.0, MEDIUM, paint::text(), max);
                                p.text(d, PAD + 20.0, *y + 34.0, 12.5, REGULAR, paint::dim(), max);
                            }
                            (false, None) => {
                                p.text_mid(title, PAD + 20.0, *y + ROW_H / 2.0, 14.0, MEDIUM, paint::text(), max);
                            }
                            _ => {}
                        }
                    }
                    Op::Folder { name, path, downloads, y } => {
                        let cy = *y + ROW_H / 2.0;
                        p.round(PAD + 16.0, cy - 18.0, 36.0, 36.0, 12.0, paint::ink(0.06));
                        p.glyph(if *downloads { icons::DOWNLOAD } else { icons::FOLDER }, PAD + 34.0, cy, 14.0, paint::text());
                        let nw = p.text(name, PAD + 64.0, *y + 13.0, 14.0, MEDIUM, paint::text(), 220.0);
                        if *downloads {
                            let label = "Saves downloads";
                            let bw = p.measure(label, 11.5, SEMIBOLD) + 14.0;
                            p.round(PAD + 64.0 + nw + 8.0, *y + 14.0, bw, 18.0, 6.0, paint::rgb(paint::ORANGE, 0.16));
                            p.text_mid(label, PAD + 64.0 + nw + 15.0, *y + 23.0, 11.5, SEMIBOLD, paint::accent_text(), bw);
                        }
                        p.text(path, PAD + 64.0, *y + 34.0, 12.5, REGULAR, paint::dim(), w - PAD * 2.0 - 64.0 - 160.0);
                    }
                    Op::Foot(t, y) => {
                        p.text(t, PAD + 6.0, *y, 12.5, REGULAR, paint::faint(), w - PAD * 2.0);
                    }
                    Op::About { y, status } => {
                        p.logo(PAD + 38.0, *y + 38.0, 36.0);
                        p.text(&format!("Stash {}", env!("CARGO_PKG_VERSION")), PAD + 68.0, *y + 18.0, 14.5, SEMIBOLD, paint::text(), 260.0);
                        p.text(status, PAD + 68.0, *y + 40.0, 12.5, REGULAR, paint::dim(), w - PAD * 2.0 - 68.0 - 190.0);
                    }
                    Op::Empty(q) => {
                        let title = format!("No settings match “{q}”");
                        let tw = p.measure(&title, 16.0, SEMIBOLD).min(w - 48.0);
                        p.text(&title, (w - tw) / 2.0, 110.0, 16.0, SEMIBOLD, paint::text(), w - 48.0);
                        let sub = "Try another word, like speed, folder or startup.";
                        let sw = p.measure(sub, 13.0, REGULAR).min(w - 48.0);
                        p.text(sub, (w - sw) / 2.0, 138.0, 13.0, REGULAR, paint::dim(), w - 48.0);
                    }
                }
            }
            // segmented controls: the track and the labels (the selection squircle is a sprite)
            for s in &self.segs {
                p.round(s.rect.x, s.rect.y, s.rect.w, s.rect.h, SEG_R, paint::well(0.25));
                for (i, (r, label)) in s.parts.iter().zip(&s.opts).enumerate() {
                    let on = s.sel == Some(i);
                    let hot = hover == Hit::Seg(s.id, i);
                    if i == s.opts.len() - 1 {
                        if let Some((v, unit)) = s.custom {
                            // − value unit +
                            let c = paint::text();
                            for (rr, glyph, h) in [(s.minus, "−", Hit::StepDown(s.id)), (s.plus, "+", Hit::StepUp(s.id))] {
                                if hover == h {
                                    p.round(rr.x, rr.y, rr.w, rr.h, 8.0, paint::ink(0.10));
                                }
                                let gw = p.measure(glyph, 14.0, MEDIUM);
                                p.text_mid(glyph, rr.x + (rr.w - gw) / 2.0, rr.y + rr.h / 2.0, 14.0, MEDIUM, c, rr.w);
                            }
                            let text = if unit.is_empty() { v.to_string() } else { format!("{v} {unit}") };
                            let tw = p.measure(&text, 13.0, SEMIBOLD);
                            let mid = (s.minus.x + s.minus.w + s.plus.x) / 2.0;
                            p.text_mid(&text, mid - tw / 2.0, r.y + r.h / 2.0, 13.0, SEMIBOLD, c, tw + 2.0);
                            continue;
                        }
                    }
                    let lw = p.measure(label, 13.0, MEDIUM);
                    let ink = if on || hot { paint::text() } else { paint::dim() };
                    p.text_mid(label, r.x + (r.w - lw) / 2.0, r.y + r.h / 2.0, 13.0, MEDIUM, ink, r.w);
                }
            }
            // switches: the off track (the on track and the knob are sprites)
            for sw in &self.switches {
                paint_switch_track(&p, sw.rect, sw.enabled);
            }
            // buttons: the app's buttons (Primary or Secondary)
            for b in &self.buttons {
                let hot = hover == Hit::Button(b.id.clone());
                let kind = if b.primary { crate::ui::Kind::Primary } else { crate::ui::Kind::Secondary };
                crate::ui::button(&p, b.rect, &b.label, b.icon, kind, hot, true);
            }
        })?;
        let brush = self.gfx.compositor.CreateSurfaceBrushWithSurface(&surface)?;
        brush.SetStretch(CompositionStretch::Fill)?;
        self.surface.SetBrush(&brush)?;
        self.surface.SetSize(Vector2 { X: (w * k).ceil(), Y: (h * k).ceil() })
    }

    /// Selection squircles and switch knobs: sprites that glide to their spots.
    fn place_controls(&mut self, animate: bool) -> Result<()> {
        let k = self.k;
        let c = self.gfx.compositor.clone();
        let back = self.back()?;
        let children = self.inner.Children()?;
        let glide = |v: &SpriteVisual, offset: Vector3, size: Option<Vector2>, animate: bool| -> Result<()> {
            if !animate {
                v.StopAnimation(&HSTRING::from("Offset"))?;
                v.SetOffset(offset)?;
                if let Some(s) = size {
                    v.StopAnimation(&HSTRING::from("Size"))?;
                    v.SetSize(s)?;
                }
                return Ok(());
            }
            let a = c.CreateVector3KeyFrameAnimation()?;
            a.InsertExpressionKeyFrame(0.0, &HSTRING::from("this.StartingValue"))?;
            a.InsertKeyFrameWithEasingFunction(1.0, offset, &back)?;
            a.SetDuration(ms(420))?;
            v.StartAnimation(&HSTRING::from("Offset"), &a)?;
            if let Some(s) = size {
                let b = c.CreateVector2KeyFrameAnimation()?;
                b.InsertExpressionKeyFrame(0.0, &HSTRING::from("this.StartingValue"))?;
                b.InsertKeyFrameWithEasingFunction(1.0, s, &back)?;
                b.SetDuration(ms(420))?;
                v.StartAnimation(&HSTRING::from("Size"), &b)?;
            }
            Ok(())
        };
        for s in &self.segs {
            let new = !self.sliders.contains_key(s.id);
            let slider = match self.sliders.get(s.id) {
                Some(v) => v.clone(),
                None => {
                    let v = c.CreateSpriteVisual()?;
                    children.InsertAbove(&v, &self.surface)?;
                    self.sliders.insert(s.id, v.clone());
                    v
                }
            };
            // the squircle's look: a soft white fill, redrawn at this scale
            let r = s.parts[s.sel.unwrap_or(0)];
            let h = r.h * k;
            slider.SetBrush(&crate::ui::seg_slider_brush(&self.gfx, r.h, k)?)?;
            slider.SetOpacity(if s.sel.is_some() { 1.0 } else { 0.0 })?;
            glide(&slider, Vector3 { X: r.x * k, Y: r.y * k, Z: 0.0 }, Some(Vector2 { X: r.w * k, Y: h }), animate && !new)?;
        }
        for sw in &self.switches {
            let new = !self.knobs.contains_key(sw.id);
            let (track, knob) = match self.knobs.get(sw.id) {
                Some(p) => p.clone(),
                None => {
                    let pair = new_switch(&self.gfx, &children, k)?;
                    self.knobs.insert(sw.id, pair.clone());
                    pair
                }
            };
            place_switch(&self.gfx, &track, &knob, sw.rect, sw.on, sw.enabled, k, animate && !new)?;
        }
        Ok(())
    }

    /// While a switch is held, its knob swells into a lens, as on current iOS.
    pub fn press(&self, id: Option<&'static str>) {
        let c = &self.gfx.compositor;
        for (sid, (_, knob)) in &self.knobs {
            let held = id == Some(*sid);
            let _ = (|| -> Result<()> {
                let a = c.CreateVector3KeyFrameAnimation()?;
                a.InsertExpressionKeyFrame(0.0, &HSTRING::from("this.StartingValue"))?;
                let s = if held { Vector3 { X: 1.35, Y: 1.5, Z: 1.0 } } else { Vector3 { X: 1.0, Y: 1.0, Z: 1.0 } };
                a.InsertKeyFrameWithEasingFunction(1.0, s, &self.back()?)?;
                a.SetDuration(ms(if held { 180 } else { 300 }))?;
                knob.StartAnimation(&HSTRING::from("Scale"), &a)?;
                let o = c.CreateScalarKeyFrameAnimation()?;
                o.InsertExpressionKeyFrame(0.0, &HSTRING::from("this.StartingValue"))?;
                o.InsertKeyFrame(1.0, if held { 0.72 } else { 1.0 })?;
                o.SetDuration(ms(150))?;
                knob.StartAnimation(&HSTRING::from("Opacity"), &o)
            })();
        }
    }

    // ---------- input ----------

    /// (x, y) in page coordinates (already scrolled).
    pub fn hit(&self, x: f32, y: f32) -> Hit {
        for s in &self.segs {
            if !s.rect.contains(x, y) {
                continue;
            }
            if s.custom.is_some() {
                if s.minus.contains(x, y) {
                    return Hit::StepDown(s.id);
                }
                if s.plus.contains(x, y) {
                    return Hit::StepUp(s.id);
                }
            }
            if let Some(i) = s.parts.iter().position(|r| r.contains(x, y)) {
                return Hit::Seg(s.id, i);
            }
        }
        for sw in &self.switches {
            if sw.enabled && Rect::new(sw.rect.x - 6.0, sw.rect.y - 6.0, sw.rect.w + 12.0, sw.rect.h + 12.0).contains(x, y) {
                return Hit::Switch(sw.id);
            }
        }
        for b in &self.buttons {
            if b.rect.contains(x, y) {
                return Hit::Button(b.id.clone());
            }
        }
        Hit::None
    }

    pub fn set_hover(&mut self, h: Hit) -> bool {
        if h == self.hover {
            return false;
        }
        self.hover = h;
        let _ = self.paint();
        true
    }

    /// A click. Returns the settings command to run, if any.
    pub fn click(&mut self, hit: &Hit, v: &Values) -> Option<Action> {
        let d = &v.settings.downloads;
        let (_, per_unit) = paint::limit_unit();
        let to_kbps = |units: u64| ((units as f64 * per_unit) / 1024.0).round() as u64;
        let step = |n: u64, up: bool| -> u64 {
            let s = if n < 10 { 1 } else if n < 100 { 5 } else { 50 };
            if up { n + s } else { n.saturating_sub(s).max(1) }
        };
        let cmd = |s: String| Some(Action::Command(s));
        match hit {
            Hit::Seg(id, i) => {
                let seg = self.segs.iter().find(|s| s.id == *id)?;
                let custom_idx = (seg.opts.last().map(String::as_str) == Some("Custom")).then(|| seg.opts.len() - 1);
                if Some(*i) == custom_idx {
                    self.custom_open.insert(id);
                    // show the stepper at the current value
                    return Some(Action::Command("noop".into()));
                }
                self.custom_open.remove(id);
                let val: Option<u64> = seg.opts[*i].split_whitespace().next().and_then(|s| s.parse().ok());
                match *id {
                    "max" => cmd(format!("dls:max:{}", val?)),
                    "conn" => cmd(format!("dls:conn:{}", val?)),
                    "limit" => cmd(format!("dls:limit:{}", to_kbps(val.unwrap_or(0)))),
                    "bits" => cmd(format!("dls:bits:{i}")),
                    "binary" => cmd(format!("dls:binary:{i}")),
                    "corner" => cmd(if *i == 0 { "corner:bottom-left".into() } else { "corner:bottom-right".into() }),
                    "display" => cmd(if *i == 0 { "display:stack".into() } else { "display:folder".into() }),
                    "theme" => cmd(["theme:system", "theme:light", "theme:dark"][(*i).min(2)].into()),
                    "tray" => cmd(if *i == 0 { "tray:stack".into() } else { "tray:window".into() }),
                    "start" => cmd(if *i == 0 { "start:tray".into() } else { "start:window".into() }),
                    "close" => cmd(if *i == 0 { "close:keep".into() } else { "close:quit".into() }),
                    "sort" => cmd(format!("sort:{}", ["added", "modified", "name", "kind"][*i])),
                    "count" => cmd(format!("limit:{}", val?)),
                    "size" => cmd(format!("size:{}", ["small", "medium", "large", "extra-large"][*i])),
                    id if id.starts_with("dle-") => cmd(format!("dle:seg:{}:{i}", &id[4..])),
                    "t-up" => cmd(format!("dls:t-up:{}", to_kbps(val.unwrap_or(0)))),
                    "t-ratio" => cmd(format!("dls:t-ratio:{}", TORRENT_RATIOS[(*i).min(3)])),
                    "t-minutes" => cmd(format!("dls:t-minutes:{}", TORRENT_MINUTES[(*i).min(3)])),
                    _ => None,
                }
            }
            Hit::StepDown(id) | Hit::StepUp(id) => {
                let up = matches!(hit, Hit::StepUp(_));
                match *id {
                    "max" => cmd(format!("dls:max:{}", step(d.max_active as u64, up).min(50))),
                    "conn" => cmd(format!("dls:conn:{}", step(d.connections as u64, up).min(32))),
                    "t-up" => {
                        let units = ((d.torrents.upload_kbps as f64 * 1024.0) / per_unit).round() as u64;
                        cmd(format!("dls:t-up:{}", to_kbps(step(units.max(1), up))))
                    }
                    "limit" => {
                        let units = ((d.limit_kbps as f64 * 1024.0) / per_unit).round() as u64;
                        cmd(format!("dls:limit:{}", to_kbps(step(units.max(1), up))))
                    }
                    _ => None,
                }
            }
            Hit::Switch(id) => match *id {
                "seed" => cmd("dls:seed".into()),
                "gaming" => cmd("dls:gaming".into()),
                "magnets" => cmd("dls:magnets".into()),
                "t-choose" => cmd("dls:t-choose".into()),
                "t-upnp" => cmd("dls:t-upnp".into()),
                "login" => cmd("login".into()),
                "show-stack" => cmd("show-stack:flip".into()),
                "arrivals" => cmd("arrivals:flip".into()),
                "autoupdate" => cmd("update:auto".into()),
                id if id.starts_with("dle-") => cmd(format!("dle:flip:{}", &id[4..])),
                _ => None,
            },
            Hit::Button(id) => cmd(id.clone()),
            Hit::None => None,
        }
    }

    // ---------- scrolling ----------

    pub fn scroll_by(&mut self, dy: f32) {
        self.scroll += dy;
        self.clamp_scroll(true);
    }

    fn clamp_scroll(&mut self, animate: bool) {
        // (a little past the end, so the last card clears the fade at the window's foot)
        let max = (self.height + 24.0 - self.view_h).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max);
        let to = Vector3 { X: 0.0, Y: (self.top - self.scroll) * self.k, Z: 0.0 };
        let c = &self.gfx.compositor;
        let _ = (|| -> Result<()> {
            if animate {
                let a = c.CreateVector3KeyFrameAnimation()?;
                a.InsertExpressionKeyFrame(0.0, &HSTRING::from("this.StartingValue"))?;
                a.InsertKeyFrameWithEasingFunction(1.0, to, &c.CreateCubicBezierEasingFunction(Vector2 { X: 0.23, Y: 1.0 }, Vector2 { X: 0.32, Y: 1.0 })?)?;
                a.SetDuration(ms(180))?;
                self.inner.StartAnimation(&HSTRING::from("Offset"), &a)
            } else {
                self.inner.StopAnimation(&HSTRING::from("Offset"))?;
                self.inner.SetOffset(to)
            }
        })();
    }
}
