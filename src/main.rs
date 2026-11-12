#![windows_subsystem = "windows"]

mod blur;
mod dl;
mod droptarget;
mod files;
mod gfx;
mod icons;
mod ipc;
mod manager;
mod menu;
mod paint;
mod popup;
mod search;
mod settings;
mod settings_page;
mod shell;
mod stash;
mod thumbs;
mod ui;
mod update;

use windows::Win32::System::Ole::OleInitialize;
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows::Win32::UI::WindowsAndMessaging::{DispatchMessageW, GetMessageW, MSG, TranslateMessage};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // A browser extension started us to hand over downloads.
    if ipc::is_native_host(&args) {
        ipc::run_native_host();
        return;
    }
    let request = ipc::from_args(&args);
    // Started by an update: the old copy is still on its way out, so wait for it.
    let mut running = shell::already_running();
    if args.iter().any(|a| a == "--updated") {
        for _ in 0..50 {
            if !running {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
            running = shell::already_running();
        }
    }
    if running {
        // Opening Stash again shows its downloads window; links go to the New Download sheet.
        ipc::forward(&request);
        return;
    }
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        // OLE (drag and drop) needs a single-threaded apartment on the UI thread.
        let _ = OleInitialize(None);
    }
    // let the browser extensions find us (the Store build can't write these keys)
    shell::repair_start_at_login();
    if !shell::is_packaged() && std::env::var_os("STASH_DATA").is_none() {
        std::thread::spawn(ipc::register_native_host);
    }
    if let Err(e) = stash::Stash::start() {
        eprintln!("Stash failed to start: {e}");
        return;
    }
    // Launched with a magnet link or a .torrent file: ask about it straight away.
    if let ipc::Request::Open(_) = request {
        stash::handle(request);
    } else if std::env::var_os("STASH_VIEW").is_some() {
        stash::handle(ipc::Request::Show); // (for screenshots)
    }
    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
