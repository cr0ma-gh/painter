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
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

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

/// Coverage of a dual tip smaller than the dab, tiled over the canvas: a
/// field of second-tip stamps (Photoshop scatters many of them along the
/// stroke), each cell one dual diameter across, `(x, y)` a pixel centre.
/// Mirrored by `tiled_coverage` in painter-render's `dab.wgsl`.
fn tiled_coverage(bitmap: &TipBitmap, x: f32, y: f32, radius: f32, level: u32) -> f32 {
    let aspect = bitmap.aspect();
    let (w, h) = bitmap.level_size(level);
    let (cw, ch) = (2.0 * radius * aspect[0], 2.0 * radius * aspect[1]);
    bitmap.sample_wrapped_level(level, x / cw * w as f32 - 0.5, y / ch * h as f32 - 0.5)
}

/// Uniform grain in 0..1 fixed to canvas pixel `(x, y)` (Photoshop's brush noise).
/// Mirrored by `grain` in painter-render's `dab.wgsl`.
pub fn grain(x: u32, y: u32) -> f32 {
    let mut h = x.wrapping_mul(1_664_525).wrapping_add(y.wrapping_mul(1_013_904_223)).wrapping_add(7u32.wrapping_mul(2_654_435_769));
    h ^= h >> 16;
    h = h.wrapping_mul(2_246_822_519);
    h ^= h >> 13;
    h = h.wrapping_mul(3_266_489_917);
    h ^= h >> 16;
    (h >> 8) as f32 / 16_777_216.0
}

/// Noise on a coverage: none where it is 0 or 1, ±½ at half coverage.
pub fn noisy(cov: f32, grain: f32) -> f32 {
    (cov + (grain - 0.5) * 4.0 * cov * (1.0 - cov)).clamp(0.0, 1.0)
}

/// Relative slack on the squared-distance bounds, far wider than the few ulps by
/// which `dx² + dy²` and `hypot` can disagree, so the fast paths never change a pixel.
const BOUND_SLACK: f32 = 1e-3;

/// Squared distances from the dab centre that classify a pixel without evaluating its tip.
#[derive(Clone, Copy, Debug)]
struct DabBounds {
    /// Beyond this the tip's coverage is exactly zero.
    outer2: f32,
    /// Within this a disc's coverage is exactly one (negative when there is no such core).
    full2: f32,
}

impl DabBounds {
    /// Bounds for a round tip (disc or ellipse) of `radius` with `aa` px of anti-aliasing.
    /// Coverage vanishes past `radius + aa/2`: for the ellipse that holds because its edge
    /// distance is never below the distance to the radius circle, and flattening only
    /// pushes tip space outwards. The solid core ends at the hardness falloff or where the
    /// anti-aliased edge starts, whichever comes first.
    fn round(radius: f32, hardness: f32, aa: f32) -> Self {
        let aa = aa.max(0.0);
        let outer = (radius + 0.5 * aa) * (1.0 + BOUND_SLACK) + BOUND_SLACK;
        let full = (radius * hardness.clamp(0.0, 1.0)).min(radius - 0.5 * aa) * (1.0 - BOUND_SLACK) - BOUND_SLACK;
        Self { outer2: outer * outer, full2: if full > 0.0 { full * full } else { -1.0 } }
    }

    /// Bounds for a sampled tip: the bitmap rectangle at `level` plus its half texel of
    /// bilinear bleed, measured in tip space, which is never nearer than canvas space.
    fn sampled(bitmap: &TipBitmap, radius: f32, level: u32) -> Self {
        let aspect = bitmap.aspect();
        let (w, h) = bitmap.level_size(level);
        let ex = radius * aspect[0] * (1.0 + 1.0 / w as f32);
        let ey = radius * aspect[1] * (1.0 + 1.0 / h as f32);
        let outer = ex.hypot(ey) * (1.0 + BOUND_SLACK) + BOUND_SLACK;
        Self { outer2: outer * outer, full2: -1.0 }
    }
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
    let color = dab.color.unwrap_or(color);
    // A whole-stroke texture is applied once the stroke is done (see `finish_stroke`).
    let texture = settings.texture.as_ref().filter(|t| t.each_dab);
    let aa = settings.antialias.width();
    let tip = &settings.tip;

