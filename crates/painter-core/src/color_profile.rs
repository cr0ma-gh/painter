//! Colour profiles a document is tagged with (Photoshop's Assign Profile):
//! written into PNG (`iCCP`) and PSD exports so other programs read the
//! colours as meant. Painter shows and paints the numbers as they are; the
//! profile doesn't convert anything.
//!
//! The ICC profiles (version 2, display class) are built here from the
//! standard primaries, white point and tone curves.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ColorProfile {
    #[default]
    Srgb,
    DisplayP3,
    AdobeRgb,
    GrayGamma22,
}

/// Primaries adapted to D50 (Bradford), as ICC profiles carry them: red, green, blue XYZ.
type Primaries = [[f64; 3]; 3];

impl ColorProfile {
    pub const RGB: [ColorProfile; 3] = [ColorProfile::Srgb, ColorProfile::DisplayP3, ColorProfile::AdobeRgb];
    pub const GRAY: [ColorProfile; 1] = [ColorProfile::GrayGamma22];

    pub fn name(self) -> &'static str {
        match self {
            ColorProfile::Srgb => "sRGB IEC61966-2.1",
            ColorProfile::DisplayP3 => "Display P3",
            ColorProfile::AdobeRgb => "Adobe RGB (1998)",
            ColorProfile::GrayGamma22 => "Gray Gamma 2.2",
        }
    }

    pub fn is_gray(self) -> bool {
        matches!(self, ColorProfile::GrayGamma22)
    }

    fn primaries(self) -> Option<Primaries> {
        Some(match self {
            ColorProfile::Srgb => [
                [0.436_074_7, 0.222_504_5, 0.013_932_2],
                [0.385_064_9, 0.716_878_6, 0.097_104_5],
                [0.143_080_4, 0.060_616_9, 0.714_173_3],
            ],
            ColorProfile::DisplayP3 => {
                [[0.515_12, 0.241_20, -0.001_05], [0.291_98, 0.692_25, 0.041_89], [0.157_10, 0.066_57, 0.784_07]]
            }
            ColorProfile::AdobeRgb => [
                [0.609_755_9, 0.311_124_2, 0.019_464_7],
                [0.205_240_1, 0.625_656_0, 0.060_890_2],
                [0.149_224_0, 0.063_219_7, 0.744_838_7],
            ],
            ColorProfile::GrayGamma22 => return None,
        })
    }

    /// The tone curve tag: sRGB's piecewise curve as a table, or a pure gamma.
    fn curve(self) -> Vec<u8> {
        let mut out = b"curv\0\0\0\0".to_vec();
        match self {
            ColorProfile::Srgb | ColorProfile::DisplayP3 => {
                let n = 1024u32;
                out.extend_from_slice(&n.to_be_bytes());
                for i in 0..n {
                    let v = i as f64 / (n - 1) as f64;
                    let lin = if v <= 0.040_45 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
                    out.extend_from_slice(&((lin * 65535.0).round() as u16).to_be_bytes());
                }
            }
            // Gamma as u8Fixed8: Adobe RGB is 563/256 ≈ 2.2; gray 2.2.
            ColorProfile::AdobeRgb => out.extend_from_slice(&[0, 0, 0, 1, 0x02, 0x33]),
            ColorProfile::GrayGamma22 => out.extend_from_slice(&[0, 0, 0, 1, 0x02, 0x33]),
        }
        out
    }

    /// The ICC profile's bytes.
    pub fn icc(self) -> Vec<u8> {
        fn s15(v: f64) -> [u8; 4] {
            ((v * 65536.0).round() as i32).to_be_bytes()
        }
        fn xyz(v: [f64; 3]) -> Vec<u8> {
            let mut out = b"XYZ \0\0\0\0".to_vec();
            for c in v {
                out.extend_from_slice(&s15(c));
            }
            out
        }
        fn desc(text: &str) -> Vec<u8> {
            let mut out = b"desc\0\0\0\0".to_vec();
            out.extend_from_slice(&(text.len() as u32 + 1).to_be_bytes());
            out.extend_from_slice(text.as_bytes());
            out.push(0);
            // No Unicode or ScriptCode description.
            out.extend_from_slice(&[0; 4 + 4 + 2 + 1 + 67]);
            out
        }
        fn text(t: &str) -> Vec<u8> {
            let mut out = b"text\0\0\0\0".to_vec();
            out.extend_from_slice(t.as_bytes());
            out.push(0);
            out
        }
        const D50: [f64; 3] = [0.9642, 1.0, 0.8249];
        let curve = self.curve();
        let mut tags: Vec<(&[u8; 4], Vec<u8>)> =
            vec![(b"desc", desc(self.name())), (b"cprt", text("No copyright, use freely")), (b"wtpt", xyz(D50))];
        match self.primaries() {
            Some([r, g, b]) => {
                tags.push((b"rXYZ", xyz(r)));
                tags.push((b"gXYZ", xyz(g)));
                tags.push((b"bXYZ", xyz(b)));
                tags.push((b"rTRC", curve.clone()));
                tags.push((b"gTRC", curve.clone()));
                tags.push((b"bTRC", curve));
            }
            None => tags.push((b"kTRC", curve)),
        }
        // Tag data after the header and table, each on a 4-byte boundary.
        let table_len = 4 + 12 * tags.len();
        let mut data = Vec::new();
        let mut table = (tags.len() as u32).to_be_bytes().to_vec();
        for (sig, bytes) in &tags {
            let offset = 128 + table_len + data.len();
            table.extend_from_slice(*sig);
            table.extend_from_slice(&(offset as u32).to_be_bytes());
            table.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
            data.extend_from_slice(bytes);
            while data.len() % 4 != 0 {
                data.push(0);
            }
        }
        let size = 128 + table_len + data.len();
        let mut header = Vec::with_capacity(128);
        header.extend_from_slice(&(size as u32).to_be_bytes());
        header.extend_from_slice(&[0; 4]); // preferred CMM
        header.extend_from_slice(&[2, 0x10, 0, 0]); // version 2.1
        header.extend_from_slice(b"mntr");
        header.extend_from_slice(if self.is_gray() { b"GRAY" } else { b"RGB " });
        header.extend_from_slice(b"XYZ ");
        for v in [2026u16, 1, 1, 0, 0, 0] {
            header.extend_from_slice(&v.to_be_bytes());
        }
        header.extend_from_slice(b"acsp");
        header.extend_from_slice(&[0; 4 + 4 + 4 + 4 + 8 + 4]); // platform, flags, maker, model, attributes, intent
        for c in D50 {
            header.extend_from_slice(&s15(c));
        }
        header.extend_from_slice(&[0; 4]); // creator
        header.resize(128, 0);
        [header, table, data].concat()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_are_well_formed() {
        for p in ColorProfile::RGB.into_iter().chain(ColorProfile::GRAY) {
            let icc = p.icc();
            assert_eq!(u32::from_be_bytes(icc[0..4].try_into().unwrap()) as usize, icc.len(), "{p:?}: size");
            assert_eq!(&icc[36..40], b"acsp");
            let count = u32::from_be_bytes(icc[128..132].try_into().unwrap()) as usize;
            for t in 0..count {
                let e = &icc[132 + t * 12..144 + t * 12];
                let (off, len) = (
                    u32::from_be_bytes(e[4..8].try_into().unwrap()) as usize,
                    u32::from_be_bytes(e[8..12].try_into().unwrap()) as usize,
                );
                assert!(off % 4 == 0 && off + len <= icc.len(), "{p:?}: tag {t} in bounds");
            }
            assert_eq!(&icc[16..20], if p.is_gray() { b"GRAY" } else { b"RGB " });
        }
    }
}
