//! The downloads window's taskbar button: overall progress (green while downloading,
//! yellow when everything left is paused, red after a failure you haven't seen), a badge
//! with the number of active downloads, and Pause all / Resume all under its thumbnail.

use std::cell::RefCell;
use std::collections::HashSet;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::UI::Shell::{
    ITaskbarList3, TBPF_ERROR, TBPF_INDETERMINATE, TBPF_NOPROGRESS, TBPF_NORMAL, TBPF_PAUSED, TBPFLAG, THB_FLAGS, THB_ICON, THB_TOOLTIP,
    THBF_DISABLED, THBF_ENABLED, THUMBBUTTON, TaskbarList,
};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, HICON, RegisterWindowMessageW, WM_ACTIVATE, WM_COMMAND};
use windows::core::{HSTRING, w};

use crate::dl::{Snapshot, State};

const BTN_PAUSE: u32 = 1;
const BTN_RESUME: u32 = 2;
const THBN_CLICKED: usize = 0x1800;

#[derive(Clone, Copy, PartialEq, Default)]
struct Look {
    flag: i32,
    permille: u64,
    active: usize,
    can_pause: bool,
    can_resume: bool,
}

#[derive(Default)]
struct Bar {
    list: Option<ITaskbarList3>,
    buttons: bool,
    shown: Option<Look>,
    want: Look,
    /// Downloads that failed since you last looked at the window.
    failed: HashSet<u64>,
    seen: Option<std::collections::HashMap<u64, State>>,
    icons: Vec<HICON>,
}

thread_local! {
    static BAR: RefCell<Bar> = RefCell::new(Bar::default());
}

fn button_created() -> u32 {
    use std::sync::OnceLock;
    static MSG: OnceLock<u32> = OnceLock::new();
    *MSG.get_or_init(|| unsafe { RegisterWindowMessageW(w!("TaskbarButtonCreated")) })
}

/// What the taskbar should show for these downloads (state, bytes done, size).
fn look(jobs: &[(State, u64, Option<u64>)], failed: bool) -> Look {
    let mut l = Look::default();
    let left: Vec<_> = jobs.iter().filter(|j| !j.0.finished() && j.0 != State::Failed).collect();
    l.active = jobs.iter().filter(|j| j.0 == State::Active).count();
    l.can_pause = left.iter().any(|j| matches!(j.0, State::Active | State::Queued));
    l.can_resume = jobs.iter().any(|j| matches!(j.0, State::Paused | State::Failed));
    let total: u64 = left.iter().filter_map(|j| j.2).sum();
    let done: u64 = left.iter().filter_map(|j| j.2.map(|t| j.1.min(t))).sum();
    l.permille = if total == 0 { 0 } else { done * 1000 / total };
    let flag = if l.can_pause {
        if total == 0 { TBPF_INDETERMINATE } else { TBPF_NORMAL }
    } else if !left.is_empty() {
        TBPF_PAUSED
    } else if failed {
        l.permille = 1000;
        TBPF_ERROR
    } else {
        TBPF_NOPROGRESS
    };
    // a failure you haven't seen turns an otherwise running bar red too
    l.flag = if failed && flag != TBPF_NOPROGRESS { TBPF_ERROR.0 } else { flag.0 };
    l
}

fn shape(snap: &Snapshot) -> Vec<(State, u64, Option<u64>)> {
    snap.jobs.iter().map(|j| (j.state, j.done, j.total)).collect()
}

/// The engine has news.
pub fn update(snap: &Snapshot) {
    BAR.with(|b| {
        let mut b = b.borrow_mut();
        let first = b.seen.is_none();
        let seen = b.seen.get_or_insert_with(Default::default);
        let mut newly_failed = Vec::new();
        for j in &snap.jobs {
            let before = seen.insert(j.id, j.state);
            if !first && j.state == State::Failed && before.is_some_and(|s| s != State::Failed) {
                newly_failed.push(j.id);
            }
        }
        b.failed.extend(newly_failed);
        // gone, retried or removed: no longer a failure to show
        b.failed.retain(|id| snap.jobs.iter().any(|j| j.id == *id && j.state == State::Failed));
        b.want = look(&shape(snap), !b.failed.is_empty());
    });
    apply();
}

fn tip(text: &str) -> [u16; 260] {
    let mut out = [0u16; 260];
    for (o, c) in out.iter_mut().zip(text.encode_utf16().take(259)) {
        *o = c;
    }
    out
}

fn glyph_icon(svg: &'static str) -> Option<HICON> {
    let px = (16.0 * crate::shell::scale_factor(crate::manager::hwnd().unwrap_or_default())).round() as i32;
    crate::sysicon::paint(px.max(16), |p| p.glyph(svg, 8.0, 8.0, 12.0, crate::gfx::color(255, 255, 255, 1.0)))
}

fn badge_icon(n: usize) -> Option<HICON> {
    let px = (16.0 * crate::shell::scale_factor(crate::manager::hwnd().unwrap_or_default())).round() as i32;
    let text = if n > 9 { "9+".to_string() } else { n.to_string() };
    crate::sysicon::paint(px.max(16), |p| {
        p.round(0.0, 0.0, 16.0, 16.0, 8.0, crate::paint::accent());
        let size = if n > 9 { 8.5 } else { 10.5 };
        let w = p.measure(&text, size, crate::paint::SEMIBOLD);
        p.text_mid(&text, (16.0 - w) / 2.0, 8.0, size, crate::paint::SEMIBOLD, crate::gfx::color(255, 255, 255, 1.0), 16.0);
    })
}

