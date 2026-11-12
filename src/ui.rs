//! Stash's components: one fixed set of styles for every button, link and segmented
//! control in the window, so the same thing always looks and behaves the same.
//! (Switches live in `settings_page`: `new_switch`, `place_switch`, `paint_switch_track`.)

use windows::UI::Composition::{CompositionBrush, SpriteVisual};
use windows::core::Result;

use crate::gfx::Gfx;
use crate::paint::{self, MEDIUM, P, SEMIBOLD};
use crate::settings_page::Rect;

/// What a button is for. Every button is a capsule (an icon-only one, a circle).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// The one action a surface is for: solid orange, white label.
    Primary,
    /// An action that destroys something: solid red, white label.
    Danger,
    /// Everything else, Cancel included: a soft fill.
    Secondary,
    /// Tools that sit in place (the dock's): no fill until hovered.
    Ghost,
}

/// Button heights: in dialogs and empty states, and inside a settings row.
pub const BUTTON_H: f32 = 36.0;
pub const BUTTON_H_SMALL: f32 = 32.0;
const LABEL: f32 = 13.5;
const PAD_X: f32 = 18.0;
const ICON_GAP: f32 = 20.0;

/// The width a button needs for its label (and icon).
pub fn button_w(measure: impl Fn(&str) -> f32, label: &str, icon: bool) -> f32 {
    if label.is_empty() {
        return 0.0;
    }
    (measure(label) + 2.0 * PAD_X + if icon { ICON_GAP } else { 0.0 }).ceil()
}

pub fn button(p: &P, r: Rect, label: &str, icon: Option<&str>, kind: Kind, hot: bool, enabled: bool) {
    let white = paint::color(255, 255, 255, if enabled { 1.0 } else { 0.7 });
    let (fill, ink, weight) = match kind {
        Kind::Primary => (
            match (enabled, hot) {
                (false, _) => paint::rgb(paint::ORANGE, 0.4),
                (true, true) => paint::accent_hot(),
                (true, false) => paint::accent(),
            },
            white,
            SEMIBOLD,
        ),
        Kind::Danger => (
            match (enabled, hot) {
                (false, _) => paint::color(229, 72, 77, 0.4),
                (true, true) => paint::color(236, 93, 97, 1.0),
                (true, false) => paint::color(229, 72, 77, 1.0),
            },
            white,
            SEMIBOLD,
        ),
        Kind::Secondary => (paint::ink(if hot && enabled { 0.13 } else { 0.075 }), if enabled { paint::text() } else { paint::faint() }, MEDIUM),
        Kind::Ghost => (paint::ink(if hot && enabled { 0.09 } else { 0.0 }), if hot { paint::text() } else { paint::dim() }, MEDIUM),
    };
    p.round(r.x, r.y, r.w, r.h, r.h / 2.0, fill);
    let cy = r.y + r.h / 2.0;
    let glyph_size = (r.h * 0.32).clamp(10.0, 14.0);
    match (icon, label.is_empty()) {
        (Some(g), true) => p.glyph(g, r.x + r.w / 2.0, cy, glyph_size, ink),
        (icon, false) => {
            let lw = p.measure(label, LABEL, weight);
            let iw = if icon.is_some() { ICON_GAP } else { 0.0 };
            let x = r.x + (r.w - lw - iw) / 2.0;
            if let Some(g) = icon {
                p.glyph(g, x + 7.0, cy, 11.0, ink);
            }
            p.text_mid(label, x + iw, cy, LABEL, weight, ink, lw + 2.0);
        }
        _ => {}
    }
}

/// A text link (with an optional leading icon), in the link colour. Returns its width.
pub fn link(p: &P, x: f32, cy: f32, label: &str, icon: Option<&str>, hot: bool) -> f32 {
    let ink = if hot { paint::accent_strong() } else { paint::link() };
    let mut w = 0.0;
    if let Some(g) = icon {
        p.glyph(g, x + 6.0, cy, 12.0, ink);
        w += 20.0;
    }
    w + p.text_mid(label, x + w, cy, 13.0, MEDIUM, ink, 400.0)
}

// ---------- segmented controls ----------

pub const SEG_H: f32 = 34.0;
pub const SEG_PAD: f32 = 3.0;
pub const SEG_R: f32 = 13.0;
pub const SEG_IN_R: f32 = SEG_R - SEG_PAD; // concentric
const SEG_GAP: f32 = 2.0;
const SEG_LABEL: f32 = 13.0;

