//! Graphics plumbing: the Windows compositor (Visual layer) for everything on
//! screen, Direct2D/DirectWrite to paint pixels into composition surfaces.

use windows::Foundation::Size;
use windows::Graphics::DirectX::{DirectXAlphaMode, DirectXPixelFormat};
use windows::System::DispatcherQueueController;
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{CompositionDrawingSurface, CompositionGraphicsDevice, Compositor};
use windows::Win32::Foundation::{HMODULE, HWND, POINT};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F,
    D2D1_COMPOSITE_MODE_SOURCE_OVER, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    CLSID_D2D1ColorMatrix, CLSID_D2D1GaussianBlur, CLSID_D2D1Saturation, D2D1_BITMAP_OPTIONS_NONE,
    D2D1_BITMAP_PROPERTIES1, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC,
    D2D1_PROPERTY_TYPE_FLOAT, D2D1_PROPERTY_TYPE_MATRIX_5X4, D2D1_ROUNDED_RECT, D2D1CreateFactory,
    ID2D1Bitmap1, ID2D1DeviceContext, ID2D1Factory1, ID2D1Image,
};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_METRICS, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER,
    DWRITE_WORD_WRAPPING_NO_WRAP, DWriteCreateFactory, IDWriteFactory, IDWriteTextLayout,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::System::WinRT::Composition::{
    ICompositionDrawingSurfaceInterop, ICompositorDesktopInterop, ICompositorInterop,
};
use windows::Win32::System::WinRT::{
    CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT, DispatcherQueueOptions,
};
use windows::core::{Interface, Result, w};
use windows_numerics::{Matrix3x2, Vector2};

/// A decoded thumbnail: premultiplied BGRA, top-down rows.
#[derive(Clone)]
pub struct Pixels {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

pub struct Gfx {
    pub compositor: Compositor,
    device: CompositionGraphicsDevice,
    dwrite: IDWriteFactory,
    _d3d: ID3D11Device,
    _queue: DispatcherQueueController,
}

pub fn color(r: u8, g: u8, b: u8, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: r as f32 / 255.0, g: g as f32 / 255.0, b: b as f32 / 255.0, a }
}

fn create_d3d(kind: D3D_DRIVER_TYPE) -> Result<ID3D11Device> {
    let mut device = None;
    unsafe {
        D3D11CreateDevice(None, kind, HMODULE::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT, None, D3D11_SDK_VERSION, Some(&mut device), None, None)?;
    }
    device.ok_or_else(|| windows::core::Error::from_win32())
}

impl Gfx {
    pub fn new() -> Result<Self> {
        unsafe {
            // The compositor needs a dispatcher queue on this (UI) thread.
            let queue = CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })?;
            let compositor = Compositor::new()?;