fn buttons(l: &Look, icons: &[HICON]) -> [THUMBBUTTON; 2] {
    let mask = THB_ICON | THB_TOOLTIP | THB_FLAGS;
    [
        THUMBBUTTON {
            dwMask: mask,
            iId: BTN_PAUSE,
            hIcon: icons.first().copied().unwrap_or_default(),
            szTip: tip("Pause all"),
            dwFlags: if l.can_pause { THBF_ENABLED } else { THBF_DISABLED },
            ..Default::default()
        },
        THUMBBUTTON {
            dwMask: mask,
            iId: BTN_RESUME,
            hIcon: icons.get(1).copied().unwrap_or_default(),
            szTip: tip("Resume all"),
            dwFlags: if l.can_resume { THBF_ENABLED } else { THBF_DISABLED },
            ..Default::default()
        },
    ]
}

/// Push the wanted look to the taskbar, if it has a button for us and something changed.
fn apply() {
    let Some(hwnd) = crate::manager::hwnd() else { return };
    BAR.with(|b| {
        let mut b = b.borrow_mut();
        let Some(list) = b.list.clone() else { return };
        let want = b.want;
        let shown = b.shown;
        if shown == Some(want) {
            return;
        }
        unsafe {
            if b.icons.is_empty() {
                b.icons = [glyph_icon(crate::icons::PAUSE), glyph_icon(crate::icons::PLAY)].into_iter().flatten().collect();
            }
            if !b.buttons {
                b.buttons = list.ThumbBarAddButtons(hwnd, &buttons(&want, &b.icons)).is_ok();
            } else if shown.is_none_or(|s| s.can_pause != want.can_pause || s.can_resume != want.can_resume) {
                let _ = list.ThumbBarUpdateButtons(hwnd, &buttons(&want, &b.icons));
            }
            if shown.is_none_or(|s| s.flag != want.flag || s.permille != want.permille) {
                let _ = list.SetProgressState(hwnd, TBPFLAG(want.flag));
                if want.flag != TBPF_NOPROGRESS.0 && want.flag != TBPF_INDETERMINATE.0 {
                    let _ = list.SetProgressValue(hwnd, want.permille, 1000);
                }
            }
            if shown.is_none_or(|s| s.active != want.active) {
                if want.active == 0 {
                    let _ = list.SetOverlayIcon(hwnd, HICON::default(), None);
                } else if let Some(icon) = badge_icon(want.active) {
                    let label = HSTRING::from(if want.active == 1 { "1 download".to_string() } else { format!("{} downloads", want.active) });
                    let _ = list.SetOverlayIcon(hwnd, icon, &label);
                    let _ = DestroyIcon(icon); // the taskbar keeps its own copy
                }
            }
        }
        b.shown = Some(want);
    });
}

/// From the downloads window's window procedure. Some(result) when it was ours.
pub fn on_message(_hwnd: HWND, msg: u32, wparam: WPARAM, _lparam: LPARAM) -> Option<LRESULT> {
    if msg == button_created() {
        // a fresh button (the window was shown again): set it up from scratch
        BAR.with(|b| {
            let mut b = b.borrow_mut();
            if b.list.is_none() {
                b.list = unsafe { CoCreateInstance::<_, ITaskbarList3>(&TaskbarList, None, CLSCTX_INPROC_SERVER) }.ok().filter(|l| unsafe { l.HrInit() }.is_ok());
            }
            b.buttons = false;
            b.shown = None;
        });
        apply();
        return Some(LRESULT(0));
    }
    match msg {
        WM_COMMAND if (wparam.0 >> 16) & 0xffff == THBN_CLICKED => {
            let e = crate::dl::engine_opt()?;
            match (wparam.0 & 0xffff) as u32 {
                BTN_PAUSE => e.pause_all(),
                BTN_RESUME => e.resume_all(),
                _ => return None,
            }
            Some(LRESULT(0))
        }
        // you've looked at the window: failures stop showing red
        WM_ACTIVATE if wparam.0 & 0xffff != 0 => {
            let had = BAR.with(|b| {
                let mut b = b.borrow_mut();
                let had = !b.failed.is_empty();
                b.failed.clear();
                had
            });
            if had {
                if let Some(snap) = crate::dl::engine_opt().map(|e| e.snapshot()) {
                    BAR.with(|b| {
                        let mut b = b.borrow_mut();
                        b.want = look(&shape(&snap), false);
                    });
                    apply();
                }
            }
            None
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_paused_failed_idle() {
        let l = look(&[(State::Active, 50, Some(100)), (State::Paused, 0, Some(100))], false);
        assert_eq!((l.flag, l.permille, l.active, l.can_pause, l.can_resume), (TBPF_NORMAL.0, 250, 1, true, true));
        let l = look(&[(State::Paused, 30, Some(100))], false);
        assert_eq!((l.flag, l.permille, l.can_pause), (TBPF_PAUSED.0, 300, false));
        assert_eq!(look(&[(State::Active, 0, None)], false).flag, TBPF_INDETERMINATE.0);
        let l = look(&[(State::Done, 1, Some(1))], false);
        assert_eq!((l.flag, l.active), (TBPF_NOPROGRESS.0, 0));
        assert_eq!(look(&[(State::Failed, 0, Some(1))], true).flag, TBPF_ERROR.0);
        assert_eq!(look(&[(State::Active, 1, Some(2))], true).flag, TBPF_ERROR.0);
    }
}
