//! Side-by-side documents: a Reference View area shows another open
//! document (line art to copy, a model sheet, a 3D render) beside the canvas,
//! with its own zoom, pan and mirror. It is a picture of the document,
//! rendered on a worker thread and refreshed when that document changes;
//! clicking "Edit" makes it the active document instead. Layers dragged onto
//! it are copied into that document.

use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver};

use eframe::egui::{self, Color32, ColorImage, Pos2, Rect, Sense, TextureHandle, TextureOptions, Vec2};
use painter_core::io;
use ui_workspace::tiles::{EditorType, SplitKind};

use super::layer_transfer::LayerDrag;
use crate::app::PainterApp;
use crate::theme;

/// Longest side of a reference picture.
const PREVIEW_SIZE: u32 = 2048;

/// A rendered picture of a parked document.
struct Preview {
    revision: u64,
    texture: TextureHandle,
    /// The document's size in pixels.
    doc_size: Vec2,
}

struct PreviewJob {
    id: u64,
    revision: u64,
    doc_size: Vec2,
    rx: Receiver<ColorImage>,
}

#[derive(Default)]
pub(crate) struct ReferenceView {
    /// The tab shown (its slot id).
    pub(crate) doc: Option<u64>,
    /// Points per document pixel; `None` fits the area.
    zoom: Option<f32>,
    pan: Vec2,
    flip: bool,
    previews: HashMap<u64, Preview>,
    jobs: Vec<PreviewJob>,
}

