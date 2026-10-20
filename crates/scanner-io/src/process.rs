//! Scan clean-up before import: crop, orientation, auto-contrast and line-art extraction.

use crate::{ScanArea, ScannedImage};

/// How dark pixels become ink and paper becomes transparent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LineArtOptions {
    /// Luminance at or above which a pixel is paper (fully transparent), 0..=1.
    pub paper: f32,
    /// Luminance at or below which a pixel is solid ink, 0..=1.
    pub ink: f32,
    /// Keep the scanned colour of the lines; otherwise they become black.
    pub keep_color: bool,
}

impl Default for LineArtOptions {
    fn default() -> Self {
        Self { paper: 0.85, ink: 0.35, keep_color: false }
    }
}

/// Clockwise rotation applied after cropping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Rotation {
    #[default]
    None,
    Cw90,
    Cw180,
    Cw270,
}

impl Rotation {
    pub const ALL: [Rotation; 4] = [Rotation::None, Rotation::Cw90, Rotation::Cw180, Rotation::Cw270];

    pub fn degrees(self) -> u32 {
        match self {
            Rotation::None => 0,
            Rotation::Cw90 => 90,
            Rotation::Cw180 => 180,
            Rotation::Cw270 => 270,
        }
    }

    /// The next quarter turn clockwise.
    pub fn turned_cw(self) -> Rotation {
        Rotation::ALL[(self as usize + 1) % 4]
    }

    /// The next quarter turn anticlockwise.
    pub fn turned_ccw(self) -> Rotation {
        Rotation::ALL[(self as usize + 3) % 4]
    }
}

/// How the page lies on the glass: rotation first, then mirroring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Orientation {
    pub rotation: Rotation,
    /// Mirror left-right.
    pub flip_horizontal: bool,
    /// Mirror top-bottom.
    pub flip_vertical: bool,
}

impl Orientation {
    /// Where a point of the oriented image (normalised 0..=1) was on the
    /// scan bed (normalised): the same mapping [`orient`] samples with.
    pub fn to_source(self, [u, v]: [f32; 2]) -> [f32; 2] {
        let mx = if self.flip_horizontal { 1.0 - u } else { u };
        let my = if self.flip_vertical { 1.0 - v } else { v };
        match self.rotation {
            Rotation::None => [mx, my],
            Rotation::Cw90 => [my, 1.0 - mx],
            Rotation::Cw180 => [1.0 - mx, 1.0 - my],
            Rotation::Cw270 => [1.0 - my, mx],
        }
    }

    /// Inverse of [`Self::to_source`]: where a bed point ends up.
    pub fn from_source(self, [x, y]: [f32; 2]) -> [f32; 2] {
        let [mx, my] = match self.rotation {
            Rotation::None => [x, y],
            Rotation::Cw90 => [1.0 - y, x],
            Rotation::Cw180 => [1.0 - x, 1.0 - y],
            Rotation::Cw270 => [y, 1.0 - x],
        };
        [if self.flip_horizontal { 1.0 - mx } else { mx }, if self.flip_vertical { 1.0 - my } else { my }]
    }

