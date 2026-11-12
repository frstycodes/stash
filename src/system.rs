//! Where Stash meets the rest of Windows: the taskbar button, notifications, the jump
//! list, the global shortcut and the clipboard watcher. The stack calls in here at a
//! few points; each feature lives in its own module.

use std::path::Path;

use windows::Win32::Foundation::HWND;

use crate::dl::Snapshot;
use crate::settings::Settings;

/// Once the stack's windows exist.
pub fn start(tile: HWND, s: &Settings) {
    crate::hotkey::register(tile, &s.hotkey);
    crate::clipwatch::listen(tile, s.watch_clipboard);
}

/// The engine has news.
pub fn on_snapshot(snap: &Snapshot, s: &Settings, downloads: &Path) {
    crate::taskbar::update(snap);
    crate::toast::on_snapshot(snap, s.notify);
    crate::jumplist::refresh(snap, downloads);
}

/// A setting changed.
pub fn apply(tile: HWND, before: &Settings, after: &Settings) {
    if before.hotkey != after.hotkey {
        crate::hotkey::register(tile, &after.hotkey);
    }
    if before.watch_clipboard != after.watch_clipboard {
        crate::clipwatch::listen(tile, after.watch_clipboard);
    }
}