    let level = tip.mip_level(radius);
    let reach = tip.reach(radius, aa, level);
    let ([cos, sin], flatness) = (tip.rotation(dab.angle), tip.flatness_for(dab.thickness));
    let kind = tip.kind_for(flatness);
    let bounds = match tip.bitmap() {
        Some(bitmap) => DabBounds::sampled(bitmap, radius, level),
        None => DabBounds::round(radius, settings.hardness, aa),
    };
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
                    // Squared distance first: the square's corners and a disc's solid core need no sqrt.
                    let d2 = dx * dx + dy * dy;
                    if d2 > bounds.outer2 {
                        continue;
                    }
                    let mut cov = if kind == TipKind::Disc {
                        if d2 <= bounds.full2 {
                            1.0
                        } else {
                            coverage(dx.hypot(dy), radius, settings.hardness, aa)
                        }
                    } else {
                        // Into tip space: undo the rotation, then the flattening.
                        let x = (cos * dx - sin * dy) * if dab.mirror { -1.0 } else { 1.0 };
                        let y = (sin * dx + cos * dy) / flatness;
                        match tip.bitmap() {
                            Some(bitmap) => sampled_coverage(bitmap, x, y, radius, level),
                            None => ellipse_coverage(x, y, radius, flatness, settings.hardness, aa),
                        }
                    };
                    // Noise: grain in the soft parts of the tip.
                    if settings.noise && cov > 0.0 {
                        cov = noisy(cov, grain(px as u32, py as u32));
                    }
                    // Dual tip: an unrotated second bitmap centred on the dab (or,
                    // smaller than it, tiled over the canvas) combined by its mode.
                    if let Some((dual, dual_radius, dual_level)) = &dual {
                        if cov > 0.0 {
                            let t = if dual.scale() < 1.0 && !dual.centred {
                                tiled_coverage(&dual.bitmap, px as f32 + 0.5, py as f32 + 0.5, *dual_radius, *dual_level)
                            } else {
                                sampled_coverage(&dual.bitmap, dx, dy, *dual_radius, *dual_level)
                            };
                            cov = dual.mode.mix(cov, t);
                        }
                    }
                    // Paper texture, fixed to the canvas.
                    if let Some(texture) = texture {
                        if cov > 0.0 {
                            cov = texture.apply_at(cov, px as f32 + 0.5, py as f32 + 0.5, dab.texture);
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
    merge_stroke_in_place(&mut out, stroke, opacity, mode, blend, alpha_locked, selection);
    out
}

/// [`merge_stroke`] writing into `out`, which already holds the pre-stroke pixels.
fn merge_stroke_in_place(
    out: &mut Tile,
    stroke: &[Rgba],
    opacity: f32,
    mode: BrushMode,
    blend: BrushBlend,
    alpha_locked: bool,
    selection: Option<&Tile>,
) {
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
}

/// Below this many dirty tiles per thread a commit stays on fewer threads:
/// spreading a few tiles over threads costs more than it saves.
const PARALLEL_COMMIT_TILES: usize = 16;

/// One tile of a commit: where it goes, its inputs, and the layer's current
/// tile there, whose allocation is reused when nothing else shares it.
struct CommitJob<'a> {
    coord: TileCoord,
    pixels: &'a [Rgba],
    before: Option<&'a Tile>,
    selection: Option<&'a Tile>,
    current: Option<Arc<Tile>>,
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
    let mut jobs = Vec::with_capacity(stroke.dirty.len());
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
        let current = tiles.remove(coord);
        jobs.push(CommitJob { coord, pixels, before: before.get(coord).map(|t| &**t), selection: selection_tile, current });
    }
    let merge = |job: CommitJob<'_>| -> (TileCoord, Arc<Tile>) {
        let tile = match job.current {
            // Nothing else holds the layer's tile (not `before`, not an undo
            // snapshot): rebuild it where it is instead of allocating a new one.
            Some(mut current) if Arc::strong_count(&current) == 1 && Arc::weak_count(&current) == 0 => {
                let out = Arc::get_mut(&mut current).expect("checked unique");
                match job.before {
                    Some(b) => out.bytes_mut().copy_from_slice(b.bytes()),
                    None => out.bytes_mut().fill(0),
                }
                merge_stroke_in_place(out, job.pixels, opacity, mode, blend, alpha_locked, job.selection);
                current
            }
            _ => Arc::new(merge_stroke(job.before, job.pixels, opacity, mode, blend, alpha_locked, job.selection)),
        };
        (job.coord, tile)
    };
    let threads = thread::available_parallelism().map_or(1, |n| n.get()).min(jobs.len() / PARALLEL_COMMIT_TILES);
    let merged: Vec<(TileCoord, Arc<Tile>)> = if threads <= 1 {
        jobs.into_iter().map(merge).collect()
    } else {
        // Broad strokes: the tiles are independent, so split them across the cores.
        let per_thread = jobs.len().div_ceil(threads);
        let mut batches = Vec::with_capacity(threads);
        while !jobs.is_empty() {
            batches.push(jobs.split_off(jobs.len().saturating_sub(per_thread)));
        }
        let merge = &merge;
        thread::scope(|scope| {
            let handles: Vec<_> = batches.into_iter().map(|batch| scope.spawn(move || batch.into_iter().map(merge).collect::<Vec<_>>())).collect();
            handles.into_iter().flat_map(|h| h.join().expect("tile merge thread panicked")).collect()
        })
    };
    for (coord, tile) in merged {
        tiles.insert_shared(coord, tile);
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
    finish_stroke(&mut stroke, settings);
    let before = tiles.clone();
    commit_stroke(tiles, &before, &stroke, settings.opacity, settings.mode, settings.blend, alpha_locked, selection);
    stroke.dirty
}

/// The stroke buffer a finished [`BackgroundRasterizer`] hands back.
pub struct RasterizedStroke {
    pub buffer: StrokeBuffer,
    pub dirty: BTreeSet<TileCoord>,
}

