//! Text and balloons on vector layers (Clip Studio's text tool and balloons,
//! Photoshop's type): each [`TextItem`] is a block of text in a font, with an
//! optional outline round the letters and an optional balloon (speech,
//! thought, shout) with a tail. They're drawn into the vector layer's cache
//! with its strokes, so they show, export and save like the rest of it, and
//! stay editable.
//!
//! Fonts come from the system's font folders ([`scan_font_dirs`]) by full
//! name ("Arial Bold"); a font missing on this computer falls back to the
//! built-in one ([`set_fallback_font`]).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

use ab_glyph::{point, Font, FontArc, FontVec, PxScale, ScaleFont};
use serde::{Deserialize, Serialize};

use crate::tile::{TileCoord, TileStore, TILE_SIZE};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextAlign {
    Left,
    #[default]
    Center,
    Right,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BalloonShape {
    #[default]
    Ellipse,
    Rounded,
    Rectangle,
    /// Scalloped edge: thoughts.
    Cloud,
    /// Spikes: shouts and sound.
    Burst,
}

impl BalloonShape {
    pub const ALL: [BalloonShape; 5] =
        [BalloonShape::Ellipse, BalloonShape::Rounded, BalloonShape::Rectangle, BalloonShape::Cloud, BalloonShape::Burst];

    pub fn name(self) -> &'static str {
        match self {
            BalloonShape::Ellipse => "Ellipse",
            BalloonShape::Rounded => "Rounded",
            BalloonShape::Rectangle => "Rectangle",
            BalloonShape::Cloud => "Cloud",
            BalloonShape::Burst => "Burst",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Balloon {
    pub shape: BalloonShape,
    /// Half width and half height, canvas pixels, round the text's centre.
    pub radii: [f32; 2],
    /// Straight RGBA.
    pub fill: [u8; 4],
    pub line: [u8; 3],
    /// Border width, pixels (0: none).
    pub width: f32,
    /// Where the tail points, if it has one.
    pub tail: Option<[f32; 2]>,
}

impl Default for Balloon {
    fn default() -> Self {
        Self { shape: BalloonShape::Ellipse, radii: [200.0, 120.0], fill: [255; 4], line: [0, 0, 0], width: 4.0, tail: None }
    }
}

fn one() -> f32 {
    1.0
}

fn white() -> [u8; 3] {
    [255; 3]
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextItem {
    pub text: String,
    /// Full font name ("Arial Bold"); empty or missing: the built-in font.
    pub font: String,
    /// Line height of the font, pixels.
    pub size: f32,
    pub color: [u8; 3],
    /// Centre of the text (and of its balloon), canvas pixels.
    pub pos: [f32; 2],
    #[serde(default)]
    pub align: TextAlign,
    /// Multiple of the font's line height.
    #[serde(default = "one")]
    pub line_spacing: f32,
    /// Extra space between letters, pixels.
    #[serde(default)]
    pub tracking: f32,
    /// Columns top to bottom, right to left (manga lettering).
    #[serde(default)]
    pub vertical: bool,
    /// Outline round the letters, pixels (0: none).
    #[serde(default)]
    pub outline: f32,
    #[serde(default = "white")]
    pub outline_color: [u8; 3],
    #[serde(default)]
    pub balloon: Option<Balloon>,
}

impl Default for TextItem {
    fn default() -> Self {
        Self {
            text: String::new(),
            font: String::new(),
            size: 48.0,
            color: [0, 0, 0],
            pos: [0.0, 0.0],
            align: TextAlign::Center,
            line_spacing: 1.0,
            tracking: 0.0,
            vertical: false,
            outline: 0.0,
            outline_color: [255; 3],
            balloon: None,
        }
    }
}

impl TextItem {
    /// Moves the item with `map` (canvas point to canvas point) and scales
    /// its sizes by `scale`: crops, resizes, copies between resolutions.
    pub fn transform(&mut self, map: impl Fn([f32; 2]) -> [f32; 2], scale: f32) {
        self.pos = map(self.pos);
        self.size *= scale;
        self.tracking *= scale;
        self.outline *= scale;
        if let Some(b) = &mut self.balloon {
            b.radii = b.radii.map(|r| r * scale);
            b.width *= scale;
            b.tail = b.tail.map(&map);
        }
    }
}

// ---- fonts ------------------------------------------------------------------

#[derive(Default)]
struct Registry {
    fallback: Option<FontArc>,
    /// Full name → file and face index.
    files: BTreeMap<String, (PathBuf, u32)>,
    loaded: HashMap<String, FontArc>,
}

fn registry() -> &'static RwLock<Registry> {
    static REGISTRY: OnceLock<RwLock<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}

/// The font used when a text's font isn't on this computer.
pub fn set_fallback_font(bytes: Vec<u8>) {
    if let Ok(font) = FontArc::try_from_vec(bytes) {
        registry().write().unwrap_or_else(|e| e.into_inner()).fallback = Some(font);
    }
}

/// The font folders of this system.
pub fn system_font_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(windows) {
        if let Some(w) = std::env::var_os("WINDIR") {
            dirs.push(PathBuf::from(w).join("Fonts"));
        }
        if let Some(l) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(l).join("Microsoft/Windows/Fonts"));
        }
    } else if cfg!(any(target_os = "macos", target_os = "ios")) {
        dirs.extend(["/System/Library/Fonts", "/Library/Fonts"].map(PathBuf::from));
        if let Some(h) = &home {
            dirs.push(h.join("Library/Fonts"));
        }
    } else if cfg!(target_os = "android") {
        dirs.push(PathBuf::from("/system/fonts"));
    } else {
        dirs.extend(["/usr/share/fonts", "/usr/local/share/fonts"].map(PathBuf::from));
        if let Some(h) = &home {
            dirs.push(h.join(".local/share/fonts"));
            dirs.push(h.join(".fonts"));
        }
    }
    dirs
}

