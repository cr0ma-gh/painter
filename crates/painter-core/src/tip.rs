//! Brush tip shapes: a round tip (optionally flattened and rotated, like Clip
//! Studio's calligraphy pens) or a sampled bitmap from an imported preset.
//!
//! Everything a rasteriser needs per dab (reach, mip level) is computed here,
//! once, so `raster::stamp_dab` and painter-render's `dab.wgsl` stamp the same
//! footprint.

use std::io::Cursor;
use std::sync::Arc;

use image::{GrayImage, ImageFormat};
use serde::{Deserialize, Serialize};

/// An 8-bit coverage bitmap (255 = full ink) with a box-filtered mip chain,
/// so tips much larger than the dab don't alias.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "TipImage", into = "TipImage")]
pub struct TipBitmap {
    width: u32,
    height: u32,
    /// Level 0 is the bitmap itself; level `k` is `max(1, size >> k)` (wgpu's mip sizes), down to 1×1.
    levels: Vec<Vec<u8>>,
}

impl TipBitmap {
    /// `None` when `alpha` doesn't hold `width × height` bytes.
    pub fn new(width: u32, height: u32, alpha: Vec<u8>) -> Option<Self> {
        if width == 0 || height == 0 || alpha.len() != width as usize * height as usize {
            return None;
        }
        let mut levels = vec![alpha];
        let (mut w, mut h) = (width, height);
        while w > 1 || h > 1 {
            let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
            let src = levels.last().expect("level 0 exists");
            let at = |x: u32, y: u32| src[(y.min(h - 1) * w + x.min(w - 1)) as usize] as u32;
            let mut next = Vec::with_capacity(nw as usize * nh as usize);
            for y in 0..nh {
                for x in 0..nw {
                    let sum = at(2 * x, 2 * y) + at(2 * x + 1, 2 * y) + at(2 * x, 2 * y + 1) + at(2 * x + 1, 2 * y + 1);
                    next.push(((sum + 2) / 4) as u8);
                }
            }
            levels.push(next);
            (w, h) = (nw, nh);
        }
        Some(Self { width, height, levels })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// The full-resolution bitmap, row-major.
    pub fn alpha(&self) -> &[u8] {
        &self.levels[0]
    }

    pub fn level_count(&self) -> u32 {
        self.levels.len() as u32
    }

    pub fn level_size(&self, level: u32) -> (u32, u32) {
        ((self.width >> level).max(1), (self.height >> level).max(1))
    }

    /// One mip level's pixels, row-major and tightly packed.
    pub fn level(&self, level: u32) -> &[u8] {
        &self.levels[level as usize]
    }

    /// Width and height over the longer side: the tip's extent in dab diameters.
    pub fn aspect(&self) -> [f32; 2] {
        let longest = self.width.max(self.height) as f32;
        [self.width as f32 / longest, self.height as f32 / longest]
    }

    /// Bilinear coverage at texel coordinates `(x, y)` of `level` (texel
    /// centres at integers), zero outside the bitmap.
    /// Mirrored by `sampled_coverage` in painter-render's `dab.wgsl`.
    pub fn sample(&self, level: u32, x: f32, y: f32) -> f32 {
        let (w, h) = self.level_size(level);
        if x <= -1.0 || y <= -1.0 || x >= w as f32 || y >= h as f32 {
            return 0.0;
        }
        let pixels = self.level(level);
        let texel = |i: i32, j: i32| {
            if i < 0 || j < 0 || i >= w as i32 || j >= h as i32 {
                0.0
            } else {
                pixels[(j as u32 * w + i as u32) as usize] as f32 / 255.0
            }
        };
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let (i, j) = (x0 as i32, y0 as i32);
        let top = texel(i, j) + (texel(i + 1, j) - texel(i, j)) * fx;
        let bottom = texel(i, j + 1) + (texel(i + 1, j + 1) - texel(i, j + 1)) * fx;
        top + (bottom - top) * fy
    }

    /// Bilinear sample of level 0 with the bitmap repeating in both directions.
    /// Mirrored by `texture_sample` in painter-render's `dab.wgsl`.
    pub fn sample_wrapped(&self, x: f32, y: f32) -> f32 {
        let (w, h) = (self.width as i32, self.height as i32);
        let pixels = self.level(0);
        let texel = |i: i32, j: i32| pixels[(j.rem_euclid(h) * w + i.rem_euclid(w)) as usize] as f32 / 255.0;
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let (i, j) = (x0 as i32, y0 as i32);
        let top = texel(i, j) + (texel(i + 1, j) - texel(i, j)) * fx;
        let bottom = texel(i, j + 1) + (texel(i + 1, j + 1) - texel(i, j + 1)) * fx;
        top + (bottom - top) * fy
    }
}

/// A paper / grain texture: a tiling grayscale bitmap fixed to the canvas that
/// lets ink through where it is light (255) and holds it back where it is dark.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DabTexture {
    pub bitmap: Arc<TipBitmap>,
    /// Canvas pixels per texture texel (1 = native size).
    pub scale: f32,
    /// How strongly the texture holds ink back, 0..=1.
    pub density: f32,
    /// Swap light and dark.
    #[serde(default)]
    pub invert: bool,
}

