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

/// Bilinear resampler for an 8-bit coverage mask.
pub fn resample_coverage(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
        return Vec::new();
    }
    if sw == dw && sh == dh {
        return src[..((sw * sh) as usize).min(src.len())].to_vec();
    }
    let mut dst = Vec::with_capacity((dw * dh) as usize);
    let sx_scale = sw as f32 / dw as f32;
    let sy_scale = sh as f32 / dh as f32;
    for dy in 0..dh {
        for dx in 0..dw {
            let src_x = (dx as f32 + 0.5) * sx_scale - 0.5;
            let src_y = (dy as f32 + 0.5) * sy_scale - 0.5;
            let x0 = (src_x.floor() as i32).clamp(0, sw as i32 - 1) as u32;
            let y0 = (src_y.floor() as i32).clamp(0, sh as i32 - 1) as u32;
            let x1 = (x0 + 1).min(sw - 1);
            let y1 = (y0 + 1).min(sh - 1);
            let fx = (src_x - src_x.floor()).clamp(0.0, 1.0);
            let fy = (src_y - src_y.floor()).clamp(0.0, 1.0);

            let p00 = src[(y0 * sw + x0) as usize] as f32;
            let p10 = src[(y0 * sw + x1) as usize] as f32;
            let p01 = src[(y1 * sw + x0) as usize] as f32;
            let p11 = src[(y1 * sw + x1) as usize] as f32;

            let top = p00 * (1.0 - fx) + p10 * fx;
            let bottom = p01 * (1.0 - fx) + p11 * fx;
            let val = (top * (1.0 - fy) + bottom * fy).round().clamp(0.0, 255.0) as u8;
            dst.push(val);
        }
    }
    dst
}

/// Samples a custom brush tip from unpremultiplied RGBA8 pixels, converting
/// luminance to ink coverage (Photoshop convention):
///
/// `coverage = ((255 - luminance) * (alpha / 255.0))`
///
/// Black `(0, 0, 0)` produces 100% ink coverage (255), white `(255, 255, 255)` produces
/// 0% ink coverage (0), scaled proportionally by source alpha and selection mask.
///
/// - If `trim` is true, automatically crops empty transparent margins where coverage is 0.
/// - If `max_dimension > 0` and the tip exceeds it, downsamples proportionally.
/// - Returns `None` if the input is empty or if all sampled pixels have zero ink coverage.
pub fn sample_tip_coverage(
    rgba: &[u8],
    width: u32,
    height: u32,
    selection_alpha: Option<&[u8]>,
    max_dimension: u32,
    trim: bool,
) -> Option<(u32, u32, Vec<u8>)> {
    if width == 0 || height == 0 || rgba.len() < (width * height * 4) as usize {
        return None;
    }

    // 1. Convert to initial coverage grid.
    let mut coverage = Vec::with_capacity((width * height) as usize);
    let mut min_x = u32::MAX;
    let mut min_y = u32::MAX;
    let mut max_x = 0u32;
    let mut max_y = 0u32;
    let mut has_ink = false;

    for y in 0..height {
        for x in 0..width {
            let idx = (y * width + x) as usize;
            let r = rgba[idx * 4] as f32;
            let g = rgba[idx * 4 + 1] as f32;
            let b = rgba[idx * 4 + 2] as f32;
            let a = rgba[idx * 4 + 3] as f32 / 255.0;
            let sel = selection_alpha.map_or(1.0, |s| s.get(idx).copied().unwrap_or(255) as f32 / 255.0);
            let total_alpha = a * sel;

            let lum = 0.299 * r + 0.587 * g + 0.114 * b;
            let cov = ((255.0 - lum) * total_alpha).round().clamp(0.0, 255.0) as u8;
            coverage.push(cov);

            if cov > 0 {
                has_ink = true;
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }
    }

    if !has_ink {
        return None;
    }

    // 2. Crop empty margins if trim is requested.
    let (cropped_w, cropped_h, cropped_cov) = if trim && (min_x > 0 || min_y > 0 || max_x + 1 < width || max_y + 1 < height) {
        let cw = max_x - min_x + 1;
        let ch = max_y - min_y + 1;
        let mut trimmed = Vec::with_capacity((cw * ch) as usize);
        for y in min_y..=max_y {
            let row_start = (y * width + min_x) as usize;
            trimmed.extend_from_slice(&coverage[row_start..row_start + cw as usize]);
        }
        (cw, ch, trimmed)
    } else {
        (width, height, coverage)
    };

    // 3. Downsample proportionally if exceeding max_dimension.
    let (final_w, final_h, final_cov) = if max_dimension > 0 && (cropped_w > max_dimension || cropped_h > max_dimension) {
        let scale = max_dimension as f32 / cropped_w.max(cropped_h) as f32;
        let dw = ((cropped_w as f32 * scale).round() as u32).max(1);
        let dh = ((cropped_h as f32 * scale).round() as u32).max(1);
        let resampled = resample_coverage(&cropped_cov, cropped_w, cropped_h, dw, dh);
        (dw, dh, resampled)
    } else {
        (cropped_w, cropped_h, cropped_cov)
    };

    Some((final_w, final_h, final_cov))
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

    #[test]
    fn test_sample_tip_coverage_photoshop_luminance() {
        // 3x3 image with white border and black cross in the center
        // (x, y) = (1, 1) is black [0, 0, 0, 255]
        let mut rgba = vec![255u8; 3 * 3 * 4];
        // Set center pixel to black
        rgba[4 * 4] = 0;
        rgba[4 * 4 + 1] = 0;
        rgba[4 * 4 + 2] = 0;
        rgba[4 * 4 + 3] = 255;

        // With trim=true, the 3x3 with only center black pixel trims down to 1x1 with coverage 255
        let (tw, th, tcov) = sample_tip_coverage(&rgba, 3, 3, None, 2500, true).expect("should sample");
        assert_eq!((tw, th), (1, 1));
        assert_eq!(tcov, vec![255]);

        // With trim=false, keeps full 3x3 dimensions
        let (w, h, cov) = sample_tip_coverage(&rgba, 3, 3, None, 2500, false).expect("should sample");
        assert_eq!((w, h), (3, 3));
        assert_eq!(cov[4], 255);
        assert_eq!(cov[0], 0);

        // Pure white is empty -> None
        let all_white = vec![255u8; 2 * 2 * 4];
        assert!(sample_tip_coverage(&all_white, 2, 2, None, 2500, true).is_none());

        // Alpha scaling: black with 50% opacity -> coverage ~128
        let mut semi_black = vec![0u8; 4];
        semi_black[3] = 128;
        let (sw, sh, scov) = sample_tip_coverage(&semi_black, 1, 1, None, 2500, true).expect("semi black");
        assert_eq!((sw, sh), (1, 1));
        assert_eq!(scov[0], 128);
    }

    #[test]
    fn test_resample_coverage_downscale() {
        let src = vec![255, 0, 0, 255]; // 2x2
        let dst = resample_coverage(&src, 2, 2, 1, 1);
        assert_eq!(dst.len(), 1);
        // Average of 255, 0, 0, 255 is ~128
        assert!((dst[0] as i32 - 128).abs() <= 2);
    }
}