/// The full names of a font file's faces.
fn face_names(data: &[u8]) -> Vec<(String, u32)> {
    let count = ttf_parser::fonts_in_collection(data).unwrap_or(1);
    (0..count)
        .filter_map(|i| {
            let face = ttf_parser::Face::parse(data, i).ok()?;
            let name = |id| face.names().into_iter().filter(|n| n.name_id == id).find_map(|n| n.to_string());
            let full = name(ttf_parser::name_id::FULL_NAME).or_else(|| name(ttf_parser::name_id::FAMILY))?;
            Some((full, i))
        })
        .collect()
}

/// Registers every font in `dirs` (and their subfolders) by full name.
/// Reads each file once; meant for a background thread.
pub fn scan_font_dirs(dirs: &[PathBuf]) {
    let mut found = BTreeMap::new();
    let mut stack: Vec<PathBuf> = dirs.to_vec();
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
            if !matches!(ext.as_deref(), Some("ttf" | "otf" | "ttc" | "otc")) {
                continue;
            }
            let Ok(data) = std::fs::read(&path) else { continue };
            for (name, index) in face_names(&data) {
                found.entry(name).or_insert((path.clone(), index));
            }
        }
    }
    registry().write().unwrap_or_else(|e| e.into_inner()).files.extend(found);
}

/// Registers one font file (a font next to a project, say).
pub fn add_font_file(path: &Path) -> Vec<String> {
    let Ok(data) = std::fs::read(path) else { return Vec::new() };
    let names = face_names(&data);
    let mut r = registry().write().unwrap_or_else(|e| e.into_inner());
    for (name, index) in &names {
        r.files.insert(name.clone(), (path.to_path_buf(), *index));
    }
    names.into_iter().map(|(n, _)| n).collect()
}

/// Every known font's full name, sorted.
pub fn font_names() -> Vec<String> {
    registry().read().unwrap_or_else(|e| e.into_inner()).files.keys().cloned().collect()
}

/// Whether `name` is on this computer (else texts in it use the built-in font).
pub fn has_font(name: &str) -> bool {
    name.is_empty() || registry().read().unwrap_or_else(|e| e.into_inner()).files.contains_key(name)
}