/// `pixels` (premultiplied RGBA8, `w` × `h`) shrunk by an integer box filter
/// so the longest side is at most `max`.
fn downscale(pixels: &[u8], w: u32, h: u32, max: u32) -> ColorImage {
    let step = w.max(h).div_ceil(max).max(1) as usize;
    let (ow, oh) = ((w as usize).div_ceil(step), (h as usize).div_ceil(step));
    let mut out = vec![Color32::TRANSPARENT; ow * oh];
    for oy in 0..oh {
        for ox in 0..ow {
            let mut sum = [0u32; 4];
            let mut n = 0;
            for y in oy * step..((oy + 1) * step).min(h as usize) {
                for x in ox * step..((ox + 1) * step).min(w as usize) {
                    let i = (y * w as usize + x) * 4;
                    for c in 0..4 {
                        sum[c] += pixels[i + c] as u32;
                    }
                    n += 1;
                }
            }
            let n = n.max(1);
            out[oy * ow + ox] =
                Color32::from_rgba_premultiplied((sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8, (sum[3] / n) as u8);
        }
    }
    ColorImage { size: [ow, oh], pixels: out }
}

impl PainterApp {
    /// Shows tab `id` in a Reference View, adding one left of the canvas if
    /// the workspace has none.
    pub(crate) fn show_in_reference(&mut self, id: u64) {
        self.docs.reference.doc = Some(id);
        self.docs.reference.zoom = None;
        self.docs.reference.pan = Vec2::ZERO;
        let tiles = &mut self.ws.set.current_mut().tiles;
        if let Some(area) = tiles.find(EditorType::Reference) {
            tiles.add_tab(area, EditorType::Reference);
        } else if let Some(canvas) = tiles.find(EditorType::Canvas) {
            tiles.split(canvas, SplitKind::SideBySide, 0.5, EditorType::Reference, true);
        }
    }

    /// Starts rendering pictures that are missing or stale; collects finished ones.
    pub(crate) fn poll_reference_previews(&mut self, ctx: &egui::Context) {
        let r = &mut self.docs.reference;
        let mut finished = Vec::new();
        r.jobs.retain(|job| match job.rx.try_recv() {
            Ok(image) => {
                finished.push((job.id, job.revision, job.doc_size, image));
                false
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => true,
            Err(_) => false,
        });
        for (id, revision, doc_size, image) in finished {
            let texture = ctx.load_texture(format!("reference-{id}"), image, TextureOptions::LINEAR);
            r.previews.insert(id, Preview { revision, texture, doc_size });
        }
        let Some(id) = r.doc else { return };
        let Some(slot) = self.docs.slots.iter().find(|s| s.id == id) else {
            self.docs.reference.doc = None;
            return;
        };
        let Some(session) = &slot.parked else { return };
        let r = &self.docs.reference;
        let fresh = r.previews.get(&id).is_some_and(|p| p.revision == slot.revision);
        let running = r.jobs.iter().any(|j| j.id == id && j.revision == slot.revision);
        if fresh || running {
            return;
        }
        let doc = session.doc.clone();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let pixels = io::flatten(&doc);
            let _ = tx.send(downscale(&pixels, doc.width, doc.height, PREVIEW_SIZE));
        });
        let doc_size = Vec2::new(session.doc.width as f32, session.doc.height as f32);
        let revision = slot.revision;
        self.docs.reference.jobs.push(PreviewJob { id, revision, doc_size, rx });
    }

    /// The Reference View editor.
    pub(crate) fn reference_view_ui(&mut self, ui: &mut egui::Ui) {
        // Which document: any open tab but the one being edited.
        let others: Vec<(u64, String)> = (0..self.docs.slots.len())
            .filter(|&i| i != self.docs.active)
            .map(|i| (self.docs.slots[i].id, self.tab_title(i)))
            .collect();
        if self.docs.reference.doc.is_none_or(|id| !others.iter().any(|(o, _)| *o == id)) {
            self.docs.reference.doc = others.first().map(|(id, _)| *id);
        }
        let mut edit = false;
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            let current = self.docs.reference.doc.and_then(|id| others.iter().find(|(o, _)| *o == id)).map(|(_, n)| n.clone());
            egui::ComboBox::from_id_salt("reference_doc")
                .selected_text(current.unwrap_or_else(|| "No other document".into()))
                .show_ui(ui, |ui| {
                    for (id, name) in &others {
                        let selected = self.docs.reference.doc == Some(*id);
                        if ui.selectable_label(selected, name).clicked() {
                            self.docs.reference.doc = Some(*id);
                            self.docs.reference.zoom = None;
                            self.docs.reference.pan = Vec2::ZERO;
                        }
                    }
                });
            ui.checkbox(&mut self.docs.reference.flip, "Mirror");
            if ui.small_button("Fit").clicked() {
                self.docs.reference.zoom = None;
                self.docs.reference.pan = Vec2::ZERO;
            }
            edit = ui
                .add_enabled(self.docs.reference.doc.is_some(), egui::Button::new("Edit"))
                .on_hover_text("Make it the active document")
                .clicked();
        });
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, theme::canvas_backdrop());
        let Some(id) = self.docs.reference.doc else {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Open another document to see it here",
                egui::FontId::proportional(13.0),
                theme::text_dim(),
            );
            return;
        };
        if edit {
            if let Some(i) = self.docs.index_of(id) {
                self.switch_document(i);
            }
            return;
        }
        // Layers dropped here go into the shown document.
        if let Some(payload) = response.dnd_release_payload::<LayerDrag>() {
            if let Some(i) = self.docs.index_of(id) {
                self.transfer_layer_to(i, payload.layer);
            }
        }
        if response.dnd_hover_payload::<LayerDrag>().is_some() {
            painter.rect_stroke(rect.shrink(2.0), 0.0, egui::Stroke::new(2.0_f32, theme::accent()), egui::StrokeKind::Inside);
        }
        let r = &mut self.docs.reference;
        let Some(preview) = r.previews.get(&id) else {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Rendering...",
                egui::FontId::proportional(13.0),
                theme::text_dim(),
            );
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
            return;
        };
        let fit = ((rect.width() - 16.0) / preview.doc_size.x).min((rect.height() - 16.0) / preview.doc_size.y).max(0.01);
        let mut zoom = r.zoom.unwrap_or(fit);
        if response.hovered() {
            let scroll = ui.input(|i| i.raw_scroll_delta.y);
            if scroll != 0.0 {
                // Zoom about the pointer.
                let factor = (scroll * 0.0015).exp();
                let new = (zoom * factor).clamp(fit * 0.1, 32.0);
                if let Some(p) = response.hover_pos() {
                    let from_center = p - rect.center() - r.pan;
                    r.pan -= from_center * (new / zoom - 1.0);
                }
                zoom = new;
                r.zoom = Some(zoom);
            }
        }
        if response.dragged() {
            r.pan += response.drag_delta();
        }
        let size = preview.doc_size * zoom;
        let shown = Rect::from_center_size(rect.center() + r.pan, size);
        let uv = if r.flip {
            Rect::from_min_max(Pos2::new(1.0, 0.0), Pos2::new(0.0, 1.0))
        } else {
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0))
        };
        painter.image(preview.texture.id(), shown, uv, Color32::WHITE);
        painter.text(
            rect.left_bottom() + Vec2::new(6.0, -4.0),
            egui::Align2::LEFT_BOTTOM,
            format!("{:.0}%{}", zoom * 100.0, if r.flip { " · mirrored" } else { "" }),
            egui::FontId::proportional(11.0),
            theme::text_dim(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downscale_averages_blocks() {
        // 4 × 2: left half opaque red, right half transparent.
        let mut px = vec![0u8; 4 * 2 * 4];
        for y in 0..2 {
            for x in 0..2 {
                let i = (y * 4 + x) * 4;
                px[i] = 255;
                px[i + 3] = 255;
            }
        }
        let img = downscale(&px, 4, 2, 2);
        assert_eq!(img.size, [2, 1]);
        assert_eq!(img.pixels[0], Color32::from_rgba_premultiplied(255, 0, 0, 255));
        assert_eq!(img.pixels[1], Color32::TRANSPARENT);
        assert_eq!(downscale(&px, 4, 2, 100).size, [4, 2], "small pictures stay as they are");
    }
}