/// What happens to a stroke once its dabs are all down, before it merges:
/// a whole-stroke texture shapes it (the GPU preview applies it as it
/// merges, in `stroke_merge.wgsl`), then the watercolor edge pools its rim
/// (shown when the pen lifts, as in Clip Studio).
pub fn finish_stroke(stroke: &mut RasterizedStroke, settings: &BrushSettings) {
    if let Some(texture) = settings.texture.as_ref().filter(|t| !t.each_dab) {
        texture_stroke(stroke, texture);
    }
    if let Some(edge) = &settings.water_edge {
        water_edge(stroke, edge);
    }
}

/// A texture applied to a whole stroke: every pixel's coverage shaped once,
/// so overlapping dabs don't fill the grain in. Runs as the pen lifts, so
/// broad strokes are split across the cores.
pub fn texture_stroke(stroke: &mut RasterizedStroke, texture: &crate::tip::DabTexture) {
    let dirty = &stroke.dirty;
    let mut tiles: Vec<(TileCoord, &mut Box<[Rgba]>)> =
        stroke.buffer.tiles.iter_mut().filter(|(coord, _)| dirty.contains(coord)).map(|(coord, tile)| (*coord, tile)).collect();
    let shape = |batch: &mut [(TileCoord, &mut Box<[Rgba]>)]| {
        for (coord, tile) in batch {
            let (ox, oy) = coord.origin();
            for (i, p) in tile.iter_mut().enumerate() {
                if p[3] <= 0.0 {
                    continue;
                }
                let (x, y) = ((ox + (i as u32 % TILE_SIZE) as i32) as f32 + 0.5, (oy + (i as u32 / TILE_SIZE) as i32) as f32 + 0.5);
                let a = texture.apply(p[3], x, y);
                let k = a / p[3];
                *p = [p[0] * k, p[1] * k, p[2] * k, a];
            }
        }
    };
    let threads = thread::available_parallelism().map_or(1, |n| n.get()).min(tiles.len() / PARALLEL_COMMIT_TILES);
    if threads <= 1 {
        shape(&mut tiles);
    } else {
        let per_thread = tiles.len().div_ceil(threads);
        let shape = &shape;
        thread::scope(|scope| {
            for batch in tiles.chunks_mut(per_thread) {
                scope.spawn(move || shape(batch));
            }
        });
    }
}

/// Ink pooling at a stroke's rim: within `edge.radius` of it the stroke is
/// denser and darker, deeper in it thins (see [`crate::brush::WaterEdge`]).
/// Runs as the pen lifts, so broad washes are split across the cores.
pub fn water_edge(stroke: &mut RasterizedStroke, edge: &crate::brush::WaterEdge) {
    let r = edge.radius.round().clamp(1.0, TILE_SIZE as f32) as i32;
    let coords: Vec<TileCoord> = stroke.dirty.iter().copied().collect();
    let buffer = &stroke.buffer;
    let pool = |batch: &[TileCoord]| -> Vec<(TileCoord, Box<[Rgba]>)> {
        let mut scratch = (Vec::new(), Vec::new());
        batch.iter().filter_map(|&coord| Some((coord, water_edge_tile(buffer, coord, r, edge, &mut scratch)?))).collect()
    };
    let threads = thread::available_parallelism().map_or(1, |n| n.get()).min(coords.len() / PARALLEL_COMMIT_TILES);
    let pooled: Vec<(TileCoord, Box<[Rgba]>)> = if threads <= 1 {
        pool(&coords)
    } else {
        let pool = &pool;
        thread::scope(|scope| {
            let handles: Vec<_> = coords.chunks(coords.len().div_ceil(threads)).map(|batch| scope.spawn(move || pool(batch))).collect();
            handles.into_iter().flat_map(|h| h.join().expect("water edge thread panicked")).collect()
        })
    };
    for (coord, tile) in pooled {
        stroke.buffer.tile_mut(coord).copy_from_slice(&tile);
    }
}