fn font(name: &str) -> Option<FontArc> {
    {
        let r = registry().read().unwrap_or_else(|e| e.into_inner());
        if let Some(f) = r.loaded.get(name) {
            return Some(f.clone());
        }
        if !r.files.contains_key(name) {
            return r.fallback.clone();
        }
    }
    let mut r = registry().write().unwrap_or_else(|e| e.into_inner());
    let (path, index) = r.files.get(name)?.clone();
    let font = std::fs::read(&path).ok().and_then(|d| FontVec::try_from_vec_and_index(d, index).ok()).map(FontArc::new);
    match font {
        Some(f) => {
            r.loaded.insert(name.to_string(), f.clone());
            Some(f)
        }
        None => r.fallback.clone(),
    }
}

// ---- layout ---------------------------------------------------------------

/// Fonts tried, in order, for characters the text's font lacks (Japanese,
/// Chinese, Korean, symbols), by full name on the usual systems.
const FALLBACK_FONTS: &[&str] = &[
    "Yu Gothic Regular",
    "Yu Gothic",
    "Meiryo",
    "MS Gothic",
    "Microsoft YaHei",
    "Malgun Gothic",
    "Hiragino Sans W3",
    "Hiragino Kaku Gothic ProN W3",
    "PingFang SC Regular",
    "Apple SD Gothic Neo Regular",
    "Noto Sans CJK JP Regular",
    "Noto Sans CJK JP",
    "Droid Sans Fallback",
    "Segoe UI Symbol",
    "Arial Unicode MS",
    "DejaVu Sans",
];

/// The text's font, then the fallbacks found on this computer (loaded when first needed).
struct Fonts {
    list: Vec<FontArc>,
    more: Option<std::vec::IntoIter<&'static str>>,
}

impl Fonts {
    fn new(main: FontArc) -> Self {
        Self { list: vec![main], more: Some(FALLBACK_FONTS.to_vec().into_iter()) }
    }

    /// The first font with `ch` (index into `list`) and its glyph.
    fn pick(&mut self, ch: char) -> (usize, ab_glyph::GlyphId) {
        for (i, f) in self.list.iter().enumerate() {
            let id = f.glyph_id(ch);
            if id.0 != 0 {
                return (i, id);
            }
        }
        if ch.is_whitespace() || ch.is_control() {
            return (0, self.list[0].glyph_id(ch));
        }
        while let Some(name) = self.more.as_mut().and_then(Iterator::next) {
            let known = registry().read().unwrap_or_else(|e| e.into_inner()).files.contains_key(name);
            if let Some(f) = known.then(|| font(name)).flatten() {
                self.list.push(f);
                let id = self.list.last().map(|f| f.glyph_id(ch)).unwrap_or_default();
                if id.0 != 0 {
                    return (self.list.len() - 1, id);
                }
            }
        }
        (0, self.list[0].glyph_id(ch))
    }
}

/// A glyph placed on the canvas: its font (index into [`Fonts::list`]), glyph id and baseline origin.
struct Placed {
    font: usize,
    glyph: ab_glyph::GlyphId,
    at: [f32; 2],
}

