use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Theme {
    /// Follow Windows (Settings > Personalization > Colors > Choose your mode).
    #[default]
    System,
    Light,
    Dark,
}

impl Theme {
    pub fn is_light(self) -> bool {
        match self {
            Theme::Light => true,
            Theme::Dark => false,
            Theme::System => windows_registry::CURRENT_USER
                .open(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
                .and_then(|k| k.get_u32("AppsUseLightTheme"))
                .is_ok_and(|v| v != 0),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Corner {
    BottomRight,
    BottomLeft,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Sort {
    Added,
    Modified,
    Name,
    Kind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Display {
    Stack,
    Folder,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IconSize {
    Small,
    Medium,
    Large,
    ExtraLarge,
}

impl IconSize {
    /// Multiplier on the tile and every icon (labels keep their size).
    pub fn scale(self) -> f32 {
        match self {
            IconSize::Small => 0.8,
            IconSize::Medium => 1.0,
            IconSize::Large => 1.25,
            IconSize::ExtraLarge => 1.5,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: Theme,
    /// Clicking the tray icon opens the downloads window (instead of fanning the stack).
    pub tray_opens_window: bool,
    /// Starting Stash (at sign-in, or from the Start menu) opens the window.
    pub start_window: bool,
    /// Closing the window quits Stash (instead of leaving it in the tray).
    pub close_quits: bool,
    /// The stack in the corner of the desktop.
    pub show_stack: bool,
    /// A new download comes up over the windows and drops into the stack.
    pub show_arrivals: bool,
    pub corner: Corner,
    pub sort: Sort,
    pub display: Display,
    pub limit: usize,
    pub icon_size: IconSize,
    /// Folders watched besides Downloads (which is always first and can't be removed).
    pub folders: Vec<PathBuf>,
    /// The folder on show; None = Downloads.
    pub current: Option<PathBuf>,
    pub downloads: DlSettings,
    /// Download new versions in the background (GitHub builds; the Store updates its own).
    pub auto_update: bool,
}

/// The download manager's settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DlSettings {
    pub max_active: usize,
    pub connections: usize,
    /// KB/s; 0 = unlimited
    pub limit_kbps: u64,
    pub seed: bool,
    /// Where new downloads go; None = Downloads.
    pub folder: Option<PathBuf>,
    /// The downloads window's size, in logical pixels.
    pub window: Option<(i32, i32)>,
    /// Speeds in bits (Mbps) instead of bytes (MB/s).
    pub bits: bool,
    /// Sizes in binary units (MiB, 1024) instead of decimal (MB, 1000).
    pub binary: bool,
    /// Retries, download hours, proxy, sorting, what to do when done.
    #[serde(default)]
    pub engine: crate::dl::Prefs,
    /// Torrent settings: upload limit, seed limits, UPnP, choosing files.
    #[serde(default)]
    pub torrents: crate::dl::TorrentOptions,
    /// Gaming mode: slow down while an online game runs, to keep its ping low.
    #[serde(default = "yes")]
    pub gaming: bool,
}

fn yes() -> bool {
    true
}

impl Default for DlSettings {
    fn default() -> Self {
        Self { max_active: 3, connections: 8, limit_kbps: 0, seed: false, folder: None, window: None, bits: false, binary: false, engine: crate::dl::Prefs::default(), torrents: Default::default(), gaming: true }
    }
}

impl DlSettings {
    pub fn config(&self) -> crate::dl::Config {
        crate::dl::Config { max_active: self.max_active, connections: self.connections, limit_bps: self.limit_kbps * 1024, seed: self.seed }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: Theme::System,
            tray_opens_window: false,
            start_window: false,
            close_quits: false,
            show_stack: true,
            show_arrivals: true,
            corner: Corner::BottomRight,
            sort: Sort::Added,
            display: Display::Stack,
            limit: 10,
            icon_size: IconSize::Medium,
            folders: Vec::new(),
            current: None,
            downloads: DlSettings::default(),
            auto_update: true,
        }
    }
}

fn file() -> Option<PathBuf> {
    // STASH_DATA (a test copy) keeps its settings there too
    if let Some(d) = std::env::var_os("STASH_DATA") {
        return Some(PathBuf::from(d).join("settings.json"));
    }
    Some(PathBuf::from(std::env::var_os("APPDATA")?).join("Stash").join("settings.json"))
}

pub fn load() -> Settings {
    file()
        .and_then(|f| std::fs::read_to_string(f).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(settings: &Settings) {
    let Some(f) = file() else { return };
    if let Some(dir) = f.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string_pretty(settings) {
        // write then rename, so a crash mid-save can't leave half a file (and lost settings)
        let tmp = f.with_extension("json.tmp");
        let written = std::fs::File::create(&tmp).and_then(|mut file| {
            use std::io::Write;
            file.write_all(json.as_bytes())?;
            file.sync_all()
        });
        if written.is_ok() {
            let _ = std::fs::rename(&tmp, &f);
        }
    }
}
