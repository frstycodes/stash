//! The downloads window's drop target: links (dragged from a browser's address bar or
//! a page, as text or as a URL) and .torrent files. Dropping one opens New Download
//! with it filled in. It always reports Copy (or Link, when that's all the source
//! allows), never Move: the dragged thing stays where it was.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::{HWND, POINT, POINTL};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, DVASPECT_CONTENT, FORMATETC, IDataObject, TYMED_HGLOBAL};
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::Ole::{CF_HDROP, CF_UNICODETEXT, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_NONE, IDropTarget, IDropTarget_Impl, ReleaseStgMedium};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Shell::{CLSID_DragDropHelper, DragQueryFileW, HDROP, IDropTargetHelper};
use windows::core::{Ref, Result, implement, w};

/// Set while the window drags its own files out, so they don't drop back in.
pub static SUPPRESS: AtomicBool = AtomicBool::new(false);

pub enum Event {
    Hover(bool),
    Drop { links: Vec<String>, torrents: Vec<PathBuf> },
}

#[implement(IDropTarget)]
pub struct LinkDrop {
    hwnd: HWND,
    helper: Option<IDropTargetHelper>,
    effect: std::cell::Cell<DROPEFFECT>,
    on_event: Box<dyn Fn(Event)>,
}

impl LinkDrop {
    pub fn new(hwnd: HWND, on_event: impl Fn(Event) + 'static) -> IDropTarget {
        let helper = unsafe { CoCreateInstance(&CLSID_DragDropHelper, None, CLSCTX_INPROC_SERVER).ok() };
        Self { hwnd, helper, effect: DROPEFFECT_NONE.into(), on_event: Box::new(on_event) }.into()
    }
}

fn format(cf: u16) -> FORMATETC {
    FORMATETC { cfFormat: cf, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 }
}

fn url_format() -> u16 {
    unsafe { RegisterClipboardFormatW(w!("UniformResourceLocatorW")) as u16 }
}

/// A UTF-16 string in an HGLOBAL of this format, if the data has one.
fn text_of(data: &IDataObject, cf: u16) -> Option<String> {
    unsafe {
        let mut medium = data.GetData(&format(cf)).ok()?;
        let g = medium.u.hGlobal;
        let p = GlobalLock(g) as *const u16;
        let out = if p.is_null() {
            None
        } else {
            let n = GlobalSize(g) / 2;
            let s = std::slice::from_raw_parts(p, n);
            let end = s.iter().position(|c| *c == 0).unwrap_or(n);
            Some(String::from_utf16_lossy(&s[..end]))
        };
        let _ = GlobalUnlock(g);
        ReleaseStgMedium(&mut medium);
        out
    }
}

fn files(data: &IDataObject) -> Vec<PathBuf> {
    let mut out = Vec::new();
    unsafe {
        let Ok(mut medium) = data.GetData(&format(CF_HDROP.0)) else { return out };
        let hdrop = HDROP(medium.u.hGlobal.0);
        for i in 0..DragQueryFileW(hdrop, u32::MAX, None) {
            let len = DragQueryFileW(hdrop, i, None) as usize;
            let mut buf = vec![0u16; len + 1];
            DragQueryFileW(hdrop, i, Some(&mut buf));
            out.push(PathBuf::from(String::from_utf16_lossy(&buf[..len])));
        }
        ReleaseStgMedium(&mut medium);
    }
    out
}

/// What a drop carries that Stash can download: links, and .torrent files.
fn contents(data: &IDataObject) -> (Vec<String>, Vec<PathBuf>) {
    let torrents: Vec<PathBuf> = files(data).into_iter().filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("torrent"))).collect();
    let text = text_of(data, url_format()).or_else(|| text_of(data, CF_UNICODETEXT.0)).unwrap_or_default();
    (links_in(&text), torrents)
}

/// The downloadable links in dropped text, in order, without repeats.
pub fn links_in(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in text.split_whitespace() {
        let t = t.trim_matches(|c| matches!(c, '"' | '\'' | '<' | '>' | '(' | ')'));
        let lower = t.to_ascii_lowercase();
        let link = lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("magnet:?");
        if link && !out.iter().any(|o| o == t) {
            out.push(t.to_string());
        }
    }
    out
}