/// Glyph positions and the text block's box `[x0, y0, x1, y1]`.
fn layout(item: &TextItem, fonts: &mut Fonts) -> (Vec<Placed>, [f32; 4]) {
    let scale = PxScale::from(item.size.max(1.0));
    let main = fonts.list[0].as_scaled(scale);
    let (ascent, descent, gap) = (main.ascent(), main.descent(), main.line_gap());
    let lines: Vec<Vec<(usize, ab_glyph::GlyphId, f32)>> = item
        .text
        .split('\n')
        .map(|line| {
            line.chars()
                .map(|ch| {
                    let (f, id) = fonts.pick(ch);
                    (f, id, fonts.list[f].as_scaled(scale).h_advance(id))
                })
                .collect()
        })
        .collect();
    let kern = |fonts: &Fonts, a: (usize, ab_glyph::GlyphId), b: (usize, ab_glyph::GlyphId)| {
        if a.0 == b.0 {
            fonts.list[a.0].as_scaled(scale).kern(a.1, b.1)
        } else {
            0.0
        }
    };
    let mut placed = Vec::new();
    if item.vertical {
        // Columns right to left; each character one em plus tracking down.
        let step = item.size + item.tracking;
        let col = item.size * item.line_spacing;
        let rows = lines.iter().map(Vec::len).max().unwrap_or(0) as f32;
        let (w, h) = (col * lines.len() as f32, step * rows);
        let (x0, y0) = (item.pos[0] - w / 2.0, item.pos[1] - h / 2.0);
        for (c, line) in lines.iter().enumerate() {
            let centre = x0 + w - (c as f32 + 0.5) * col;
            let count = line.len() as f32;
            let top = match item.align {
                TextAlign::Left => y0,
                TextAlign::Center => y0 + (h - count * step) / 2.0,
                TextAlign::Right => y0 + h - count * step,
            };
            for (k, &(f, id, adv)) in line.iter().enumerate() {
                let y = top + k as f32 * step + (step - (ascent - descent)) / 2.0 + ascent;
                placed.push(Placed { font: f, glyph: id, at: [centre - adv / 2.0, y] });
            }
        }
        return (placed, [x0, y0, x0 + w, y0 + h]);
    }
    let line_h = (ascent - descent + gap) * item.line_spacing;
    let widths: Vec<f32> = lines
        .iter()
        .map(|line| {
            let mut w = 0.0;
            for (k, &(f, id, adv)) in line.iter().enumerate() {
                if k > 0 {
                    w += kern(fonts, (line[k - 1].0, line[k - 1].1), (f, id)) + item.tracking;
                }
                w += adv;
            }
            w
        })
        .collect();
    let w = widths.iter().copied().fold(0.0, f32::max);
    let h = line_h * lines.len() as f32;
    let (x0, y0) = (item.pos[0] - w / 2.0, item.pos[1] - h / 2.0);
    for (i, line) in lines.iter().enumerate() {
        let mut x = match item.align {
            TextAlign::Left => x0,
            TextAlign::Center => x0 + (w - widths[i]) / 2.0,
            TextAlign::Right => x0 + w - widths[i],
        };
        let y = y0 + i as f32 * line_h + ascent;
        for (k, &(f, id, adv)) in line.iter().enumerate() {
            if k > 0 {
                x += kern(fonts, (line[k - 1].0, line[k - 1].1), (f, id)) + item.tracking;
            }
            placed.push(Placed { font: f, glyph: id, at: [x, y] });
            x += adv;
        }
    }
    (placed, [x0, y0, x0 + w, y0 + h])
}

/// The box `[x0, y0, x1, y1]` the item covers: letters, outline, balloon and tail.
pub fn bounds(item: &TextItem) -> [f32; 4] {
    let mut b = match font(&item.font) {
        Some(font) => layout(item, &mut Fonts::new(font)).1,
        None => [item.pos[0], item.pos[1], item.pos[0], item.pos[1]],
    };
    let o = item.outline + 2.0;
    b = [b[0] - o, b[1] - o, b[2] + o, b[3] + o];
    if let Some(bal) = &item.balloon {
        let spike = if bal.shape == BalloonShape::Burst { 1.3 } else { 1.12 };
        let (rx, ry) = (bal.radii[0] * spike + bal.width + 2.0, bal.radii[1] * spike + bal.width + 2.0);
        b = [b[0].min(item.pos[0] - rx), b[1].min(item.pos[1] - ry), b[2].max(item.pos[0] + rx), b[3].max(item.pos[1] + ry)];
        if let Some(t) = bal.tail {
            let m = bal.width + 2.0;
            b = [b[0].min(t[0] - m), b[1].min(t[1] - m), b[2].max(t[0] + m), b[3].max(t[1] + m)];
        }
    }
    b
}

// ---- drawing ----------------------------------------------------------------

