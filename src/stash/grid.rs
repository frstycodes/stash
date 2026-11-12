//! Settings > Stack > Open as Grid: instead of fanning out, the stack opens a rounded
//! panel above itself with the files in a grid (like a macOS Stack shown as a grid).
//! The panel grows out of the stack on the same `Progress` spring the fan uses, and
//! its cells are ordinary `Row`s, so hover, click, drag and the keyboard all work as
//! they do for the fan.

use windows::UI::Composition::{CompositionStretch, Visual};
use windows::core::{HSTRING, Interface, Result};
use windows_numerics::{Vector2, Vector3};

use super::{Entry, Row, Stash, job_label};
use crate::dl::State as DlState;
use crate::gfx::color;
use crate::paint::{self, P};
use crate::settings::View;
use crate::shell;

const COLS: usize = 4;
const CELL_W: f32 = 92.0;
const CELL_ICON: f32 = 56.0;
const PAD: f32 = 12.0;
const HEAD: f32 = 46.0;
const RADIUS: f32 = 20.0;
const GAP: f32 = 12.0; // between the panel and the stack

impl Stash {
    pub(super) fn grid(&self) -> bool {
        self.settings.view == View::Grid
    }

    fn cell_size(&self) -> (f32, f32, f32) {
        let (k, ik) = (self.k, self.ik);
        let icon = (CELL_ICON * ik).round();
        let w = (CELL_W * ik.max(k)).round();
        let h = (icon + 10.0 * k + 8.0 * k + 16.0 * k + 8.0 * k).round();
        (w, h, icon)
    }