    /// Width and height after orienting a `width` × `height` image.
    pub fn size(self, width: f32, height: f32) -> [f32; 2] {
        match self.rotation {
            Rotation::Cw90 | Rotation::Cw270 => [height, width],
            _ => [width, height],
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImportOptions {
    pub crop: ScanArea,
    pub orientation: Orientation,
    /// Stretch levels so the darkest and lightest `clip` fraction saturate.
    pub auto_contrast: Option<f32>,
    pub extract_line_art: Option<LineArtOptions>,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            crop: ScanArea::FULL,
            orientation: Orientation::default(),
            auto_contrast: Some(0.005),
            extract_line_art: Some(LineArtOptions::default()),
        }
    }
}

/// Crop (in scan-bed coordinates), orientation, auto-contrast, then line-art
/// extraction, in that order.
pub fn prepare_for_import(image: &ScannedImage, options: &ImportOptions) -> ScannedImage {
    let mut out = orient(&crop(image, options.crop), options.orientation);
    if let Some(clip) = options.auto_contrast {
        auto_contrast(&mut out, clip);
    }
    match options.extract_line_art {
        Some(line_art) => extract_line_art(&out, &line_art),
        None => out,
    }
}

pub fn crop(image: &ScannedImage, area: ScanArea) -> ScannedImage {
    let (x, y, w, h) = area.to_pixels(image.width, image.height);
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for row in y..y + h {
        let start = ((row * image.width + x) * 4) as usize;
        rgba.extend_from_slice(&image.rgba[start..start + (w * 4) as usize]);
    }
    ScannedImage { width: w, height: h, dpi: image.dpi, rgba }
}

/// Rotates clockwise by `orientation.rotation`, then mirrors.
pub fn orient(image: &ScannedImage, orientation: Orientation) -> ScannedImage {
    let (w, h) = (image.width, image.height);
    let quarter = matches!(orientation.rotation, Rotation::Cw90 | Rotation::Cw270);
    let (ow, oh) = if quarter { (h, w) } else { (w, h) };
    let mut rgba = vec![0u8; image.rgba.len()];
    for oy in 0..oh {
        for ox in 0..ow {
            // Undo the mirroring, then the rotation, to find the source pixel.
            let mx = if orientation.flip_horizontal { ow - 1 - ox } else { ox };
            let my = if orientation.flip_vertical { oh - 1 - oy } else { oy };
            let (sx, sy) = match orientation.rotation {
                Rotation::None => (mx, my),
                Rotation::Cw90 => (my, h - 1 - mx),
                Rotation::Cw180 => (w - 1 - mx, h - 1 - my),
                Rotation::Cw270 => (w - 1 - my, mx),
            };
            let (src, dst) = (((sy * w + sx) * 4) as usize, ((oy * ow + ox) * 4) as usize);
            rgba[dst..dst + 4].copy_from_slice(&image.rgba[src..src + 4]);
        }
    }
    ScannedImage { width: ow, height: oh, dpi: image.dpi, rgba }
}

fn luminance(p: &[u8]) -> f32 {
    (0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32) / 255.0
}

/// Levels stretch on luminance: the `clip` darkest pixels go to black, the
/// `clip` lightest to white, everything between linearly. Scans of pencil on
/// greyish paper come out with white paper and dark lines.
pub fn auto_contrast(image: &mut ScannedImage, clip: f32) {
    let mut histogram = [0u32; 256];
    for p in image.rgba.as_chunks::<4>().0.iter() {
        histogram[(luminance(p) * 255.0).round() as usize] += 1;
    }
    let total: u32 = histogram.iter().sum();
    let cut = (total as f32 * clip.clamp(0.0, 0.49)) as u32;
    let level = |from_dark: bool| {
        let mut seen = 0;
        let order: Box<dyn Iterator<Item = usize>> = if from_dark { Box::new(0..256) } else { Box::new((0..256).rev()) };
        for i in order {
            seen += histogram[i];
            if seen > cut {
                return i as f32;
            }
        }
        if from_dark { 0.0 } else { 255.0 }
    };
    let (lo, hi) = (level(true), level(false));
    if hi - lo < 1.0 {
        return;
    }
    for p in image.rgba.as_chunks_mut::<4>().0.iter_mut() {
        for c in &mut p[..3] {
            *c = ((*c as f32 - lo) / (hi - lo) * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
}

/// Paper to transparency: alpha falls from 1 at `ink` luminance to 0 at
/// `paper`, smoothly, so anti-aliased pencil edges survive.
pub fn extract_line_art(image: &ScannedImage, options: &LineArtOptions) -> ScannedImage {
    let span = (options.paper - options.ink).max(1e-3);
    let rgba = image
        .rgba
        .as_chunks::<4>().0.iter()
        .flat_map(|p| {
            let t = ((options.paper - luminance(p)) / span).clamp(0.0, 1.0);
            let alpha = (t * t * (3.0 - 2.0 * t) * p[3] as f32).round() as u8;
            if options.keep_color {
                [p[0], p[1], p[2], alpha]
            } else {
                [0, 0, 0, alpha]
            }
        })
        .collect();
    ScannedImage { rgba, ..image.clone() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gray(values: &[u8]) -> ScannedImage {
        ScannedImage::from_gray(values.len() as u32, 1, 300, values).unwrap()
    }

    #[test]
    fn auto_contrast_stretches_levels() {
        let mut image = gray(&[60, 100, 180, 200]);
        auto_contrast(&mut image, 0.0);
        let levels: Vec<u8> = image.rgba.as_chunks::<4>().0.iter().map(|p| p[0]).collect();
        assert_eq!(levels[0], 0);
        assert_eq!(levels[3], 255);
        assert!(levels[1] > 0 && levels[1] < levels[2]);
        // A flat image is left alone.
        let mut flat = gray(&[128, 128]);
        auto_contrast(&mut flat, 0.0);
        assert_eq!(flat.rgba[0], 128);
    }

    #[test]
    fn line_art_makes_paper_transparent() {
        let out = extract_line_art(&gray(&[255, 230, 150, 20]), &LineArtOptions::default());
        let alpha: Vec<u8> = out.rgba.as_chunks::<4>().0.iter().map(|p| p[3]).collect();
        assert_eq!(alpha[0], 0, "white paper");
        assert_eq!(alpha[1], 0, "off-white paper");
        assert!(alpha[2] > 0 && alpha[2] < 255, "soft edge");
        assert_eq!(alpha[3], 255, "ink");
        assert!(out.rgba.as_chunks::<4>().0.iter().all(|p| p[..3] == [0, 0, 0]));
    }

    #[test]
    fn orient_rotates_clockwise_then_mirrors() {
        // 3×2:  0 1 2
        //       3 4 5
        let image = ScannedImage::from_gray(3, 2, 300, &[0, 1, 2, 3, 4, 5]).unwrap();
        let grid = |o: Orientation| {
            let out = orient(&image, o);
            ((out.width, out.height), out.rgba.as_chunks::<4>().0.iter().map(|p| p[0]).collect::<Vec<_>>())
        };
        let r = |rotation| Orientation { rotation, ..Orientation::default() };
        assert_eq!(grid(r(Rotation::None)), ((3, 2), vec![0, 1, 2, 3, 4, 5]));
        // Clockwise: the left column (3 over 0) becomes the top row.
        assert_eq!(grid(r(Rotation::Cw90)), ((2, 3), vec![3, 0, 4, 1, 5, 2]));
        assert_eq!(grid(r(Rotation::Cw180)), ((3, 2), vec![5, 4, 3, 2, 1, 0]));
        assert_eq!(grid(r(Rotation::Cw270)), ((2, 3), vec![2, 5, 1, 4, 0, 3]));
        let flip_h = Orientation { flip_horizontal: true, ..Orientation::default() };
        assert_eq!(grid(flip_h), ((3, 2), vec![2, 1, 0, 5, 4, 3]));
        let flip_v = Orientation { flip_vertical: true, ..Orientation::default() };
        assert_eq!(grid(flip_v), ((3, 2), vec![3, 4, 5, 0, 1, 2]));
        // Both flips = a half turn.
        let both = Orientation { flip_horizontal: true, flip_vertical: true, ..Orientation::default() };
        assert_eq!(grid(both), grid(r(Rotation::Cw180)));
        // The normalised mapping agrees with the pixels: output top-left after a
        // clockwise turn is the source bottom-left, and from_source inverts it.
        for rotation in Rotation::ALL {
            for (flip_horizontal, flip_vertical) in [(false, false), (true, false), (false, true)] {
                let o = Orientation { rotation, flip_horizontal, flip_vertical };
                let p = [0.2, 0.7];
                let back = o.from_source(o.to_source(p));
                assert!((back[0] - p[0]).abs() < 1e-6 && (back[1] - p[1]).abs() < 1e-6, "{o:?}");
            }
        }
        assert_eq!(r(Rotation::Cw90).to_source([0.0, 0.0]), [0.0, 1.0]);
        assert_eq!(Rotation::Cw270.turned_cw(), Rotation::None);
        assert_eq!(Rotation::None.turned_ccw(), Rotation::Cw270);
    }

    #[test]
    fn crop_takes_the_area() {
        let image = ScannedImage::from_gray(4, 2, 300, &[0, 1, 2, 3, 4, 5, 6, 7]).unwrap();
        let out = crop(&image, ScanArea { x: 0.5, y: 0.5, width: 0.5, height: 0.5 });
        assert_eq!((out.width, out.height), (2, 1));
        assert_eq!(out.rgba.as_chunks::<4>().0.iter().map(|p| p[0]).collect::<Vec<_>>(), [6, 7]);
    }
}