/// The balloon's outline, canvas pixels.
pub fn balloon_outline(bal: &Balloon, centre: [f32; 2]) -> Vec<[f32; 2]> {
    let [rx, ry] = bal.radii.map(|r| r.max(1.0));
    let [cx, cy] = centre;
    let ellipse = |n: usize, r: &dyn Fn(f32) -> f32| -> Vec<[f32; 2]> {
        (0..n)
            .map(|i| {
                let t = i as f32 / n as f32 * std::f32::consts::TAU;
                let k = r(t);
                [cx + rx * k * t.cos(), cy + ry * k * t.sin()]
            })
            .collect()
    };
    match bal.shape {
        BalloonShape::Ellipse => ellipse(128, &|_| 1.0),
        BalloonShape::Cloud => {
            let bumps = ((rx + ry) / 40.0).clamp(7.0, 16.0).round();
            ellipse(360, &|t: f32| 0.9 + 0.14 * (t * bumps / 2.0).sin().abs())
        }
        BalloonShape::Burst => {
            let spikes = ((rx + ry) / 30.0).clamp(10.0, 24.0).round() as usize;
            (0..spikes * 2)
                .map(|i| {
                    let t = i as f32 / (spikes * 2) as f32 * std::f32::consts::TAU;
                    // Uneven spikes look drawn by hand; seeded by the index so they don't move.
                    let jitter = ((i * 7919 % 13) as f32 / 13.0 - 0.5) * 0.12;
                    let k = if i % 2 == 0 { 1.25 + jitter } else { 0.92 };
                    [cx + rx * k * t.cos(), cy + ry * k * t.sin()]
                })
                .collect()
        }
        BalloonShape::Rectangle => vec![[cx - rx, cy - ry], [cx + rx, cy - ry], [cx + rx, cy + ry], [cx - rx, cy + ry]],
        BalloonShape::Rounded => {
            let r = rx.min(ry) * 0.45;
            let mut out = Vec::new();
            for (ox, oy, start) in [
                (cx + rx - r, cy - ry + r, -90.0f32),
                (cx + rx - r, cy + ry - r, 0.0),
                (cx - rx + r, cy + ry - r, 90.0),
                (cx - rx + r, cy - ry + r, 180.0),
            ] {
                for k in 0..=12 {
                    let a = (start + k as f32 * 90.0 / 12.0).to_radians();
                    out.push([ox + r * a.cos(), oy + r * a.sin()]);
                }
            }
            out
        }
    }
}

/// Coverage of a polygon over a `w` × `h` buffer whose pixel (0, 0) is at canvas `origin`.
fn fill_polygon(cov: &mut [f32], w: usize, h: usize, origin: [i32; 2], points: &[[f32; 2]]) {
    let local: Vec<[f32; 2]> = points.iter().map(|p| [p[0] - origin[0] as f32, p[1] - origin[1] as f32]).collect();
    let mask = crate::selection::polygon_mask(&local, (w as u32, h as u32));
    for (coord, tile) in mask.iter() {
        let (ox, oy) = coord.origin();
        for (i, px) in tile.bytes().as_chunks::<4>().0.iter().enumerate() {
            let (x, y) = (ox + (i as u32 % TILE_SIZE) as i32, oy + (i as u32 / TILE_SIZE) as i32);
            if x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < h {
                let c = &mut cov[y as usize * w + x as usize];
                *c = c.max(px[3] as f32 / 255.0);
            }
        }
    }
}

/// `cov` grown by `r` pixels (chamfer distance from the covered pixels), soft edged.
fn dilate(cov: &[f32], w: usize, h: usize, r: f32) -> Vec<f32> {
    if r <= 0.0 {
        return cov.to_vec();
    }
    let far = f32::MAX / 4.0;
    let mut d: Vec<f32> = cov.iter().map(|&c| if c >= 0.5 { 0.0 } else { far }).collect();
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
    d.iter().zip(cov).map(|(&dist, &c)| (r + 0.5 - dist).clamp(0.0, 1.0).max(c)).collect()
}

/// Lays `cov` in straight colour `rgba` over the premultiplied buffer `out`.
fn paint(out: &mut [[f32; 4]], cov: &[f32], rgba: [u8; 4]) {
    let a = rgba[3] as f32 / 255.0;
    let c = [rgba[0], rgba[1], rgba[2]].map(|v| v as f32 / 255.0);
    for (o, &k) in out.iter_mut().zip(cov) {
        let sa = k * a;
        if sa <= 0.0 {
            continue;
        }
        *o = [c[0] * sa + o[0] * (1.0 - sa), c[1] * sa + o[1] * (1.0 - sa), c[2] * sa + o[2] * (1.0 - sa), sa + o[3] * (1.0 - sa)];
    }
}