            let d3d = create_d3d(D3D_DRIVER_TYPE_HARDWARE).or_else(|_| create_d3d(D3D_DRIVER_TYPE_WARP))?;
            let dxgi: IDXGIDevice = d3d.cast()?;
            let factory: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let d2d = factory.CreateDevice(&dxgi)?;
            let device = compositor.cast::<ICompositorInterop>()?.CreateGraphicsDevice(&d2d)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            Ok(Self { compositor, device, dwrite, _d3d: d3d, _queue: queue })
        }
    }

    pub fn target(&self, hwnd: HWND) -> Result<DesktopWindowTarget> {
        unsafe { self.compositor.cast::<ICompositorDesktopInterop>()?.CreateDesktopWindowTarget(hwnd, false) }
    }

    /// A new composition surface of `w`×`h` pixels, painted by `draw`.
    pub fn surface(&self, w: f32, h: f32, draw: impl FnOnce(&ID2D1DeviceContext)) -> Result<CompositionDrawingSurface> {
        let surface = self.device.CreateDrawingSurface(
            Size { Width: w.max(1.0).ceil(), Height: h.max(1.0).ceil() },
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            DirectXAlphaMode::Premultiplied,
        )?;
        unsafe {
            let interop: ICompositionDrawingSurfaceInterop = surface.cast()?;
            let mut offset = POINT::default();
            let dc: ID2D1DeviceContext = interop.BeginDraw(None, &mut offset)?;
            dc.SetTransform(&Matrix3x2::translation(offset.x as f32, offset.y as f32));
            dc.Clear(Some(&color(0, 0, 0, 0.0)));
            draw(&dc);
            interop.EndDraw()?;
        }
        Ok(surface)
    }

    pub fn bitmap(dc: &ID2D1DeviceContext, px: &Pixels) -> Result<ID2D1Bitmap1> {
        unsafe {
            dc.CreateBitmap(
                D2D_SIZE_U { width: px.width, height: px.height },
                Some(px.bgra.as_ptr() as *const _),
                px.width * 4,
                &D2D1_BITMAP_PROPERTIES1 {
                    pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                    dpiX: 96.0,
                    dpiY: 96.0,
                    bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
                    colorContext: std::mem::ManuallyDrop::new(None),
                },
            )
        }
    }

    /// Thumbnail scaled to fit a `size`-pixel square, optionally blurred, dimmed
    /// and desaturated (the pile's depth of field).
    pub fn image_surface(&self, px: &Pixels, size: f32, blur: f32, brightness: f32, saturation: f32) -> Result<CompositionDrawingSurface> {
        // The surface is whole pixels; draw to fill all of it, so stretching it onto a
        // `size` sprite shows the image at exactly `size`.
        let size = size.max(1.0).ceil();
        self.surface(size, size, |dc| unsafe {
            let Ok(bitmap) = Self::bitmap(dc, px) else { return };
            let k = size / px.width.max(px.height) as f32;
            let (w, h) = (px.width as f32 * k, px.height as f32 * k);
            let (x, y) = ((size - w) / 2.0, (size - h) / 2.0);
            let mut base = Matrix3x2::default();
            dc.GetTransform(&mut base);

            if blur == 0.0 && brightness == 1.0 && saturation == 1.0 {
                let dest = D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h };
                dc.DrawBitmap(&bitmap, Some(&dest), 1.0, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None);
                return;
            }
            // effects chain: saturation -> brightness (color matrix) -> gaussian blur
            let (Ok(sat), Ok(matrix), Ok(gauss)) = (
                dc.CreateEffect(&CLSID_D2D1Saturation),
                dc.CreateEffect(&CLSID_D2D1ColorMatrix),
                dc.CreateEffect(&CLSID_D2D1GaussianBlur),
            ) else {
                return;
            };
            sat.SetInput(0, &bitmap.cast::<ID2D1Image>().unwrap(), true);
            let _ = sat.SetValue(0, D2D1_PROPERTY_TYPE_FLOAT, &saturation.to_le_bytes());
            let Ok(sat_out) = sat.GetOutput() else { return };
            matrix.SetInput(0, &sat_out, true);
            let b = brightness;
            let m: [f32; 20] = [b, 0.0, 0.0, 0.0, 0.0, b, 0.0, 0.0, 0.0, 0.0, b, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0];
            let bytes: Vec<u8> = m.iter().flat_map(|f| f.to_le_bytes()).collect();
            let _ = matrix.SetValue(0, D2D1_PROPERTY_TYPE_MATRIX_5X4, &bytes);
            let Ok(matrix_out) = matrix.GetOutput() else { return };
            gauss.SetInput(0, &matrix_out, true);
            let _ = gauss.SetValue(0, D2D1_PROPERTY_TYPE_FLOAT, &(blur / k).to_le_bytes());
            let Ok(out) = gauss.GetOutput() else { return };
            dc.SetTransform(&(Matrix3x2::scale(k, k) * Matrix3x2::translation(x, y) * base));
            dc.DrawImage(&out, None, None, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, D2D1_COMPOSITE_MODE_SOURCE_OVER);
        })
    }

    /// Text laid out on one line, trimmed with an ellipsis at `max_w`.
    pub fn text(&self, text: &str, size: f32, max_w: f32) -> Result<(IDWriteTextLayout, f32, f32)> {
        unsafe {
            let format = self.dwrite.CreateTextFormat(
                w!("Segoe UI"),
                None,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                w!("en-us"),
            )?;
            format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            let sign = self.dwrite.CreateEllipsisTrimmingSign(&format)?;
            format.SetTrimming(&DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 }, &sign)?;
            let wide: Vec<u16> = text.encode_utf16().collect();
            let layout = self.dwrite.CreateTextLayout(&wide, &format, max_w, size * 2.0)?;
            let mut m = DWRITE_TEXT_METRICS::default();
            layout.GetMetrics(&mut m)?;
            Ok((layout, m.width.min(max_w), m.height))
        }
    }

    /// The name pill beside a fan icon. Returns the surface and its size.
    pub fn label(&self, text: &str, k: f32, hover: bool) -> Result<(CompositionDrawingSurface, f32, f32)> {
        let pad = 10.0 * k;
        let h = 24.0 * k;
        let (layout, tw, th) = self.text(text, 12.0 * k, 220.0 * k - pad * 2.0)?;
        let w = (tw + pad * 2.0).ceil();
        let surface = self.surface(w, h, |dc| unsafe {
            let rect = D2D1_ROUNDED_RECT { rect: D2D_RECT_F { left: 0.5, top: 0.5, right: w - 0.5, bottom: h - 0.5 }, radiusX: 7.0 * k, radiusY: 7.0 * k };
            let fill = if hover { color(72, 72, 76, 0.95) } else { color(28, 28, 30, 0.9) };
            let edge = if hover { color(255, 255, 255, 0.18) } else { color(255, 255, 255, 0.08) };
            if let (Ok(f), Ok(e), Ok(t)) = (
                dc.CreateSolidColorBrush(&fill, None),
                dc.CreateSolidColorBrush(&edge, None),
                dc.CreateSolidColorBrush(&color(255, 255, 255, 1.0), None),
            ) {
                dc.FillRoundedRectangle(&rect, &f);
                dc.DrawRoundedRectangle(&rect, &e, 1.0, None);
                dc.DrawTextLayout(Vector2 { X: pad, Y: (h - th) / 2.0 }, &layout, &t, Default::default());
            }
        })?;
        Ok((surface, w, h))
    }

    /// The pill shown above the stack when switching folders: the folder's name, and a
    /// row of dots with the current one lit. Returns the surface and its size.
    pub fn badge(&self, text: &str, count: usize, current: usize, k: f32) -> Result<(CompositionDrawingSurface, f32, f32)> {
        use windows::Win32::Graphics::Direct2D::D2D1_ELLIPSE;
        let pad = 12.0 * k;
        let (layout, tw, th) = self.text(text, 13.0 * k, 220.0 * k)?;
        let (dot, gap) = (5.0 * k, 5.0 * k);
        let dots_w = count as f32 * dot + count.saturating_sub(1) as f32 * gap;
        let w = (tw.max(dots_w) + pad * 2.0).ceil();
        let h = (7.0 * k + th + 5.0 * k + dot + 9.0 * k).ceil();
        let surface = self.surface(w, h, |dc| unsafe {
            let rect = D2D1_ROUNDED_RECT { rect: D2D_RECT_F { left: 0.5, top: 0.5, right: w - 0.5, bottom: h - 0.5 }, radiusX: 9.0 * k, radiusY: 9.0 * k };
            if let (Ok(fill), Ok(edge), Ok(text_brush), Ok(dim)) = (
                dc.CreateSolidColorBrush(&color(28, 28, 30, 0.92), None),
                dc.CreateSolidColorBrush(&color(255, 255, 255, 0.1), None),
                dc.CreateSolidColorBrush(&color(255, 255, 255, 1.0), None),
                dc.CreateSolidColorBrush(&color(255, 255, 255, 0.32), None),
            ) {
                dc.FillRoundedRectangle(&rect, &fill);
                dc.DrawRoundedRectangle(&rect, &edge, 1.0, None);
                dc.DrawTextLayout(Vector2 { X: (w - tw) / 2.0, Y: 7.0 * k }, &layout, &text_brush, Default::default());
                let y = 7.0 * k + th + 5.0 * k + dot / 2.0;
                let x0 = (w - dots_w) / 2.0 + dot / 2.0;
                for i in 0..count {
                    let e = D2D1_ELLIPSE { point: Vector2 { X: x0 + i as f32 * (dot + gap), Y: y }, radiusX: dot / 2.0, radiusY: dot / 2.0 };
                    dc.FillEllipse(&e, if i == current { &text_brush } else { &dim });
                }
            }
        })?;
        Ok((surface, w, h))
    }

    /// The "Open in Explorer" button: a dark circle with an outward arrow.
    pub fn open_button(&self, size: f32, k: f32) -> Result<CompositionDrawingSurface> {
        self.surface(size, size, |dc| unsafe {
            use windows::Win32::Graphics::Direct2D::D2D1_ELLIPSE;
            let r = size / 2.0;
            let ellipse = D2D1_ELLIPSE { point: Vector2 { X: r, Y: r }, radiusX: r - 1.0, radiusY: r - 1.0 };
            if let (Ok(f), Ok(e), Ok(a)) = (
                dc.CreateSolidColorBrush(&color(58, 58, 60, 1.0), None),
                dc.CreateSolidColorBrush(&color(255, 255, 255, 0.22), None),
                dc.CreateSolidColorBrush(&color(255, 255, 255, 1.0), None),
            ) {
                dc.FillEllipse(&ellipse, &f);
                dc.DrawEllipse(&ellipse, &e, 1.0, None);
                let s = size * 0.2;
                let p = |x: f32, y: f32| Vector2 { X: r + x * s, Y: r + y * s };
                let stroke = 2.6 * k;
                dc.DrawLine(p(-1.0, 1.0), p(1.0, -1.0), &a, stroke, None);
                dc.DrawLine(p(-0.2, -1.0), p(1.0, -1.0), &a, stroke, None);
                dc.DrawLine(p(1.0, -1.0), p(1.0, 0.2), &a, stroke, None);
            }
        })
    }
}
