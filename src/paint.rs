//! Small Direct2D painting helpers for the downloads window. Coordinates are logical
//! pixels; `k` (the DPI scale) is applied here.

use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_COLOR_F};
use windows::Win32::Graphics::Direct2D::{
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ROUNDED_RECT, ID2D1DeviceContext, ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_END_CLOSED};
use windows::Win32::Graphics::Direct2D::ID2D1PathGeometry;
use windows_numerics::Matrix3x2;
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_METRICS,
    DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP, IDWriteFactory, IDWriteTextLayout,
};
use windows::core::{HSTRING, w};
use windows_numerics::Vector2;

pub use crate::gfx::color;

pub const REGULAR: DWRITE_FONT_WEIGHT = DWRITE_FONT_WEIGHT(400);
pub const MEDIUM: DWRITE_FONT_WEIGHT = DWRITE_FONT_WEIGHT(500);
pub const SEMIBOLD: DWRITE_FONT_WEIGHT = DWRITE_FONT_WEIGHT(600);

// The palette: dark, like the stack's pills.
pub fn text() -> D2D1_COLOR_F {
    color(255, 255, 255, 0.92)
}
pub fn dim() -> D2D1_COLOR_F {
    color(255, 255, 255, 0.55)
}
pub fn faint() -> D2D1_COLOR_F {
    color(255, 255, 255, 0.32)
}
// Stash's own colors: the four bars of the icon, a sunset from gold to plum.
pub const GOLD: (u8, u8, u8) = (249, 199, 79);
pub const ORANGE: (u8, u8, u8) = (242, 125, 46);
pub const ROSE: (u8, u8, u8) = (209, 56, 75);
pub const PLUM: (u8, u8, u8) = (106, 44, 112);

pub fn rgb(c: (u8, u8, u8), a: f32) -> D2D1_COLOR_F {
    color(c.0, c.1, c.2, a)
}
pub fn accent() -> D2D1_COLOR_F {
    rgb(ORANGE, 1.0)
}
pub fn accent_hot() -> D2D1_COLOR_F {
    color(250, 150, 80, 1.0)
}


pub fn red() -> D2D1_COLOR_F {
    color(255, 105, 97, 1.0)
}
pub fn green() -> D2D1_COLOR_F {
    color(48, 209, 88, 1.0)
}

/// The UI typeface: DM Sans (SIL Open Font License), built into the exe.
/// Asked for by name only as a fallback label; the real family name is read from the
/// font file (the variable font calls itself "DM Sans 14pt", after its optical size).
pub const FAMILY: &str = "DM Sans";
static DM_SANS: &[u8] = include_bytes!("../assets/fonts/DMSans.ttf");

thread_local! {
    static FAMILY_NAME: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
    static FONTS: std::cell::OnceCell<Option<windows::Win32::Graphics::DirectWrite::IDWriteFontCollection>> = const { std::cell::OnceCell::new() };
}

/// DM Sans as a private font collection (its variable font's named weights become
/// regular faces). None if this Windows can't load in-memory fonts: text then falls
/// back to the system font.
pub fn fonts(dw: &IDWriteFactory) -> Option<windows::Win32::Graphics::DirectWrite::IDWriteFontCollection> {
    use windows::Win32::Graphics::DirectWrite::{IDWriteFactory3, IDWriteFactory5, IDWriteFontCollection};
    use windows::core::Interface;
    FONTS.with(|cell| {
        cell.get_or_init(|| unsafe {
            let f5: IDWriteFactory5 = dw.cast().ok()?;
            let loader = f5.CreateInMemoryFontFileLoader().ok()?;
            f5.RegisterFontFileLoader(&loader).ok()?;
            let file = loader.CreateInMemoryFontFileReference(dw, DM_SANS.as_ptr() as _, DM_SANS.len() as u32, None).ok()?;
            let builder = f5.CreateFontSetBuilder().ok()?;
            builder.AddFontFile(&file).ok()?;
            let set = builder.CreateFontSet().ok()?;
            let f3: IDWriteFactory3 = dw.cast().ok()?;
            let coll = f3.CreateFontCollectionFromFontSet(&set).ok()?.cast::<IDWriteFontCollection>().ok()?;
            // which families the file really holds (STASH_LOG=1)
            for i in 0..coll.GetFontFamilyCount() {
                if let Ok(fam) = coll.GetFontFamily(i) {
                    if let Ok(names) = fam.GetFamilyNames() {
                        let mut buf = [0u16; 64];
                        if names.GetString(0, &mut buf).is_ok() {
                            let n = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
                            let name = String::from_utf16_lossy(&buf[..n]);
                            crate::stash::log(format!("font family {i}: {name} ({} fonts)", fam.GetFontCount()));
                            if i == 0 {
                                FAMILY_NAME.with(|f| *f.borrow_mut() = name);
                            }
                        }
                    }
                }
            }
            Some(coll)
        })
        .clone()
    })
}

