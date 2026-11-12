//! The stack's system hooks: the global shortcut that opens it with keyboard focus
//! (arrows move, Enter opens, Esc closes) and the clipboard watcher's prompt.

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_DOWN, VK_END, VK_ESCAPE, VK_HOME, VK_LEFT, VK_RETURN, VK_RIGHT, VK_TAB, VK_UP};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, HHOOK, IsWindowVisible, KBDLLHOOKSTRUCT, PostMessageW, SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_APP, WM_KEYDOWN,
    WM_SYSKEYDOWN,
};

use super::{Stash, TILE_HWND};
use crate::{manager, shell};

/// A navigation key, caught by the keyboard hook; wparam = virtual key.
pub(super) const WM_APP_KEY: u32 = WM_APP + 30;

// The stack's windows never take focus, so while the shortcut has it open a low-level
// keyboard hook takes just the navigation keys (the app you were in keeps the rest).
static KB_HOOK: AtomicIsize = AtomicIsize::new(0);
static KB_ON: AtomicBool = AtomicBool::new(false);
static KB_FAN: AtomicIsize = AtomicIsize::new(0);
const KEYS: [u16; 9] = [VK_UP.0, VK_DOWN.0, VK_LEFT.0, VK_RIGHT.0, VK_RETURN.0, VK_ESCAPE.0, VK_TAB.0, VK_HOME.0, VK_END.0];

fn keys_on(fan: HWND) {
    KB_FAN.store(fan.0 as isize, Ordering::Relaxed);
    KB_ON.store(true, Ordering::Relaxed);
    if KB_HOOK.load(Ordering::Relaxed) == 0 {
        unsafe {
            let instance = GetModuleHandleW(None).ok().map(|h| h.into());
            if let Ok(h) = SetWindowsHookExW(WH_KEYBOARD_LL, Some(key_hook), instance, 0) {
                KB_HOOK.store(h.0 as isize, Ordering::Relaxed);
            }
        }
    }
}

fn keys_off() {
    KB_ON.store(false, Ordering::Relaxed);
    let h = KB_HOOK.swap(0, Ordering::Relaxed);
    if h != 0 {
        unsafe {
            let _ = UnhookWindowsHookEx(HHOOK(h as *mut _));
        }
    }
}

extern "system" fn key_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && KB_ON.load(Ordering::Relaxed) {
        let k = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let fan = HWND(KB_FAN.load(Ordering::Relaxed) as *mut _);
        // only while the fan is up, and never with Ctrl/Alt/Win shortcuts
        let mods = unsafe {
            use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN};
            [VK_CONTROL, VK_MENU, VK_LWIN, VK_RWIN].iter().any(|v| GetAsyncKeyState(v.0 as i32) < 0)
        };
        if KEYS.contains(&(k.vkCode as u16)) && !mods && unsafe { IsWindowVisible(fan).as_bool() } {
            if matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN) {
                let tile = HWND(TILE_HWND.load(Ordering::Relaxed) as *mut _);
                unsafe {
                    let _ = PostMessageW(Some(tile), WM_APP_KEY, WPARAM(k.vkCode as usize), LPARAM(0));
                }
            }
            return LRESULT(1); // swallowed (key-ups too)
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

impl Stash {
    /// The global shortcut: open the stack (above every window) with keyboard focus, or
    /// close it if it's open.
    pub(super) fn on_hotkey(&mut self) {
        if !self.settings.show_stack {
            self.show_downloads(None);
            return;
        }
        if self.open {
            self.kb_close();
            return;
        }
        self.toggle_from_tray();
        self.kb = true;
        keys_on(self.fan);
        // start on the newest file (or "Open" when the folder is empty)
        let first = usize::from(self.rows.len() > 1);
        self.set_hover_row(Some(first));
    }

    /// Close, and give the navigation keys back.
    fn kb_close(&mut self) {
        self.close();
        self.kb = false;
        keys_off();
    }

    /// A key while the stack has keyboard focus. True if it was handled.
    pub(super) fn on_key(&mut self, vk: u16) -> bool {
        if !self.open || self.rows.is_empty() {
            self.kb = false;
            keys_off();
            return false;
        }
        let cur = self.hover_row.filter(|i| *i < self.rows.len());
        let Some(cur) = cur else {
            self.set_hover_row(Some(0));
            return true;
        };
        let k = |v: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY| v.0 == vk;
        let next = if k(VK_ESCAPE) {
            self.kb_close();
            return true;
        } else if k(VK_RETURN) {
            self.activate_row(cur);
            return true;
        } else if k(VK_UP) {
            self.neighbour(cur, 0.0, -1.0)
        } else if k(VK_DOWN) {
            self.neighbour(cur, 0.0, 1.0)
        } else if k(VK_LEFT) && self.grid() {
            self.neighbour(cur, -1.0, 0.0)
        } else if k(VK_RIGHT) && self.grid() {
            self.neighbour(cur, 1.0, 0.0)
        } else if k(VK_TAB) {
            Some((cur + 1) % self.rows.len())
        } else if k(VK_HOME) {
            // the newest file
            Some(usize::from(self.rows.len() > 1))
        } else if k(VK_END) {
            Some(self.rows.len() - 1)
        } else {
            return false;
        };
        if let Some(n) = next {
            self.set_hover_row(Some(n));
        }
        true
    }

    /// The nearest row from `from` in direction (dx, dy), on screen: works for the fan's
    /// arc and the grid alike.
    fn neighbour(&self, from: usize, dx: f32, dy: f32) -> Option<usize> {
        let (fx, fy) = self.rows[from].center;
        self.rows
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != from)
            .filter_map(|(i, r)| {
                let (vx, vy) = (r.center.0 - fx, r.center.1 - fy);
                let along = vx * dx + vy * dy;
                let across = (vx * dy - vy * dx).abs();
                (along > 1.0).then_some((i, along + across * 2.0))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    }

    /// What a click on row `i` does.
    fn activate_row(&mut self, i: usize) {
        self.kb = false;
        keys_off();
        if self.rows[i].job.is_some() {
            self.show_downloads(None);
        } else {
            let path = self.rows[i].path.clone();
            self.activate(path);
        }
    }

    /// The clipboard changed: offer a copied file link, above the stack.
    pub(super) fn on_clipboard(&mut self) {
        if !self.settings.watch_clipboard || self.demo.is_some() {
            return;
        }
        let Some((link, name)) = crate::clipwatch::check() else { return };
        if crate::toast::fullscreen() {
            return;
        }
        let anchor = if self.settings.show_stack {
            shell::window_rect(self.tile)
        } else {
            // no stack: the corner it would sit in
            let wa = shell::work_area();
            let t = (88.0 * self.k) as i32;
            let m = (8.0 * self.k) as i32;
            if self.right() {
                RECT { left: wa.right - m - t, top: wa.bottom - m, right: wa.right - m, bottom: wa.bottom - m }
            } else {
                RECT { left: wa.left + m, top: wa.bottom - m, right: wa.left + m + t, bottom: wa.bottom - m }
            }
        };
        crate::clipwatch::prompt(&self.gfx, link, name, manager::default_dir(&self.settings), anchor, self.right());
    }
}
