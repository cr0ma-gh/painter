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
    /// Older files: a rectangle with round corners (now Rectangle with `corner`).
    Rounded,
    /// With corners as round as `corner` says.
    Rectangle,
    /// Scalloped edge: thoughts.
    Cloud,
    /// Spikes: shouts and sound.
    Burst,
    /// `sides` straight sides.
    Polygon,
    /// Drawn by hand (Balloon pen): `points`.
    Free,
}

impl BalloonShape {
    /// The figures to pick from (Rounded is Rectangle with round corners, Free comes from the pen).
    pub const ALL: [BalloonShape; 5] =
        [BalloonShape::Rectangle, BalloonShape::Ellipse, BalloonShape::Polygon, BalloonShape::Cloud, BalloonShape::Burst];

    pub fn name(self) -> &'static str {
        match self {
            BalloonShape::Ellipse => "Ellipse",
            BalloonShape::Rounded => "Rounded",
            BalloonShape::Rectangle => "Rectangle",
            BalloonShape::Cloud => "Cloud",
            BalloonShape::Burst => "Flash",
            BalloonShape::Polygon => "Polygon",
            BalloonShape::Free => "Free",
        }
    }
}

/// Smoothing of the balloon's edges, as Clip Studio's anti-aliasing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Antialias {
    None,
    #[default]
    Weak,
    Medium,
    Strong,
}

impl Antialias {
    pub const ALL: [Antialias; 4] = [Antialias::None, Antialias::Weak, Antialias::Medium, Antialias::Strong];

    pub fn name(self) -> &'static str {
        match self {
            Antialias::None => "None",
            Antialias::Weak => "Weak",
            Antialias::Medium => "Medium",
            Antialias::Strong => "Strong",
        }
    }
}

/// A balloon's tail: from `base` (inside the balloon) to `tip`, through `bend` when it's curved.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tail {
    pub base: [f32; 2],
    #[serde(default)]
    pub bend: Option<[f32; 2]>,
    pub tip: [f32; 2],
    /// Width at the base, pixels.
    pub width: f32,
    /// A trail of little bubbles (thoughts) instead of a wedge.
    #[serde(default)]
    pub thought: bool,
}

impl Tail {
    /// The tail's middle line, base to tip.
    pub fn path(&self) -> Vec<[f32; 2]> {
        let (a, c) = (self.base, self.tip);
        match self.bend {
            None => vec![a, c],
            Some(m) => {
                // A quadratic curve through the bend point.
                let k = [2.0 * m[0] - (a[0] + c[0]) / 2.0, 2.0 * m[1] - (a[1] + c[1]) / 2.0];
                (0..=24)
                    .map(|i| {
                        let t = i as f32 / 24.0;
                        let u = 1.0 - t;
                        [u * u * a[0] + 2.0 * u * t * k[0] + t * t * c[0], u * u * a[1] + 2.0 * u * t * k[1] + t * t * c[1]]
                    })
                    .collect()
            }
        }
    }

    /// Where the bend handle sits: the bend, or the middle of a straight tail.
    pub fn middle(&self) -> [f32; 2] {
        self.bend.unwrap_or([(self.base[0] + self.tip[0]) / 2.0, (self.base[1] + self.tip[1]) / 2.0])
    }

    fn map(&mut self, map: &impl Fn([f32; 2]) -> [f32; 2], scale: f32) {
        self.base = map(self.base);
        self.tip = map(self.tip);
        self.bend = self.bend.map(map);
        self.width *= scale;
    }
}

fn six() -> u32 {
    6
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
    /// Older files' single straight tail: where it points.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tail: Option<[f32; 2]>,
    #[serde(default)]
    pub tails: Vec<Tail>,
    /// Roundness of a rectangle's corners, 0–100 % of its shorter half side.
    #[serde(default)]
    pub corner: f32,
    /// Polygon sides.
    #[serde(default = "six")]
    pub sides: u32,
    /// Turn round the centre, radians.
    #[serde(default)]
    pub rotation: f32,
    /// Rough, hand-inked edge, 0–100 (0: smooth).
    #[serde(default)]
    pub rough: f32,
    #[serde(default)]
    pub antialias: Antialias,
    /// A hand-drawn outline (Free), as fractions of `radii` round the centre.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub points: Vec<[f32; 2]>,
}