/// A text format in DM Sans (or the system font, if DM Sans couldn't load).
pub fn format(dw: &IDWriteFactory, size: f32, weight: DWRITE_FONT_WEIGHT) -> windows::core::Result<windows::Win32::Graphics::DirectWrite::IDWriteTextFormat> {
    let coll = fonts(dw);
    let family = match &coll {
        Some(_) => FAMILY_NAME.with(|f| f.borrow().clone()),
        None => "Segoe UI".to_string(),
    };
    unsafe { dw.CreateTextFormat(&HSTRING::from(family), coll.as_ref(), weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size.max(1.0), w!("en-us")) }
}

pub struct P<'a> {
    pub dc: &'a ID2D1DeviceContext,
    pub dw: &'a IDWriteFactory,
    pub k: f32,
}

impl P<'_> {
    fn brush(&self, c: D2D1_COLOR_F) -> Option<ID2D1SolidColorBrush> {
        unsafe { self.dc.CreateSolidColorBrush(&c, None).ok() }
    }

    fn rect(&self, x: f32, y: f32, w: f32, h: f32) -> D2D_RECT_F {
        let k = self.k;
        D2D_RECT_F { left: x * k, top: y * k, right: (x + w) * k, bottom: (y + h) * k }
    }

    /// A squircle: a rounded rect whose corners are superellipse quadrants, so the curve
    /// eases into the straight edges instead of meeting them at a seam (continuous corners).
    pub fn squircle(&self, x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<ID2D1PathGeometry> {
        let k = self.k;
        let (x, y, w, h) = (x * k, y * k, w * k, h * k);
        // a superellipse corner needs a longer run than a circular one to look as round
        let r = (r * k * 1.6).min(w / 2.0).min(h / 2.0);
        const N: f32 = 5.0;
        const STEPS: usize = 10;
        let mut pts: Vec<Vector2> = Vec::with_capacity(STEPS * 4 + 4);
        // corners clockwise from top-left: center, and the direction of its arc
        let corners = [(x + r, y + r, -1.0, -1.0), (x + w - r, y + r, 1.0, -1.0), (x + w - r, y + h - r, 1.0, 1.0), (x + r, y + h - r, -1.0, 1.0)];
        for (i, (cx, cy, sx, sy)) in corners.into_iter().enumerate() {
            for step in 0..=STEPS {
                // sweep each quadrant in drawing order
                let t = step as f32 / STEPS as f32 * std::f32::consts::FRAC_PI_2;
                let t = if i % 2 == 0 { t } else { std::f32::consts::FRAC_PI_2 - t };
                let (c, sn) = (t.cos().max(0.0), t.sin().max(0.0)); // cos(pi/2) is a hair below 0: NaN under powf
                let px = cx + sx * r * c.powf(2.0 / N);
                let py = cy + sy * r * sn.powf(2.0 / N);
                pts.push(Vector2 { X: px, Y: py });
            }
        }
        // corner 0 runs left edge -> top edge; corner 1 must run top -> right, etc.
        unsafe {
            let factory = self.dc.GetFactory().ok()?;
            let geom = factory.CreatePathGeometry().ok()?;
            let sink = geom.Open().ok()?;
            let ordered = &pts;
            sink.BeginFigure(ordered[0], D2D1_FIGURE_BEGIN_FILLED);
            sink.AddLines(&ordered[1..]);
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            sink.Close().ok()?;
            Some(geom)
        }
    }

    /// A capsule: circular ends (radius = half the height), as a switch is.
    pub fn capsule(&self, x: f32, y: f32, w: f32, h: f32, c: D2D1_COLOR_F) {
        if let Some(b) = self.brush(c) {
            let r = h / 2.0 * self.k;
            let rr = D2D1_ROUNDED_RECT { rect: self.rect(x, y, w, h), radiusX: r, radiusY: r };
            unsafe { self.dc.FillRoundedRectangle(&rr, &b) };
        }
    }

    pub fn round(&self, x: f32, y: f32, w: f32, h: f32, r: f32, c: D2D1_COLOR_F) {
        if let (Some(b), Some(g)) = (self.brush(c), self.squircle(x, y, w, h, r)) {
            unsafe { self.dc.FillGeometry(&g, &b, None) };
        }
    }

    /// The Stash logo pressed into the surface: its bars a shade lighter than the
    /// background under a thin dark rim, no color.
    pub fn logo_imprint(&self, cx: f32, cy: f32, size: f32) {
        // a dark rim just above, the bars a touch lighter than the surface: pressed in
        self.logo_with(cx, cy - 1.2, size, Some(color(0, 0, 0, 0.32)));
        self.logo_with(cx, cy, size, Some(color(255, 255, 255, 0.075)));
    }

    /// The Stash icon: four rounded bars fanning out, `size` across, centered on (cx, cy).
    pub fn logo(&self, cx: f32, cy: f32, size: f32) {
        self.logo_with(cx, cy, size, None);
    }

    /// The logo in its colors, or every bar in `tint`.
    fn logo_with(&self, cx: f32, cy: f32, size: f32, tint: Option<D2D1_COLOR_F>) {
        const BARS: [(f32, f32, f32, (u8, u8, u8)); 4] =
            [(617.6, 885.0, 0.0, PLUM), (590.4, 674.2, -14.73, ROSE), (510.4, 477.2, -29.47, ORANGE), (382.9, 307.1, -44.20, GOLD)];
        let k = self.k;
        let s = size * k / 1024.0;
        let (ox, oy) = ((cx - size / 2.0) * k, (cy - size / 2.0) * k);
        let mut base = Matrix3x2::default();
        unsafe { self.dc.GetTransform(&mut base) };
        for (bx, by, angle, c) in BARS {
            let Some(b) = self.brush(tint.unwrap_or(rgb(c, 1.0))) else { continue };
            let m = Matrix3x2::rotation(angle) * Matrix3x2::translation(bx * s + ox, by * s + oy) * base;
            let rr = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F { left: -272.4 * s, top: -77.0 * s, right: 272.4 * s, bottom: 77.0 * s },
                radiusX: 43.1 * s,
                radiusY: 43.1 * s,
            };
            unsafe {
                self.dc.SetTransform(&m);
                self.dc.FillRoundedRectangle(&rr, &b);
            }
        }
        unsafe { self.dc.SetTransform(&base) };
    }

    pub fn round_stroke(&self, x: f32, y: f32, w: f32, h: f32, r: f32, c: D2D1_COLOR_F, width: f32) {
        let inset = width / 2.0;
        if let (Some(b), Some(g)) = (self.brush(c), self.squircle(x + inset, y + inset, w - width, h - width, r)) {
            unsafe { self.dc.DrawGeometry(&g, &b, width * self.k, None) };
        }
    }

    pub fn fill(&self, x: f32, y: f32, w: f32, h: f32, c: D2D1_COLOR_F) {
        if let Some(b) = self.brush(c) {
            unsafe { self.dc.FillRectangle(&self.rect(x, y, w, h), &b) };
        }
    }

    pub fn layout(&self, s: &str, family: &str, size: f32, weight: DWRITE_FONT_WEIGHT, max_w: f32, center: bool) -> Option<(IDWriteTextLayout, f32, f32)> {
        layout(self.dw, s, family, size * self.k, weight, max_w * self.k, center).map(|(l, w, h)| (l, w / self.k, h / self.k))
    }

    /// Text on one line from (x, y), trimmed with an ellipsis at `max_w`. Returns its width.
    pub fn text(&self, s: &str, x: f32, y: f32, size: f32, weight: DWRITE_FONT_WEIGHT, c: D2D1_COLOR_F, max_w: f32) -> f32 {
        let Some((l, w, _)) = self.layout(s, FAMILY, size, weight, max_w.max(1.0), false) else { return 0.0 };
        self.draw(&l, x, y, c);
        w
    }

    /// Text vertically centered on `cy`.
    pub fn text_mid(&self, s: &str, x: f32, cy: f32, size: f32, weight: DWRITE_FONT_WEIGHT, c: D2D1_COLOR_F, max_w: f32) -> f32 {
        let Some((l, w, h)) = self.layout(s, FAMILY, size, weight, max_w.max(1.0), false) else { return 0.0 };
        self.draw(&l, x, cy - h / 2.0, c);
        w
    }

    /// Text ending at `right`, vertically centered on `cy`. Returns its width.
    pub fn text_right(&self, s: &str, right: f32, cy: f32, size: f32, weight: DWRITE_FONT_WEIGHT, c: D2D1_COLOR_F) -> f32 {
        let w = self.measure(s, size, weight);
        self.text_mid(s, right - w, cy, size, weight, c, w + 1.0)
    }

    pub fn measure(&self, s: &str, size: f32, weight: DWRITE_FONT_WEIGHT) -> f32 {
        self.layout(s, FAMILY, size, weight, 10_000.0, false).map_or(0.0, |(_, w, _)| w)
    }

    /// An icon-font glyph centered on (cx, cy).
    /// A Reicon icon (an SVG body from `icons`) `size` tall, centered on (cx, cy), in `c`.
    /// Drawn by Direct2D's own SVG renderer.
    pub fn glyph(&self, svg: &str, cx: f32, cy: f32, size: f32, c: D2D1_COLOR_F) {
        use windows::Win32::Graphics::Direct2D::Common::D2D_SIZE_F;
        use windows::Win32::Graphics::Direct2D::ID2D1DeviceContext5;
        use windows::core::Interface;
        let k = self.k;
        let px = (size * 1.3 * k).round().max(8.0);
        let hex = format!("#{:02x}{:02x}{:02x}", (c.r * 255.0).round() as u8, (c.g * 255.0).round() as u8, (c.b * 255.0).round() as u8);
        let doc = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="{px}" height="{px}" viewBox="0 0 24 24"><g opacity="{:.3}">{}</g></svg>"#,
            c.a,
            svg.replace("currentColor", &hex)
        );
        unsafe {
            let Ok(dc5) = self.dc.cast::<ID2D1DeviceContext5>() else { return };
            let Some(stream) = windows::Win32::UI::Shell::SHCreateMemStream(Some(doc.as_bytes())) else { return };
            let Ok(svg_doc) = dc5.CreateSvgDocument(&stream, D2D_SIZE_F { width: px, height: px }) else { return };
            let mut base = Matrix3x2::default();
            self.dc.GetTransform(&mut base);
            let (x, y) = ((cx * k - px / 2.0).round(), (cy * k - px / 2.0).round());
            self.dc.SetTransform(&(Matrix3x2::translation(x, y) * base));
            dc5.DrawSvgDocument(&svg_doc);
            self.dc.SetTransform(&base);
        }
    }

    /// A Windows caption glyph (minimize, maximize, restore, close) from the system icon
    /// font, so our caption buttons look like every other window's.
    pub fn caption_glyph(&self, ch: char, cx: f32, cy: f32, size: f32, c: D2D1_COLOR_F) {
        unsafe {
            let mut coll = None;
            let mut family = "Segoe MDL2 Assets";
            if self.dw.GetSystemFontCollection(&mut coll, false).is_ok() {
                if let Some(c) = &coll {
                    let (mut i, mut found) = (0u32, Default::default());
                    if c.FindFamilyName(w!("Segoe Fluent Icons"), &mut i, &mut found).is_ok() && found.as_bool() {
                        family = "Segoe Fluent Icons";
                    }
                }
            }
            let k = self.k;
            let Ok(format) = self.dw.CreateTextFormat(&HSTRING::from(family), None, REGULAR, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size * k, w!("en-us")) else { return };
            let _ = format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
            let wide: Vec<u16> = ch.to_string().encode_utf16().collect();
            let Ok(l) = self.dw.CreateTextLayout(&wide, &format, size * 3.0 * k, size * 2.0 * k) else { return };
            let mut m = DWRITE_TEXT_METRICS::default();
            let _ = l.GetMetrics(&mut m);
            self.draw(&l, cx - size * 1.5, cy - m.height / k / 2.0, c);
        }
    }

    /// Text with some of its characters (by index) marked: a soft accent squircle behind
    /// each run of marked letters, the way search shows what matched. Returns the width.
    pub fn text_marked(&self, s: &str, x: f32, y: f32, size: f32, weight: DWRITE_FONT_WEIGHT, c: D2D1_COLOR_F, max_w: f32, marks: &[usize]) -> f32 {
        use windows::Win32::Graphics::DirectWrite::DWRITE_HIT_TEST_METRICS;
        let Some((l, w, _)) = self.layout(s, FAMILY, size, weight, max_w.max(1.0), false) else { return 0.0 };
        if !marks.is_empty() {
            // character index -> UTF-16 offset
            let mut utf16 = Vec::with_capacity(s.len() + 1);
            let mut n = 0u32;
            for ch in s.chars() {
                utf16.push(n);
                n += ch.len_utf16() as u32;
            }
            utf16.push(n);
            let mut i = 0;
            while i < marks.len() {
                let start = marks[i];
                let mut end = start;
                while i + 1 < marks.len() && marks[i + 1] == end + 1 {
                    end += 1;
                    i += 1;
                }
                i += 1;
                let (Some(&a), Some(&b)) = (utf16.get(start), utf16.get(end + 1)) else { continue };
                let mut m = [DWRITE_HIT_TEST_METRICS::default(); 4];
                let mut count = 0u32;
                unsafe {
                    if l.HitTestTextRange(a, b - a, 0.0, 0.0, Some(&mut m), &mut count).is_err() {
                        continue;
                    }
                }
                for r in &m[..(count as usize).min(4)] {
                    let k = self.k;
                    self.round(x + r.left / k - 1.5, y + r.top / k + 1.0, r.width / k + 3.0, r.height / k - 1.0, 4.0, rgb(ORANGE, 0.3));
                }
            }
        }
        self.draw(&l, x, y, c);
        w
    }

    /// A sparkline of `values` in the box, fading in from its left end.
    pub fn spark(&self, x: f32, y: f32, w: f32, h: f32, values: &[f32], c: D2D1_COLOR_F) {
        use windows::Win32::Graphics::Direct2D::Common::{D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_OPEN, D2D1_GRADIENT_STOP};
        use windows::Win32::Graphics::Direct2D::{
            D2D1_CAP_STYLE_ROUND, D2D1_EXTEND_MODE_CLAMP, D2D1_GAMMA_2_2, D2D1_LINE_JOIN_ROUND, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES, D2D1_STROKE_STYLE_PROPERTIES,
            ID2D1RenderTarget,
        };
        if values.len() < 2 {
            return;
        }
        let k = self.k;
        let (lo, hi) = values.iter().fold((f32::MAX, f32::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
        let span = (hi - lo).max(hi * 0.2).max(1.0);
        let pts: Vec<Vector2> = values
            .iter()
            .enumerate()
            .map(|(i, v)| Vector2 { X: (x + w * i as f32 / (values.len() - 1) as f32) * k, Y: (y + h - 2.0 - ((v - lo) / span) * (h - 4.0)) * k })
            .collect();
        unsafe {
            let Ok(factory) = self.dc.GetFactory() else { return };
            let Ok(geom) = factory.CreatePathGeometry() else { return };
            let Ok(sink) = geom.Open() else { return };
            sink.BeginFigure(pts[0], D2D1_FIGURE_BEGIN_HOLLOW);
            sink.AddLines(&pts[1..]);
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
            let _ = sink.Close();
            let stops = [
                D2D1_GRADIENT_STOP { position: 0.0, color: D2D1_COLOR_F { a: 0.0, ..c } },
                D2D1_GRADIENT_STOP { position: 0.35, color: c },
                D2D1_GRADIENT_STOP { position: 1.0, color: c },
            ];
            let rt: &ID2D1RenderTarget = self.dc;
            let Ok(coll) = rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP) else { return };
            let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: Vector2 { X: x * k, Y: 0.0 }, endPoint: Vector2 { X: (x + w) * k, Y: 0.0 } };
            let Ok(brush) = self.dc.CreateLinearGradientBrush(&props, None, &coll) else { return };
            let style = factory
                .CreateStrokeStyle(
                    &D2D1_STROKE_STYLE_PROPERTIES { startCap: D2D1_CAP_STYLE_ROUND, endCap: D2D1_CAP_STYLE_ROUND, lineJoin: D2D1_LINE_JOIN_ROUND, ..Default::default() },
                    None,
                )
                .ok();
            self.dc.DrawGeometry(&geom, &brush, 1.6 * k, style.as_ref());
        }
    }

    fn draw(&self, l: &IDWriteTextLayout, x: f32, y: f32, c: D2D1_COLOR_F) {
        if let Some(b) = self.brush(c) {
            unsafe { self.dc.DrawTextLayout(Vector2 { X: x * self.k, Y: y * self.k }, l, &b, D2D1_DRAW_TEXT_OPTIONS_NONE) };
        }
    }
}

