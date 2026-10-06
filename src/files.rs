use crate::settings::Sort;
use std::cmp::Ordering;
use std::fs;
use std::io;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
const ERROR_NOT_SAME_DEVICE: i32 = 17;

#[derive(Clone, Debug)]
pub struct Item {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub added: SystemTime,
    pub modified: SystemTime,
}

/// Downloads that are still in progress (browser temp files).
pub fn is_partial(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".crdownload", ".part", ".partial", ".tmp", ".download", ".opdownload"]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

pub fn list(dir: &Path, sort: Sort) -> Vec<Item> {
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
    let mut items: Vec<Item> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || name.eq_ignore_ascii_case("desktop.ini") {
                return None;
            }
            let meta = e.metadata().ok()?;
            if meta.file_attributes() & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0 {
                return None;
            }
            let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            Some(Item {
                name,
                path: e.path(),
                is_dir: meta.is_dir(),
                added: meta.created().unwrap_or(modified),
                modified,
            })
        })
        .collect();

    let by_name = |a: &Item, b: &Item| a.name.to_lowercase().cmp(&b.name.to_lowercase());
    let ext = |i: &Item| {
        Path::new(&i.name)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    };
    items.sort_by(|a, b| match sort {
        Sort::Added => b.added.cmp(&a.added),
        Sort::Modified => b.modified.cmp(&a.modified),
        Sort::Name => by_name(a, b),
        Sort::Kind => b
            .is_dir
            .cmp(&a.is_dir)
            .then_with(|| ext(a).cmp(&ext(b)))
            .then_with(|| by_name(a, b)),
    });
    items
}

fn unique_dest(dir: &Path, name: &str) -> PathBuf {
    let path = Path::new(name);
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let mut dest = dir.join(name);
    let mut n = 2;
    while dest.exists() {
        dest = dir.join(format!("{stem} ({n}){ext}"));
        n += 1;
    }
    dest
}

fn copy_recursive(src: &Path, dest: &Path) -> io::Result<()> {
    if src.is_dir() {
        fs::create_dir(dest)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            copy_recursive(&entry.path(), &dest.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        fs::copy(src, dest).map(|_| ())
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy())
}

/// Move (or copy) a dropped file or folder into `dir`, never overwriting.
pub fn import(src: &Path, dir: &Path, copy: bool) -> io::Result<()> {
    if src.parent().is_some_and(|p| same_path(p, dir)) || same_path(src, dir) {
        return Ok(()); // already in Downloads
    }
    if dir.to_string_lossy().to_lowercase().starts_with(&(src.to_string_lossy().to_lowercase() + "\\")) {
        return Ok(()); // can't put a folder inside itself
    }
    let name = src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let dest = unique_dest(dir, &name);
    if copy {
        return copy_recursive(src, &dest);
    }
    match fs::rename(src, &dest) {
        Ok(()) => Ok(()),
        Err(e) if e.raw_os_error() == Some(ERROR_NOT_SAME_DEVICE) => {
            copy_recursive(src, &dest)?;
            if src.is_dir() { fs::remove_dir_all(src) } else { fs::remove_file(src) }
        }
        Err(e) => Err(e),
    }
}

pub fn changed(old: &[Item], new: &[Item]) -> bool {
    old.len() != new.len()
        || old
            .iter()
            .zip(new)
            .any(|(a, b)| a.path != b.path || a.modified.cmp(&b.modified) != Ordering::Equal)
}