impl Default for Balloon {
    fn default() -> Self {
        Self {
            shape: BalloonShape::Ellipse,
            radii: [200.0, 120.0],
            fill: [255; 4],
            line: [0, 0, 0],
            width: 4.0,
            tail: None,
            tails: Vec::new(),
            corner: 0.0,
            sides: 6,
            rotation: 0.0,
            rough: 0.0,
            antialias: Antialias::Weak,
            points: Vec::new(),
        }
    }
}

impl Balloon {
    /// Older files' forms in today's: Rounded as a round-cornered Rectangle,
    /// the single tail as a tail in `tails`. Looks the same.
    pub fn upgrade(&mut self, centre: [f32; 2]) {
        if self.shape == BalloonShape::Rounded {
            self.shape = BalloonShape::Rectangle;
            self.corner = 45.0;
        }
        if let Some(tip) = self.tail.take() {
            self.tails.insert(0, self.old_tail(centre, tip));
        }
    }

    /// The old straight tail: from halfway to the edge, a fifth of the balloon wide.
    fn old_tail(&self, centre: [f32; 2], tip: [f32; 2]) -> Tail {
        let (dx, dy) = (tip[0] - centre[0], tip[1] - centre[1]);
        let len = dx.hypot(dy).max(1e-3);
        let r = self.radii[0].min(self.radii[1]);
        Tail { base: [centre[0] + dx / len * r * 0.5, centre[1] + dy / len * r * 0.5], bend: None, tip, width: r * 0.44, thought: false }
    }

    /// Every tail, the old single one included.
    pub fn all_tails(&self, centre: [f32; 2]) -> Vec<Tail> {
        let mut out: Vec<Tail> = self.tail.map(|t| self.old_tail(centre, t)).into_iter().collect();
        out.extend(self.tails.iter().cloned());
        out
    }

    /// A tail as wide as `percent` of the balloon's shorter half side.
    pub fn tail_width(&self, percent: f32) -> f32 {
        self.radii[0].min(self.radii[1]) * percent / 100.0
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
            for t in &mut b.tails {
                t.map(&map, scale);
            }
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
        let m = bal.width + rough_amplitude(bal) * 2.0 + 3.0;
        let mut grow = |p: [f32; 2], m: f32| b = [b[0].min(p[0] - m), b[1].min(p[1] - m), b[2].max(p[0] + m), b[3].max(p[1] + m)];
        for p in balloon_outline(bal, item.pos) {
            grow(p, m);
        }
        for t in bal.all_tails(item.pos) {
            for p in t.path() {
                grow(p, t.width / 2.0 + bal.width + 3.0);
            }
        }
    }
    b
}

/// The box `[x0, y0, x1, y1]` of the balloon alone (no tails), turned with it:
/// its corners in balloon space are `±radii` round the centre.
pub fn balloon_box(bal: &Balloon, centre: [f32; 2]) -> [[f32; 2]; 4] {
    let [rx, ry] = bal.radii;
    let (s, c) = bal.rotation.sin_cos();
    [[-rx, -ry], [rx, -ry], [rx, ry], [-rx, ry]].map(|[x, y]| [centre[0] + x * c - y * s, centre[1] + x * s + y * c])
}

