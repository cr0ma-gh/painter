//! CPU dab rasterisation and stroke merging.
//!
//! A stroke paints into its own float buffer at per-dab `density`; each touched
//! tile is then rebuilt as `merge(layer_before_stroke, buffer × opacity)`. That
//! is what makes opacity a per-stroke ceiling, and it works the same for
//! erasing and for transparency-locked layers.
//!
//! The buffer is f32 (the GPU preview uses `Rgba16Float`) so that many faint,
//! overlapping dabs build up smoothly instead of stalling at 8-bit rounding
//! steps. Pixels are quantised to 8 bits once, when merged into the layer.

use std::collections::{BTreeSet, HashMap};
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};

use crate::blend::{to_f32, to_u8, Rgba};
use crate::brush::{BrushBlend, BrushMode, BrushSettings, Dab};
use crate::tile::{Tile, TileCoord, TileStore, TILE_PIXELS, TILE_SIZE};
use crate::tip::{TipBitmap, TipKind};

/// Coverage of a pixel whose centre is `d` px from the dab centre.
/// Mirrored by `coverage` in painter-render's `dab.wgsl`.
fn coverage(d: f32, radius: f32, hardness: f32, aa: f32) -> f32 {
    let edge = if aa <= 0.0 {
        if d <= radius {
            1.0
        } else {
            0.0
        }
    } else {
        ((radius - d) / aa + 0.5).clamp(0.0, 1.0)
    };
    if edge <= 0.0 {
        return 0.0;
    }
    let inner = radius * hardness.clamp(0.0, 1.0);
    let soft = if d <= inner || radius - inner <= 1e-3 {
        1.0
    } else {
        let t = ((d - inner) / (radius - inner)).clamp(0.0, 1.0);
        1.0 - t * t * (3.0 - 2.0 * t)
    };
    edge * soft
}

/// Coverage of a flattened round tip at tip-local offset `(x, y)`, where `y`
/// is already divided by `flatness` (so the ellipse is the unit circle × radius).
/// The edge uses the first-order distance to the ellipse, so anti-aliasing
/// stays `aa` pixels wide across the thin axis too.
/// Mirrored by `ellipse_coverage` in painter-render's `dab.wgsl`.
fn ellipse_coverage(x: f32, y: f32, radius: f32, flatness: f32, hardness: f32, aa: f32) -> f32 {
    let n = x.hypot(y) / radius;
    let edge = if aa <= 0.0 {
        if n <= 1.0 {
            1.0
        } else {
            0.0
        }
    } else {
        let gradient = x.hypot(y / flatness);
        if gradient <= 0.0 {
            1.0
        } else {
            // The ellipse lies inside the radius circle, so it is never nearer than that circle:
            // this keeps coverage inside `radius + aa`, the stamped square.
            let circle = x.hypot(y * flatness) - radius;
            let distance = ((n - 1.0) * radius * radius * n / gradient).max(circle);
            (0.5 - distance / aa).clamp(0.0, 1.0)
        }
    };
    if edge <= 0.0 {
        return 0.0;
    }
    let inner = hardness.clamp(0.0, 1.0);
    let soft = if n <= inner || radius * (1.0 - inner) <= 1e-3 {
        1.0
    } else {
        let t = ((n - inner) / (1.0 - inner)).clamp(0.0, 1.0);
        1.0 - t * t * (3.0 - 2.0 * t)
    };
    edge * soft
}

/// Coverage of a sampled tip at tip-local offset `(x, y)` (`y` already divided
/// by the flatness): the bitmap's longer side spans the dab diameter.
/// Mirrored by `sampled_coverage` in painter-render's `dab.wgsl`.
fn sampled_coverage(bitmap: &TipBitmap, x: f32, y: f32, radius: f32, level: u32) -> f32 {
    let aspect = bitmap.aspect();
    let (w, h) = bitmap.level_size(level);
    let fx = x / (2.0 * radius * aspect[0]) + 0.5;
    let fy = y / (2.0 * radius * aspect[1]) + 0.5;
    bitmap.sample(level, fx * w as f32 - 0.5, fy * h as f32 - 0.5)
}