/// Width of `s` in physical pixels, counting trailing spaces (for the text field's caret).
pub fn advance(dw: &IDWriteFactory, s: &str, size: f32) -> f32 {
    let Some((l, _, _)) = layout(dw, s, FAMILY, size, REGULAR, 100_000.0, false) else { return 0.0 };
    let mut m = DWRITE_TEXT_METRICS::default();
    unsafe {
        let _ = l.GetMetrics(&mut m);
    }
    m.widthIncludingTrailingWhitespace
}

fn layout(dw: &IDWriteFactory, s: &str, family: &str, size: f32, weight: DWRITE_FONT_WEIGHT, max_w: f32, center: bool) -> Option<(IDWriteTextLayout, f32, f32)> {
    unsafe {
        let _ = family;
        let format = format(dw, size, weight).ok()?;
        let _ = format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
        if center {
            let _ = format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
        }
        if let Ok(sign) = dw.CreateEllipsisTrimmingSign(&format) {
            let _ = format.SetTrimming(&DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 }, &sign);
        }
        let wide: Vec<u16> = s.encode_utf16().collect();
        let l = dw.CreateTextLayout(&wide, &format, max_w, size * 2.0).ok()?;
        let mut m = DWRITE_TEXT_METRICS::default();
        l.GetMetrics(&mut m).ok()?;
        Some((l, m.width.min(max_w), m.height))
    }
}