/// How far a rough edge wanders, pixels.
fn rough_amplitude(bal: &Balloon) -> f32 {
    bal.rough.clamp(0.0, 100.0) / 100.0 * (bal.width * 1.2 + bal.radii[0].min(bal.radii[1]) * 0.012)
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
    let rect = |r: f32| -> Vec<[f32; 2]> {
        let r = r.clamp(0.0, rx.min(ry));
        if r < 0.5 {
            return vec![[cx - rx, cy - ry], [cx + rx, cy - ry], [cx + rx, cy + ry], [cx - rx, cy + ry]];
        }
        let steps = ((r / 4.0).ceil() as usize).clamp(6, 32);
        let mut out = Vec::new();
        for (ox, oy, start) in
            [(cx + rx - r, cy - ry + r, -90.0f32), (cx + rx - r, cy + ry - r, 0.0), (cx - rx + r, cy + ry - r, 90.0), (cx - rx + r, cy - ry + r, 180.0)]
        {
            for k in 0..=steps {
                let a = (start + k as f32 * 90.0 / steps as f32).to_radians();
                out.push([ox + r * a.cos(), oy + r * a.sin()]);
            }
        }
        out
    };
    let mut out = match bal.shape {
        BalloonShape::Ellipse => ellipse(128, &|_| 1.0),
        BalloonShape::Rectangle => rect(bal.corner / 100.0 * rx.min(ry)),
        BalloonShape::Rounded => rect(rx.min(ry) * 0.45),
        BalloonShape::Polygon => {
            let n = bal.sides.clamp(3, 32) as usize;
            (0..n)
                .map(|i| {
                    let t = (i as f32 / n as f32 - 0.25) * std::f32::consts::TAU;
                    [cx + rx * t.cos(), cy + ry * t.sin()]
                })
                .collect()
        }
        BalloonShape::Free if bal.points.len() >= 3 => bal.points.iter().map(|p| [cx + p[0] * rx, cy + p[1] * ry]).collect(),
        BalloonShape::Free => ellipse(128, &|_| 1.0),
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
    };
    if bal.rotation != 0.0 {
        let (s, c) = bal.rotation.sin_cos();
        for p in &mut out {
            let (x, y) = (p[0] - cx, p[1] - cy);
            *p = [cx + x * c - y * s, cy + x * s + y * c];
        }
    }
    out
}

/// `points` walked every `step` pixels and pushed out along the edge's normal
/// by `offset(distance along the edge, perimeter)`.
fn displace(points: &[[f32; 2]], step: f32, offset: &dyn Fn(f32, f32) -> f32) -> Vec<[f32; 2]> {
    let n = points.len();
    let perimeter: f32 = (0..n).map(|i| dist(points[i], points[(i + 1) % n])).sum();
    if perimeter <= 0.0 {
        return points.to_vec();
    }
    // Outward normals point away from the inside, whichever way round the outline goes.
    let area: f32 = (0..n).map(|i| points[i][0] * points[(i + 1) % n][1] - points[(i + 1) % n][0] * points[i][1]).sum();
    let sign = if area > 0.0 { 1.0 } else { -1.0 };
    let mut out = Vec::new();
    let mut walked = 0.0;
    for i in 0..n {
        let (a, b) = (points[i], points[(i + 1) % n]);
        let len = dist(a, b);
        if len <= 0.0 {
            continue;
        }
        let normal = [(b[1] - a[1]) / len * sign, -(b[0] - a[0]) / len * sign];
        let count = (len / step).ceil().max(1.0) as usize;
        for k in 0..count {
            let t = k as f32 / count as f32;
            let s = walked + len * t;
            let o = offset(s, perimeter);
            out.push([a[0] + (b[0] - a[0]) * t + normal[0] * o, a[1] + (b[1] - a[1]) * t + normal[1] * o]);
        }
        walked += len;
    }
    out
}

fn dist(a: [f32; 2], b: [f32; 2]) -> f32 {
    (b[0] - a[0]).hypot(b[1] - a[1])
}

