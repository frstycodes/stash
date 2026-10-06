#![windows_subsystem = "windows"]

mod droptarget;
mod files;
mod gfx;
mod menu;
mod settings;
mod shell;
mod stash;
mod thumbs;

use windows::Win32::System::Ole::OleInitialize;
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows::Win32::UI::WindowsAndMessaging::{DispatchMessageW, GetMessageW, MSG, TranslateMessage};

fn main() {
    if shell::already_running() {
        return;
    }
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        // OLE (drag and drop) needs a single-threaded apartment on the UI thread.
        let _ = OleInitialize(None);
    }
    if let Err(e) = stash::Stash::start() {
        eprintln!("Stash failed to start: {e}");
        return;
    }
    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
