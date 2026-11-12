//! Windows notifications when a download finishes (Open, Show in Folder) or fails
//! (Try Again, Show in Stash). The Store build has package identity; the installer and
//! portable builds send them under the ID Windows gives their Start menu shortcut (a
//! portable copy adds one).
//! Over a full-screen app, a presentation or Focus Assist they go quietly to the
//! notification center instead of popping up.

use std::cell::RefCell;
use std::collections::HashMap;

use windows::Data::Xml::Dom::XmlDocument;
use windows::Foundation::TypedEventHandler;
use windows::UI::Notifications::{ToastActivatedEventArgs, ToastNotification, ToastNotificationManager, ToastNotifier};
use windows::Win32::UI::Shell::{
    QUNS_BUSY, QUNS_PRESENTATION_MODE, QUNS_QUIET_TIME, QUNS_RUNNING_D3D_FULL_SCREEN, SHQueryUserNotificationState,
};
use windows::core::{HSTRING, IInspectable, Interface};

use crate::dl::{JobView, Snapshot, State};

#[derive(Default)]
struct Toasts {
    notifier: Option<ToastNotifier>,
    tried: bool,
    seen: Option<HashMap<u64, State>>,
    /// Shown toasts, kept alive so their Activated handlers stay registered.
    live: Vec<ToastNotification>,
}

thread_local! {
    static TOASTS: RefCell<Toasts> = RefCell::new(Toasts::default());
}

fn test_copy() -> bool {
    std::env::var_os("STASH_DATA").is_some()
}

/// The ID Windows gives an unpackaged app found through a Start menu shortcut: the exe's
/// path, with a few known folders written as their GUIDs. Sending toasts under it (rather
/// than an ID of our own) keeps taskbar grouping and pinned icons exactly as they are.
pub fn aumid() -> String {
    use windows::Win32::UI::Shell::{
        FOLDERID_ProgramFilesX64, FOLDERID_ProgramFilesX86, FOLDERID_System, FOLDERID_SystemX86, FOLDERID_Windows, KF_FLAG_DEFAULT, SHGetKnownFolderPath,
    };
    let exe = std::env::current_exe().unwrap_or_default().display().to_string();
    for id in [FOLDERID_System, FOLDERID_SystemX86, FOLDERID_ProgramFilesX64, FOLDERID_ProgramFilesX86, FOLDERID_Windows] {
        let Ok(dir) = (unsafe { SHGetKnownFolderPath(&id, KF_FLAG_DEFAULT, None) }) else { continue };
        let dir = unsafe { dir.to_string() }.unwrap_or_default();
        if !dir.is_empty() && exe.len() > dir.len() && exe[..dir.len()].eq_ignore_ascii_case(&dir) && exe[dir.len()..].starts_with('\\') {
            return format!("{{{id:?}}}{}", &exe[dir.len()..]);
        }
    }
    exe
}

/// Whether the Start menu knows the app by this ID (it needs a shortcut there).
fn in_start_menu(aumid: &str) -> bool {
    use windows::Win32::UI::Shell::{IShellItem, SHCreateItemFromParsingName};
    unsafe { SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(format!("shell:AppsFolder\\{aumid}")), None) }.is_ok()
}

/// A portable copy has no Start menu shortcut, and Windows only shows notifications from
/// apps it can find there: add one. (A test copy only with STASH_TEST_SHORTCUT.)
fn add_shortcut() {
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, IPersistFile};
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
    if test_copy() && std::env::var_os("STASH_TEST_SHORTCUT").is_none() {
        return;
    }
    let Some(dir) = std::env::var_os("APPDATA").map(|a| std::path::PathBuf::from(a).join(r"Microsoft\Windows\Start Menu\Programs")) else { return };
    let Ok(exe) = std::env::current_exe() else { return };
    let name = if test_copy() {
        "Stash (test).lnk"
    } else if dir.join("Stash.lnk").exists() {
        "Stash (portable).lnk"
    } else {
        "Stash.lnk"
    };
    let _ = (|| -> windows::core::Result<()> {
        unsafe {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            link.SetPath(&HSTRING::from(exe.as_os_str()))?;
            if let Some(d) = exe.parent() {
                link.SetWorkingDirectory(&HSTRING::from(d.as_os_str()))?;
            }
            link.cast::<IPersistFile>()?.Save(&HSTRING::from(dir.join(name).as_os_str()), true)
        }
    })();
    crate::stash::log(format!("added Start menu shortcut {name}"));
}

/// Something full screen (a game, a video, a presentation) is in front.
pub fn fullscreen() -> bool {
    matches!(unsafe { SHQueryUserNotificationState() }, Ok(s) if s == QUNS_BUSY || s == QUNS_RUNNING_D3D_FULL_SCREEN || s == QUNS_PRESENTATION_MODE)
}