/// One tile of [`water_edge`] with its rim pooled (`None` where the stroke
/// has no tile). `scratch` holds the alpha window and its summed areas
/// between calls, `r` is at most a tile.
fn water_edge_tile(
    buffer: &StrokeBuffer,
    coord: TileCoord,
    r: i32,
    edge: &crate::brush::WaterEdge,
    scratch: &mut (Vec<f32>, Vec<f64>),
) -> Option<Box<[Rgba]>> {
    let own = buffer.get(coord)?;
    let t = TILE_SIZE as i32;
    let side = t + 2 * r;
    let (window, sum) = scratch;
    // Alpha over the tile and `r` around it: the tile and its eight neighbours, row by row.
    window.clear();
    window.resize((side * side) as usize, 0.0);
    for ny in -1..=1 {
        for nx in -1..=1 {
            let Some(tile) = buffer.get(TileCoord::new(coord.x + nx, coord.y + ny)) else { continue };
            // Where that tile's top-left pixel falls in the window.
            let (bx, by) = (nx * t + r, ny * t + r);
            let (x0, x1) = (bx.max(0), (bx + t).min(side));
            for wy in by.max(0)..(by + t).min(side) {
                let (src, dst) = (((wy - by) * t - bx) as isize, (wy * side) as usize);
                for wx in x0..x1 {
                    window[dst + wx as usize] = tile[(src + wx as isize) as usize][3];
                }
            }
        }
    }
    // Its box averages by summed areas.
    let stride = (side + 1) as usize;
    sum.clear();
    sum.resize(stride * stride, 0.0);
    for wy in 0..side as usize {
        let mut row = 0.0f64;
        for wx in 0..side as usize {
            row += window[wy * side as usize + wx] as f64;
            let i = (wy + 1) * stride + wx + 1;
            sum[i] = sum[i - stride] + row;
        }
    }
    let area = ((2 * r + 1) * (2 * r + 1)) as f64;
    let (thinning, rim, darkening) = (edge.thinning.clamp(0.0, 1.0), edge.rim.clamp(0.0, 1.0), edge.darkening.clamp(0.0, 1.0));
    let at = |x: i32, y: i32| sum[y as usize * stride + x as usize];
    let mut out = own.to_vec().into_boxed_slice();
    for (i, p) in out.iter_mut().enumerate() {
        let a = p[3];
        if a <= 0.0 {
            continue;
        }
        let (lx, ly) = ((i as i32 % t) + r, (i as i32 / t) + r);
        let box_sum = at(lx + r + 1, ly + r + 1) - at(lx - r, ly + r + 1) - at(lx + r + 1, ly - r) + at(lx - r, ly - r);
        let mean = (box_sum / area) as f32;
        // 0 deep inside the stroke, 1 at its rim.
        let e = ((a - mean) / a.max(1e-4) * 4.0).clamp(0.0, 1.0);
        let thinned = a * ((1.0 - thinning) + thinning * e);
        let pooled = thinned + (1.0 - thinned) * rim * e;
        let k = pooled / a * (1.0 - darkening * e);
        *p = [p[0] * k, p[1] * k, p[2] * k, pooled];
    }
    Some(out)
}

/// Most idle stroke workers kept alive between strokes: one serves a single
/// pen, the spare covers a stroke begun before the last one was joined.
const IDLE_STROKE_WORKERS: usize = 2;

/// Work for a stroke worker: rasterises one stroke and returns what hands it
/// back, which runs once the worker is idle again so the next stroke finds it free.
type StrokeJob = Box<dyn FnOnce() -> Box<dyn FnOnce() + Send> + Send>;

/// Stroke workers waiting for their next stroke, by the sender that feeds them.
static IDLE_WORKERS: Mutex<Vec<Sender<StrokeJob>>> = Mutex::new(Vec::new());

/// Runs `job` on an idle stroke worker, starting a thread only when none is
/// free, so rapid strokes reuse the same thread instead of creating one each.
fn run_on_stroke_worker(job: StrokeJob) {
    let idle = IDLE_WORKERS.lock().unwrap_or_else(|e| e.into_inner()).pop();
    let job = match idle {
        Some(worker) => match worker.send(job) {
            Ok(()) => return,
            // A worker that is gone hands the job back for a new one.
            Err(mpsc::SendError(job)) => job,
        },
        None => job,
    };
    let (sender, receiver) = mpsc::channel::<StrokeJob>();
    sender.send(job).expect("the receiver is alive");
    thread::Builder::new()
        .name("stroke-raster".into())
        .spawn(move || stroke_worker(sender, receiver))
        .expect("failed to spawn stroke rasteriser thread");
}

/// A stroke worker's loop: after each stroke it offers itself back to the
/// idle list, or exits when that list is already full.
fn stroke_worker(own: Sender<StrokeJob>, jobs: Receiver<StrokeJob>) {
    while let Ok(job) = jobs.recv() {
        let deliver = job();
        let keep = {
            let mut idle = IDLE_WORKERS.lock().unwrap_or_else(|e| e.into_inner());
            let keep = idle.len() < IDLE_STROKE_WORKERS;
            if keep {
                idle.push(own.clone());
            }
            keep
        };
        deliver();
        if !keep {
            return;
        }
    }
}

/// Stamps a stroke's dabs on a worker thread, so the UI thread only places
/// dabs while the GPU draws the live preview. Joining it yields the
/// authoritative stroke buffer. Worker threads persist between strokes, so
/// rapid strokes don't each pay for creating an OS thread.
pub struct BackgroundRasterizer {
    sender: Option<Sender<Vec<Dab>>>,
    result: Receiver<RasterizedStroke>,
}