/// Draws `item` over `tiles` (source-over), clipped to the canvas.
pub fn render(item: &TextItem, tiles: &mut TileStore, canvas: (u32, u32)) {
    let b = bounds(item);
    let x0 = (b[0].floor() as i32).max(0);
    let y0 = (b[1].floor() as i32).max(0);
    let x1 = (b[2].ceil() as i32).min(canvas.0 as i32);
    let y1 = (b[3].ceil() as i32).min(canvas.1 as i32);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
    let mut out = vec![[0.0f32; 4]; w * h];
    if let Some(bal) = &item.balloon {
        let mut shape = vec![0.0f32; w * h];
        fill_polygon(&mut shape, w, h, [x0, y0], &balloon_outline(bal, item.pos));
        if let Some(tip) = bal.tail {
            // A wedge from inside the balloon to the tip, as wide as a fifth of the balloon.
            let (dx, dy) = (tip[0] - item.pos[0], tip[1] - item.pos[1]);
            let len = dx.hypot(dy).max(1e-3);
            let (nx, ny) = (-dy / len, dx / len);
            let half = bal.radii[0].min(bal.radii[1]) * 0.22;
            let base = [
                item.pos[0] + dx / len * bal.radii[0].min(bal.radii[1]) * 0.5,
                item.pos[1] + dy / len * bal.radii[0].min(bal.radii[1]) * 0.5,
            ];
            fill_polygon(
                &mut shape,
                w,
                h,
                [x0, y0],
                &[[base[0] + nx * half, base[1] + ny * half], tip, [base[0] - nx * half, base[1] - ny * half]],
            );
        }
        if bal.width > 0.0 {
            paint(&mut out, &dilate(&shape, w, h, bal.width), [bal.line[0], bal.line[1], bal.line[2], 255]);
        }
        paint(&mut out, &shape, bal.fill);
    }
    if let Some(font) = font(&item.font) {
        let scale = PxScale::from(item.size.max(1.0));
        let mut letters = vec![0.0f32; w * h];
        let mut fonts = Fonts::new(font);
        for p in layout(item, &mut fonts).0 {
            let glyph = p.glyph.with_scale_and_position(scale, point(p.at[0] - x0 as f32, p.at[1] - y0 as f32));
            if let Some(outlined) = fonts.list[p.font].outline_glyph(glyph) {
                let r = outlined.px_bounds();
                outlined.draw(|gx, gy, c| {
                    let (x, y) = (r.min.x as i32 + gx as i32, r.min.y as i32 + gy as i32);
                    if x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < h {
                        let v = &mut letters[y as usize * w + x as usize];
                        *v = (*v + c).min(1.0);
                    }
                });
            }
        }
        if item.outline > 0.0 {
            let o = item.outline_color;
            paint(&mut out, &dilate(&letters, w, h, item.outline), [o[0], o[1], o[2], 255]);
        }
        paint(&mut out, &letters, [item.color[0], item.color[1], item.color[2], 255]);
    }
    // Over the layer's tiles.
    let (t0, t1) = (TileCoord::containing(x0, y0), TileCoord::containing(x1 - 1, y1 - 1));
    for ty in t0.y..=t1.y {
        for tx in t0.x..=t1.x {
            let coord = TileCoord::new(tx, ty);
            let (ox, oy) = coord.origin();
            let mut tile = tiles.get(coord).map(|t| (**t).clone()).unwrap_or_else(crate::tile::Tile::empty);
            let mut touched = false;
            for py in oy.max(y0)..(oy + TILE_SIZE as i32).min(y1) {
                for px in ox.max(x0)..(ox + TILE_SIZE as i32).min(x1) {
                    let s = out[(py - y0) as usize * w + (px - x0) as usize];
                    if s[3] <= 0.0 {
                        continue;
                    }
                    touched = true;
                    let i = (((py - oy) as u32 * TILE_SIZE + (px - ox) as u32) * 4) as usize;
                    let d = crate::blend::to_f32([tile.bytes()[i], tile.bytes()[i + 1], tile.bytes()[i + 2], tile.bytes()[i + 3]]);
                    let o = [0, 1, 2, 3].map(|k| s[k] + d[k] * (1.0 - s[3]));
                    tile.bytes_mut()[i..i + 4].copy_from_slice(&crate::blend::to_u8(o));
                }
            }
            if touched {
                tiles.insert(coord, tile);
            }
        }
    }
}