/// Full screen, or Focus Assist / Do not disturb is on.
fn quiet() -> bool {
    fullscreen() || matches!(unsafe { SHQueryUserNotificationState() }, Ok(s) if s == QUNS_QUIET_TIME)
}

fn notifier() -> Option<ToastNotifier> {
    TOASTS.with(|t| {
        let mut t = t.borrow_mut();
        if !t.tried {
            t.tried = true;
            t.notifier = if crate::shell::is_packaged() {
                ToastNotificationManager::CreateToastNotifier().ok()
            } else {
                let id = aumid();
                if !in_start_menu(&id) {
                    add_shortcut();
                }
                ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(id)).ok()
            };
            if t.notifier.is_none() {
                crate::stash::log("notifications unavailable");
            }
        }
        t.notifier.clone()
    })
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

/// The toast's XML. Actions carry "dl:<action>:<id>" (or "downloads"), run by the stack.
fn xml(j: &JobView) -> String {
    let name = esc(if j.name.is_empty() { "Download" } else { &j.name });
    let action = |label: &str, cmd: String| format!(r#"<action content="{}" arguments="{}" activationType="foreground"/>"#, esc(label), esc(&cmd));
    let (title, body, actions) = if j.state == State::Failed {
        let why = j.error.as_deref().map(esc).unwrap_or_else(|| "Something went wrong.".into());
        ("Couldn't download".to_string(), format!("<text>{name}</text><text>{why}</text>"), [action("Try Again", format!("dl:again:{}", j.id)), action("Show in Stash", "downloads".into())])
    } else {
        ("Downloaded".to_string(), format!("<text>{name}</text>"), [action("Open", format!("dl:open:{}", j.id)), action("Show in Folder", format!("dl:reveal:{}", j.id))])
    };
    format!(
        r#"<toast launch="downloads" activationType="foreground"><visual><binding template="ToastGeneric"><text>{title}</text>{body}</binding></visual><actions>{}</actions></toast>"#,
        actions.join("")
    )
}

fn show(j: &JobView) {
    let Some(notifier) = notifier() else { return };
    let result = (|| -> windows::core::Result<ToastNotification> {
        let doc = XmlDocument::new()?;
        doc.LoadXml(&HSTRING::from(xml(j)))?;
        let toast = ToastNotification::CreateToastNotification(&doc)?;
        toast.SetTag(&HSTRING::from(j.id.to_string()))?;
        if quiet() {
            toast.SetSuppressPopup(true)?; // straight to the notification center
        }
        toast.Activated(&TypedEventHandler::<ToastNotification, IInspectable>::new(|_, args| {
            if let Some(args) = args.as_ref().and_then(|a| a.cast::<ToastActivatedEventArgs>().ok()) {
                let cmd = args.Arguments().map(|a| a.to_string()).unwrap_or_default();
                let cmd = if cmd.is_empty() { "downloads".to_string() } else { cmd };
                // runs on a notification thread: hand it to the UI thread
                crate::stash::queue(cmd);
            }
            Ok(())
        }))?;
        notifier.Show(&toast)?;
        Ok(toast)
    })();
    match result {
        Ok(toast) => TOASTS.with(|t| {
            let mut t = t.borrow_mut();
            t.live.push(toast);
            let extra = t.live.len().saturating_sub(16);
            t.live.drain(..extra);
        }),
        Err(e) => crate::stash::log(format!("notification failed: {e}")),
    }
}

/// Downloads that just finished or failed, given the previous states (None: first look).
fn changes<'a>(seen: &mut Option<HashMap<u64, State>>, snap: &'a Snapshot) -> Vec<&'a JobView> {
    let first = seen.is_none();
    let seen = seen.get_or_insert_with(HashMap::new);
    let mut out = Vec::new();
    for j in &snap.jobs {
        let before = seen.insert(j.id, j.state);
        let Some(before) = before else { continue }; // new to us: not a change we watched
        if first {
            continue;
        }
        let finished = j.state.finished() && !before.finished();
        let failed = j.state == State::Failed && before != State::Failed;
        if finished || failed {
            out.push(j);
        }
    }
    seen.retain(|id, _| snap.jobs.iter().any(|j| j.id == *id));
    out
}

/// The engine has news. `enabled`: Settings > Notifications.
pub fn on_snapshot(snap: &Snapshot, enabled: bool) {
    let changed: Vec<JobView> = TOASTS.with(|t| changes(&mut t.borrow_mut().seen, snap).into_iter().cloned().collect());
    if !enabled || changed.is_empty() {
        return;
    }
    // you're looking at the downloads window: it already shows this
    let fg = unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() };
    if crate::manager::hwnd().is_some_and(|h| h == fg) {
        return;
    }
    for j in changed.iter().take(3) {
        show(j);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_names() {
        assert_eq!(esc(r#"a&b<c>"d'"#), "a&amp;b&lt;c&gt;&quot;d&apos;");
    }
}