/// Sparse premultiplied RGBA f32 tiles holding one stroke.
#[derive(Default, Debug)]
pub struct StrokeBuffer {
    tiles: HashMap<TileCoord, Box<[Rgba]>>,
}

impl StrokeBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// A tile's pixels, row-major.
    pub fn get(&self, coord: TileCoord) -> Option<&[Rgba]> {
        self.tiles.get(&coord).map(|t| &t[..])
    }

    pub fn pixel(&self, x: i32, y: i32) -> Rgba {
        let coord = TileCoord::containing(x, y);
        let (ox, oy) = coord.origin();
        self.get(coord).map_or([0.0; 4], |t| t[((y - oy) as u32 * TILE_SIZE + (x - ox) as u32) as usize])
    }

    /// A tile's pixels for writing, allocated transparent if absent.
    pub fn tile_mut(&mut self, coord: TileCoord) -> &mut [Rgba] {
        self.tiles.entry(coord).or_insert_with(|| vec![[0.0; 4]; TILE_PIXELS].into_boxed_slice())
    }
}

/// Stamps one dab of opaque `color` into `buffer` with the brush's tip, clipped to the canvas.
pub fn stamp_dab(
    buffer: &mut StrokeBuffer,
    dab: &Dab,
    color: [f32; 3],
    settings: &BrushSettings,
    canvas: (u32, u32),
    dirty: &mut BTreeSet<TileCoord>,
) {
    let dab = dab.normalized();
    let (radius, alpha) = (dab.radius, dab.alpha);
    if alpha <= 0.0 {
        return;
    }
    let aa = settings.antialias.width();
    let tip = &settings.tip;

    let level = tip.mip_level(radius);
    let reach = tip.reach(radius, aa, level);
    let ([cos, sin], flatness) = (tip.rotation(dab.angle), tip.flatness_for(dab.thickness));
    let kind = tip.kind_for(flatness);
    let dual = settings.dual.as_ref().map(|d| (d, d.radius(radius), d.mip_level(radius)));
    let x0 = ((dab.x - reach).floor() as i32).max(0);
    let y0 = ((dab.y - reach).floor() as i32).max(0);
    let x1 = ((dab.x + reach).ceil() as i32).min(canvas.0 as i32 - 1);
    let y1 = ((dab.y + reach).ceil() as i32).min(canvas.1 as i32 - 1);
    if x0 > x1 || y0 > y1 {
        return;
    }
    let (t0, t1) = (TileCoord::containing(x0, y0), TileCoord::containing(x1, y1));
    let last = TILE_SIZE as i32 - 1;
    for ty in t0.y..=t1.y {
        for tx in t0.x..=t1.x {
            let coord = TileCoord::new(tx, ty);
            let (ox, oy) = coord.origin();
            let tile = buffer.tile_mut(coord);
            let mut touched = false;
            for py in y0.max(oy)..=y1.min(oy + last) {
                for px in x0.max(ox)..=x1.min(ox + last) {
                    let (dx, dy) = (px as f32 + 0.5 - dab.x, py as f32 + 0.5 - dab.y);
                    let mut cov = if kind == TipKind::Disc {
                        coverage(dx.hypot(dy), radius, settings.hardness, aa)
                    } else {
                        // Into tip space: undo the rotation, then the flattening.
                        let x = (cos * dx - sin * dy) * if dab.mirror { -1.0 } else { 1.0 };
                        let y = (sin * dx + cos * dy) / flatness;
                        match tip.bitmap() {
                            Some(bitmap) => sampled_coverage(bitmap, x, y, radius, level),
                            None => ellipse_coverage(x, y, radius, flatness, settings.hardness, aa),
                        }
                    };
                    // Dual tip: an unrotated second bitmap centred on the dab masks it.
                    if let Some((dual, dual_radius, dual_level)) = &dual {
                        if cov > 0.0 {
                            cov *= sampled_coverage(&dual.bitmap, dx, dy, *dual_radius, *dual_level);
                        }
                    }
                    // Paper texture, fixed to the canvas.
                    if let Some(texture) = &settings.texture {
                        if cov > 0.0 {
                            cov *= texture.factor(px as f32 + 0.5, py as f32 + 0.5);
                        }
                    }
                    if cov <= 0.0 {
                        continue;
                    }
                    let a = cov * alpha;
                    let dst = &mut tile[((py - oy) as u32 * TILE_SIZE + (px - ox) as u32) as usize];
                    *dst = [
                        color[0] * a + dst[0] * (1.0 - a),
                        color[1] * a + dst[1] * (1.0 - a),
                        color[2] * a + dst[2] * (1.0 - a),
                        a + dst[3] * (1.0 - a),
                    ];
                    touched = true;
                }
            }
            if touched {
                dirty.insert(coord);
            }
        }
    }
}

