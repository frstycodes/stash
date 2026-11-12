//! The taskbar / Start jump list: New Download, Open Downloads folder, and the files
//! that finished downloading most recently.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::Variant::VT_LPWSTR;
use windows::Win32::UI::Shell::Common::{IObjectArray, IObjectCollection};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows::Win32::UI::Shell::{DestinationList, EnumerableObjectCollection, ICustomDestinationList, IShellLinkW, ShellLink};
use windows::core::{GUID, HSTRING, Interface, PWSTR, Result};

use crate::dl::Snapshot;

/// System.Title: what the jump list shows for a link.
const PKEY_TITLE: PROPERTYKEY = PROPERTYKEY { fmtid: GUID::from_u128(0xf29f85e0_4ff9_1068_ab91_08002b27b3d9), pid: 2 };
const RECENT: usize = 6;

thread_local! {
    static LAST: RefCell<Option<(PathBuf, Vec<PathBuf>)>> = const { RefCell::new(None) };
}

fn link(path: &Path, args: &str, title: &str, icon: Option<(&Path, i32)>) -> Result<IShellLinkW> {
    unsafe {
        let l: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        l.SetPath(&HSTRING::from(path.as_os_str()))?;
        if !args.is_empty() {
            l.SetArguments(&HSTRING::from(args))?;
        }
        if let Some((file, index)) = icon {
            l.SetIconLocation(&HSTRING::from(file.as_os_str()), index)?;
        }
        // PROPVARIANT's Drop frees the string with CoTaskMemFree, so it's allocated that way
        let wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
        let text = windows::Win32::System::Com::CoTaskMemAlloc(wide.len() * 2) as *mut u16;
        if text.is_null() {
            return Err(windows::core::Error::from_win32());
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), text, wide.len());
        let value = PROPVARIANT {
            Anonymous: PROPVARIANT_0 {
                Anonymous: std::mem::ManuallyDrop::new(PROPVARIANT_0_0 {
                    vt: VT_LPWSTR,
                    Anonymous: PROPVARIANT_0_0_0 { pwszVal: PWSTR(text) },
                    ..Default::default()
                }),
            },
        };
        let store: IPropertyStore = l.cast()?;
        store.SetValue(&PKEY_TITLE, &value)?;
        store.Commit()?;
        Ok(l)
    }
}

fn build(downloads: &Path, recent: &[PathBuf]) -> Result<()> {
    unsafe {
        let list: ICustomDestinationList = CoCreateInstance(&DestinationList, None, CLSCTX_INPROC_SERVER)?;
        let mut slots = 0u32;
        let _removed: IObjectArray = list.BeginList(&mut slots)?;
        let exe = std::env::current_exe().map_err(|_| windows::core::Error::from_win32())?;
        if !recent.is_empty() {
            let files: IObjectCollection = CoCreateInstance(&EnumerableObjectCollection, None, CLSCTX_INPROC_SERVER)?;
            for f in recent.iter().take((slots as usize).max(1)) {
                let name = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                if let Ok(l) = link(f, "", &name, None) {
                    files.AddObject(&l)?;
                }
            }
            let _ = list.AppendCategory(&HSTRING::from("Recent downloads"), &files.cast::<IObjectArray>()?);
        }
        let tasks: IObjectCollection = CoCreateInstance(&EnumerableObjectCollection, None, CLSCTX_INPROC_SERVER)?;
        tasks.AddObject(&link(&exe, "--new-download", "New Download", Some((&exe, 0)))?)?;
        let windir = PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()));
        let explorer = windir.join("explorer.exe");
        let imageres = windir.join("System32").join("imageres.dll");
        // -184: the Downloads folder's icon
        tasks.AddObject(&link(&explorer, &format!("\"{}\"", downloads.display()), "Open Downloads Folder", Some((&imageres, -184)))?)?;
        list.AddUserTasks(&tasks.cast::<IObjectArray>()?)?;
        list.CommitList()
    }
}

/// The files to list: finished downloads still on disk, newest first.
fn recent(snap: &Snapshot) -> Vec<PathBuf> {
    let mut done: Vec<_> = snap.jobs.iter().filter(|j| j.state.finished()).filter_map(|j| Some((j.finished_at.unwrap_or(j.added), j.path.clone()?))).collect();
    done.sort_by(|a, b| b.0.cmp(&a.0));
    done.into_iter().map(|(_, p)| p).filter(|p| p.is_file()).take(RECENT).collect()
}

/// Rebuild the jump list if what it shows changed.
pub fn refresh(snap: &Snapshot, downloads: &Path) {
    let files = recent(snap);
    let next = (downloads.to_path_buf(), files);
    if LAST.with(|l| l.borrow().as_ref() == Some(&next)) {
        return;
    }
    match build(&next.0, &next.1) {
        Ok(()) => LAST.with(|l| *l.borrow_mut() = Some(next)),
        Err(e) => {
            crate::stash::log(format!("jump list failed: {e}"));
            LAST.with(|l| *l.borrow_mut() = Some(next)); // don't retry on every snapshot
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore]
    fn builds() {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
        }
        let d = std::env::temp_dir();
        super::build(&d, &[]).unwrap();
        eprintln!("built");
    }
}