impl DabTexture {
    pub fn scale(&self) -> f32 {
        self.scale.clamp(0.05, 64.0)
    }

    /// Coverage multiplier at canvas point `(x, y)` (a pixel centre).
    /// Mirrored by `texture_factor` in painter-render's `dab.wgsl`.
    pub fn factor(&self, x: f32, y: f32) -> f32 {
        let s = self.scale();
        let t = self.bitmap.sample_wrapped(x / s - 0.5, y / s - 0.5);
        let t = if self.invert { 1.0 - t } else { t };
        1.0 - self.density.clamp(0.0, 1.0) * (1.0 - t)
    }
}

/// A second tip that masks every dab (Photoshop's "dual brush"), centred on
/// the dab and scaled relative to it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DualTip {
    pub bitmap: Arc<TipBitmap>,
    /// Dual tip diameter over the main dab diameter.
    pub scale: f32,
}

impl DualTip {
    pub fn scale(&self) -> f32 {
        self.scale.clamp(0.05, 8.0)
    }

    /// The dual tip's radius for a dab of `radius`.
    pub fn radius(&self, radius: f32) -> f32 {
        radius * self.scale()
    }

    /// Mip level for a dab of `radius` (same rule as [`DabTip::mip_level`]).
    pub fn mip_level(&self, radius: f32) -> u32 {
        let tip = DabTip { shape: TipShape::Sampled(self.bitmap.clone()), ..DabTip::default() };
        tip.mip_level(self.radius(radius))
    }
}

/// Serialised form: dimensions plus PNG bytes.
#[derive(Serialize, Deserialize)]
struct TipImage {
    width: u32,
    height: u32,
    png: Vec<u8>,
}

impl From<TipBitmap> for TipImage {
    fn from(tip: TipBitmap) -> Self {
        let (width, height) = (tip.width, tip.height);
        let alpha = tip.levels.into_iter().next().expect("level 0 exists");
        let image = GrayImage::from_raw(width, height, alpha).expect("TipBitmap dimensions are validated");
        let mut png = Vec::new();
        image.write_to(&mut Cursor::new(&mut png), ImageFormat::Png).expect("PNG encoding to memory cannot fail");
        TipImage { width, height, png }
    }
}

impl TryFrom<TipImage> for TipBitmap {
    type Error = String;

    fn try_from(file: TipImage) -> Result<Self, String> {
        let image = image::load_from_memory_with_format(&file.png, ImageFormat::Png)
            .map_err(|e| format!("bad tip PNG: {e}"))?
            .to_luma8();
        if image.dimensions() != (file.width, file.height) {
            return Err(format!("tip image is {:?}, header says {}×{}", image.dimensions(), file.width, file.height));
        }
        TipBitmap::new(file.width, file.height, image.into_raw()).ok_or_else(|| "empty tip image".into())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum TipShape {
    /// A disc with `hardness` falloff and `antialias` edge.
    #[default]
    Round,
    /// A bitmap scaled so its longer side spans the dab diameter. Hardness and
    /// anti-aliasing don't apply; the bitmap's own edges do.
    Sampled(Arc<TipBitmap>),
}

/// How a rasteriser evaluates coverage; see `raster::stamp_dab` and `dab.wgsl`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TipKind {
    /// Round and not flattened: the plain distance-to-centre falloff.
    Disc = 0,
    /// Round but flattened into a rotated ellipse.
    Ellipse = 1,
    Sampled = 2,
}

/// A brush's tip: its shape, flattened to `thickness` across and rotated by `angle`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DabTip {
    pub shape: TipShape,
    /// Height relative to width, 0..=1 (1 = unflattened).
    pub thickness: f32,
    /// Counter-clockwise on screen, in degrees.
    pub angle: f32,
}

impl Default for DabTip {
    fn default() -> Self {
        Self { shape: TipShape::Round, thickness: 1.0, angle: 0.0 }
    }
}

impl DabTip {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub fn kind(&self) -> TipKind {
        match self.shape {
            TipShape::Sampled(_) => TipKind::Sampled,
            TipShape::Round if self.flatness() < 1.0 => TipKind::Ellipse,
            TipShape::Round => TipKind::Disc,
        }
    }

    pub fn bitmap(&self) -> Option<&Arc<TipBitmap>> {
        match &self.shape {
            TipShape::Sampled(bitmap) => Some(bitmap),
            TipShape::Round => None,
        }
    }

    /// `thickness`, kept away from zero.
    pub fn flatness(&self) -> f32 {
        self.thickness.clamp(0.01, 1.0)
    }

    /// The flattening of one dab: the tip's thickness × the dab's own (thickness dynamics).
    pub fn flatness_for(&self, dab_thickness: f32) -> f32 {
        (self.thickness * dab_thickness).clamp(0.01, 1.0)
    }