/// Each option's width, from its label.
pub fn seg_widths(opts: &[&str], measure: impl Fn(&str) -> f32) -> Vec<f32> {
    opts.iter().map(|o| (measure(o) + 22.0).max(38.0)).collect()
}

/// The control's box and its options' boxes, from its left edge `x` and middle `cy`.
pub fn seg_layout(x: f32, cy: f32, widths: &[f32]) -> (Rect, Vec<Rect>) {
    let total: f32 = widths.iter().sum::<f32>() + 2.0 * SEG_PAD + SEG_GAP * (widths.len() as f32 - 1.0);
    let rect = Rect::new(x, cy - SEG_H / 2.0, total, SEG_H);
    let mut px = x + SEG_PAD;
    let parts = widths
        .iter()
        .map(|w| {
            let r = Rect::new(px, rect.y + SEG_PAD, *w, SEG_H - 2.0 * SEG_PAD);
            px += w + SEG_GAP;
            r
        })
        .collect();
    (rect, parts)
}

/// The track and the labels. The selection is a sprite that slides (`seg_slider_brush`),
/// or, where nothing slides (a menu that closes on a pick), painted with `selection: true`.
pub fn paint_seg(p: &P, rect: Rect, parts: &[Rect], opts: &[&str], sel: Option<usize>, hover: Option<usize>, selection: bool) {
    p.round(rect.x, rect.y, rect.w, rect.h, SEG_R, paint::well(0.25));
    for (i, (r, label)) in parts.iter().zip(opts).enumerate() {
        let on = sel == Some(i);
        let hot = hover == Some(i);
        if on && selection {
            p.round(r.x, r.y, r.w, r.h, SEG_IN_R, paint::ink(0.13));
        } else if hot && !on {
            p.round(r.x, r.y, r.w, r.h, SEG_IN_R, paint::ink(0.05));
        }
        let lw = p.measure(label, SEG_LABEL, MEDIUM);
        let ink = if on || hot { paint::text() } else { paint::dim() };
        p.text_mid(label, r.x + (r.w - lw) / 2.0, r.y + r.h / 2.0, SEG_LABEL, MEDIUM, ink, r.w);
    }
}

/// The sliding selection's look, for an option `h` tall.
pub fn seg_slider_brush(gfx: &Gfx, h: f32, k: f32) -> Result<CompositionBrush> {
    use windows::core::Interface;
    gfx.shape_brush(paint::ink(0.13), h, SEG_IN_R, k)?.cast()
}

/// Slide (or set) a selection sprite onto option `r`.
pub fn place_seg_slider(gfx: &Gfx, slider: &SpriteVisual, r: Rect, k: f32, animate: bool) -> Result<()> {
    use windows::core::HSTRING;
    use windows_numerics::{Vector2, Vector3};
    let c = &gfx.compositor;
    slider.SetBrush(&seg_slider_brush(gfx, r.h, k)?)?;
    let (to, size) = (Vector3 { X: r.x * k, Y: r.y * k, Z: 0.0 }, Vector2 { X: r.w * k, Y: r.h * k });
    if !animate {
        slider.StopAnimation(&HSTRING::from("Offset"))?;
        slider.StopAnimation(&HSTRING::from("Size"))?;
        slider.SetOffset(to)?;
        return slider.SetSize(size);
    }
    let ease = c.CreateCubicBezierEasingFunction(Vector2 { X: 0.3, Y: 1.18 }, Vector2 { X: 0.6, Y: 1.0 })?;
    let dur = windows::Foundation::TimeSpan { Duration: 420 * 10_000 };
    let a = c.CreateVector3KeyFrameAnimation()?;
    a.InsertExpressionKeyFrame(0.0, &HSTRING::from("this.StartingValue"))?;
    a.InsertKeyFrameWithEasingFunction(1.0, to, &ease)?;
    a.SetDuration(dur)?;
    slider.StartAnimation(&HSTRING::from("Offset"), &a)?;
    let b = c.CreateVector2KeyFrameAnimation()?;
    b.InsertExpressionKeyFrame(0.0, &HSTRING::from("this.StartingValue"))?;
    b.InsertKeyFrameWithEasingFunction(1.0, size, &ease)?;
    b.SetDuration(dur)?;
    slider.StartAnimation(&HSTRING::from("Size"), &b)
}
