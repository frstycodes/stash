//! Thin wrappers over the Win32 / shell APIs Stash needs.

use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC, GetDIBits,
    GetObjectW, HBITMAP, HGDIOBJ, ReleaseDC,
};
use windows::Win32::System::Com::{
    COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, IDataObject,
};
use windows::Win32::System::Ole::{DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_MOVE};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Shell::{
    BHID_DataObject, FOLDERID_Downloads, IShellItem, IShellItemImageFactory, KF_FLAG_DEFAULT,
    SHCreateItemFromParsingName, SHDoDragDrop, SHGetKnownFolderPath, SIIGBF_BIGGERSIZEOK, SIIGBF_ICONONLY,
    SIIGBF_RESIZETOFIT, SIIGBF_THUMBNAILONLY,
    ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetClassNameW, GetCursorPos, GetForegroundWindow, GetWindowLongPtrW, GetWindowRect,
    GetWindowThreadProcessId, HWND_BOTTOM, HWND_TOPMOST, SPI_GETWORKAREA, SW_SHOWNORMAL, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SetWindowPos,
    SystemParametersInfoW,
};
use windows::core::{PCWSTR, w};

use crate::gfx::Pixels;

fn wide(s: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(std::iter::once(0)).collect()
}

pub fn downloads_dir() -> PathBuf {
    unsafe {
        if let Ok(p) = SHGetKnownFolderPath(&FOLDERID_Downloads, KF_FLAG_DEFAULT, None) {
            let path = PathBuf::from(p.to_string().unwrap_or_default());
            CoTaskMemFree(Some(p.0 as *const c_void));
            if !path.as_os_str().is_empty() {
                return path;
            }
        }
    }
    PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_default()).join("Downloads")
}

/// A folder's name for menus and labels ("Downloads", "Screenshots", or "D:\" for a drive).
pub fn folder_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string())
}

/// The standard Windows folder picker. Blocks (in a modal loop) until closed.
pub fn pick_folder() -> Option<PathBuf> {
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
    use windows::Win32::UI::Shell::{FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH};
    unsafe {
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let options = dialog.GetOptions().ok()?;
        dialog.SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM).ok()?;
        let _ = dialog.SetTitle(w!("Add a folder to Stash"));
        dialog.Show(None).ok()?; // cancelled: an error
        let name = dialog.GetResult().ok()?.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path = PathBuf::from(name.to_string().unwrap_or_default());
        CoTaskMemFree(Some(name.0 as *const c_void));
        (!path.as_os_str().is_empty()).then_some(path)
    }
}

/// Call once on the thumbnail thread.
pub fn init_com_thread() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
}

/// The shell's own thumbnail (or icon) for a file or folder, as premultiplied BGRA.
pub fn thumbnail(path: &Path, size: i32) -> Option<Pixels> {
    unsafe {
        let p = wide(path);
        let factory: IShellItemImageFactory = SHCreateItemFromParsingName(PCWSTR(p.as_ptr()), None).ok()?;
        let size = SIZE { cx: size, cy: size };
        // Apps, installers and shortcuts have no real thumbnail: the shell would hand
        // back their icon inside a frame, so go straight to the plain icon.
        let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        let icon_only = matches!(ext.as_str(), "exe" | "msi" | "msix" | "msixbundle" | "appx" | "appxbundle" | "lnk" | "url" | "bat" | "cmd" | "com" | "scr" | "dll" | "sys" | "ps1");
        if icon_only {
            let hbmp = factory.GetImage(size, SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK).ok()?;
            let px = bitmap_pixels(hbmp).map(strip_faint_frame);
            let _ = DeleteObject(HGDIOBJ(hbmp.0));
            return px;
        }
        // A real thumbnail (photos, videos, documents) when there is one; otherwise the
        // plain icon. Asking for a "thumbnail" of an app makes the shell frame its icon.
        let hbmp = factory
            .GetImage(size, SIIGBF_THUMBNAILONLY | SIIGBF_RESIZETOFIT)
            .or_else(|_| factory.GetImage(size, SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK))
            .ok()?;
        let px = bitmap_pixels(hbmp);
        let _ = DeleteObject(HGDIOBJ(hbmp.0));
        px
    }
}

/// Some app icons carry a faint light outline around the whole tile (invisible on
/// white, a visible box on a dark desktop). Clear near-transparent pixels at the edge.
fn strip_faint_frame(mut px: Pixels) -> Pixels {
    const RING: u32 = 3;
    const FAINT: u8 = 64;
    let (w, h) = (px.width, px.height);
    for y in 0..h {
        for x in 0..w {
            let edge = x < RING || y < RING || x >= w - RING || y >= h - RING;
            let i = ((y * w + x) * 4) as usize;
            if edge && px.bgra[i + 3] < FAINT {
                px.bgra[i..i + 4].fill(0);
            }
        }
    }
    px
}

