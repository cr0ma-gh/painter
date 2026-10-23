//! A developer aid for checking the interface without clicking through it:
//!
//! - `PAINTER_COMMANDS="view.ruler_bar;tool.4"` runs those commands (palette
//!   ids, `;`-separated; `dev.float:N` floats panel N) once the window is up;
//! - `PAINTER_SCREENSHOT=out.png` then saves the window as a PNG and quits.
//!
//! Neither is set in normal use, and then this does nothing.

use eframe::egui;

use crate::app::PainterApp;

/// Frames to wait before running the commands, and before the picture.
const SETTLE: u32 = 20;

#[derive(Default)]
pub(crate) struct DevShot {
    frames: u32,
    commands: Vec<String>,
    path: Option<std::path::PathBuf>,
    requested: bool,
}

impl DevShot {
    pub(crate) fn from_env() -> Option<Self> {
        let commands: Vec<String> =
            std::env::var("PAINTER_COMMANDS").ok()?.split(';').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect();
        Some(Self { commands, path: std::env::var_os("PAINTER_SCREENSHOT").map(Into::into), ..Default::default() })
    }

    fn screenshot_only() -> Option<Self> {
        Some(Self { path: Some(std::env::var_os("PAINTER_SCREENSHOT")?.into()), ..Default::default() })
    }

    pub(crate) fn new() -> Option<Self> {
        Self::from_env().or_else(Self::screenshot_only)
    }
}

impl PainterApp {
    pub(crate) fn dev_shot_tick(&mut self, ctx: &egui::Context) {
        let Some(shot) = &mut self.dev_shot else { return };
        shot.frames += 1;
        ctx.request_repaint();
        if shot.frames == SETTLE {
            let commands = std::mem::take(&mut shot.commands);
            for c in commands {
                // `dev.float:N`: panel N (EditorType::ALL order) as a large floating window.
                if let Some(i) = c.strip_prefix("dev.float:").and_then(|n| n.parse::<usize>().ok()) {
                    if let Some(e) = ui_workspace::EditorType::ALL.get(i) {
                        self.ws.set.current_mut().add_floating(*e, [480.0, 120.0], [1000.0, 700.0], 1.0);
                    }
                    continue;
                }
                // `dev.file_object:PATH`: a file object layer showing PATH.
                if let Some(path) = c.strip_prefix("dev.file_object:") {
                    self.add_file_object(std::path::Path::new(path));
                    continue;
                }
                // `dev.vector_demo`: a vector layer of crossing lines, one picked in Vector Points.
                if c == "dev.vector_demo" {
                    self.vector_demo();
                    continue;
                }
                self.run_command(&c);
            }
            return;
        }
        let Some(shot) = &mut self.dev_shot else { return };
        if shot.frames == SETTLE * 3 && shot.path.is_some() && !shot.requested {
            shot.requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
        let image = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let (Some(image), Some(path)) = (image, shot.path.clone()) {
            let [w, h] = image.size;
            let rgba: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
            if let Some(img) = image::RgbaImage::from_raw(w as u32, h as u32, rgba) {
                let _ = img.save(&path);
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

impl PainterApp {
    /// A vector layer with a few crossing lines, the curved one picked in Vector Points.
    fn vector_demo(&mut self) {
        let (w, h) = self.doc.canvas();
        let (w, h) = (w as f32, h as f32);
        let id = self.doc.alloc_id();
        let mut layer = painter_core::Layer::vector(id, "Vector demo");
        let settings = painter_core::BrushSettings { size: w / 120.0, stabilization: 0, ..painter_core::BrushSettings::g_pen() };
        let line = |pts: Vec<[f32; 2]>| painter_core::VectorStroke {
            points: pts.into_iter().map(|[x, y]| painter_core::StrokePoint::new(x, y, 1.0)).collect(),
            settings: settings.clone(),
            color: [0.1, 0.1, 0.1],
            seed: 0,
        };
        let strokes = vec![
            line((0..=60).map(|i| i as f32 / 60.0).map(|t| [w * (0.15 + 0.7 * t), h * (0.45 - 0.15 * (t * std::f32::consts::PI).sin())]).collect()),
            line((0..=30).map(|i| i as f32 / 30.0).map(|t| [w * 0.35, h * (0.2 + 0.5 * t)]).collect()),
            line((0..=30).map(|i| i as f32 / 30.0).map(|t| [w * 0.65, h * (0.2 + 0.5 * t)]).collect()),
        ];
        if let painter_core::LayerKind::Vector(v) = &mut layer.kind {
            v.strokes = strokes;
            painter_core::vector::rasterize(v, (w as u32, h as u32));
        }
        self.doc.add_layer(layer);
        self.doc.active = id;
        self.upload_plane(painter_render::Plane::Color(id));
        self.tool = crate::tools::Tool::VectorEdit;
        self.vector_edit.picked = Some((id, 0));
    }
}
