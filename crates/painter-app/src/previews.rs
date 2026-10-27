//! Stroke thumbnails for the brush list: each preset paints a short tapered
//! S curve with its own tip, texture and dynamics, at a common size.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use eframe::egui::{self, Color32, ColorImage, TextureHandle, TextureOptions};
use painter_core::raster::paint_stroke;
use painter_core::{blend, BrushMode, BrushSettings, StrokeEngine, StrokePoint, TileStore, TipShape};

pub const PREVIEW_SIZE: [usize; 2] = [200, 34];
/// The larger preview at the foot of Advanced Tool Settings.
pub const DETAIL_PREVIEW_SIZE: [usize; 2] = [420, 64];

/// Rendered previews by brush index, re-rendered when a brush's look changes.
#[derive(Default)]
pub struct StrokePreviews {
    cache: HashMap<usize, (u64, TextureHandle)>,
    /// Brush count the cache was built for; a change (import, delete) clears it.
    brushes: usize,
    /// Pixel size of the previews; `None`: [`PREVIEW_SIZE`].
    size: Option<[usize; 2]>,
}

impl StrokePreviews {
    /// A cache rendering its previews at `size` instead of [`PREVIEW_SIZE`].
    pub fn with_size(size: [usize; 2]) -> Self {
        Self { size: Some(size), ..Self::default() }
    }

    pub fn get(&mut self, ctx: &egui::Context, index: usize, settings: &BrushSettings, total: usize) -> &TextureHandle {
        if total != self.brushes {
            self.cache.clear();
            self.brushes = total;
        }
        let size = self.size.unwrap_or(PREVIEW_SIZE);
        // Above the preview's own thickness cap a size change looks the same.
        let key = fingerprint(settings, size[1] as f32 * 0.5);
        let stale = self.cache.get(&index).is_none_or(|(k, _)| *k != key);
        if stale {
            let image = render(settings, size);
            let texture = ctx.load_texture(format!("stroke_preview_{index}"), image, TextureOptions::LINEAR);
            self.cache.insert(index, (key, texture));
        }
        &self.cache[&index].1
    }
}

/// Paints the preview stroke as white coverage on transparent.
pub fn render(settings: &BrushSettings, [w, h]: [usize; 2]) -> ColorImage {
    let mut s = settings.clone();
    // Same thickness for every brush, so the list compares looks, not sizes.
    s.size = (h as f32 * 0.42).min(settings.size.max(2.0));
    s.mode = BrushMode::Paint;
    s.opacity = s.opacity.max(0.6);
    s.stabilization = 0;
    let (wf, hf) = (w as f32, h as f32);
    let margin = s.size * 0.6 + 2.0;
    let mut engine = StrokeEngine::with_seed(&s, 1);
    let mut dabs = Vec::new();
    let n = 120;
    for i in 0..=n {
        let t = i as f32 / n as f32;
        let x = margin + t * (wf - 2.0 * margin);
        let y = hf * 0.5 - (t * std::f32::consts::TAU).sin() * (hf * 0.5 - margin) * 0.75;
        let pressure = (std::f32::consts::PI * t).sin().powf(0.7) * 0.9 + 0.1;
        dabs.extend(engine.add(StrokePoint::new(x, y, pressure).at_time(t * 0.6)));
    }
    dabs.extend(engine.finish());
    let mut tiles = TileStore::new();
    paint_stroke(&mut tiles, &dabs, [1.0, 1.0, 1.0], &s, (w as u32, h as u32), false, None);
    let pixels = (0..w * h)
        .map(|i| {
            let a = blend::to_f32(tiles.pixel((i % w) as i32, (i / w) as i32))[3];
            Color32::from_white_alpha((a * 255.0).round() as u8)
        })
        .collect();
    ColorImage { size: [w, h], pixels }
}

/// Hash of what the panels can change on a brush. Tip, texture and dual tip
/// bitmaps are told apart by their allocation: loading or resetting one in
/// Advanced Tool Settings makes a new one.
fn fingerprint(s: &BrushSettings, size_cap: f32) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let floats = [
        s.size.min(size_cap),
        s.min_size_ratio,
        s.opacity,
        s.density,
        s.spacing,
        s.hardness,
        s.tip.thickness,
        s.tip.angle,
        s.rotation.jitter,
        s.size_dynamics.jitter,
        s.density_dynamics.jitter,
        s.opacity_dynamics.jitter,
        s.thickness_dynamics.jitter,
    ];
    for f in floats {
        f.to_bits().hash(&mut h);
    }
    for curve in [&s.size_curve, &s.density_curve] {
        for p in curve.points() {
            p[0].to_bits().hash(&mut h);
            p[1].to_bits().hash(&mut h);
        }
    }
    (s.pressure_size, s.pressure_density, s.antialias.name(), s.rotation.random_flip_x, s.rotation.random_flip_y).hash(&mut h);
    (s.rotation.follow_direction, s.scatter.count, s.scatter.both_axes, s.mode == BrushMode::Erase).hash(&mut h);
    for f in [s.scatter.amount, s.scatter.count_jitter, s.taper_start, s.tilt_side] {
        f.to_bits().hash(&mut h);
    }
    // Speed, tilt and rotation responses: small structs, hashed through their debug text.
    format!("{:?}", (&s.size_dynamics, &s.density_dynamics, &s.opacity_dynamics, &s.thickness_dynamics, &s.rotation)).hash(&mut h);
    match &s.tip.shape {
        TipShape::Round => 0usize.hash(&mut h),
        TipShape::Sampled(bitmap) => (Arc::as_ptr(bitmap) as usize).hash(&mut h),
    }
    s.texture.as_ref().map(|t| (Arc::as_ptr(&t.bitmap) as usize, t.scale.to_bits(), t.density.to_bits(), t.invert)).hash(&mut h);
    s.dual.as_ref().map(|d| (Arc::as_ptr(&d.bitmap) as usize, d.scale.to_bits())).hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_has_ink_and_fades_at_the_ends() {
        let image = render(&BrushSettings::g_pen(), PREVIEW_SIZE);
        let [w, h] = image.size;
        let column = |x: usize| (0..h).map(|y| image.pixels[y * w + x].a() as u32).sum::<u32>();
        assert!(column(w / 2) > 255, "ink in the middle");
        assert_eq!(column(0), 0, "margin left clear");
        assert!(column(w / 4) > 0);
    }
}
