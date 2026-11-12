//! Sorting downloads into a folder per type (Videos, Music, Pictures…), when asked.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Category {
    Videos,
    Music,
    Pictures,
    Documents,
    Programs,
    Archives,
}

pub const ALL: [Category; 6] = [Category::Videos, Category::Music, Category::Pictures, Category::Documents, Category::Programs, Category::Archives];

impl Category {
    /// Also the default folder's name.
    pub fn name(self) -> &'static str {
        match self {
            Category::Videos => "Videos",
            Category::Music => "Music",
            Category::Pictures => "Pictures",
            Category::Documents => "Documents",
            Category::Programs => "Programs",
            Category::Archives => "Archives",
        }
    }
}

/// A file's type, by its extension. None: it stays where it was saved.
pub fn category(name: &str) -> Option<Category> {
    let ext = Path::new(name).extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "mp4" | "mkv" | "webm" | "avi" | "mov" | "wmv" | "flv" | "m4v" | "mpg" | "mpeg" | "3gp" | "ts" | "m2ts" => Category::Videos,
        "mp3" | "flac" | "wav" | "aac" | "ogg" | "opus" | "m4a" | "wma" | "aiff" | "alac" | "mid" | "midi" => Category::Music,
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "tif" | "tiff" | "heic" | "heif" | "avif" | "svg" | "ico" | "raw" | "cr2" | "nef" | "dng" | "psd" => {
            Category::Pictures
        }
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "odt" | "ods" | "odp" | "rtf" | "txt" | "md" | "csv" | "epub" | "mobi" => Category::Documents,
        "exe" | "msi" | "msix" | "msixbundle" | "appx" | "appxbundle" | "bat" | "cmd" | "ps1" | "apk" | "jar" => Category::Programs,
        "zip" | "rar" | "7z" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "iso" | "img" | "cab" => Category::Archives,
        _ => return None,
    })
}

/// Where a file named `name`, saved to `dir`, goes when sorting is on: its type's own
/// folder if one is set, else a subfolder of `dir` named for the type.
pub fn folder_for(dir: &Path, name: &str, overrides: &[(Category, PathBuf)]) -> Option<PathBuf> {
    let c = category(name)?;
    Some(overrides.iter().find(|(k, p)| *k == c && !p.as_os_str().is_empty()).map(|(_, p)| p.clone()).unwrap_or_else(|| dir.join(c.name())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_by_type() {
        assert_eq!(category("Movie.MKV"), Some(Category::Videos));
        assert_eq!(category("song.flac"), Some(Category::Music));
        assert_eq!(category("setup.exe"), Some(Category::Programs));
        assert_eq!(category("backup.tar.gz"), Some(Category::Archives));
        assert_eq!(category("README"), None);
        assert_eq!(category("thing.xyz"), None);
        let d = Path::new(r"C:\Users\me\Downloads");
        assert_eq!(folder_for(d, "a.pdf", &[]), Some(d.join("Documents")));
        let over = [(Category::Documents, PathBuf::from(r"D:\Docs")), (Category::Music, PathBuf::new())];
        assert_eq!(folder_for(d, "a.pdf", &over), Some(PathBuf::from(r"D:\Docs")));
        assert_eq!(folder_for(d, "a.mp3", &over), Some(d.join("Music")));
        assert_eq!(folder_for(d, "a.bin", &over), None);
    }
}