/// Rebuilds a layer tile from its pre-stroke content and the stroke buffer tile,
/// limited by the selection tile when one is given.
/// Mirrored by `stroke_merge.wgsl` in painter-render.
pub fn merge_stroke(
    before: Option<&Tile>,
    stroke: &[Rgba],
    opacity: f32,
    mode: BrushMode,
    blend: BrushBlend,
    alpha_locked: bool,
    selection: Option<&Tile>,
) -> Tile {
    let mut out = before.cloned().unwrap_or_default();
    for (i, (o, s)) in out.bytes_mut().as_chunks_mut::<4>().0.iter_mut().zip(stroke).enumerate() {
        if s[3] <= 0.0 {
            continue;
        }
        let selected = selection.map_or(1.0, |t| t.bytes()[i * 4 + 3] as f32 / 255.0);
        if selected <= 0.0 {
            continue;
        }
        let b = to_f32(*o);
        let s = s.map(|v| v * opacity * selected);
        let sa = s[3];
        // The brush's blend mode: the colour the stroke lays where it fully covers.
        // Expressed as an equivalent premultiplied source of the same alpha, so
        // opacity, the alpha lock and the selection apply as for Normal:
        // source-over gives `s·(1 − ba) + b·(1 − sa) + sa·ba·B(cb, cs)` (the W3C
        // blend), source-atop gives `ba·(sa·B + (1 − sa)·cb)`.
        let s = match blend.layer_mode() {
            Some(m) if m != crate::blend::BlendMode::Normal && b[3] > 0.0 => {
                let ba = b[3];
                let mixed = [0, 1, 2].map(|i| {
                    let (cs, cb) = ((s[i] / sa).min(1.0), (b[i] / ba).min(1.0));
                    let blended = m.channel(cb, cs);
                    if alpha_locked {
                        sa * blended
                    } else {
                        s[i] * (1.0 - ba) + sa * ba * blended
                    }
                });
                [mixed[0], mixed[1], mixed[2], sa]
            }
            _ => s,
        };
        let result = match (mode, alpha_locked, blend) {
            (BrushMode::Erase, _, _) => b.map(|v| v * (1.0 - sa)),
            // Behind: under what's there (destination-over); a locked alpha leaves nothing to fill.
            (BrushMode::Paint, false, BrushBlend::Behind) => {
                [b[0] + s[0] * (1.0 - b[3]), b[1] + s[1] * (1.0 - b[3]), b[2] + s[2] * (1.0 - b[3]), b[3] + sa * (1.0 - b[3])]
            }
            (BrushMode::Paint, true, BrushBlend::Behind) => b,
            (BrushMode::Paint, false, _) => {
                [s[0] + b[0] * (1.0 - sa), s[1] + b[1] * (1.0 - sa), s[2] + b[2] * (1.0 - sa), sa + b[3] * (1.0 - sa)]
            }
            // Source-atop: recolour existing pixels, keep their alpha.
            (BrushMode::Paint, true, _) => [
                s[0] * b[3] + b[0] * (1.0 - sa),
                s[1] * b[3] + b[1] * (1.0 - sa),
                s[2] * b[3] + b[2] * (1.0 - sa),
                b[3],
            ],
        };
        *o = to_u8(result);
    }
    out
}

