//! Small icons (HICON) painted with Direct2D, for places Windows wants a classic icon:
//! the taskbar's overlay badge and its thumbnail toolbar buttons.

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT};
use windows::Win32::Graphics::Direct2D::{
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT, D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT,
    D2D1_RENDER_TARGET_USAGE_NONE, D2D1CreateFactory, ID2D1DeviceContext, ID2D1Factory,
};
use windows::Win32::Graphics::DirectWrite::{DWRITE_FACTORY_TYPE_SHARED, DWriteCreateFactory, IDWriteFactory};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC,
    HGDIOBJ, ReleaseDC, SelectObject,
};
use windows::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, HICON, ICONINFO};
use windows::core::Interface;

use crate::paint::P;

/// An icon `px` pixels square. `draw` paints in a 16×16 logical space (P's `k` maps it).
pub fn paint(px: i32, draw: impl FnOnce(&P)) -> Option<HICON> {
    unsafe {
        let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).ok()?;
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };
        let rt = factory.CreateDCRenderTarget(&props).ok()?;
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: px,
                biHeight: -px,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let screen = GetDC(None);
        let dc = CreateCompatibleDC(Some(screen));
        let mut bits = std::ptr::null_mut();
        let Ok(bmp) = CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) else {
            let _ = DeleteDC(dc);
            ReleaseDC(None, screen);
            return None;
        };
        let old = SelectObject(dc, HGDIOBJ(bmp.0));
        let mut ok = rt.BindDC(dc, &RECT { left: 0, top: 0, right: px, bottom: px }).is_ok();
        if ok {
            rt.BeginDraw();
            rt.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
            if let (Ok(ctx), Ok(dw)) = (rt.cast::<ID2D1DeviceContext>(), DWriteCreateFactory::<IDWriteFactory>(DWRITE_FACTORY_TYPE_SHARED)) {
                draw(&P { dc: &ctx, dw: &dw, k: px as f32 / 16.0 });
            }
            ok = rt.EndDraw(None, None).is_ok();
        }
        SelectObject(dc, old);
        let mut icon = None;
        if ok && !bits.is_null() {
            // icons take straight alpha; Direct2D drew premultiplied
            let px_bytes = std::slice::from_raw_parts_mut(bits as *mut u8, (px * px * 4) as usize);
            for p in px_bytes.chunks_mut(4) {
                let a = p[3] as u32;
                if a > 0 && a < 255 {
                    for c in &mut p[..3] {
                        *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
                    }
                }
            }
            let mask = CreateBitmap(px, px, 1, 1, None);
            let info = ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: bmp };
            icon = CreateIconIndirect(&info).ok();
            let _ = DeleteObject(HGDIOBJ(mask.0));
        }
        let _ = DeleteObject(HGDIOBJ(bmp.0));
        let _ = DeleteDC(dc);
        ReleaseDC(None, screen);
        icon
    }
}
