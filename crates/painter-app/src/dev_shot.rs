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
    /// `dev.wait:MS`: the commands left wait until then.
    resume_at: Option<std::time::Instant>,
    /// The frame the last commands ran on.
    done_frame: u32,
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
        // A wait that's over: nothing holds the picture back any more.
        if shot.resume_at.is_some_and(|t| std::time::Instant::now() >= t) && shot.commands.is_empty() {
            shot.resume_at = None;
            shot.done_frame = shot.frames;
        }
        let ready = shot.frames >= SETTLE && !shot.commands.is_empty() && shot.resume_at.is_none_or(|t| std::time::Instant::now() >= t);
        if shot.frames == SETTLE && !shot.commands.is_empty() && !shot.commands.iter().any(|c| c == "dev.welcome") {
            // `dev.welcome` keeps the welcome screen; otherwise commands go straight to the canvas.
            self.welcome_open = false;
        }
        if ready {
            shot.resume_at = None;
            shot.done_frame = shot.frames;
            let mut commands = std::mem::take(&mut shot.commands).into_iter();
            for c in commands.by_ref() {
                // `dev.wait:MS`: the rest of the commands later.
                if let Some(ms) = c.strip_prefix("dev.wait:").and_then(|n| n.parse::<u64>().ok()) {
                    if let Some(shot) = self.dev_shot.as_mut() {
                        shot.resume_at = Some(std::time::Instant::now() + std::time::Duration::from_millis(ms));
                        shot.commands = commands.collect();
                    }
                    break;
                }
                // `dev.host:PORT` and `dev.join:ADDRESS`: a shared session.
                if let Some(port) = c.strip_prefix("dev.host:").and_then(|n| n.parse::<u16>().ok()) {
                    self.dev_host(port);
                    continue;
                }
                if let Some(addr) = c.strip_prefix("dev.join:") {
                    self.dev_join(addr);
                    continue;
                }
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
                // `dev.flyout:N`: the tool strip's family N flyout open.
                if let Some(f) = c.strip_prefix("dev.flyout:").and_then(|n| n.parse::<u8>().ok()) {
                    self.tool_groups.dev_open = Some(f);
                    continue;
                }
                // `dev.text_demo`: texts and balloons (once the font scan has had a moment).
                if c == "dev.text_demo" {
                    std::thread::sleep(std::time::Duration::from_millis(1500));
                    self.text_demo();
                    continue;
                }
                // `dev.bitmap`: Mode → Bitmap with an 8 px halftone screen.
                if c == "dev.bitmap" {
                    self.convert_to_bitmap(painter_core::color_mode::BitmapMethod::Halftone { cell: 24.0, angle: 45.0 });
                    continue;
                }
                // `dev.cmyk`: Mode → CMYK Color with the default press profile.
                if c == "dev.cmyk" {
                    if let Some(setup) = crate::cmyk_ui::CmykDialog::new(None).setup() {
                        self.convert_to_cmyk(setup);
                    }
                    continue;
                }
                // `dev.reset`: Reset Settings with its default choice.
                if c == "dev.reset" {
                    self.reset_settings(ctx, Default::default());
                    continue;
                }
                // `dev.color:RRGGBB`: the main colour.
                if let Some(rgb) = c.strip_prefix("dev.color:").and_then(|h| u32::from_str_radix(h, 16).ok()) {
                    self.color = egui::Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8).into();
                    continue;
                }
                // `dev.channel:N`: show and edit only red (0), green (1) or blue (2).
                if let Some(c) = c.strip_prefix("dev.channel:").and_then(|n| n.parse::<usize>().ok()).filter(|c| *c < 3) {
                    self.channels.edit = [false; 3];
                    self.channels.edit[c] = true;
                    self.channels.visible = self.channels.edit;
                    continue;
                }
                // `dev.select_demo`: the magnetic lasso and object selection.
                if c == "dev.select_demo" {
                    self.select_demo();
                    continue;
                }
                // `dev.stamp_demo`: the clone and pattern stamps and the history brush at work.
                if c == "dev.stamp_demo" {
                    self.stamp_demo();
                    continue;
                }
                // `dev.shape_demo`: a filled rounded rectangle, an outlined ellipse and a line.
                if c == "dev.shape_demo" {
                    self.shape_demo();
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
        let idle = shot.commands.is_empty() && shot.resume_at.is_none() && shot.frames >= shot.done_frame + 2 * SETTLE;
        if shot.frames >= SETTLE * 3 && idle && shot.path.is_some() && !shot.requested {
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
            // No "save changes?" question for a screenshot run.
            self.allow_close = true;
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
