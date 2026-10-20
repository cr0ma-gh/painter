//! Thresholding a layer into the binary bitmap the tracer works on.

use painter_core::TileStore;

/// What decides whether a pixel is "ink".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThresholdSource {
    /// Coverage: right for line art on a transparent layer (painted or extracted from a scan).
    Alpha,
    /// Darkness of the pixel composited over white: for flattened scans on paper.
    Luminance,
}

/// One bit per pixel, row-major.
#[derive(Debug, Clone, PartialEq)]
pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    pub bits: Vec<bool>,
}

impl Bitmap {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height, bits: vec![false; (width * height) as usize] }
    }

    /// Off-canvas pixels are empty.
    pub fn get(&self, x: i32, y: i32) -> bool {
        x >= 0 && y >= 0 && (x as u32) < self.width && (y as u32) < self.height && self.bits[(y as u32 * self.width + x as u32) as usize]
    }

    pub fn set(&mut self, x: u32, y: u32, on: bool) {
        self.bits[(y * self.width + x) as usize] = on;
    }

    pub fn count(&self) -> usize {
        self.bits.iter().filter(|&&b| b).count()
    }
}

/// Thresholds premultiplied `tiles` over a `canvas`-sized area. `threshold`
/// is 0..=255: alpha above it (or darkness above it) is ink. Also returns
/// the average straight colour of the ink pixels, for the traced shape's fill.
pub fn threshold(tiles: &TileStore, canvas: (u32, u32), source: ThresholdSource, threshold: u8) -> (Bitmap, [f32; 3]) {
    let (w, h) = canvas;
    let mut bitmap = Bitmap::new(w, h);
    let mut sum = [0.0f64; 3];
    let mut weight = 0.0f64;
    // Only tiles that exist can hold ink.
    for (coord, tile) in tiles.iter() {
        let (ox, oy) = coord.origin();
        for ty in 0..painter_core::TILE_SIZE {
            for tx in 0..painter_core::TILE_SIZE {
                let (x, y) = (ox + tx as i32, oy + ty as i32);
                if x < 0 || y < 0 || x as u32 >= w || y as u32 >= h {
                    continue;
                }
                let [r, g, b, a] = tile.pixel(tx, ty);
                let ink = match source {
                    ThresholdSource::Alpha => a > threshold,
                    ThresholdSource::Luminance => {
                        // Over white: premultiplied colour plus the uncovered paper.
                        let paper = 255 - a as u32;
                        let lum = (2126 * (r as u32 + paper) + 7152 * (g as u32 + paper) + 722 * (b as u32 + paper)) / 10_000;
                        255 - lum.min(255) as u8 > threshold
                    }
                };
                if ink {
                    bitmap.set(x as u32, y as u32, true);
                    // Alpha-weighted mean of straight colour = Σ premultiplied / Σ alpha.
                    sum[0] += r as f64;
                    sum[1] += g as f64;
                    sum[2] += b as f64;
                    weight += a as f64;
                }
            }
        }
    }
    let color = if weight > 0.0 { sum.map(|c| (c / weight) as f32) } else { [0.0; 3] };
    (bitmap, color)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thresholds_alpha_and_luminance() {
        // 3×1: opaque red, half-transparent black, empty.
        let rgba = [255, 0, 0, 255, 0, 0, 0, 128, 0, 0, 0, 0];
        let tiles = TileStore::from_rgba(3, 1, &rgba);
        let (bits, color) = threshold(&tiles, (3, 1), ThresholdSource::Alpha, 100);
        assert_eq!(bits.bits, [true, true, false]);
        assert!(color[0] > 0.6 && color[1] == 0.0, "{color:?}");
        let (bits, _) = threshold(&tiles, (3, 1), ThresholdSource::Luminance, 100);
        // Red over white is light (luminance ~0.21 → darkness 200): ink; grey 50%: ink; empty: paper.
        assert_eq!(bits.bits, [true, true, false]);
        let (bits, _) = threshold(&tiles, (3, 1), ThresholdSource::Luminance, 210);
        assert_eq!(bits.bits, [false, false, false]);
    }
}