    /// How a dab with `flatness` is evaluated: same rule as [`Self::kind`].
    pub fn kind_for(&self, flatness: f32) -> TipKind {
        match self.shape {
            TipShape::Sampled(_) => TipKind::Sampled,
            TipShape::Round if flatness < 1.0 => TipKind::Ellipse,
            TipShape::Round => TipKind::Disc,
        }
    }

    /// `[cos, sin]` of `angle`: tip-local `(x, y)` maps to canvas
    /// `(x·cos + y·sin, −x·sin + y·cos)` (counter-clockwise with y down).
    /// `extra` is a per-dab rotation in degrees (rotation dynamics).
    pub fn rotation(&self, extra: f32) -> [f32; 2] {
        let (sin, cos) = (self.angle + extra).to_radians().sin_cos();
        [cos, sin]
    }

    /// Mip level to sample for a dab of `radius`: the finest level whose
    /// texels are still at most one canvas pixel. 0 for round tips.
    pub fn mip_level(&self, radius: f32) -> u32 {
        let Some(bitmap) = self.bitmap() else {
            return 0;
        };
        let texel = 2.0 * radius / bitmap.width().max(bitmap.height()) as f32;
        let mut level = 0;
        while level + 1 < bitmap.level_count() && texel * (1u32 << (level + 1)) as f32 <= 1.0 {
            level += 1;
        }
        level
    }

    /// Distance from the dab centre (along either axis) beyond which coverage is zero.
    pub fn reach(&self, radius: f32, aa: f32, level: u32) -> f32 {
        match self.bitmap() {
            None => radius + aa,
            Some(bitmap) => {
                // Corners of the scaled bitmap, plus half a texel of bilinear bleed at `level`.
                let texel = 2.0 * radius / bitmap.width().max(bitmap.height()) as f32;
                0.5 * texel * (bitmap.width() as f32).hypot(bitmap.height() as f32) + texel * (1u32 << level) as f32
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mips_halve_down_to_one_pixel() {
        let bitmap = TipBitmap::new(5, 2, vec![255; 10]).unwrap();
        assert_eq!(bitmap.level_count(), 3);
        assert_eq!(bitmap.level_size(1), (2, 1));
        assert_eq!(bitmap.level_size(2), (1, 1));
        assert_eq!(bitmap.level(2), &[255]);
        assert!(TipBitmap::new(2, 2, vec![0; 3]).is_none());
    }

    #[test]
    fn bilinear_sampling_and_border() {
        let bitmap = TipBitmap::new(2, 1, vec![0, 255]).unwrap();
        assert_eq!(bitmap.sample(0, 0.0, 0.0), 0.0);
        assert_eq!(bitmap.sample(0, 1.0, 0.0), 1.0);
        assert!((bitmap.sample(0, 0.5, 0.0) - 0.5).abs() < 1e-6);
        // Fades to zero half a texel past the edge.
        assert!((bitmap.sample(0, 1.5, 0.0) - 0.5).abs() < 1e-6);
        assert_eq!(bitmap.sample(0, 2.0, 0.0), 0.0);
    }

    #[test]
    fn mip_level_tracks_dab_size() {
        let tip = DabTip { shape: TipShape::Sampled(Arc::new(TipBitmap::new(64, 32, vec![255; 64 * 32]).unwrap())), ..DabTip::default() };
        assert_eq!(tip.mip_level(32.0), 0);
        assert_eq!(tip.mip_level(8.0), 2);
        assert_eq!(tip.mip_level(0.01), 6);
    }

    #[test]
    fn texture_wraps_and_scales() {
        // 2×1 texture: dark, light.
        let texture = DabTexture { bitmap: Arc::new(TipBitmap::new(2, 1, vec![0, 255]).unwrap()), scale: 1.0, density: 1.0, invert: false };
        assert_eq!(texture.factor(0.5, 0.5), 0.0);
        assert_eq!(texture.factor(1.5, 0.5), 1.0);
        assert_eq!(texture.factor(2.5, 7.5), 0.0, "repeats");
        let half = DabTexture { density: 0.5, ..texture.clone() };
        assert_eq!(half.factor(0.5, 0.5), 0.5);
        let inverted = DabTexture { invert: true, ..texture.clone() };
        assert_eq!(inverted.factor(0.5, 0.5), 1.0);
        let doubled = DabTexture { scale: 2.0, ..texture };
        assert_eq!(doubled.factor(1.0, 0.5), 0.0, "texel 0 spans two pixels");
        assert_eq!(doubled.factor(3.0, 0.5), 1.0);
    }

    #[test]
    fn serde_round_trip() {
        let tip = DabTip {
            shape: TipShape::Sampled(Arc::new(TipBitmap::new(3, 2, vec![0, 128, 255, 10, 20, 30]).unwrap())),
            thickness: 0.4,
            angle: 30.0,
        };
        let json = serde_json::to_string(&tip).unwrap();
        assert_eq!(serde_json::from_str::<DabTip>(&json).unwrap(), tip);
    }
}