unsafe fn bitmap_pixels(hbmp: HBITMAP) -> Option<Pixels> {
    unsafe {
        let mut bm = BITMAP::default();
        if GetObjectW(HGDIOBJ(hbmp.0), size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut c_void)) == 0 {
            return None;
        }
        let (w, h) = (bm.bmWidth, bm.bmHeight.abs());
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bgra = vec![0u8; (w * h * 4) as usize];
        let dc = GetDC(None);
        let lines = GetDIBits(dc, hbmp, 0, h as u32, Some(bgra.as_mut_ptr() as *mut c_void), &mut info, DIB_RGB_COLORS);
        ReleaseDC(None, dc);
        if lines == 0 {
            return None;
        }
        // Shell bitmaps are already premultiplied BGRA; photos often carry no alpha at all.
        if !bgra.chunks_exact(4).any(|p| p[3] != 0) {
            bgra.chunks_exact_mut(4).for_each(|p| p[3] = 255);
        }
        Some(Pixels { width: w as u32, height: h as u32, bgra })
    }
}

// ---------- windows on the desktop ----------

/// Position in physical pixels, at the bottom of the z-order (just above the desktop),
/// or directly below `above` when given.
pub fn place(hwnd: HWND, above: Option<HWND>, x: i32, y: i32, w: i32, h: i32, show: bool) {
    let flags = SWP_NOACTIVATE | if show { SWP_SHOWWINDOW } else { Default::default() };
    unsafe {
        let _ = SetWindowPos(hwnd, Some(above.unwrap_or(HWND_BOTTOM)), x, y, w, h, flags);
    }
}

pub fn restack(hwnd: HWND, above: Option<HWND>) {
    unsafe {
        let _ = SetWindowPos(hwnd, Some(above.unwrap_or(HWND_BOTTOM)), 0, 0, 0, 0, SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE);
    }
}

/// Above every non-topmost window (other apps, full-screen windows), without activating.
pub fn raise_topmost(hwnd: HWND) {
    unsafe {
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE);
    }
}

pub fn foreground() -> HWND {
    unsafe { GetForegroundWindow() }
}

/// Our own windows (tray and context menus) or the taskbar / tray overflow / desktop:
/// focus moving to these doesn't mean the user switched to another app.
pub fn is_own_or_shell(hwnd: HWND) -> bool {
    use windows::Win32::System::Threading::GetCurrentProcessId;
    if hwnd.is_invalid() {
        return true;
    }
    unsafe {
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == GetCurrentProcessId() {
            return true;
        }
        let mut buf = [0u16; 64];
        let n = GetClassNameW(hwnd, &mut buf) as usize;
        let class = String::from_utf16_lossy(&buf[..n]);
        matches!(
            class.as_str(),
            "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" | "NotifyIconOverflowWindow" | "TopLevelWindowForOverflowXamlIsland" | "Progman" | "WorkerW"
        )
    }
}

/// Whether another app's window sits over `hwnd` (as it would with `hwnd` at the
/// bottom of the z-order): visible, not minimized, not cloaked, not topmost.
pub fn covered(hwnd: HWND) -> bool {
    use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, IsIconic, IsWindowVisible, WS_EX_TOPMOST, WS_EX_TRANSPARENT};
    use windows::core::BOOL;

    struct Search {
        rect: RECT,
        pid: u32,
        found: bool,
    }
    unsafe extern "system" fn visit(w: HWND, lparam: windows::Win32::Foundation::LPARAM) -> BOOL {
        let s = unsafe { &mut *(lparam.0 as *mut Search) };
        unsafe {
            if !IsWindowVisible(w).as_bool() || IsIconic(w).as_bool() {
                return true.into();
            }
            let mut pid = 0;
            GetWindowThreadProcessId(w, Some(&mut pid));
            let ex = GetWindowLongPtrW(w, GWL_EXSTYLE) as u32;
            if pid == s.pid || ex & (WS_EX_TOPMOST.0 | WS_EX_TRANSPARENT.0) != 0 {
                return true.into();
            }
            let mut cloaked = 0u32;
            let _ = DwmGetWindowAttribute(w, DWMWA_CLOAKED, &mut cloaked as *mut _ as *mut c_void, size_of::<u32>() as u32);
            if cloaked != 0 || is_own_or_shell(w) {
                return true.into();
            }
            let r = window_rect(w);
            if r.left < s.rect.right && r.right > s.rect.left && r.top < s.rect.bottom && r.bottom > s.rect.top {
                s.found = true;
                return false.into(); // stop
            }
        }
        true.into()
    }

    let mut s = Search { rect: window_rect(hwnd), pid: unsafe { GetCurrentProcessId() }, found: false };
    unsafe {
        let _ = EnumWindows(Some(visit), windows::Win32::Foundation::LPARAM(&mut s as *mut _ as isize));
    }
    s.found
}