/// A smooth wobble in -1..1 along a closed edge, the same every time for one `seed`.
fn wobble(s: f32, perimeter: f32, wave: f32, seed: u32) -> f32 {
    let mut v = 0.0;
    for k in 0..3u32 {
        // Whole waves round the edge, so it joins up.
        let waves = (perimeter / wave * [1.0, 1.7, 2.9][k as usize]).round().max(1.0);
        let phase = ((seed.wrapping_mul(2654435761).wrapping_add(k * 40503)) % 1000) as f32 / 1000.0 * std::f32::consts::TAU;
        v += (s / perimeter * waves * std::f32::consts::TAU + phase).sin() / (k + 1) as f32;
    }
    v / 1.83
}

/// A tail's shape: a wedge narrowing to the tip, or bubbles for thoughts
/// (only outside `outline`, getting smaller towards the tip).
fn tail_shapes(t: &Tail, outline: &[[f32; 2]]) -> Vec<Vec<[f32; 2]>> {
    let path = t.path();
    if !t.thought {
        let n = path.len();
        let mut left = Vec::new();
        let mut right = Vec::new();
        for i in 0..n {
            let (a, b) = (path[i.saturating_sub(1)], path[(i + 1).min(n - 1)]);
            let len = dist(a, b).max(1e-3);
            let normal = [-(b[1] - a[1]) / len, (b[0] - a[0]) / len];
            let half = t.width / 2.0 * (1.0 - i as f32 / (n - 1) as f32);
            left.push([path[i][0] + normal[0] * half, path[i][1] + normal[1] * half]);
            right.push([path[i][0] - normal[0] * half, path[i][1] - normal[1] * half]);
        }
        right.reverse();
        left.extend(right);
        return vec![left];
    }
    // Where the path leaves the balloon.
    let inside = |p: [f32; 2]| {
        let mut odd = false;
        for i in 0..outline.len() {
            let (a, b) = (outline[i], outline[(i + 1) % outline.len()]);
            if (a[1] > p[1]) != (b[1] > p[1]) && p[0] < a[0] + (p[1] - a[1]) / (b[1] - a[1]) * (b[0] - a[0]) {
                odd = !odd;
            }
        }
        odd
    };
    let mut lengths = vec![0.0f32];
    for w in path.windows(2) {
        lengths.push(lengths[lengths.len() - 1] + dist(w[0], w[1]));
    }
    let total = *lengths.last().unwrap_or(&0.0);
    let at = |s: f32| -> [f32; 2] {
        let i = lengths.iter().position(|&l| l >= s).unwrap_or(lengths.len() - 1).max(1);
        let (l0, l1) = (lengths[i - 1], lengths[i]);
        let k = if l1 > l0 { ((s - l0) / (l1 - l0)).clamp(0.0, 1.0) } else { 0.0 };
        [path[i - 1][0] + (path[i][0] - path[i - 1][0]) * k, path[i - 1][1] + (path[i][1] - path[i - 1][1]) * k]
    };
    let exit = (0..=64).map(|i| total * i as f32 / 64.0).find(|&s| !inside(at(s))).unwrap_or(total);
    let room = total - exit;
    let r0 = t.width / 2.0;
    let mut bubbles = Vec::new();
    let mut s = exit + r0 * 0.6;
    let mut r = r0;
    while s <= total && r > 0.5 && bubbles.len() < 12 {
        let c = at(s);
        bubbles.push(
            (0..32)
                .map(|i| {
                    let a = i as f32 / 32.0 * std::f32::consts::TAU;
                    [c[0] + r * a.cos(), c[1] + r * 0.8 * a.sin()]
                })
                .collect(),
        );
        let next = (r * 0.62).max(r0 * 0.18);
        s += r + next + (room * 0.06).max(r0 * 0.25);
        r = next;
    }
    bubbles
}

/// Softens coverage by about `passes` pixels (a [¼ ½ ¼] blur each way, `passes` times).
fn soften(cov: &mut [f32], w: usize, h: usize, passes: usize) {
    let mut tmp = vec![0.0f32; cov.len()];
    for _ in 0..passes {
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                tmp[i] = cov[i] * 0.5 + cov[y * w + x.saturating_sub(1)] * 0.25 + cov[y * w + (x + 1).min(w - 1)] * 0.25;
            }
        }
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                cov[i] = tmp[i] * 0.5 + tmp[y.saturating_sub(1) * w + x] * 0.25 + tmp[(y + 1).min(h - 1) * w + x] * 0.25;
            }
        }
    }
}

