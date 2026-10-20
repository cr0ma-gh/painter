//! Decoding the image files drivers hand back (BMP from WIA, PNG/JPEG from some).

use crate::{ScanError, ScannedImage};

pub fn decode(bytes: &[u8], dpi: u32) -> Result<ScannedImage, ScanError> {
    let image = image::load_from_memory(bytes).map_err(|e| ScanError::Device(format!("unreadable scan: {e}")))?.to_rgba8();
    let (width, height) = image.dimensions();
    ScannedImage::new(width, height, dpi, image.into_raw())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_a_bmp() {
        let mut bmp = Vec::new();
        let gray = image::GrayImage::from_fn(3, 2, |x, y| image::Luma([(x * 100 + y * 10) as u8]));
        gray.write_to(&mut std::io::Cursor::new(&mut bmp), image::ImageFormat::Bmp).unwrap();
        let scan = decode(&bmp, 300).unwrap();
        assert_eq!((scan.width, scan.height, scan.dpi), (3, 2, 300));
        assert_eq!(&scan.rgba[4 * 4..4 * 4 + 4], &[110, 110, 110, 255]);
        assert!(decode(b"not an image", 300).is_err());
    }
}
