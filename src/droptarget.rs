//! OLE drop target for the stack tile: accepts files dragged from Explorer and
//! other apps, shows the system drag image, and reports Move (default) or Copy (Ctrl).

use std::path::PathBuf;

use windows::Win32::Foundation::{HWND, POINT, POINTL};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, CoCreateInstance, DVASPECT_CONTENT, FORMATETC, IDataObject, TYMED_HGLOBAL,
};
use windows::Win32::System::Ole::{
    CF_HDROP, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_MOVE, DROPEFFECT_NONE, IDropTarget,
    IDropTarget_Impl, ReleaseStgMedium,
};
use windows::Win32::System::SystemServices::{MK_CONTROL, MODIFIERKEYS_FLAGS};
use windows::Win32::UI::Shell::{CLSID_DragDropHelper, DragQueryFileW, HDROP, IDropTargetHelper};
use windows::core::{Ref, Result, implement};

pub enum Event {
    Hover(bool),
    Drop { paths: Vec<PathBuf>, copy: bool },
}

#[implement(IDropTarget)]
pub struct DropTarget {
    hwnd: HWND,
    helper: Option<IDropTargetHelper>,
    accepts: std::cell::Cell<bool>,
    on_event: Box<dyn Fn(Event)>,
}

impl DropTarget {
    pub fn new(hwnd: HWND, on_event: impl Fn(Event) + 'static) -> IDropTarget {
        let helper = unsafe { CoCreateInstance(&CLSID_DragDropHelper, None, CLSCTX_INPROC_SERVER).ok() };
        Self { hwnd, helper, accepts: false.into(), on_event: Box::new(on_event) }.into()
    }
}

fn hdrop_format() -> FORMATETC {
    FORMATETC { cfFormat: CF_HDROP.0, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 }
}

fn has_files(data: &IDataObject) -> bool {
    unsafe { data.QueryGetData(&hdrop_format()).is_ok() }
}

fn files(data: &IDataObject) -> Vec<PathBuf> {
    let mut out = Vec::new();
    unsafe {
        let Ok(mut medium) = data.GetData(&hdrop_format()) else { return out };
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

fn effect(accepts: bool, keys: MODIFIERKEYS_FLAGS) -> DROPEFFECT {
    if !accepts {
        DROPEFFECT_NONE
    } else if keys.0 & MK_CONTROL.0 != 0 {
        DROPEFFECT_COPY
    } else {
        DROPEFFECT_MOVE
    }
}

impl IDropTarget_Impl for DropTarget_Impl {
    fn DragEnter(&self, data: Ref<'_, IDataObject>, keys: MODIFIERKEYS_FLAGS, pt: &POINTL, out: *mut DROPEFFECT) -> Result<()> {
        let accepts = data.as_ref().is_some_and(has_files);
        self.accepts.set(accepts);
        let e = effect(accepts, keys);
        unsafe {
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

    fn DragOver(&self, keys: MODIFIERKEYS_FLAGS, pt: &POINTL, out: *mut DROPEFFECT) -> Result<()> {
        let e = effect(self.accepts.get(), keys);
        unsafe {
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

    fn Drop(&self, data: Ref<'_, IDataObject>, keys: MODIFIERKEYS_FLAGS, pt: &POINTL, out: *mut DROPEFFECT) -> Result<()> {
        let e = effect(self.accepts.get(), keys);
        unsafe {
            *out = e;
            if let (Some(h), Some(d)) = (&self.helper, data.as_ref()) {
                let _ = h.Drop(d, &POINT { x: pt.x, y: pt.y }, e);
            }
        }
        (self.on_event)(Event::Hover(false));
        if let Some(d) = data.as_ref() {
            let paths = files(d);
            if !paths.is_empty() {
                (self.on_event)(Event::Drop { paths, copy: e == DROPEFFECT_COPY });
            }
        }
        Ok(())
    }
}