pub fn scale_factor(hwnd: HWND) -> f32 {
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 }
}

pub fn work_area() -> RECT {
    let mut rect = RECT::default();
    unsafe {
        let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut rect as *mut _ as *mut c_void), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
    }
    rect
}

pub fn window_rect(hwnd: HWND) -> RECT {
    let mut rect = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut rect);
    }
    rect
}

pub fn cursor() -> POINT {
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    p
}

// ---------- file actions ----------

/// Native shell drag of a file to other apps. Blocks (in a modal loop) until dropped.
pub fn start_drag(hwnd: HWND, path: &Path) {
    unsafe {
        let p = wide(path);
        let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(PCWSTR(p.as_ptr()), None) else { return };
        let Ok(data) = item.BindToHandler::<_, IDataObject>(None, &BHID_DataObject) else { return };
        let _ = SHDoDragDrop(Some(hwnd), &data, None, DROPEFFECT_COPY | DROPEFFECT_MOVE | DROPEFFECT_LINK);
    }
}

pub fn open(path: &Path) {
    let p = wide(path);
    unsafe {
        ShellExecuteW(None, w!("open"), PCWSTR(p.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

pub fn reveal(path: &Path) {
    let args = wide(format!("/select,\"{}\"", path.display()));
    unsafe {
        ShellExecuteW(None, w!("open"), w!("explorer.exe"), PCWSTR(args.as_ptr()), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

/// Delete a file or folder for good, skipping the Recycle Bin (Explorer's Shift+Delete).
pub fn delete_permanently(path: &Path) {
    use windows::Win32::UI::Shell::{FO_DELETE, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, SHFILEOPSTRUCTW, SHFileOperationW};
    let mut from: Vec<u16> = path.as_os_str().encode_wide().collect();
    from.extend([0, 0]); // double-null terminated list
    let mut op = SHFILEOPSTRUCTW {
        wFunc: FO_DELETE,
        pFrom: PCWSTR(from.as_ptr()),
        fFlags: (FOF_NOCONFIRMATION.0 | FOF_SILENT.0 | FOF_NOERRORUI.0) as u16,
        ..Default::default()
    };
    unsafe {
        SHFileOperationW(&mut op);
    }
}

pub fn recycle(path: &Path) {
    use windows::Win32::UI::Shell::{FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_SILENT, SHFILEOPSTRUCTW, SHFileOperationW};
    let mut from: Vec<u16> = path.as_os_str().encode_wide().collect();
    from.extend([0, 0]); // double-null terminated list
    let mut op = SHFILEOPSTRUCTW {
        wFunc: FO_DELETE,
        pFrom: PCWSTR(from.as_ptr()),
        fFlags: (FOF_ALLOWUNDO.0 | FOF_NOCONFIRMATION.0 | FOF_SILENT.0) as u16,
        ..Default::default()
    };
    unsafe {
        SHFileOperationW(&mut op);
    }
}

// ---------- start at login ----------

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";

/// The Microsoft Store build runs as an MSIX package. Packaged apps start at sign-in
/// through the StartupTask declared in their manifest, not the Run key.
const STARTUP_TASK: &str = "StashStartup";

pub fn is_packaged() -> bool {
    use windows::Win32::Foundation::APPMODEL_ERROR_NO_PACKAGE;
    use windows::Win32::Storage::Packaging::Appx::GetCurrentPackageFullName;
    let mut len = 0u32;
    unsafe { GetCurrentPackageFullName(&mut len, None) != APPMODEL_ERROR_NO_PACKAGE }
}

fn startup_task() -> Option<windows::ApplicationModel::StartupTask> {
    windows::ApplicationModel::StartupTask::GetAsync(&windows::core::HSTRING::from(STARTUP_TASK)).ok()?.get().ok()
}

pub fn start_at_login() -> bool {
    if is_packaged() {
        use windows::ApplicationModel::StartupTaskState;
        return startup_task()
            .and_then(|t| t.State().ok())
            .is_some_and(|s| s == StartupTaskState::Enabled || s == StartupTaskState::EnabledByPolicy);
    }
    windows_registry::CURRENT_USER
        .open(RUN_KEY)
        .and_then(|k| k.get_string("Stash"))
        .is_ok()
}

/// The sign-in entry points at a file that's gone (an install renamed the exe):
/// point it at this one. Leaves a missing entry, or one that works, alone.
pub fn repair_start_at_login() {
    if is_packaged() || std::env::var_os("STASH_DATA").is_some() {
        return;
    }
    let Ok(key) = windows_registry::CURRENT_USER.create(RUN_KEY) else { return };
    let Ok(value) = key.get_string("Stash") else { return };
    let target = value.trim().trim_matches('"');
    if !std::path::Path::new(target).exists() {
        if let Ok(exe) = std::env::current_exe() {
            let _ = key.set_string("Stash", format!("\"{}\"", exe.display()));
        }
    }
}

pub fn set_start_at_login(enabled: bool) {
    // a test copy (STASH_DATA) never points your sign-in at itself
    if std::env::var_os("STASH_DATA").is_some() {
        return;
    }
    if is_packaged() {
        // If the user turned it off in Task Manager, only they can turn it back on there.
        if let Some(task) = startup_task() {
            if enabled {
                let _ = task.RequestEnableAsync().and_then(|op| op.get());
            } else {
                let _ = task.Disable();
            }
        }
        return;
    }
    let Ok(key) = windows_registry::CURRENT_USER.create(RUN_KEY) else { return };
    if enabled {
        if let Ok(exe) = std::env::current_exe() {
            let _ = key.set_string("Stash", format!("\"{}\"", exe.display()));
        }
    } else {
        let _ = key.remove_value("Stash");
    }
}

// ---------- single instance ----------

pub fn already_running() -> bool {
    use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
    use windows::Win32::System::Threading::CreateMutexW;
    unsafe {
        // a test copy (STASH_DATA) runs beside the real one
        let name = if std::env::var_os("STASH_DATA").is_some() { w!("Local\\StashDownloadsStackTest") } else { w!("Local\\StashDownloadsStack") };
        let handle = CreateMutexW(None, true, name);
        let exists = GetLastError() == ERROR_ALREADY_EXISTS;
        match handle {
            // not ours: let it go, so asking again (after the other copy exits) can succeed
            Ok(h) if exists => {
                let _ = windows::Win32::Foundation::CloseHandle(h);
            }
            h => std::mem::forget(h), // held for the life of the process
        }
        exists
    }
}

// ---------- download manager helpers ----------

/// The shell's icon for a file type (by name), for files that don't exist yet. Folders
/// when `folder`.
pub fn type_icon(name: &str, folder: bool, size: i32) -> Option<Pixels> {
    use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL};
    use windows::Win32::UI::Controls::{IImageList, ILD_TRANSPARENT};
    use windows::Win32::UI::Shell::{SHFILEINFOW, SHGFI_SYSICONINDEX, SHGFI_USEFILEATTRIBUTES, SHGetFileInfoW, SHGetImageList, SHIL_EXTRALARGE, SHIL_JUMBO};
    use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, ICONINFO};
    unsafe {
        let n = wide(if folder { "folder" } else { name });
        let mut info = SHFILEINFOW::default();
        let attrs = if folder { FILE_ATTRIBUTE_DIRECTORY } else { FILE_ATTRIBUTE_NORMAL };
        if SHGetFileInfoW(PCWSTR(n.as_ptr()), attrs, Some(&mut info), size_of::<SHFILEINFOW>() as u32, SHGFI_SYSICONINDEX | SHGFI_USEFILEATTRIBUTES) == 0 {
            return None;
        }
        let list: IImageList = SHGetImageList(if size > 48 { SHIL_JUMBO } else { SHIL_EXTRALARGE } as i32).ok()?;
        let icon = list.GetIcon(info.iIcon, ILD_TRANSPARENT.0).ok()?;
        let mut ii = ICONINFO::default();
        let ok = GetIconInfo(icon, &mut ii).is_ok();
        let _ = DestroyIcon(icon);
        if !ok {
            return None;
        }
        let px = bitmap_pixels(ii.hbmColor);
        let _ = DeleteObject(HGDIOBJ(ii.hbmColor.0));
        let _ = DeleteObject(HGDIOBJ(ii.hbmMask.0));
        // The jumbo list draws types without a big icon as a 48px icon in the corner.
        px.map(|p| crop_small(p))
    }
}

fn crop_small(px: Pixels) -> Pixels {
    let (w, h) = (px.width as usize, px.height as usize);
    if w <= 64 {
        return px;
    }
    let used = |x: usize, y: usize| px.bgra[(y * w + x) * 4 + 3] != 0;
    let beyond = (0..h).any(|y| (64..w).any(|x| used(x, y))) || (64..h).any(|y| (0..w).any(|x| used(x, y)));
    if beyond {
        return px;
    }
    let mut out = Vec::with_capacity(48 * 48 * 4);
    for y in 0..48 {
        out.extend_from_slice(&px.bgra[(y * w) * 4..(y * w + 48) * 4]);
    }
    Pixels { width: 48, height: 48, bgra: out }
}

pub fn clipboard_text() -> Option<String> {
    use windows::Win32::Foundation::HGLOBAL;
    use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
    use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
    unsafe {
        OpenClipboard(None).ok()?;
        let text = (|| {
            let h = GetClipboardData(13 /* CF_UNICODETEXT */).ok()?;
            let g = HGLOBAL(h.0);
            let p = GlobalLock(g) as *const u16;
            if p.is_null() {
                return None;
            }
            let mut n = 0;
            while *p.add(n) != 0 {
                n += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
            let _ = GlobalUnlock(g);
            Some(s)
        })();
        let _ = CloseClipboard();
        text
    }
}

pub fn set_clipboard_text(text: &str) {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
    use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
    let w: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        if OpenClipboard(None).is_err() {
            return;
        }
        let _ = EmptyClipboard();
        if let Ok(g) = GlobalAlloc(GMEM_MOVEABLE, w.len() * 2) {
            let p = GlobalLock(g) as *mut u16;
            if !p.is_null() {
                std::ptr::copy_nonoverlapping(w.as_ptr(), p, w.len());
                let _ = GlobalUnlock(g);
                let _ = SetClipboardData(13, Some(HANDLE(g.0)));
            }
        }
        let _ = CloseClipboard();
    }
}

/// A file picker for .torrent files.
pub fn pick_torrent() -> Option<PathBuf> {
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::{FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH};
    unsafe {
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let filters = [COMDLG_FILTERSPEC { pszName: w!("Torrent files"), pszSpec: w!("*.torrent") }];
        let _ = dialog.SetFileTypes(&filters);
        let _ = dialog.SetTitle(w!("Open a torrent file"));
        dialog.Show(None).ok()?;
        let name = dialog.GetResult().ok()?.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path = PathBuf::from(name.to_string().unwrap_or_default());
        CoTaskMemFree(Some(name.0 as *const c_void));
        (!path.as_os_str().is_empty()).then_some(path)
    }
}

// ---------- magnet links and .torrent files ----------

const MAGNET_KEY: &str = r"Software\Classes\magnet";
const TORRENT_PROGID: &str = r"Software\Classes\Stash.Torrent";

/// Whether magnet links open in Stash (registered for this user).
pub fn handles_magnets() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    windows_registry::CURRENT_USER
        .open(format!(r"{MAGNET_KEY}\shell\open\command"))
        .and_then(|k| k.get_string(""))
        .is_ok_and(|cmd| cmd.to_lowercase().contains(&exe.display().to_string().to_lowercase()))
}

pub fn set_handles_magnets(on: bool) {
    let reg = windows_registry::CURRENT_USER;
    let Ok(exe) = std::env::current_exe() else { return };
    let open = format!("\"{}\" \"%1\"", exe.display());
    if on {
        if let Ok(k) = reg.create(MAGNET_KEY) {
            let _ = k.set_string("", "URL:Magnet link");
            let _ = k.set_string("URL Protocol", "");
        }
        if let Ok(k) = reg.create(format!(r"{MAGNET_KEY}\DefaultIcon")) {
            let _ = k.set_string("", format!("\"{}\",0", exe.display()));
        }
        if let Ok(k) = reg.create(format!(r"{MAGNET_KEY}\shell\open\command")) {
            let _ = k.set_string("", &open);
        }
        if let Ok(k) = reg.create(TORRENT_PROGID) {
            let _ = k.set_string("", "Torrent file");
        }
        if let Ok(k) = reg.create(format!(r"{TORRENT_PROGID}\shell\open\command")) {
            let _ = k.set_string("", &open);
        }
        if let Ok(k) = reg.create(r"Software\Classes\.torrent\OpenWithProgids") {
            let _ = k.set_string("Stash.Torrent", "");
        }
        if let Ok(k) = reg.create(r"Software\Classes\.torrent") {
            if k.get_string("").is_err() {
                let _ = k.set_string("", "Stash.Torrent");
            }
        }
    } else {
        let _ = reg.remove_tree(MAGNET_KEY);
        let _ = reg.remove_tree(TORRENT_PROGID);
    }
    unsafe {
        use windows::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};
        SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None);
    }
}
