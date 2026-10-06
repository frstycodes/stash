use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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
    pub corner: Corner,
    pub sort: Sort,
    pub display: Display,
    pub limit: usize,
    pub icon_size: IconSize,
    /// Folders watched besides Downloads (which is always first and can't be removed).
    pub folders: Vec<PathBuf>,
    /// The folder on show; None = Downloads.
    pub current: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            corner: Corner::BottomRight,
            sort: Sort::Added,
            display: Display::Stack,
            limit: 10,
            icon_size: IconSize::Medium,
            folders: Vec::new(),
            current: None,
        }
    }
}

fn file() -> Option<PathBuf> {
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
        let _ = std::fs::write(f, json);
    }
}
