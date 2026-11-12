//! What the downloads window does with finished files: drag several out at once, copy
//! them to the clipboard (as Explorer does, so Paste in a folder copies them), rename
//! one, and take its SHA-256.

use std::path::{Path, PathBuf};

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CoTaskMemFree, IDataObject};
use windows::Win32::System::Ole::{DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_MOVE, OleFlushClipboard, OleSetClipboard};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{BHID_DataObject, IShellItemArray, SHCreateShellItemArrayFromIDLists, SHDoDragDrop, SHParseDisplayName};
use windows::core::PCWSTR;

fn wide(p: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    p.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

/// The shell's own data object for these files (what Explorer drags or copies).
fn data_object(paths: &[PathBuf]) -> Option<IDataObject> {
    let mut pidls: Vec<*mut ITEMIDLIST> = Vec::new();
    for p in paths {
        let w = wide(p);
        let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
        if unsafe { SHParseDisplayName(PCWSTR(w.as_ptr()), None, &mut pidl, 0, None) }.is_ok() && !pidl.is_null() {
            pidls.push(pidl);
        }
    }
    let data = (|| unsafe {
        if pidls.is_empty() {
            return None;
        }
        let list: Vec<*const ITEMIDLIST> = pidls.iter().map(|p| *p as *const _).collect();
        let items: IShellItemArray = SHCreateShellItemArrayFromIDLists(&list).ok()?;
        items.BindToHandler::<_, IDataObject>(None, &BHID_DataObject).ok()
    })();
    for p in pidls {
        unsafe { CoTaskMemFree(Some(p as *const _)) };
    }
    data
}

/// Drag these files to Explorer or another app. Blocks (in a modal loop) until dropped.
pub fn drag(hwnd: HWND, paths: &[PathBuf]) {
    let Some(data) = data_object(paths) else { return };
    unsafe {
        let _ = SHDoDragDrop(Some(hwnd), &data, None, DROPEFFECT_COPY | DROPEFFECT_MOVE | DROPEFFECT_LINK);
    }
}

/// Put these files on the clipboard; Paste in a folder copies them there.
pub fn copy_files(paths: &[PathBuf]) -> bool {
    let Some(data) = data_object(paths) else { return false };
    unsafe {
        if OleSetClipboard(&data).is_err() {
            return false;
        }
        // keep them on the clipboard after Stash quits
        let _ = OleFlushClipboard();
    }
    true
}

/// Why a new name can't be used, or None if it can.
pub fn name_problem(name: &str) -> Option<&'static str> {
    let t = name.trim();
    if t.is_empty() {
        return Some("Type a name for the file.");
    }
    if t.chars().any(|c| matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control()) {
        return Some("A file name can't contain \\ / : * ? \" < > |");
    }
    if t == "." || t == ".." || t.ends_with('.') {
        return Some("A file name can't end with a period.");
    }
    let stem = t.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = ["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str()) || ((stem.starts_with("COM") || stem.starts_with("LPT")) && stem.len() == 4 && stem.as_bytes()[3].is_ascii_digit());
    if reserved {
        return Some("Windows keeps that name for itself.");
    }
    None
}

/// Rename a file in place. Returns its new path, or what went wrong.
pub fn rename(path: &Path, name: &str) -> Result<PathBuf, String> {
    if let Some(p) = name_problem(name) {
        return Err(p.into());
    }
    let to = path.with_file_name(name.trim());
    if to == path {
        return Ok(to);
    }
    // a different file of that name (not just this one in other letter case)
    let same = to.to_string_lossy().eq_ignore_ascii_case(&path.to_string_lossy());
    if to.exists() && !same {
        return Err(format!("There's already a file called \u{201c}{}\u{201d} there.", name.trim()));
    }
    std::fs::rename(path, &to).map_err(|e| match e.kind() {
        std::io::ErrorKind::PermissionDenied => "The file is in use or protected. Close it and try again.".to_string(),
        std::io::ErrorKind::NotFound => "The file isn't there any more.".to_string(),
        _ => e.to_string(),
    })?;
    Ok(to)
}

/// The file's SHA-256, in lowercase hex.
pub fn sha256(path: &Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert!(name_problem("report.pdf").is_none());
        assert!(name_problem("  ").is_some());
        assert!(name_problem("a/b.txt").is_some());
        assert!(name_problem("con.txt").is_some());
        assert!(name_problem("COM1").is_some());
        assert!(name_problem("console.txt").is_none());
        assert!(name_problem("x.").is_some());
    }

    #[test]
    fn renames_and_hashes() {
        let dir = std::env::temp_dir().join(format!("stash-fileops-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let a = dir.join("a.txt");
        std::fs::write(&a, b"abc").unwrap();
        assert_eq!(sha256(&a).unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        std::fs::write(dir.join("taken.txt"), b"").unwrap();
        assert!(rename(&a, "taken.txt").is_err());
        let b = rename(&a, "b.txt").unwrap();
        assert!(b.exists() && !a.exists());
        // only the case changes
        let c = rename(&b, "B.txt").unwrap();
        assert!(c.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