/// Coverage of the balloon and its tails: the fill and the line round it.
fn balloon_coverage(bal: &Balloon, centre: [f32; 2], w: usize, h: usize, origin: [i32; 2]) -> (Vec<f32>, Vec<f32>) {
    let outline = balloon_outline(bal, centre);
    let tails: Vec<Vec<[f32; 2]>> = bal.all_tails(centre).iter().flat_map(|t| tail_shapes(t, &outline)).collect();
    let amp = rough_amplitude(bal);
    let shape_of = |edge: &[[f32; 2]]| {
        let mut cov = vec![0.0f32; w * h];
        fill_polygon(&mut cov, w, h, origin, edge);
        for t in &tails {
            fill_polygon(&mut cov, w, h, origin, t);
        }
        cov
    };
    let (mut fill, line) = if amp > 0.0 {
        // A hand-inked edge: the shape wanders a little, and the line's outer
        // and inner edges wander apart, so it swells and thins like a pen's.
        let wave = (bal.radii[0].min(bal.radii[1]) * 0.9).max(30.0);
        let step = 2.0f32.max(bal.radii[0].max(bal.radii[1]) / 600.0);
        let swing = bal.radii[0].min(bal.radii[1]) * 0.012 * bal.rough.clamp(0.0, 100.0) / 100.0;
        let inner = amp - swing;
        let fill = displace(&outline, step, &|s, p| wobble(s, p, wave, 1) * swing - inner * (0.5 + 0.5 * wobble(s, p, wave * 0.8, 2)));
        let outer = displace(&outline, step, &|s, p| wobble(s, p, wave, 1) * swing + inner * 0.5 * wobble(s, p, wave * 0.7, 3));
        (shape_of(&fill), Some(shape_of(&outer)))
    } else {
        (shape_of(&outline), None)
    };
    let mut line = if bal.width > 0.0 { dilate(line.as_deref().unwrap_or(&fill), w, h, bal.width) } else { vec![0.0; w * h] };
    for cov in [&mut fill, &mut line] {
        match bal.antialias {
            Antialias::None => cov.iter_mut().for_each(|v| *v = if *v >= 0.5 { 1.0 } else { 0.0 }),
            Antialias::Weak => {}
            Antialias::Medium => soften(cov, w, h, 1),
            Antialias::Strong => soften(cov, w, h, 2),
        }
    }
    (fill, line)
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
        let (shape, line) = balloon_coverage(bal, item.pos, w, h, [x0, y0]);
        if bal.width > 0.0 {
            paint(&mut out, &line, [bal.line[0], bal.line[1], bal.line[2], 255]);
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
    fn older_balloons_upgrade_without_changing_their_look() {
        let old = TextItem {
            pos: [100.0, 100.0],
            balloon: Some(Balloon { shape: BalloonShape::Rounded, radii: [60.0, 40.0], tail: Some([30.0, 190.0]), ..Default::default() }),
            ..Default::default()
        };
        let mut new = old.clone();
        new.balloon.as_mut().unwrap().upgrade(new.pos);
        let b = new.balloon.as_ref().unwrap();
        assert_eq!((b.shape, b.tail, b.tails.len()), (BalloonShape::Rectangle, None, 1));
        let (mut a, mut c) = (TileStore::new(), TileStore::new());
        render(&old, &mut a, (256, 256));
        render(&new, &mut c, (256, 256));
        for (x, y) in [(100, 100), (100, 59), (40, 60), (42, 170), (35, 185)] {
            assert_eq!(a.pixel(x, y), c.pixel(x, y), "({x}, {y})");
        }
        // Older files load, newer ones keep everything.
        let back: Balloon = serde_json::from_str(r#"{"shape":"Rounded","radii":[1,2],"fill":[1,2,3,4],"line":[0,0,0],"width":1,"tail":null}"#).unwrap();
        assert_eq!((back.sides, back.antialias, back.tails.len()), (6, Antialias::Weak, 0));
    }

    #[test]
    fn tails_curve_and_thoughts_are_bubbles() {
        let bal = Balloon { radii: [50.0, 30.0], width: 3.0, ..Default::default() };
        let outline = balloon_outline(&bal, [100.0, 100.0]);
        let curved = Tail { base: [100.0, 110.0], bend: Some([160.0, 160.0]), tip: [120.0, 220.0], width: 30.0, thought: false };
        let path = curved.path();
        assert_eq!((path[0], path[path.len() - 1]), (curved.base, curved.tip));
        assert!(path.iter().any(|p| p[0] > 140.0), "it bends towards the bend point");
        let bubbles = tail_shapes(&Tail { thought: true, ..curved.clone() }, &outline);
        assert!((2..=12).contains(&bubbles.len()), "{} bubbles", bubbles.len());
        // None of them inside the balloon, and smaller towards the tip.
        let size = |b: &Vec<[f32; 2]>| b.iter().map(|p| p[0]).fold(f32::MIN, f32::max) - b.iter().map(|p| p[0]).fold(f32::MAX, f32::min);
        assert!(size(&bubbles[0]) > size(&bubbles[bubbles.len() - 1]));
        let mut tiles = TileStore::new();
        let item = TextItem { pos: [100.0, 100.0], balloon: Some(Balloon { tails: vec![curved], ..bal }), ..Default::default() };
        render(&item, &mut tiles, (256, 256));
        assert!(tiles.pixel(123, 217)[3] > 0, "the tail reaches its tip");
    }

    #[test]
    fn rough_rotated_and_drawn_balloons() {
        let base = Balloon { radii: [60.0, 40.0], width: 5.0, ..Default::default() };
        let item = |b: Balloon| TextItem { pos: [128.0, 128.0], balloon: Some(b), ..Default::default() };
        // Rough: the line's thickness varies round the edge, but it's there all round.
        let rough = item(Balloon { shape: BalloonShape::Rectangle, corner: 95.0, rough: 100.0, ..base.clone() });
        let mut tiles = TileStore::new();
        render(&rough, &mut tiles, (256, 256));
        let thickness: Vec<usize> = (70..186)
            .step_by(4)
            .map(|x| (60..128).filter(|&y| tiles.pixel(x, y)[3] > 128 && tiles.pixel(x, y)[0] < 128).count())
            .collect();
        assert!(thickness.iter().all(|&t| t >= 1), "{thickness:?}");
        assert!(thickness.iter().max() > thickness.iter().min(), "{thickness:?}");
        // Turned a quarter: taller than wide.
        let turned = item(Balloon { rotation: std::f32::consts::FRAC_PI_2, ..base.clone() });
        let b = bounds(&turned);
        assert!(b[3] - b[1] > b[2] - b[0]);
        // Drawn by hand: a triangle.
        let free = Balloon { shape: BalloonShape::Free, points: vec![[0.0, -1.0], [1.0, 1.0], [-1.0, 1.0]], ..base.clone() };
        let mut tiles = TileStore::new();
        render(&item(free), &mut tiles, (256, 256));
        assert_eq!(tiles.pixel(128, 150), [255; 4]);
        assert_eq!(tiles.pixel(80, 95)[3], 0, "outside the triangle's slope");
        // No anti-aliasing: only fully covered or empty pixels.
        let mut tiles = TileStore::new();
        render(&item(Balloon { antialias: Antialias::None, ..base }), &mut tiles, (256, 256));
        assert!((0..256).flat_map(|y| (0..256).map(move |x| (x, y))).all(|(x, y)| matches!(tiles.pixel(x, y)[3], 0 | 255)));
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
