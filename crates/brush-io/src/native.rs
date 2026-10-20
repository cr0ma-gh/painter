//! `.rustbrush`: Painter's own brush format.
//!
//! A JSON document:
//! ```json
//! { "format": "rustbrush", "version": 1, "presets": [ BrushPreset, ... ] }
//! ```
//! Each preset holds the engine `settings`, the source `dynamics`, tip shape,
//! `source` and any untranslated `extras`. Bitmaps (tips, textures) are
//! embedded as `{ "width", "height", "png" }` with a base64 grayscale PNG.

use std::io::Cursor;
use std::path::Path;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use image::{GrayImage, ImageFormat};
use painter_core::DabTip;
use serde::{Deserialize, Serialize};

use crate::types::{BrushIoError, BrushPreset, BrushTip};

pub const EXTENSION: &str = "rustbrush";
const FORMAT: &str = "rustbrush";
const VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct BrushFile {
    format: String,
    version: u32,
    presets: Vec<BrushPreset>,
}

/// Serialised bitmap: dimensions plus a base64 PNG.
#[derive(Serialize, Deserialize)]
pub struct ImageFile {
    width: u32,
    height: u32,
    png: String,
}

impl From<BrushTip> for ImageFile {
    fn from(tip: BrushTip) -> Self {
        let image = GrayImage::from_raw(tip.width, tip.height, tip.alpha).expect("BrushTip dimensions are validated");
        let mut png = Vec::new();
        image.write_to(&mut Cursor::new(&mut png), ImageFormat::Png).expect("PNG encoding to memory cannot fail");
        ImageFile { width: image.width(), height: image.height(), png: STANDARD.encode(png) }
    }
}

impl TryFrom<ImageFile> for BrushTip {
    type Error = String;

    fn try_from(file: ImageFile) -> Result<Self, String> {
        let bytes = STANDARD.decode(file.png).map_err(|e| format!("bad base64 image: {e}"))?;
        let image = image::load_from_memory_with_format(&bytes, ImageFormat::Png)
            .map_err(|e| format!("bad PNG image: {e}"))?
            .to_luma8();
        if image.dimensions() != (file.width, file.height) {
            return Err(format!("image is {:?}, header says {}×{}", image.dimensions(), file.width, file.height));
        }
        BrushTip::new(file.width, file.height, image.into_raw()).map_err(|e| e.to_string())
    }
}

pub fn to_bytes(presets: &[BrushPreset]) -> Result<Vec<u8>, BrushIoError> {
    let mut presets = presets.to_vec();
    // `settings.tip` is rebuilt from the preset's own tip fields on read; don't store it twice.
    for preset in &mut presets {
        preset.settings.tip = DabTip::default();
        preset.settings.texture = None;
        preset.settings.dual = None;
    }
    let file = BrushFile { format: FORMAT.into(), version: VERSION, presets };
    Ok(serde_json::to_vec_pretty(&file)?)
}

pub fn read(bytes: &[u8]) -> Result<Vec<BrushPreset>, BrushIoError> {
    let file: BrushFile = serde_json::from_slice(bytes)?;
    if file.format != FORMAT {
        return Err(BrushIoError::Corrupt(format!("format is {:?}, expected {FORMAT:?}", file.format)));
    }
    if file.version > VERSION {
        return Err(BrushIoError::Unsupported(format!("rustbrush version {} (this build reads {VERSION})", file.version)));
    }
    let mut presets = file.presets;
    presets.iter_mut().for_each(BrushPreset::apply_tip);
    Ok(presets)
}

pub fn save(presets: &[BrushPreset], path: &Path) -> Result<(), BrushIoError> {
    std::fs::write(path, to_bytes(presets)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{BrushTexture, PresetSource};
    use painter_core::{BrushSettings, PressureCurve};

    #[test]
    fn round_trip_keeps_everything() {
        let mut preset = BrushPreset::new(BrushSettings::pencil());
        preset.tip = Some(BrushTip::new(3, 2, vec![0, 128, 255, 10, 20, 30]).unwrap());
        preset.texture = Some(BrushTexture { image: BrushTip::new(1, 1, vec![77]).unwrap(), scale: 150.0, density: 0.4, invert: true });
        preset.dual = Some(crate::types::DualBrush { tip: BrushTip::new(2, 2, vec![1, 2, 3, 4]).unwrap(), scale: 0.6 });
        preset.dynamics.size.pressure = Some(PressureCurve::gamma(0.7));
        preset.thickness = 0.3;
        preset.angle = 45.0;
        preset.source = PresetSource::Photoshop { version: 10, sample_id: Some("abc".into()) };
        preset.extras.insert("BrushRibbon".into(), serde_json::json!(1));
        preset.apply_tip();
        assert!(matches!(preset.settings.tip.shape, painter_core::TipShape::Sampled(_)));
        assert!(preset.settings.texture.as_ref().is_some_and(|t| t.invert && (t.scale - 1.5).abs() < 1e-6));
        assert!(preset.settings.dual.is_some());

        let back = read(&to_bytes(&[preset.clone()]).unwrap()).unwrap();
        assert_eq!(back, vec![preset]);
    }

    #[test]
    fn rejects_other_json() {
        assert!(read(br#"{"format":"other","version":1,"presets":[]}"#).is_err());
        assert!(read(br#"{"format":"rustbrush","version":99,"presets":[]}"#).is_err());
    }
}