// ---------- formatting ----------

/// Units for every size and speed on screen (a setting): bits for speeds (Mbps, like an
/// internet plan) instead of bytes, and binary sizes (MiB, like File Explorer).
static BITS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static BINARY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_units(bits: bool, binary: bool) {
    BITS.store(bits, std::sync::atomic::Ordering::Relaxed);
    BINARY.store(binary, std::sync::atomic::Ordering::Relaxed);
}

pub fn units() -> (bool, bool) {
    (BITS.load(std::sync::atomic::Ordering::Relaxed), BINARY.load(std::sync::atomic::Ordering::Relaxed))
}

pub fn bytes(n: u64) -> String {
    let n = n as f64;
    let (_, binary) = units();
    let (k, names) = if binary { (1024.0, ["KiB", "MiB", "GiB"]) } else { (1000.0, ["KB", "MB", "GB"]) };
    if n < k {
        format!("{} bytes", n as u64)
    } else if n < k * k {
        format!("{:.0} {}", n / k, names[0])
    } else if n < k * k * k {
        format!("{:.1} {}", n / (k * k), names[1])
    } else {
        format!("{:.2} {}", n / (k * k * k), names[2])
    }
}

pub fn speed(bps: u64) -> String {
    let (bits, binary) = units();
    let n = bps as f64;
    if bits {
        let b = n * 8.0;
        return if b < 1e6 { format!("{:.0} kbps", b / 1e3) } else if b < 1e9 { format!("{:.1} Mbps", b / 1e6) } else { format!("{:.2} Gbps", b / 1e9) };
    }
    let k = if binary { 1024.0 } else { 1000.0 };
    let (kn, mn) = if binary { ("KiB/s", "MiB/s") } else { ("KB/s", "MB/s") };
    if n < k * k { format!("{:.0} {kn}", n / k) } else { format!("{:.1} {mn}", n / (k * k)) }
}

/// The unit the speed limit is set in, and bytes per second per one of it.
pub fn limit_unit() -> (&'static str, f64) {
    match units() {
        (true, _) => ("Mbps", 125_000.0),
        (false, true) => ("MiB/s", 1_048_576.0),
        (false, false) => ("MB/s", 1_000_000.0),
    }
}

pub fn eta_short(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{} min", (secs + 30) / 60),
        _ => format!("{}h {}m", secs / 3600, (secs % 3600) / 60),
    }
}