/// The item under canvas point `at`, topmost first.
pub fn hit(items: &[TextItem], at: [f32; 2]) -> Option<usize> {
    items
        .iter()
        .enumerate()
        .rev()
        .find(|(_, t)| {
            let b = bounds(t);
            at[0] >= b[0] && at[0] <= b[2] && at[1] >= b[1] && at[1] <= b[3]
        })
        .map(|(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn balloons_and_their_tails_are_drawn_without_a_font() {
        // No fallback font in tests: the balloon still draws.
        let item = TextItem {
            pos: [100.0, 100.0],
            balloon: Some(Balloon { radii: [60.0, 40.0], tail: Some([30.0, 180.0]), ..Default::default() }),
            ..Default::default()
        };
        let mut tiles = TileStore::new();
        render(&item, &mut tiles, (256, 256));
        assert_eq!(tiles.pixel(100, 100), [255, 255, 255, 255], "white inside");
        assert_eq!(tiles.pixel(100, 100 - 41)[0], 0, "black border on the edge");
        assert!(tiles.pixel(40, 168)[3] > 0, "the tail reaches towards its tip");
        assert_eq!(tiles.pixel(200, 30), [0; 4], "nothing outside");
        for shape in BalloonShape::ALL {
            let b = Balloon { shape, ..Default::default() };
            assert!(balloon_outline(&b, [0.0, 0.0]).len() >= 4);
        }
    }

    #[test]
    fn letters_are_drawn_in_a_system_font() {
        scan_font_dirs(&system_font_dirs());
        let Some(name) =
            font_names().into_iter().find(|n| n.contains("Arial") || n.contains("DejaVu Sans") || n.contains("Helvetica"))
        else {
            eprintln!("no common system font here; skipped");
            return;
        };
        let item = TextItem {
            text: "HI\nHI".into(),
            font: name.clone(),
            size: 40.0,
            pos: [100.0, 100.0],
            outline: 3.0,
            ..Default::default()
        };
        let b = bounds(&item);
        assert!(b[2] - b[0] > 30.0 && b[3] - b[1] > 70.0, "{name}: two lines of two letters, got {b:?}");
        let mut tiles = TileStore::new();
        render(&item, &mut tiles, (256, 256));
        let dark = (0..256)
            .flat_map(|y| (0..256).map(move |x| (x, y)))
            .filter(|&(x, y)| tiles.pixel(x, y)[3] == 255 && tiles.pixel(x, y)[0] < 50)
            .count();
        let white = (0..256).flat_map(|y| (0..256).map(move |x| (x, y))).filter(|&(x, y)| tiles.pixel(x, y) == [255; 4]).count();
        assert!(dark > 200, "{name}: black letters ({dark} px)");
        assert!(white > 100, "{name}: a white outline ({white} px)");
        // Japanese in a Latin font: drawn from a fallback font, when this computer has one.
        let has_kana = |f: &&str| *f != name && has_font(f) && font(f).is_some_and(|font| font.glyph_id('あ').0 != 0);
        if FALLBACK_FONTS.iter().any(has_kana) {
            let kana = TextItem { text: "あ".into(), ..item.clone() };
            let mut fonts = Fonts::new(font(&kana.font).unwrap());
            let placed = layout(&kana, &mut fonts).0;
            assert!(placed[0].font > 0 && placed[0].glyph.0 != 0, "{name}: あ from a fallback font");
        }
        // Vertical: taller than wide.
        let v = bounds(&TextItem { vertical: true, text: "ABCD".into(), ..item });
        assert!(v[3] - v[1] > 2.0 * (v[2] - v[0]));
    }

    #[test]
    fn texts_survive_a_save() {
        let item = TextItem { text: "Ciao!".into(), balloon: Some(Balloon::default()), ..Default::default() };
        let back: TextItem = serde_json::from_str(&serde_json::to_string(&item).unwrap()).unwrap();
        assert_eq!(back, item);
        // Older or hand-written files without the optional fields still load.
        let min: TextItem = serde_json::from_str(r#"{"text":"A","font":"","size":10,"color":[0,0,0],"pos":[1,2]}"#).unwrap();
        assert_eq!(min.line_spacing, 1.0);
    }
}