/// Merges every dirty stroke tile into `tiles`, rebuilding from `before`.
/// With a selection, only selected pixels change.
#[allow(clippy::too_many_arguments)]
pub fn commit_stroke(
    tiles: &mut TileStore,
    before: &TileStore,
    stroke: &RasterizedStroke,
    opacity: f32,
    mode: BrushMode,
    blend: BrushBlend,
    alpha_locked: bool,
    selection: Option<&TileStore>,
) {
    for &coord in &stroke.dirty {
        let Some(pixels) = stroke.buffer.get(coord) else {
            continue;
        };
        let selection_tile = match selection {
            None => None,
            Some(sel) => match sel.get(coord) {
                Some(tile) => Some(&**tile),
                None => continue,
            },
        };
        let merged = merge_stroke(before.get(coord).map(|t| &**t), pixels, opacity, mode, blend, alpha_locked, selection_tile);
        tiles.insert(coord, merged);
    }
}

/// Applies a whole stroke at once: the non-interactive path (scripted strokes, tests).
pub fn paint_stroke(
    tiles: &mut TileStore,
    dabs: &[Dab],
    color: [f32; 3],
    settings: &BrushSettings,
    canvas: (u32, u32),
    alpha_locked: bool,
    selection: Option<&TileStore>,
) -> BTreeSet<TileCoord> {
    let mut stroke = RasterizedStroke { buffer: StrokeBuffer::new(), dirty: BTreeSet::new() };
    for dab in dabs {
        stamp_dab(&mut stroke.buffer, dab, color, settings, canvas, &mut stroke.dirty);
    }
    let before = tiles.clone();
    commit_stroke(tiles, &before, &stroke, settings.opacity, settings.mode, settings.blend, alpha_locked, selection);
    stroke.dirty
}

/// The stroke buffer a finished [`BackgroundRasterizer`] hands back.
pub struct RasterizedStroke {
    pub buffer: StrokeBuffer,
    pub dirty: BTreeSet<TileCoord>,
}

/// Stamps a stroke's dabs on a worker thread, so the UI thread only places
/// dabs while the GPU draws the live preview. Joining it yields the
/// authoritative stroke buffer.
pub struct BackgroundRasterizer {
    sender: Option<Sender<Vec<Dab>>>,
    worker: Option<JoinHandle<RasterizedStroke>>,
}

impl BackgroundRasterizer {
    pub fn spawn(color: [f32; 3], settings: BrushSettings, canvas: (u32, u32)) -> Self {
        let (sender, receiver) = mpsc::channel::<Vec<Dab>>();
        let worker = thread::Builder::new()
            .name("stroke-raster".into())
            .spawn(move || {
                let mut stroke = RasterizedStroke { buffer: StrokeBuffer::new(), dirty: BTreeSet::new() };
                for dabs in receiver {
                    for dab in &dabs {
                        stamp_dab(&mut stroke.buffer, dab, color, &settings, canvas, &mut stroke.dirty);
                    }
                }
                stroke
            })
            .expect("failed to spawn stroke rasteriser thread");
        Self { sender: Some(sender), worker: Some(worker) }
    }

    pub fn push(&self, dabs: Vec<Dab>) {
        if let Some(sender) = &self.sender {
            // The worker only stops after `finish`, so a send can't fail before it.
            let _ = sender.send(dabs);
        }
    }

    /// Waits for every queued dab to be stamped.
    pub fn finish(mut self) -> RasterizedStroke {
        drop(self.sender.take());
        self.worker
            .take()
            .expect("worker is only taken here")
            .join()
            .expect("stroke rasteriser thread panicked")
    }
}