impl BackgroundRasterizer {
    pub fn spawn(color: [f32; 3], settings: BrushSettings, canvas: (u32, u32)) -> Self {
        let (sender, receiver) = mpsc::channel::<Vec<Dab>>();
        let (done, result) = mpsc::sync_channel::<RasterizedStroke>(1);
        run_on_stroke_worker(Box::new(move || {
            let mut stroke = RasterizedStroke { buffer: StrokeBuffer::new(), dirty: BTreeSet::new() };
            for dabs in receiver {
                for dab in &dabs {
                    stamp_dab(&mut stroke.buffer, dab, color, &settings, canvas, &mut stroke.dirty);
                }
            }
            Box::new(move || {
                // An abandoned stroke has no one waiting for it.
                let _ = done.send(stroke);
            })
        }));
        Self { sender: Some(sender), result }
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
        self.result.recv().expect("stroke rasteriser thread panicked")
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
        let dab = Dab { x, y, radius: 5.0, alpha, angle: 0.0, thickness: 1.0, mirror: false, color: None, texture: 1.0 };
        stamp_dab(buffer, &dab, [1.0, 0.0, 0.0], &BrushSettings::g_pen(), (128, 128), &mut dirty);
        dirty
    }

    #[test]
    fn background_matches_inline_stamping() {
        let settings = BrushSettings::airbrush();
        let dabs: Vec<Dab> =
            (0..50).map(|i| Dab { x: 20.0 + i as f32 * 1.7, y: 40.0, radius: 18.0, alpha: 0.2, angle: 0.0, thickness: 1.0, mirror: false, color: None, texture: 1.0 }).collect();
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
    fn sequential_strokes_reuse_worker_threads() {
        let mut threads = std::collections::HashSet::new();
        for _ in 0..20 {
            let (tx, rx) = mpsc::channel();
            run_on_stroke_worker(Box::new(move || {
                Box::new(move || tx.send(thread::current().id()).unwrap()) as Box<dyn FnOnce() + Send>
            }));
            threads.insert(rx.recv().unwrap());
        }
        // Other tests may hold a worker meanwhile, but most strokes land on a reused one.
        assert!(threads.len() <= 1 + IDLE_STROKE_WORKERS, "{} threads for 20 strokes", threads.len());
    }

    #[test]
    fn broad_commit_matches_per_tile_merge() {
        // Wide enough for the parallel path, over a layer whose tiles are shared and unique.
        let canvas = (1024, 1024);
        let mut layer = TileStore::new();
        for ty in 0..16 {
            for tx in 0..16 {
                if (tx + ty) % 3 != 0 {
                    let mut tile = Tile::empty();
                    tile.bytes_mut().as_chunks_mut::<4>().0.iter_mut().for_each(|p| *p = [40, (tx * 10) as u8, (ty * 10) as u8, 255]);
                    layer.insert(TileCoord::new(tx, ty), tile);
                }
            }
        }
        let dabs: Vec<Dab> =
            (0..60).map(|i| Dab { x: 20.0 + i as f32 * 16.0, y: 30.0 + i as f32 * 15.0, radius: 120.0, alpha: 0.7, angle: 0.0, thickness: 1.0, mirror: false, color: None, texture: 1.0 }).collect();
        for mode in [BrushMode::Paint, BrushMode::Erase] {
            let settings = BrushSettings { mode, ..BrushSettings::airbrush() };
            let mut stroke = RasterizedStroke { buffer: StrokeBuffer::new(), dirty: BTreeSet::new() };
            for dab in &dabs {
                stamp_dab(&mut stroke.buffer, dab, [0.9, 0.1, 0.3], &settings, canvas, &mut stroke.dirty);
            }
            assert!(stroke.dirty.len() >= 4 * PARALLEL_COMMIT_TILES);

            let mut expected = layer.clone();
            for &coord in &stroke.dirty {
                let tile = merge_stroke(layer.get(coord).map(|t| &**t), stroke.buffer.get(coord).unwrap(), 0.8, mode, BrushBlend::Normal, false, None);
                expected.insert(coord, tile);
            }

            // Shared with `before`, as in the app.
            let mut shared = layer.clone();
            commit_stroke(&mut shared, &layer, &stroke, 0.8, mode, BrushBlend::Normal, false, None);
            // Unique tiles, rebuilt in place.
            let mut unique = TileStore::new();
            for (coord, tile) in layer.iter() {
                unique.insert(coord, (**tile).clone());
            }
            commit_stroke(&mut unique, &layer, &stroke, 0.8, mode, BrushBlend::Normal, false, None);

            for store in [&shared, &unique] {
                assert_eq!(store.len(), expected.len());
                for (coord, tile) in expected.iter() {
                    assert_eq!(store.get(coord).map(|t| t.bytes()), Some(tile.bytes()), "{mode:?} at {coord:?}");
                }
            }
            // Untouched layer tiles are still shared with the snapshot.
            let untouched = layer.iter().find(|(c, _)| !stroke.dirty.contains(c)).map(|(c, _)| c).unwrap();
            assert!(Arc::ptr_eq(shared.get(untouched).unwrap(), layer.get(untouched).unwrap()));
        }
    }

    #[test]
    fn squared_distance_bounds_never_change_coverage() {
        for radius in [0.5, 0.75, 1.0, 2.3, 5.0, 17.5, 64.0, 300.0] {
            for hardness in [0.0, 0.3, 0.8, 1.0] {
                for aa in [0.0, 0.5, 1.0, 2.0] {
                    let bounds = DabBounds::round(radius, hardness, aa);
                    let reach = (radius + aa).ceil() as i32 + 1;
                    for (ox, oy) in [(0.5, 0.5), (0.0, 0.0), (0.37, 0.81)] {
                        for py in -reach..=reach {
                            for px in -reach..=reach {
                                let (dx, dy) = (px as f32 + ox, py as f32 + oy);
                                let d2 = dx * dx + dy * dy;
                                let exact = coverage(dx.hypot(dy), radius, hardness, aa);
                                if d2 > bounds.outer2 {
                                    assert_eq!(exact, 0.0, "r {radius} h {hardness} aa {aa} at ({dx}, {dy})");
                                    for flatness in [0.99, 0.5, 0.01] {
                                        let ellipse = ellipse_coverage(dx, dy / flatness, radius, flatness, hardness, aa);
                                        assert_eq!(ellipse, 0.0, "ellipse r {radius} f {flatness} at ({dx}, {dy})");
                                    }
                                }
                                if d2 <= bounds.full2 {
                                    assert_eq!(exact, 1.0, "r {radius} h {hardness} aa {aa} at ({dx}, {dy})");
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn sampled_bounds_contain_the_bitmap() {
        let bitmap = TipBitmap::new(5, 2, vec![255; 10]).unwrap();
        for radius in [0.5, 1.0, 4.0, 9.0] {
            for level in 0..bitmap.level_count() {
                let bounds = DabBounds::sampled(&bitmap, radius, level);
                let reach = (2.0 * radius).ceil() as i32 + 2;
                for py in -reach..=reach {
                    for px in -reach..=reach {
                        let (x, y) = (px as f32 + 0.5, py as f32 + 0.5);
                        if x * x + y * y > bounds.outer2 {
                            assert_eq!(sampled_coverage(&bitmap, x, y, radius, level), 0.0, "r {radius} l {level} at ({x}, {y})");
                        }
                    }
                }
            }
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
        let dab = Dab { x: 32.0, y: 32.0, radius, alpha: 1.0, angle: 0.0, thickness: 1.0, mirror: false, color: None, texture: 1.0 };
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
            dual: Some(crate::tip::DualTip::new(half, 1.0)),
            ..BrushSettings::g_pen()
        };
        let mut buffer = StrokeBuffer::new();
        let mut dirty = BTreeSet::new();
        stamp_dab(&mut buffer, &Dab { x: 32.0, y: 32.0, radius: 8.0, alpha: 1.0, angle: 0.0, thickness: 1.0, mirror: false, color: None, texture: 1.0 }, [1.0; 3], &settings, (64, 64), &mut dirty);
        // Solid half of the dual tip (a 1-px-tall bitmap is bilinearly softened ~6% vertically).
        assert!(buffer.pixel(28, 31)[3] > 0.8);
        assert_eq!(buffer.pixel(38, 31)[3], 0.0);

        // Texture: alternating dark/light columns at full density.
        let stripes = Arc::new(TipBitmap::new(2, 1, vec![0, 255]).unwrap());
        let settings = BrushSettings {
            texture: Some(crate::tip::DabTexture::new(stripes, 1.0, 1.0, false)),
            ..BrushSettings::g_pen()
        };
        let mut buffer = StrokeBuffer::new();
        stamp_dab(&mut buffer, &Dab { x: 32.0, y: 32.0, radius: 8.0, alpha: 1.0, angle: 0.0, thickness: 1.0, mirror: false, color: None, texture: 1.0 }, [1.0; 3], &settings, (64, 64), &mut dirty);
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
            stamp_dab(&mut buffer, &Dab { x: 32.0, y: 32.0, radius: 12.0, alpha: 1.0, angle, thickness: 1.0, mirror: false, color: None, texture: 1.0 }, [1.0; 3], settings, (64, 64), &mut dirty);
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
        let dabs = [Dab { x: 5.5, y: 5.5, radius: 3.0, alpha: 1.0, angle: 0.0, thickness: 1.0, mirror: false, color: None, texture: 1.0 }];
        let dirty = paint_stroke(&mut tiles, &dabs, [0.0, 1.0, 0.0], &BrushSettings::g_pen(), (32, 32), false, None);
        assert_eq!(dirty.len(), 1);
        assert_eq!(tiles.pixel(5, 5), [0, 255, 0, 255]);
    }

    #[test]
    fn selection_limits_commit() {
        let mut tiles = TileStore::new();
        let selection = crate::selection::rect_mask([0.0, 0.0], [6.0, 32.0], (32, 32));
        let dabs = [Dab { x: 6.0, y: 6.0, radius: 4.0, alpha: 1.0, angle: 0.0, thickness: 1.0, mirror: false, color: None, texture: 1.0 }];
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

#[cfg(test)]
mod effect_tests {
    use std::sync::Arc;

    use super::*;
    use crate::brush::WaterEdge;
    use crate::tip::{DabTexture, DualTip, MaskMode, TipBitmap};

    fn square_stroke(settings: &BrushSettings) -> RasterizedStroke {
        let mut stroke = RasterizedStroke { buffer: StrokeBuffer::new(), dirty: BTreeSet::new() };
        for y in 0..8 {
            for x in 0..8 {
                let dab = Dab::at(30.0 + x as f32 * 5.0, 30.0 + y as f32 * 5.0, 6.0, 0.6);
                stamp_dab(&mut stroke.buffer, &dab, [0.8, 0.4, 0.2], settings, (128, 128), &mut stroke.dirty);
            }
        }
        stroke
    }

    #[test]
    fn water_edge_pools_at_the_rim() {
        let settings = BrushSettings { hardness: 1.0, ..BrushSettings::g_pen() };
        let mut stroke = square_stroke(&settings);
        let (inside, rim) = (stroke.buffer.pixel(47, 47)[3], stroke.buffer.pixel(25, 47)[3]);
        water_edge(&mut stroke, &WaterEdge { radius: 4.0, thinning: 0.5, rim: 0.5, darkening: 0.3 });
        let (inside2, rim2) = (stroke.buffer.pixel(47, 47), stroke.buffer.pixel(25, 47));
        assert!(inside2[3] < inside * 0.6, "the inside thins: {inside} -> {}", inside2[3]);
        assert!(rim2[3] >= rim * 0.95, "the rim keeps its ink: {rim} -> {}", rim2[3]);
        assert!(rim2[0] / rim2[3] < 0.8 * 0.95, "and darkens");
        assert_eq!(stroke.buffer.pixel(5, 5), [0.0; 4], "nothing outside");
    }

    #[test]
    fn whole_stroke_texture_keeps_its_grain() {
        let stripes = Arc::new(TipBitmap::new(2, 1, vec![0, 255]).unwrap());
        let each = DabTexture { mode: MaskMode::Subtract, density: 0.6, ..DabTexture::new(stripes, 1.0, 1.0, false) };
        let whole = DabTexture { each_dab: false, ..each.clone() };
        let settings = BrushSettings { texture: Some(whole), ..BrushSettings::g_pen() };
        let mut b = square_stroke(&settings);
        finish_stroke(&mut b, &settings);
        let (dark, light) = (b.buffer.pixel(46, 47)[3], b.buffer.pixel(47, 47)[3]);
        assert!(dark < light, "{dark} vs {light}");
        // Subtract takes the texture's darkness off the stroke once: a full one keeps 0.4 there.
        assert!((dark - (light - 0.6).max(0.0)).abs() < 0.02, "{dark} {light}");
        // Per dab, every dab loses it and the overlaps build back up.
        let per_dab = BrushSettings { texture: Some(each), ..BrushSettings::g_pen() };
        let a = square_stroke(&per_dab);
        assert!(a.buffer.pixel(46, 47)[3] > 0.0);
    }

    #[test]
    fn modes_noise_and_tiled_duals() {
        assert_eq!(MaskMode::Multiply.mix(0.5, 0.5), 0.25);
        assert!((MaskMode::Subtract.mix(0.8, 0.5) - 0.3).abs() < 1e-6);
        assert_eq!(MaskMode::Darken.mix(0.8, 0.5), 0.5);
        assert_eq!(MaskMode::HardMix.mix(0.4, 0.5), 0.0);
        assert_eq!(MaskMode::HardMix.mix(0.6, 0.5), 0.6);
        assert!((MaskMode::ColorBurn.mix(0.75, 0.5) - 0.5).abs() < 1e-6);
        // Noise only touches soft coverage, the same way every time.
        assert_eq!(noisy(1.0, 0.9), 1.0);
        assert_eq!(noisy(0.0, 0.9), 0.0);
        assert_eq!(grain(3, 4), grain(3, 4));
        assert_ne!(grain(3, 4), grain(4, 3));
        // A dual tip a quarter of the dab repeats inside it.
        let dot = Arc::new(TipBitmap::new(3, 3, vec![0, 0, 0, 0, 255, 0, 0, 0, 0]).unwrap());
        let settings = BrushSettings { size: 40.0, dual: Some(DualTip::new(dot, 0.25)), ..BrushSettings::g_pen() };
        let mut buffer = StrokeBuffer::new();
        stamp_dab(&mut buffer, &Dab::at(32.0, 32.0, 20.0, 1.0), [1.0; 3], &settings, (64, 64), &mut BTreeSet::new());
        // On a row through the dots' centres (every 10 px, from 4.5).
        let inked = (12..52).filter(|x| buffer.pixel(*x, 34)[3] > 0.5).count();
        assert!((4..30).contains(&inked), "dots, not a solid run: {inked}");
        // Replaying a vector stroke saved before 1.0.3 (`DualTip::centred`): one dot in the middle.
        let centred = DualTip { centred: true, ..settings.dual.clone().unwrap() };
        let settings = BrushSettings { dual: Some(centred), ..settings };
        let mut buffer = StrokeBuffer::new();
        stamp_dab(&mut buffer, &Dab::at(32.0, 32.0, 20.0, 1.0), [1.0; 3], &settings, (64, 64), &mut BTreeSet::new());
        let inked: Vec<i32> = (0..64).filter(|x| buffer.pixel(*x, 32)[3] > 0.1).collect();
        assert!(!inked.is_empty() && inked.iter().all(|x| (27..=37).contains(x)), "only the centre: {inked:?}");
    }

    /// A broad wash: soft dabs over most of a `w`×`h` canvas.
    fn wash(w: u32, h: u32) -> RasterizedStroke {
        let settings = BrushSettings { hardness: 0.3, ..BrushSettings::airbrush() };
        let mut stroke = RasterizedStroke { buffer: StrokeBuffer::new(), dirty: BTreeSet::new() };
        let mut y = 60.0;
        while y < h as f32 - 60.0 {
            let mut x = 60.0;
            while x < w as f32 - 60.0 {
                stamp_dab(&mut stroke.buffer, &Dab::at(x, y, 70.0, 0.4), [0.3, 0.5, 0.8], &settings, (w, h), &mut stroke.dirty);
                x += 45.0;
            }
            y += 45.0 + (y * 0.37) % 9.0;
        }
        stroke
    }

    /// The first [`water_edge`]: a pixel lookup per window pixel, one thread.
    fn water_edge_reference(stroke: &mut RasterizedStroke, edge: &WaterEdge) {
        let r = edge.radius.round().clamp(1.0, TILE_SIZE as f32) as i32;
        let t = TILE_SIZE as i32;
        let side = t + 2 * r;
        let mut out: Vec<(TileCoord, Box<[Rgba]>)> = Vec::new();
        for &coord in &stroke.dirty {
            let Some(own) = stroke.buffer.get(coord) else { continue };
            let (ox, oy) = coord.origin();
            let mut sum = vec![0.0f64; ((side + 1) * (side + 1)) as usize];
            for wy in 0..side {
                let mut row = 0.0f64;
                for wx in 0..side {
                    row += stroke.buffer.pixel(ox - r + wx, oy - r + wy)[3] as f64;
                    let i = ((wy + 1) * (side + 1) + wx + 1) as usize;
                    sum[i] = sum[i - (side + 1) as usize] + row;
                }
            }
            let area = ((2 * r + 1) * (2 * r + 1)) as f64;
            let mut tile = own.to_vec().into_boxed_slice();
            for (i, p) in tile.iter_mut().enumerate() {
                let a = p[3];
                if a <= 0.0 {
                    continue;
                }
                let (lx, ly) = ((i as i32 % t) + r, (i as i32 / t) + r);
                let at = |x: i32, y: i32| sum[(y * (side + 1) + x) as usize];
                let box_sum = at(lx + r + 1, ly + r + 1) - at(lx - r, ly + r + 1) - at(lx + r + 1, ly - r) + at(lx - r, ly - r);
                let mean = (box_sum / area) as f32;
                let e = ((a - mean) / a.max(1e-4) * 4.0).clamp(0.0, 1.0);
                let thinned = a * ((1.0 - edge.thinning.clamp(0.0, 1.0)) + edge.thinning.clamp(0.0, 1.0) * e);
                let pooled = thinned + (1.0 - thinned) * edge.rim.clamp(0.0, 1.0) * e;
                let k = pooled / a * (1.0 - edge.darkening.clamp(0.0, 1.0) * e);
                *p = [p[0] * k, p[1] * k, p[2] * k, pooled];
            }
            out.push((coord, tile));
        }
        for (coord, tile) in out {
            stroke.buffer.tile_mut(coord).copy_from_slice(&tile);
        }
    }

    #[test]
    fn water_edge_matches_the_reference_exactly() {
        // ~40 tiles, so the work is split across threads, with gaps and ragged edges.
        for radius in [1.0, 5.0, 64.0] {
            let edge = WaterEdge { radius, thinning: 0.5, rim: 0.4, darkening: 0.2 };
            let (mut fast, mut slow) = (wash(420, 330), wash(420, 330));
            water_edge(&mut fast, &edge);
            water_edge_reference(&mut slow, &edge);
            assert_eq!(fast.dirty, slow.dirty);
            for coord in &slow.dirty {
                assert_eq!(fast.buffer.get(*coord), slow.buffer.get(*coord), "tile {coord:?}, radius {radius}");
            }
        }
    }

    #[test]
    #[ignore = "timing; run with --release -- --ignored --nocapture"]
    fn water_edge_timing() {
        for radius in [4.0, 32.0, 64.0] {
            let mut s = wash(3000, 3000);
            println!("{} tiles", s.dirty.len());
            let t = std::time::Instant::now();
            water_edge(&mut s, &WaterEdge { radius, thinning: 0.5, rim: 0.3, darkening: 0.1 });
            println!("radius {radius}: {:?}", t.elapsed());
        }
        let paper = Arc::new(TipBitmap::new(4, 4, (0..16).map(|i| (i * 16) as u8).collect()).unwrap());
        let mut s = wash(3000, 3000);
        let t = std::time::Instant::now();
        texture_stroke(&mut s, &DabTexture { each_dab: false, ..DabTexture::new(paper, 3.0, 0.8, false) });
        println!("whole-stroke texture: {:?}", t.elapsed());
    }
}
