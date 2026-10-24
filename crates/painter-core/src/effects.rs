//! Clip Studio's Layer Property effects on a raster or vector layer, drawn
//! from its pixels without changing them (they show on the canvas and in
//! exports, and can be turned off again):
//! - Extract line: the picture's edges and darkest areas as black lines;
//! - Expression color: Monochrome layers can hide their black or white;
//! - Tone: the layer as halftone dots (circle, square, diamond, line, cross
//!   or noise) at a screen size and angle, dense where the layer is dark
//!   (or opaque);
//! - Border: an edge of a colour round the shapes, or a watercolour edge
//!   (darker, more opaque rims).
//!
//! The colours a Gray or Monochrome layer may hold are kept by the app as it
//! paints (see `Expression`); this module only draws.

use serde::{Deserialize, Serialize};

use crate::blend::unpremultiply;
use crate::color_mode::luma;
use crate::tile::{Tile, TileCoord, TileStore, TILE_SIZE};

/// What colours the layer holds (Clip Studio's Expression color).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Expression {
    #[default]
    Color,
    /// Greys only.
    Gray,
    /// Black and white only.
    Monochrome,
}

impl Expression {
    pub const ALL: [Expression; 3] = [Expression::Color, Expression::Gray, Expression::Monochrome];

    pub fn name(self) -> &'static str {
        match self {
            Expression::Color => "Color",
            Expression::Gray => "Gray",
            Expression::Monochrome => "Monochrome",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BorderKind {
    /// A rim of one colour round the shapes.
    #[default]
    Edge,
    /// Darker, denser rims inside the shapes, as watercolour dries.
    Watercolor,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Border {
    pub kind: BorderKind,
    /// Edge: thickness; watercolour: how far in the rim reaches. Pixels.
    pub width: f32,
    pub color: [u8; 3],
    /// Watercolour: how see-through the middle gets, 0–1.
    pub transparency: f32,
    /// Watercolour: how dark the rim gets, 0–1.
    pub darkness: f32,
    /// Watercolour: how soft the rim is, pixels.
    pub blur: f32,
}

impl Default for Border {
    fn default() -> Self {
        Self { kind: BorderKind::Edge, width: 5.0, color: [255, 255, 255], transparency: 0.4, darkness: 0.5, blur: 3.0 }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DotShape {
    #[default]
    Circle,
    Square,
    Diamond,
    Line,
    Cross,
    Noise,
}

impl DotShape {
    pub const ALL: [DotShape; 6] =
        [DotShape::Circle, DotShape::Square, DotShape::Diamond, DotShape::Line, DotShape::Cross, DotShape::Noise];

    pub fn name(self) -> &'static str {
        match self {
            DotShape::Circle => "Circle",
            DotShape::Square => "Square",
            DotShape::Diamond => "Diamond",
            DotShape::Line => "Line",
            DotShape::Cross => "Cross",
            DotShape::Noise => "Noise",
        }
    }

    /// The share of a cell a dot covers when it reaches `(fx, fy)` (−0.5..0.5
    /// in the cell): a pixel is inked when this is under the ink wanted.
    fn spot(self, fx: f32, fy: f32, x: f32, y: f32) -> f32 {
        let (ax, ay) = (fx.abs(), fy.abs());
        match self {
            DotShape::Circle => {
                let d = ax.hypot(ay);
                if d <= 0.5 {
                    std::f32::consts::PI * d * d
                } else {
                    0.785 + (d - 0.5) / (std::f32::consts::FRAC_1_SQRT_2 - 0.5) * 0.215
                }
            }
            DotShape::Square => (2.0 * ax.max(ay)).powi(2),
            DotShape::Diamond => {
                let a = ax + ay;
                if a <= 0.5 {
                    2.0 * a * a
                } else {
                    1.0 - 2.0 * (1.0 - a).powi(2)
                }
            }
            DotShape::Line => 2.0 * ay,
            DotShape::Cross => 1.0 - (1.0 - 2.0 * ax.min(ay)).powi(2),
            DotShape::Noise => {
                let h = ((x as u32).wrapping_mul(73_856_093) ^ (y as u32).wrapping_mul(19_349_663)).wrapping_mul(2_654_435_761);
                (h >> 8) as f32 / (1u32 << 24) as f32
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToneEffect {
    /// Dot pitch, canvas pixels (the panel shows lines per inch).
    pub cell: f32,
    pub angle: f32,
    pub dot: DotShape,
    /// How much ink: from the layer's opacity, else from its darkness.
    pub from_alpha: bool,
    /// Dots in this colour, else in the layer's own colours.
    pub color: Option<[u8; 3]>,
    /// White paper between the dots, else see-through.
    pub paper: bool,
}

impl Default for ToneEffect {
    fn default() -> Self {
        Self { cell: 5.0, angle: 45.0, dot: DotShape::Circle, from_alpha: false, color: Some([0, 0, 0]), paper: false }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExtractLine {
    /// How strong an edge must be to become a line, 0–1 (lower: more lines).
    pub edge: f32,
    /// Line thickness, pixels.
    pub width: f32,
    /// Areas darker than this become solid black, 0–1 (0: none).
    pub black: f32,
}

impl Default for ExtractLine {
    fn default() -> Self {
        Self { edge: 0.25, width: 1.0, black: 0.15 }
    }
}

fn yes() -> bool {
    true
}

fn half() -> f32 {
    0.5
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayerEffects {
    #[serde(default)]
    pub expression: Expression,
    /// Monochrome: show the black / the white parts.
    #[serde(default = "yes")]
    pub show_black: bool,
    #[serde(default = "yes")]
    pub show_white: bool,
    /// Monochrome: brightness under which a pixel is black, 0–1.
    #[serde(default = "half")]
    pub threshold: f32,
    #[serde(default)]
    pub border: Option<Border>,
    #[serde(default)]
    pub tone: Option<ToneEffect>,
    #[serde(default)]
    pub extract: Option<ExtractLine>,
}

impl Default for LayerEffects {
    fn default() -> Self {
        Self {
            expression: Expression::Color,
            show_black: true,
            show_white: true,
            threshold: 0.5,
            border: None,
            tone: None,
            extract: None,
        }
    }
}

impl LayerEffects {
    /// Whether anything is drawn differently from the pixels.
    pub fn is_active(&self) -> bool {
        self.border.is_some()
            || self.tone.is_some()
            || self.extract.is_some()
            || (self.expression == Expression::Monochrome && !(self.show_black && self.show_white))
    }
}

/// Keeps the `coords` tiles of a layer (every tile with `None`) in its
/// expression's colours: greys, or opaque black and white (see-through below
/// half opacity). Colour layers are left as they are.
pub fn keep_expression(tiles: &mut TileStore, coords: Option<&[TileCoord]>, fx: &LayerEffects) {
    match fx.expression {
        Expression::Color => {}
        Expression::Gray => crate::color_mode::gray_tiles(tiles, coords),
        Expression::Monochrome => {
            let all: Vec<TileCoord>;
            let coords = match coords {
                Some(c) => c,
                None => {
                    all = tiles.iter().map(|(c, _)| c).collect();
                    &all
                }
            };
            let cut = (fx.threshold.clamp(0.0, 1.0) * 255.0) as u8;
            for &coord in coords {
                let Some(mut tile) = tiles.get(coord).map(|t| (**t).clone()) else { continue };
                for px in tile.bytes_mut().as_chunks_mut::<4>().0 {
                    *px = if px[3] < 128 {
                        [0; 4]
                    } else {
                        let s = unpremultiply(*px);
                        let v = if luma([s[0], s[1], s[2]]) < cut { 0 } else { 255 };
                        [v, v, v, 255]
                    };
                }
                tiles.insert(coord, tile);
            }
        }
    }
}

/// The colour a brush paints with on a layer of `fx`'s expression (straight, 0–1).
pub fn expression_color(fx: &LayerEffects, rgb: [f32; 3]) -> [f32; 3] {
    let v = luma(rgb.map(|c| (c.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)) as f32 / 255.0;
    match fx.expression {
        Expression::Color => rgb,
        Expression::Gray => [v; 3],
        Expression::Monochrome => [if v < fx.threshold { 0.0 } else { 1.0 }; 3],
    }
}

/// Distance from each pixel to the nearest `inside` one (chamfer, 0 inside).
fn distance(inside: &[bool], w: usize, h: usize) -> Vec<f32> {
    let far = f32::MAX / 4.0;
    let mut d: Vec<f32> = inside.iter().map(|&i| if i { 0.0 } else { far }).collect();
    let (a, b) = (1.0, std::f32::consts::SQRT_2);
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let mut v = d[i];
            if x > 0 {
                v = v.min(d[i - 1] + a);
            }
            if y > 0 {
                v = v.min(d[i - w] + a);
                if x > 0 {
                    v = v.min(d[i - w - 1] + b);
                }
                if x + 1 < w {
                    v = v.min(d[i - w + 1] + b);
                }
            }
            d[i] = v;
        }
    }
    for y in (0..h).rev() {
        for x in (0..w).rev() {
            let i = y * w + x;
            let mut v = d[i];
            if x + 1 < w {
                v = v.min(d[i + 1] + a);
            }
            if y + 1 < h {
                v = v.min(d[i + w] + a);
                if x + 1 < w {
                    v = v.min(d[i + w + 1] + b);
                }
                if x > 0 {
                    v = v.min(d[i + w - 1] + b);
                }
            }
            d[i] = v;
        }
    }
    d
}

/// A box blur of radius `r` pixels, both ways.
fn blur(v: &mut [f32], w: usize, h: usize, r: usize) {
    if r == 0 {
        return;
    }
    let mut tmp = vec![0.0f32; v.len()];
    for y in 0..h {
        for x in 0..w {
            let (a, b) = (x.saturating_sub(r), (x + r).min(w - 1));
            tmp[y * w + x] = v[y * w + a..=y * w + b].iter().sum::<f32>() / (b - a + 1) as f32;
        }
    }
    for y in 0..h {
        for x in 0..w {
            let (a, b) = (y.saturating_sub(r), (y + r).min(h - 1));
            v[y * w + x] = (a..=b).map(|yy| tmp[yy * w + x]).sum::<f32>() / (b - a + 1) as f32;
        }
    }
}

/// The layer `tiles` with `fx` applied (canvas `canvas`): what it shows.
pub fn render(tiles: &TileStore, fx: &LayerEffects, canvas: (u32, u32)) -> TileStore {
    if !fx.is_active() || tiles.is_empty() {
        return tiles.clone();
    }
    // The part of the canvas the layer and its border can reach.
    let pad = fx.border.map_or(0.0, |b| if b.kind == BorderKind::Edge { b.width + 2.0 } else { 0.0 }).ceil() as i32;
    let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for (c, _) in tiles.iter() {
        let (ox, oy) = c.origin();
        x0 = x0.min(ox);
        y0 = y0.min(oy);
        x1 = x1.max(ox + TILE_SIZE as i32);
        y1 = y1.max(oy + TILE_SIZE as i32);
    }
    let (x0, y0) = ((x0 - pad).max(0), (y0 - pad).max(0));
    let (x1, y1) = ((x1 + pad).min(canvas.0 as i32), (y1 + pad).min(canvas.1 as i32));
    if x1 <= x0 || y1 <= y0 {
        return TileStore::new();
    }
    let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
    // Straight colour 0–1 and alpha, per pixel.
    let mut px: Vec<[f32; 4]> = vec![[0.0; 4]; w * h];
    for (c, tile) in tiles.iter() {
        let (ox, oy) = c.origin();
        for ty in 0..TILE_SIZE as i32 {
            let y = oy + ty;
            if y < y0 || y >= y1 {
                continue;
            }
            for tx in 0..TILE_SIZE as i32 {
                let x = ox + tx;
                if x < x0 || x >= x1 {
                    continue;
                }
                let p = tile.pixel(tx as u32, ty as u32);
                if p[3] > 0 {
                    let s = unpremultiply(p);
                    px[(y - y0) as usize * w + (x - x0) as usize] = [s[0], s[1], s[2], s[3]].map(|v| v as f32 / 255.0);
                }
            }
        }
    }
    let lum = |p: &[f32; 4]| luma([p[0], p[1], p[2]].map(|v| (v * 255.0) as u8)) as f32 / 255.0;

    if let Some(e) = fx.extract {
        // Brightness over white, its gradient (Sobel), lines where it's steep.
        let grey: Vec<f32> = px.iter().map(|p| 1.0 - p[3] * (1.0 - lum(p))).collect();
        let g = |x: usize, y: usize| grey[y.min(h - 1) * w + x.min(w - 1)];
        let mut line = vec![false; w * h];
        for y in 0..h {
            for x in 0..w {
                let (xm, ym) = (x.saturating_sub(1), y.saturating_sub(1));
                let gx = g(x + 1, ym) + 2.0 * g(x + 1, y) + g(x + 1, y + 1) - g(xm, ym) - 2.0 * g(xm, y) - g(xm, y + 1);
                let gy = g(xm, y + 1) + 2.0 * g(x, y + 1) + g(x + 1, y + 1) - g(xm, ym) - 2.0 * g(x, ym) - g(x + 1, ym);
                let edge = gx.hypot(gy) / 4.0;
                line[y * w + x] = edge > e.edge.max(0.01) || grey[y * w + x] < e.black;
            }
        }
        let d = distance(&line, w, h);
        let r = (e.width - 1.0).max(0.0) / 2.0;
        for (p, d) in px.iter_mut().zip(d) {
            let a = (r + 1.0 - d).clamp(0.0, 1.0);
            *p = [0.0, 0.0, 0.0, a];
        }
    }

    if fx.expression == Expression::Monochrome && !(fx.show_black && fx.show_white) {
        for p in &mut px {
            let black = lum(p) < fx.threshold;
            if (black && !fx.show_black) || (!black && !fx.show_white) {
                *p = [0.0; 4];
            }
        }
    }

    if let Some(t) = fx.tone {
        let cell = t.cell.max(2.0);
        let (s, c) = t.angle.to_radians().sin_cos();
        for y in 0..h {
            for x in 0..w {
                let p = &mut px[y * w + x];
                if p[3] <= 0.0 {
                    continue;
                }
                let ink = if t.from_alpha { p[3] } else { (1.0 - lum(p)) * p[3] };
                let (cx, cy) = ((x as i32 + x0) as f32 + 0.5, (y as i32 + y0) as f32 + 0.5);
                let (u, v) = ((cx * c + cy * s) / cell, (-cx * s + cy * c) / cell);
                let inked = ink > 0.0 && t.dot.spot(u - u.round(), v - v.round(), cx, cy) < ink;
                *p = if inked {
                    let col = t.color.map_or([p[0], p[1], p[2]], |k| k.map(|v| v as f32 / 255.0));
                    [col[0], col[1], col[2], 1.0]
                } else if t.paper {
                    [1.0, 1.0, 1.0, p[3]]
                } else {
                    [0.0; 4]
                };
            }
        }
    }

    if let Some(b) = fx.border {
        match b.kind {
            BorderKind::Edge => {
                let inside: Vec<bool> = px.iter().map(|p| p[3] >= 0.5).collect();
                let d = distance(&inside, w, h);
                let col = b.color.map(|v| v as f32 / 255.0);
                for (p, d) in px.iter_mut().zip(d) {
                    let rim = (b.width + 0.5 - d).clamp(0.0, 1.0);
                    // The layer over its border.
                    let a = p[3] + rim * (1.0 - p[3]);
                    if a > 0.0 {
                        let mix = |k: usize| (p[k] * p[3] + col[k] * rim * (1.0 - p[3])) / a;
                        *p = [mix(0), mix(1), mix(2), a];
                    }
                }
            }
            BorderKind::Watercolor => {
                let outside: Vec<bool> = px.iter().map(|p| p[3] < 0.5).collect();
                let d = distance(&outside, w, h);
                let mut rim: Vec<f32> = d.iter().map(|&d| (1.0 - d / b.width.max(1.0)).clamp(0.0, 1.0)).collect();
                blur(&mut rim, w, h, b.blur.round().max(0.0) as usize);
                for (p, r) in px.iter_mut().zip(rim) {
                    if p[3] <= 0.0 {
                        continue;
                    }
                    let dark = 1.0 - b.darkness.clamp(0.0, 1.0) * r;
                    *p = [p[0] * dark, p[1] * dark, p[2] * dark, p[3] * (1.0 - b.transparency.clamp(0.0, 1.0) * (1.0 - r))];
                }
            }
        }
    }

    // Back into tiles, premultiplied.
    let mut out = TileStore::new();
    let (t0, t1) = (TileCoord::containing(x0, y0), TileCoord::containing(x1 - 1, y1 - 1));
    for ty in t0.y..=t1.y {
        for tx in t0.x..=t1.x {
            let coord = TileCoord::new(tx, ty);
            let (ox, oy) = coord.origin();
            let mut tile = Tile::empty();
            let mut any = false;
            for py in oy.max(y0)..(oy + TILE_SIZE as i32).min(y1) {
                for pxx in ox.max(x0)..(ox + TILE_SIZE as i32).min(x1) {
                    let p = px[(py - y0) as usize * w + (pxx - x0) as usize];
                    if p[3] <= 0.0 {
                        continue;
                    }
                    let a = p[3].clamp(0.0, 1.0);
                    let q = [p[0] * a, p[1] * a, p[2] * a, a].map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
                    tile.set_pixel((pxx - ox) as u32, (py - oy) as u32, q);
                    any = true;
                }
            }
            if any {
                out.insert(coord, tile);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 40 × 40 grey square at (20, 20) on a 100 × 100 canvas.
    fn square(v: u8) -> TileStore {
        let mut rgba = vec![0u8; 100 * 100 * 4];
        for y in 20..60 {
            for x in 20..60 {
                rgba[(y * 100 + x) * 4..(y * 100 + x) * 4 + 4].copy_from_slice(&[v, v, v, 255]);
            }
        }
        TileStore::from_rgba(100, 100, &rgba)
    }

    #[test]
    fn nothing_on_means_the_pixels_as_they_are() {
        let t = square(80);
        assert!(!LayerEffects::default().is_active());
        assert_eq!(render(&t, &LayerEffects::default(), (100, 100)).pixel(30, 30), t.pixel(30, 30));
        // Older files load without effects.
        let back: LayerEffects = serde_json::from_str("{}").unwrap();
        assert_eq!(back, LayerEffects::default());
    }

    #[test]
    fn edges_round_the_shapes() {
        let fx = LayerEffects { border: Some(Border { width: 4.0, color: [255, 0, 0], ..Default::default() }), ..Default::default() };
        let out = render(&square(0), &fx, (100, 100));
        assert_eq!(out.pixel(30, 30), [0, 0, 0, 255], "the layer stays on top");
        assert_eq!(out.pixel(17, 30), [255, 0, 0, 255], "a red rim outside");
        assert_eq!(out.pixel(10, 30)[3], 0, "nothing past the rim");
        // Watercolour: darker at the rim, lighter in the middle.
        let fx = LayerEffects {
            border: Some(Border { kind: BorderKind::Watercolor, width: 8.0, blur: 0.0, ..Default::default() }),
            ..Default::default()
        };
        let out = render(&square(200), &fx, (100, 100));
        let (rim, middle) = (out.pixel(20, 40), out.pixel(40, 40));
        assert!(rim[3] > middle[3] && middle[3] > 0, "{rim:?} {middle:?}");
    }

    #[test]
    fn tones_follow_the_greys() {
        let fx = |dot| LayerEffects { tone: Some(ToneEffect { cell: 6.0, dot, ..Default::default() }), ..Default::default() };
        for dot in DotShape::ALL {
            for (v, want) in [(200u8, 0.22f32), (60, 0.76)] {
                let out = render(&square(v), &fx(dot), (100, 100));
                let inked = (20..60).flat_map(|y| (20..60).map(move |x| (x, y))).filter(|&(x, y)| out.pixel(x, y)[3] == 255).count();
                let share = inked as f32 / 1600.0;
                assert!((share - want).abs() < 0.12, "{dot:?} at {v}: {share}");
            }
        }
        // Only black or nothing (no paper).
        let out = render(&square(128), &fx(DotShape::Circle), (100, 100));
        assert!((20..60).all(|x| matches!(out.pixel(x, 40), [0, 0, 0, 255] | [0, 0, 0, 0])));
    }

    #[test]
    fn monochrome_can_hide_white_and_lines_come_out_of_pictures() {
        let mut rgba = vec![255u8; 100 * 100 * 4];
        for y in 0..100 {
            for x in 50..100 {
                rgba[(y * 100 + x) * 4..(y * 100 + x) * 4 + 3].copy_from_slice(&[0, 0, 0]);
            }
        }
        let t = TileStore::from_rgba(100, 100, &rgba);
        let fx = LayerEffects { expression: Expression::Monochrome, show_white: false, ..Default::default() };
        let out = render(&t, &fx, (100, 100));
        assert_eq!(out.pixel(10, 10)[3], 0, "white hidden");
        assert_eq!(out.pixel(80, 10), [0, 0, 0, 255], "black kept");
        // Extract line: the boundary and the dark half.
        let lines = LayerEffects { extract: Some(ExtractLine { black: 0.0, ..Default::default() }), ..Default::default() };
        let out = render(&t, &lines, (100, 100));
        assert_eq!(out.pixel(49, 50), [0, 0, 0, 255], "a line on the edge");
        assert_eq!(out.pixel(20, 50)[3], 0, "flat white: no line");
        assert_eq!(out.pixel(80, 50)[3], 0, "flat black without black fill: no line");
        let filled = LayerEffects { extract: Some(ExtractLine::default()), ..Default::default() };
        assert_eq!(render(&t, &filled, (100, 100)).pixel(80, 50), [0, 0, 0, 255], "dark areas filled");
    }

    #[test]
    fn gray_and_monochrome_layers_keep_their_colours() {
        let mut t = TileStore::from_rgba(64, 64, &[200, 60, 30, 255].repeat(64 * 64));
        let mono = LayerEffects { expression: Expression::Monochrome, ..Default::default() };
        keep_expression(&mut t, None, &mono);
        assert_eq!(t.pixel(5, 5), [0, 0, 0, 255], "a dark red is black");
        let mut t = TileStore::from_rgba(64, 64, &[200, 60, 30, 255].repeat(64 * 64));
        keep_expression(&mut t, None, &LayerEffects { expression: Expression::Gray, ..Default::default() });
        let p = t.pixel(5, 5);
        assert!(p[0] == p[1] && p[1] == p[2] && p[3] == 255);
        assert_eq!(expression_color(&mono, [0.9, 0.9, 0.1]), [1.0; 3]);
        assert_eq!(expression_color(&LayerEffects::default(), [0.9, 0.9, 0.1]), [0.9, 0.9, 0.1]);
    }
}