/// Copy where the source allows it, else Link; never Move.
fn effect_for(allowed: DROPEFFECT) -> DROPEFFECT {
    if allowed.0 & DROPEFFECT_COPY.0 != 0 {
        DROPEFFECT_COPY
    } else if allowed.0 & DROPEFFECT_LINK.0 != 0 {
        DROPEFFECT_LINK
    } else {
        DROPEFFECT_NONE
    }
}

impl IDropTarget_Impl for LinkDrop_Impl {
    fn DragEnter(&self, data: Ref<'_, IDataObject>, _keys: MODIFIERKEYS_FLAGS, pt: &POINTL, out: *mut DROPEFFECT) -> Result<()> {
        let accepts = !SUPPRESS.load(Ordering::Relaxed) && data.as_ref().is_some_and(|d| {
            let (links, torrents) = contents(d);
            !links.is_empty() || !torrents.is_empty()
        });
        unsafe {
            let e = if accepts { effect_for(*out) } else { DROPEFFECT_NONE };
            self.effect.set(e);
            *out = e;
            if let (Some(h), Some(d)) = (&self.helper, data.as_ref()) {
                let _ = h.DragEnter(self.hwnd, d, &POINT { x: pt.x, y: pt.y }, e);
            }
        }
        if accepts {
            (self.on_event)(Event::Hover(true));
        }
        Ok(())
    }

    fn DragOver(&self, _keys: MODIFIERKEYS_FLAGS, pt: &POINTL, out: *mut DROPEFFECT) -> Result<()> {
        unsafe {
            let e = if self.effect.get() == DROPEFFECT_NONE { DROPEFFECT_NONE } else { effect_for(*out) };
            *out = e;
            if let Some(h) = &self.helper {
                let _ = h.DragOver(&POINT { x: pt.x, y: pt.y }, e);
            }
        }
        Ok(())
    }

    fn DragLeave(&self) -> Result<()> {
        if let Some(h) = &self.helper {
            unsafe {
                let _ = h.DragLeave();
            }
        }
        (self.on_event)(Event::Hover(false));
        Ok(())
    }

    fn Drop(&self, data: Ref<'_, IDataObject>, _keys: MODIFIERKEYS_FLAGS, pt: &POINTL, out: *mut DROPEFFECT) -> Result<()> {
        let e = if self.effect.get() == DROPEFFECT_NONE { DROPEFFECT_NONE } else { unsafe { effect_for(*out) } };
        unsafe {
            *out = e;
            if let (Some(h), Some(d)) = (&self.helper, data.as_ref()) {
                let _ = h.Drop(d, &POINT { x: pt.x, y: pt.y }, e);
            }
        }
        (self.on_event)(Event::Hover(false));
        if e != DROPEFFECT_NONE {
            if let Some(d) = data.as_ref() {
                let (links, torrents) = contents(d);
                if !links.is_empty() || !torrents.is_empty() {
                    (self.on_event)(Event::Drop { links, torrents });
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_links() {
        assert_eq!(links_in("see <https://a.com/x.zip> and magnet:?xt=urn:btih:abc https://a.com/x.zip"), vec!["https://a.com/x.zip", "magnet:?xt=urn:btih:abc"]);
        assert!(links_in("just words, ftp://nope").is_empty());
    }

    #[test]
    fn never_move() {
        use windows::Win32::System::Ole::DROPEFFECT_MOVE;
        assert_eq!(effect_for(DROPEFFECT(DROPEFFECT_COPY.0 | DROPEFFECT_MOVE.0)), DROPEFFECT_COPY);
        assert_eq!(effect_for(DROPEFFECT(DROPEFFECT_LINK.0 | DROPEFFECT_MOVE.0)), DROPEFFECT_LINK);
        assert_eq!(effect_for(DROPEFFECT_MOVE), DROPEFFECT_NONE);
    }
}