impl Drop for BackgroundRasterizer {
    fn drop(&mut self) {
        // An abandoned stroke: close the channel and let the worker exit on its own.
        drop(self.sender.take());
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::tip::{DabTip, TipShape};

    fn stamp(buffer: &mut StrokeBuffer, x: f32, y: f32, alpha: f32) -> BTreeSet<TileCoord> {
        let mut dirty = BTreeSet::new();
        let dab = Dab { x, y, radius: 5.0, alpha, angle: 0.0, thickness: 1.0, mirror: false };
        stamp_dab(buffer, &dab, [1.0, 0.0, 0.0], &BrushSettings::g_pen(), (128, 128), &mut dirty);
        dirty
    }

    #[test]
    fn background_matches_inline_stamping() {
        let settings = BrushSettings::airbrush();
        let dabs: Vec<Dab> =
            (0..50).map(|i| Dab { x: 20.0 + i as f32 * 1.7, y: 40.0, radius: 18.0, alpha: 0.2, angle: 0.0, thickness: 1.0, mirror: false }).collect();
        let worker = BackgroundRasterizer::spawn([0.2, 0.4, 0.9], settings.clone(), (200, 100));
        for chunk in dabs.chunks(7) {
            worker.push(chunk.to_vec());
        }
        let background = worker.finish();

        let mut inline = StrokeBuffer::new();
        let mut dirty = BTreeSet::new();
        for dab in &dabs {
            stamp_dab(&mut inline, dab, [0.2, 0.4, 0.9], &settings, (200, 100), &mut dirty);
        }
        assert_eq!(background.dirty, dirty);
        for coord in dirty {
            assert_eq!(background.buffer.get(coord), inline.get(coord));
        }
    }

    #[test]
    fn dab_centre_is_solid() {
        let mut buffer = StrokeBuffer::new();
        let dirty = stamp(&mut buffer, 10.5, 10.5, 1.0);
        assert_eq!(buffer.pixel(10, 10), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(buffer.pixel(30, 30), [0.0; 4]);
        assert_eq!(dirty.into_iter().collect::<Vec<_>>(), vec![TileCoord::new(0, 0)]);
    }

    fn stamp_tip(tip: DabTip, radius: f32) -> StrokeBuffer {
        let mut buffer = StrokeBuffer::new();
        let settings = BrushSettings { tip, ..BrushSettings::g_pen() };
        let dab = Dab { x: 32.0, y: 32.0, radius, alpha: 1.0, angle: 0.0, thickness: 1.0, mirror: false };
        stamp_dab(&mut buffer, &dab, [1.0, 0.0, 0.0], &settings, (64, 64), &mut BTreeSet::new());
        buffer
    }

    fn sampled(width: u32, height: u32, alpha: Vec<u8>) -> TipShape {
        TipShape::Sampled(Arc::new(TipBitmap::new(width, height, alpha).unwrap()))
    }

    /// Alpha at the pixel whose centre is `(dx, dy)` + ½ from the dab centre.
    fn alpha_at(buffer: &StrokeBuffer, dx: i32, dy: i32) -> f32 {
        buffer.pixel(32 + dx, 32 + dy)[3]
    }

    #[test]
    fn sampled_tip_longest_side_spans_diameter() {
        // A solid 4×2 tip at radius 8 spans 16×8 px (4 px texels), its edges
        // fading over the half texel either side.
        let buffer = stamp_tip(DabTip { shape: sampled(4, 2, vec![255; 8]), ..DabTip::default() }, 8.0);
        assert_eq!(alpha_at(&buffer, -6, -2), 1.0);
        assert_eq!(alpha_at(&buffer, 5, 1), 1.0);
        assert_eq!(alpha_at(&buffer, -8, 0), 0.625);
        assert_eq!(alpha_at(&buffer, 0, 6), 0.0);
        assert_eq!(alpha_at(&buffer, 10, 0), 0.0);
    }

    #[test]
    fn sampled_tip_is_bilinear_and_rotates() {
        // Ink on the left half only.
        let half = sampled(4, 4, [255, 255, 0, 0].repeat(4));
        let buffer = stamp_tip(DabTip { shape: half.clone(), ..DabTip::default() }, 8.0);
        assert_eq!(alpha_at(&buffer, -6, 0), 1.0);
        assert_eq!(alpha_at(&buffer, 5, 0), 0.0);
        // Half a pixel left of the midline: 3/8 of the way from an inked to a blank texel.
        assert!((alpha_at(&buffer, -1, 0) - 0.625).abs() < 1e-5, "{}", alpha_at(&buffer, -1, 0));

        // Rotated a quarter turn counter-clockwise, the inked half points down.
        let turned = stamp_tip(DabTip { shape: half, angle: 90.0, ..DabTip::default() }, 8.0);
        assert!(alpha_at(&turned, 0, 5) > 0.999);
        assert!(alpha_at(&turned, 0, -6) < 0.001);
        assert!((alpha_at(&turned, -6, 0) - 0.625).abs() < 1e-3, "{}", alpha_at(&turned, -6, 0));
    }

    #[test]
    fn large_sampled_tips_use_mips() {
        // A 256² checkerboard at radius 4 averages to grey instead of aliasing.
        let checker: Vec<u8> = (0..256 * 256).map(|i| if (i % 256 + i / 256) % 2 == 0 { 255 } else { 0 }).collect();
        let buffer = stamp_tip(DabTip { shape: sampled(256, 256, checker), ..DabTip::default() }, 4.0);
        for (dx, dy) in [(0, 0), (-3, 2), (2, -3)] {
            assert!((alpha_at(&buffer, dx, dy) - 0.5).abs() < 0.01, "{}", alpha_at(&buffer, dx, dy));
        }
    }

    #[test]
    fn flat_round_tip_is_a_rotated_ellipse() {
        let flat = DabTip { thickness: 0.25, ..DabTip::default() };
        let buffer = stamp_tip(flat.clone(), 10.0);
        assert_eq!(alpha_at(&buffer, 8, 0), 1.0);
        assert_eq!(alpha_at(&buffer, 0, 4), 0.0);
        assert_eq!(alpha_at(&buffer, 0, 1), 1.0);
        // The anti-aliased rim stays about a pixel wide across the thin axis.
        let rim = alpha_at(&buffer, 0, 2);
        assert!(rim > 0.0 && rim < 1.0, "{rim}");

        let upright = stamp_tip(DabTip { angle: 90.0, ..flat }, 10.0);
        assert_eq!(alpha_at(&upright, 0, 8), 1.0);
        assert_eq!(alpha_at(&upright, 4, 0), 0.0);
    }

    #[test]
    fn dual_tip_masks_and_texture_modulates() {
        // Dual tip: left half solid, right half empty → the dab keeps only its left side.
        let half = Arc::new(TipBitmap::new(2, 1, vec![255, 0]).unwrap());
        let settings = BrushSettings {
            dual: Some(crate::tip::DualTip { bitmap: half, scale: 1.0 }),
            ..BrushSettings::g_pen()
        };
        let mut buffer = StrokeBuffer::new();
        let mut dirty = BTreeSet::new();
        stamp_dab(&mut buffer, &Dab { x: 32.0, y: 32.0, radius: 8.0, alpha: 1.0, angle: 0.0, thickness: 1.0, mirror: false }, [1.0; 3], &settings, (64, 64), &mut dirty);
        // Solid half of the dual tip (a 1-px-tall bitmap is bilinearly softened ~6% vertically).
        assert!(buffer.pixel(28, 31)[3] > 0.8);
        assert_eq!(buffer.pixel(38, 31)[3], 0.0);

        // Texture: alternating dark/light columns at full density.
        let stripes = Arc::new(TipBitmap::new(2, 1, vec![0, 255]).unwrap());
        let settings = BrushSettings {
            texture: Some(crate::tip::DabTexture { bitmap: stripes, scale: 1.0, density: 1.0, invert: false }),
            ..BrushSettings::g_pen()
        };
        let mut buffer = StrokeBuffer::new();
        stamp_dab(&mut buffer, &Dab { x: 32.0, y: 32.0, radius: 8.0, alpha: 1.0, angle: 0.0, thickness: 1.0, mirror: false }, [1.0; 3], &settings, (64, 64), &mut dirty);
        assert_eq!(buffer.pixel(30, 32)[3], 0.0, "dark column");
        assert_eq!(buffer.pixel(31, 32)[3], 1.0, "light column");
    }

    #[test]
    fn per_dab_angle_adds_to_tip_angle() {
        let tip = DabTip { thickness: 0.3, angle: 30.0, ..DabTip::default() };
        let settings = BrushSettings { tip, ..BrushSettings::g_pen() };
        let stamp = |angle: f32, settings: &BrushSettings| {
            let mut buffer = StrokeBuffer::new();
            let mut dirty = BTreeSet::new();
            stamp_dab(&mut buffer, &Dab { x: 32.0, y: 32.0, radius: 12.0, alpha: 1.0, angle, thickness: 1.0, mirror: false }, [1.0; 3], settings, (64, 64), &mut dirty);
            (0..64 * 64).map(|i| buffer.pixel(i % 64, i / 64)[3]).collect::<Vec<f32>>()
        };
        let combined = stamp(60.0, &settings);
        let direct = stamp(0.0, &BrushSettings { tip: DabTip { angle: 90.0, ..settings.tip.clone() }, ..settings.clone() });
        assert!(combined.iter().zip(&direct).all(|(a, b)| (a - b).abs() < 1e-5));
    }
    #[test]
    fn unflattened_round_tip_ignores_angle() {
        let plain = stamp_tip(DabTip::default(), 6.0);
        let turned = stamp_tip(DabTip { angle: 37.0, ..DabTip::default() }, 6.0);
        assert_eq!(plain.get(TileCoord::new(0, 0)), turned.get(TileCoord::new(0, 0)));
    }

    #[test]
    fn dab_across_tile_seam_marks_both() {
        let mut buffer = StrokeBuffer::new();
        assert_eq!(stamp(&mut buffer, 64.0, 10.0, 1.0).len(), 2);
    }

    #[test]
    fn opacity_caps_overlapping_dabs() {
        let mut buffer = StrokeBuffer::new();
        for _ in 0..10 {
            stamp(&mut buffer, 10.5, 10.5, 1.0);
        }
        let tile = merge_stroke(None, buffer.get(TileCoord::new(0, 0)).unwrap(), 0.5, BrushMode::Paint, BrushBlend::Normal, false, None);
        assert_eq!(tile.pixel(10, 10)[3], 128);
    }

    #[test]
    fn faint_dabs_build_up_to_full_coverage() {
        // At 8-bit precision a 2% dab stops adding anything long before opaque.
        let mut buffer = StrokeBuffer::new();
        for _ in 0..400 {
            stamp(&mut buffer, 10.5, 10.5, 0.02);
        }
        let tile = merge_stroke(None, buffer.get(TileCoord::new(0, 0)).unwrap(), 1.0, BrushMode::Paint, BrushBlend::Normal, false, None);
        assert_eq!(tile.pixel(10, 10), [255, 0, 0, 255]);
    }

    #[test]
    fn erase_and_alpha_lock() {
        let mut before = Tile::empty();
        before.set_pixel(0, 0, [0, 0, 255, 255]);
        let mut stroke = vec![[0.0; 4]; TILE_PIXELS];
        stroke[0] = [1.0, 0.0, 0.0, 1.0];
        stroke[1] = [1.0, 0.0, 0.0, 1.0];

        let erased = merge_stroke(Some(&before), &stroke, 1.0, BrushMode::Erase, BrushBlend::Normal, false, None);
        assert_eq!(erased.pixel(0, 0), [0, 0, 0, 0]);

        let locked = merge_stroke(Some(&before), &stroke, 1.0, BrushMode::Paint, BrushBlend::Normal, true, None);
        assert_eq!(locked.pixel(0, 0), [255, 0, 0, 255]);
        assert_eq!(locked.pixel(1, 0), [0, 0, 0, 0]);
    }

    #[test]
    fn paint_stroke_commits_to_tiles() {
        let mut tiles = TileStore::new();
        let dabs = [Dab { x: 5.5, y: 5.5, radius: 3.0, alpha: 1.0, angle: 0.0, thickness: 1.0, mirror: false }];
        let dirty = paint_stroke(&mut tiles, &dabs, [0.0, 1.0, 0.0], &BrushSettings::g_pen(), (32, 32), false, None);
        assert_eq!(dirty.len(), 1);
        assert_eq!(tiles.pixel(5, 5), [0, 255, 0, 255]);
    }

    #[test]
    fn selection_limits_commit() {
        let mut tiles = TileStore::new();
        let selection = crate::selection::rect_mask([0.0, 0.0], [6.0, 32.0], (32, 32));
        let dabs = [Dab { x: 6.0, y: 6.0, radius: 4.0, alpha: 1.0, angle: 0.0, thickness: 1.0, mirror: false }];
        paint_stroke(&mut tiles, &dabs, [0.0, 1.0, 0.0], &BrushSettings::g_pen(), (32, 32), false, Some(&selection));
        assert_eq!(tiles.pixel(5, 6), [0, 255, 0, 255]);
        assert_eq!(tiles.pixel(7, 6), [0, 0, 0, 0]);
        // An empty selection protects everything.
        let mut tiles = TileStore::new();
        paint_stroke(&mut tiles, &dabs, [0.0, 1.0, 0.0], &BrushSettings::g_pen(), (32, 32), false, Some(&TileStore::new()));
        assert!(tiles.is_empty());
    }
}

#[cfg(test)]
mod blend_tests {
    use super::*;
    use crate::tile::Tile;

    /// A tile of one straight-colour pixel value everywhere.
    fn filled(px: [u8; 4]) -> Tile {
        let mut t = Tile::default();
        for p in t.bytes_mut().as_chunks_mut::<4>().0 {
            *p = crate::blend::premultiply(px);
        }
        t
    }

    fn stroke_of(rgba: [f32; 4]) -> Vec<Rgba> {
        // Premultiplied.
        vec![[rgba[0] * rgba[3], rgba[1] * rgba[3], rgba[2] * rgba[3], rgba[3]]; TILE_PIXELS]
    }

    #[test]
    fn brush_blend_modes_mix_with_the_layer() {
        let red = filled([255, 0, 0, 255]);
        // Multiply: red × half grey = dark red, alpha stays opaque.
        let out = merge_stroke(Some(&red), &stroke_of([0.5, 0.5, 0.5, 1.0]), 1.0, BrushMode::Paint, BrushBlend::Multiply, false, None);
        let p = out.bytes();
        assert!((p[0] as i32 - 128).abs() <= 1 && p[1] == 0 && p[3] == 255, "{:?}", &p[..4]);
        // Half opacity: halfway between red and dark red.
        let half = merge_stroke(Some(&red), &stroke_of([0.5, 0.5, 0.5, 1.0]), 0.5, BrushMode::Paint, BrushBlend::Multiply, false, None);
        assert!((half.bytes()[0] as i32 - 191).abs() <= 2, "{}", half.bytes()[0]);
        // On transparent pixels Multiply paints the colour itself (nothing to mix with).
        let empty = merge_stroke(None, &stroke_of([0.0, 0.0, 1.0, 1.0]), 1.0, BrushMode::Paint, BrushBlend::Multiply, false, None);
        assert_eq!(&empty.bytes()[..4], &[0, 0, 255, 255]);
        // With the alpha locked, a half-transparent pixel keeps its alpha.
        let faint = filled([255, 0, 0, 128]);
        let locked = merge_stroke(Some(&faint), &stroke_of([0.5, 0.5, 0.5, 1.0]), 1.0, BrushMode::Paint, BrushBlend::Multiply, true, None);
        let l = crate::blend::unpremultiply([locked.bytes()[0], locked.bytes()[1], locked.bytes()[2], locked.bytes()[3]]);
        assert_eq!(l[3], 128);
        assert!((l[0] as i32 - 128).abs() <= 2, "{l:?}");
        // Behind: nothing changes under opaque paint, transparent pixels fill in.
        let behind = merge_stroke(Some(&red), &stroke_of([0.0, 0.0, 1.0, 1.0]), 1.0, BrushMode::Paint, BrushBlend::Behind, false, None);
        assert_eq!(&behind.bytes()[..4], &[255, 0, 0, 255]);
        let under = merge_stroke(None, &stroke_of([0.0, 0.0, 1.0, 1.0]), 1.0, BrushMode::Paint, BrushBlend::Behind, false, None);
        assert_eq!(&under.bytes()[..4], &[0, 0, 255, 255]);
        // Screen lightens.
        let screen = merge_stroke(Some(&filled([100, 100, 100, 255])), &stroke_of([0.5, 0.5, 0.5, 1.0]), 1.0, BrushMode::Paint, BrushBlend::Screen, false, None);
        assert!(screen.bytes()[0] > 100);
    }
}