    /// Build the grid panel and its rows (replaces the fan's rows).
    pub(super) fn build_grid(&mut self) -> Result<()> {
        let k = self.k;
        let c = self.gfx.compositor.clone();
        let right = self.right();
        let (ox, oy) = self.fan_origin();
        let (fw, fh) = self.fan_size;
        let (cell_w, cell_h, icon) = self.cell_size();
        let (pad, head) = ((PAD * k).round(), (HEAD * k).round());
        let tile = self.tile_px();
        let room = fh - tile - GAP * k - head - pad;
        let rows_fit = ((room / cell_h).floor() as usize).max(1);
        let visible = self.visible();
        let n = visible.len().min(rows_fit * COLS);
        let hidden = self.entries().len() - n;
        let grid_rows = n.div_ceil(COLS).max(1);
        let pw = (COLS as f32 * cell_w + pad * 2.0).min(fw);
        let ph = head + grid_rows as f32 * cell_h + pad * 0.5;
        let px = if right { fw - pw } else { 0.0 };
        let py = (oy - tile / 2.0 - GAP * k - ph).max(0.0);
        self.grid_panel = Some((px, py, px + pw, py + ph));

        let panel = c.CreateContainerVisual()?;
        panel.SetSize(Vector2 { X: pw, Y: ph })?;
        panel.SetOffset(Vector3 { X: px, Y: py, Z: 0.0 })?;
        // it grows out of the stack beneath it
        panel.SetCenterPoint(Vector3 { X: ox - px, Y: oy - py, Z: 0.0 })?;
        let expr = |e: &str, prop: &str, target: &Visual| -> Result<()> {
            let a = c.CreateExpressionAnimationWithExpression(&HSTRING::from(e))?;
            a.SetReferenceParameter(&HSTRING::from("p"), &self.props)?;
            target.StartAnimation(&HSTRING::from(prop), &a)
        };
        let pv: Visual = panel.cast()?;
        expr("Vector3(0.6 + 0.4 * p.Progress, 0.6 + 0.4 * p.Progress, 1)", "Scale", &pv)?;
        expr("Clamp(p.Progress * 1.8, 0, 1)", "Opacity", &pv)?;

        // the panel: one dark surface like the stack's labels, the folder's name on top
        let title = shell::folder_name(&self.dir);
        let more = (hidden > 0).then(|| format!("{hidden} more"));
        let dw = self.gfx.dwrite.clone();
        let bg_surface = self.gfx.surface(pw, ph, |dc| {
            let p = P { dc, dw: &dw, k };
            let (w, h) = (pw / k, ph / k);
            p.round(0.0, 0.0, w, h, RADIUS, color(28, 28, 30, 0.94));
            p.round_stroke(0.5, 0.5, w - 1.0, h - 1.0, RADIUS - 0.5, color(255, 255, 255, 0.08), 1.0);
            let tw = p.text_mid(&title, PAD + 6.0, HEAD / 2.0, 14.0, paint::SEMIBOLD, color(255, 255, 255, 1.0), w * 0.45);
            if let Some(m) = &more {
                p.text_mid(m, PAD + 6.0 + tw + 8.0, HEAD / 2.0 + 0.5, 12.0, paint::REGULAR, color(255, 255, 255, 0.5), w * 0.2);
            }
        })?;
        let bg = self.sprite(&bg_surface, pw, ph)?;
        self.shadow(&bg, 24.0, 8.0, 0.45)?;
        panel.Children()?.InsertAtTop(&bg)?;

        let mut rows = Vec::new();
        // "Open in Explorer", at the right of the header
        {
            let text = "Open in Explorer".to_string();
            let (normal, lw, lh) = self.gfx.label(&text, k, false)?;
            let (hover, _, _) = self.gfx.label(&text, k, true)?;
            let label = self.sprite(&normal, lw, lh)?;
            let label_normal = label.Brush()?.cast()?;
            let label_hover = c.CreateSurfaceBrushWithSurface(&hover)?;
            label_hover.SetStretch(CompositionStretch::Fill)?;
            let (lx, ly) = ((pw - pad - lw).round(), ((head - lh) / 2.0 + 2.0 * k).round());
            label.SetOffset(Vector3 { X: lx, Y: ly, Z: 0.0 })?;
            panel.Children()?.InsertAtTop(&label)?;
            rows.push(Row {
                label,
                label_normal,
                label_hover,
                center: (px + lx + lw / 2.0, py + ly + lh / 2.0),
                angle: 0.0,
                extent: (-lw / 2.0, -lh / 2.0, lw / 2.0, lh / 2.0),
                path: None,
                job: None,
                bar: None,
                text,
            });
        }

        for (i, entry) in visible[..n].iter().enumerate() {
            let (col, row) = (i % COLS, i / COLS);
            // newest at the top, nearest the corner the stack sits in
            let col = if right { COLS - 1 - col } else { col };
            let cx = (pad + col as f32 * cell_w).round();
            let cy = (head + row as f32 * cell_h).round();
            let cell = c.CreateContainerVisual()?;
            cell.SetOffset(Vector3 { X: cx, Y: cy, Z: 0.0 })?;
            cell.SetSize(Vector2 { X: cell_w, Y: cell_h })?;

            let name = match entry {
                Entry::File(item) => item.name.clone(),
                Entry::Job(j) => j.name.clone(),
            };
            // the cell's highlight and name, painted together; hover swaps the brush
            let (inset, name_y) = (3.0 * k, 10.0 * k + icon + 8.0 * k);
            let (sw, sh) = (cell_w - inset * 2.0, cell_h - inset * 2.0);
            let paint_cell = |lit: bool| {
                let dw = self.gfx.dwrite.clone();
                let name = name.clone();
                self.gfx.surface(sw, sh, move |dc| {
                    let p = P { dc, dw: &dw, k };
                    let (w, h) = (sw / k, sh / k);
                    if lit {
                        p.round(0.0, 0.0, w, h, 12.0, color(255, 255, 255, 0.12));
                    }
                    let max = w - 10.0;
                    let tw = p.measure(&name, 11.5, paint::REGULAR).min(max);
                    p.text(&name, (w - tw) / 2.0, (name_y - inset) / k, 11.5, paint::REGULAR, color(255, 255, 255, if lit { 1.0 } else { 0.86 }), max);
                })
            };
            let normal = paint_cell(false)?;
            let hover = paint_cell(true)?;
            let label = self.sprite(&normal, sw, sh)?;
            label.SetOffset(Vector3 { X: inset, Y: inset, Z: 0.0 })?;
            let label_normal = label.Brush()?.cast()?;
            let label_hover = c.CreateSurfaceBrushWithSurface(&hover)?;
            label_hover.SetStretch(CompositionStretch::Fill)?;
            cell.Children()?.InsertAtTop(&label)?;

            let (ix, iy) = (((cell_w - icon) / 2.0).round(), (10.0 * k).round());
            let thumb = self.thumb(&entry.key()).cloned();
            if let Some(px) = &thumb {
                let v = self.sprite(&self.gfx.image_surface(px, icon, 0.0, 1.0, 1.0)?, icon, icon)?;
                v.SetOffset(Vector3 { X: ix, Y: iy, Z: 0.0 })?;
                self.shadow(&v, 6.0, 3.0, 0.4)?;
                if let Entry::Job(j) = entry {
                    if j.state != DlState::Active {
                        v.SetOpacity(0.55)?;
                    }
                }
                cell.Children()?.InsertAtTop(&v)?;
            }
            let bar = match entry {
                Entry::Job(j) if j.state != DlState::Failed => {
                    let bh = (4.0 * k).round().max(3.0);
                    let bw = (icon * 0.86).round();
                    let (bx, by) = (((cell_w - bw) / 2.0).round(), iy + icon - bh - 2.0 * k);
                    let track = c.CreateSpriteVisual()?;
                    track.SetBrush(&self.gfx.pill_brush(color(20, 20, 22, 0.85), bh)?)?;
                    track.SetSize(Vector2 { X: bw, Y: bh })?;
                    track.SetOffset(Vector3 { X: bx, Y: by, Z: 0.0 })?;
                    let fill = c.CreateSpriteVisual()?;
                    let tone = if j.state == DlState::Active { paint::accent() } else { color(160, 160, 166, 1.0) };
                    fill.SetBrush(&self.gfx.pill_brush(tone, bh)?)?;
                    fill.SetSize(Vector2 { X: (bw * j.fraction().unwrap_or(0.0)).max(bh), Y: bh })?;
                    fill.SetOffset(Vector3 { X: bx, Y: by, Z: 0.0 })?;
                    cell.Children()?.InsertAtTop(&track)?;
                    cell.Children()?.InsertAtTop(&fill)?;
                    Some(fill)
                }
                _ => None,
            };
            panel.Children()?.InsertAtTop(&cell)?;
            let (hw, hh) = (sw / 2.0, sh / 2.0);
            rows.push(Row {
                label,
                label_normal,
                label_hover,
                center: (px + cx + cell_w / 2.0, py + cy + cell_h / 2.0),
                angle: 0.0,
                extent: (-hw, -hh, hw, hh),
                path: match entry {
                    Entry::File(item) => Some(item.path.clone()),
                    Entry::Job(_) => None,
                },
                job: match entry {
                    Entry::Job(j) => Some(j.id),
                    _ => None,
                },
                bar,
                text: match entry {
                    Entry::Job(j) => job_label(j),
                    Entry::File(_) => name,
                },
            });
        }
        self.fan_layer.Children()?.InsertAtTop(&panel)?;
        self.popping.clear();
        self.rows = rows;
        self.update_fan_region();
        Ok(())
    }

    /// Progress bars glide; the cells stay put.
    pub(super) fn update_grid_jobs(&mut self) -> Result<()> {
        let (_, _, icon) = self.cell_size();
        let width = (icon * 0.86).round();
        for row in &self.rows {
            let (Some(id), Some(bar)) = (row.job, row.bar.as_ref()) else { continue };
            if let Some(j) = self.inflight.iter().find(|j| j.id == id) {
                self.glide_bar(bar, j, width)?;
            }
        }
        Ok(())
    }

    /// The panel's box in fan-window pixels (clicks in its gaps stay ours).
    pub(super) fn over_grid_panel(&self, x: f32, y: f32) -> bool {
        self.grid_panel.is_some_and(|(l, t, r, b)| x >= l && x < r && y >= t && y < b)
    }
}
