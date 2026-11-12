//! The global shortcut that opens the stack from anywhere (Settings > Shortcuts).
//! Written like "Ctrl+Alt+D"; "" or "Off" turns it off.

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, RegisterHotKey, UnregisterHotKey};

pub const ID: i32 = 0x5354;
pub const DEFAULT: &str = "Ctrl+Alt+D";
/// The choices Settings offers (any other combination works from settings.json).
pub const CHOICES: [&str; 3] = ["Ctrl+Alt+D", "Ctrl+Alt+S", "Ctrl+Shift+Space"];

/// "Ctrl+Alt+D" → (modifiers, virtual key). None if it's off or not understood.
pub fn parse(spec: &str) -> Option<(HOT_KEY_MODIFIERS, u32)> {
    let mut mods = HOT_KEY_MODIFIERS(0);
    let mut key = None;
    for part in spec.split('+').map(|p| p.trim().to_ascii_lowercase()) {
        match part.as_str() {
            "ctrl" | "control" => mods |= MOD_CONTROL,
            "alt" => mods |= MOD_ALT,
            "shift" => mods |= MOD_SHIFT,
            "win" | "windows" => mods |= MOD_WIN,
            "space" => key = Some(0x20),
            "" | "off" => return None,
            p if p.len() == 1 && p.as_bytes()[0].is_ascii_alphanumeric() => key = Some(p.as_bytes()[0].to_ascii_uppercase() as u32),
            p if p.starts_with('f') && p[1..].parse::<u32>().is_ok_and(|n| (1..=24).contains(&n)) => {
                key = Some(0x70 + p[1..].parse::<u32>().unwrap() - 1)
            }
            _ => return None,
        }
    }
    // a bare letter would take that key from every app
    if mods.0 & (MOD_CONTROL.0 | MOD_ALT.0 | MOD_WIN.0) == 0 {
        return None;
    }
    Some((mods, key?))
}

/// (Re)register on `hwnd`. False if it's off, not understood, or another app has it.
pub fn register(hwnd: HWND, spec: &str) -> bool {
    unsafe {
        let _ = UnregisterHotKey(Some(hwnd), ID);
    }
    let Some((mods, vk)) = parse(spec) else { return false };
    let ok = unsafe { RegisterHotKey(Some(hwnd), ID, mods | MOD_NOREPEAT, vk) }.is_ok();
    if !ok {
        crate::stash::log(format!("shortcut {spec} is taken"));
    }
    ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses() {
        assert_eq!(parse("Ctrl+Alt+D"), Some((MOD_CONTROL | MOD_ALT, b'D' as u32)));
        assert_eq!(parse("ctrl + shift + space"), Some((MOD_CONTROL | MOD_SHIFT, 0x20)));
        assert_eq!(parse("Win+Alt+F5"), Some((MOD_WIN | MOD_ALT, 0x74)));
        assert_eq!(parse("Shift+D"), None);
        assert_eq!(parse("Off"), None);
        assert_eq!(parse(""), None);
        assert_eq!(parse("Ctrl+Alt+Banana"), None);
    }
}
