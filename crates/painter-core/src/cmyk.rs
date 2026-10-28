//! CMYK through Little CMS: a document in CMYK mode keeps its colours inside
//! what the chosen press profile can print (every pixel goes RGB → CMYK → RGB
//! with that profile), shows the C M Y K values of a colour, and exports as a
//! real CMYK PSD with the profile embedded.
//!
//! Painter still keeps RGB pixels; the profile decides which of them exist.
//! Press profiles (FOGRA39, SWOP, Japan Color…) aren't shipped with Painter
//! (their licences don't allow it): they're found where other programs put
//! them ([`find_profiles`]), or loaded from a file.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lcms2::{ColorSpaceSignature, Flags, InfoType, Intent, Locale, PixelFormat, Profile, Transform};
use serde::{Deserialize, Serialize};

use crate::blend::{premultiply, unpremultiply};
use crate::layer::{Layer, LayerKind};
use crate::tile::TileStore;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CmykIntent {
    #[default]
    Perceptual,
    RelativeColorimetric,
    Saturation,
    AbsoluteColorimetric,
}

impl CmykIntent {
    pub const ALL: [CmykIntent; 4] =
        [CmykIntent::Perceptual, CmykIntent::RelativeColorimetric, CmykIntent::Saturation, CmykIntent::AbsoluteColorimetric];

    pub fn name(self) -> &'static str {
        match self {
            CmykIntent::Perceptual => "Perceptual",
            CmykIntent::RelativeColorimetric => "Relative Colorimetric",
            CmykIntent::Saturation => "Saturation",
            CmykIntent::AbsoluteColorimetric => "Absolute Colorimetric",
        }
    }

    fn lcms(self) -> Intent {
        match self {
            CmykIntent::Perceptual => Intent::Perceptual,
            CmykIntent::RelativeColorimetric => Intent::RelativeColorimetric,
            CmykIntent::Saturation => Intent::Saturation,
            CmykIntent::AbsoluteColorimetric => Intent::AbsoluteColorimetric,
        }
    }
}

/// The press a CMYK document is made for: its profile and how colours map to it.
#[derive(Clone, Debug, PartialEq)]
pub struct CmykSetup {
    pub name: String,
    pub icc: Arc<Vec<u8>>,
    pub intent: CmykIntent,
    pub black_point: bool,
}

/// Entries per side of [`Proofer::lut`].
pub const LUT_SIZE: usize = 33;

/// A CMYK profile found on this computer.
#[derive(Clone, Debug, PartialEq)]
pub struct ProfileFile {
    pub name: String,
    pub path: PathBuf,
}

/// The description of a CMYK output profile, or `None` when `icc` isn't one.
pub fn cmyk_profile_name(icc: &[u8]) -> Option<String> {
    let profile = Profile::new_icc(icc).ok()?;
    if profile.color_space() != ColorSpaceSignature::CmykData {
        return None;
    }
    profile.info(InfoType::Description, Locale::none()).filter(|n| !n.trim().is_empty())
}

/// Where systems and Adobe programs keep colour profiles.
pub fn profile_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(windows) {
        if let Some(w) = std::env::var_os("WINDIR") {
            dirs.push(PathBuf::from(w).join("System32/spool/drivers/color"));
        }
        for var in ["ProgramFiles(x86)", "ProgramFiles", "CommonProgramFiles", "CommonProgramFiles(x86)"] {
            if let Some(p) = std::env::var_os(var) {
                let p = PathBuf::from(p);
                dirs.push(p.join("Common Files/Adobe/Color/Profiles"));
                dirs.push(p.join("Adobe/Color/Profiles"));
            }
        }
    } else if cfg!(target_os = "macos") {
        dirs.extend(
            ["/System/Library/ColorSync/Profiles", "/Library/ColorSync/Profiles", "/Library/Application Support/Adobe/Color/Profiles"]
                .map(PathBuf::from),
        );
        if let Some(h) = &home {
            dirs.push(h.join("Library/ColorSync/Profiles"));
        }
    } else {
        dirs.extend(["/usr/share/color/icc", "/usr/local/share/color/icc", "/var/lib/colord/icc"].map(PathBuf::from));
        if let Some(h) = &home {
            dirs.push(h.join(".local/share/color/icc"));
            dirs.push(h.join(".color/icc"));
        }
    }
    dirs
}

/// Every CMYK profile in `dirs` (and their subfolders), by name.
pub fn find_profiles(dirs: &[PathBuf]) -> Vec<ProfileFile> {
    let mut out: Vec<ProfileFile> = Vec::new();
    let mut stack: Vec<PathBuf> = dirs.to_vec();
    let mut visited = 0;
    while let Some(dir) = stack.pop() {
        visited += 1;
        if visited > 500 {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
            if !matches!(ext.as_deref(), Some("icc" | "icm")) {
                continue;
            }
            // The colour space is in the header: skip the rest without parsing them.
            let Ok(bytes) = std::fs::read(&path) else { continue };
            if bytes.get(16..20) != Some(b"CMYK".as_slice()) {
                continue;
            }
            if let Some(name) = cmyk_profile_name(&bytes) {
                if !out.iter().any(|p| p.name == name) {
                    out.push(ProfileFile { name, path });
                }
            }
        }
    }
    out.sort_by_key(|a| a.name.to_lowercase());
    out
}

/// A setup from a profile file.
pub fn load_setup(path: &Path, intent: CmykIntent, black_point: bool) -> Result<CmykSetup, String> {
    let icc = std::fs::read(path).map_err(|e| e.to_string())?;
    let name = cmyk_profile_name(&icc).ok_or("not a CMYK profile")?;
    Ok(CmykSetup { name, icc: Arc::new(icc), intent, black_point })
}

/// The conversions a CMYK document needs, built once per setup.
pub struct Proofer {
    to_cmyk: Transform<[u8; 3], [u8; 4]>,
    /// Back to RGB: with `to_cmyk`, the printable version of a colour.
    from_cmyk: Transform<[u8; 4], [u8; 3]>,
}

impl Proofer {
    /// `rgb_icc`: the document's RGB profile.
    pub fn new(rgb_icc: &[u8], setup: &CmykSetup) -> Result<Self, String> {
        let rgb = Profile::new_icc(rgb_icc).map_err(|e| e.to_string())?;
        let cmyk = Profile::new_icc(&setup.icc).map_err(|e| e.to_string())?;
        let flags = if setup.black_point { Flags::BLACKPOINT_COMPENSATION } else { Flags::default() };
        let intent = setup.intent.lcms();
        let to_cmyk =
            Transform::new_flags(&rgb, PixelFormat::RGB_8, &cmyk, PixelFormat::CMYK_8, intent, flags).map_err(|e| e.to_string())?;
        // Back with relative colorimetric, as Photoshop shows CMYK on screen.
        let from_cmyk =
            Transform::new_flags(&cmyk, PixelFormat::CMYK_8, &rgb, PixelFormat::RGB_8, Intent::RelativeColorimetric, flags)
                .map_err(|e| e.to_string())?;
        Ok(Self { to_cmyk, from_cmyk })
    }

    /// The printable version of many colours.
    fn round_trip(&self, rgb: &[[u8; 3]], out: &mut [[u8; 3]]) {
        let mut ink = vec![[0u8; 4]; rgb.len()];
        self.to_cmyk.transform_pixels(rgb, &mut ink);
        self.from_cmyk.transform_pixels(&ink, out);
    }

    /// The printable version of a colour.
    pub fn proof(&self, rgb: [u8; 3]) -> [u8; 3] {
        let mut out = [[0u8; 3]];
        self.round_trip(&[rgb], &mut out);
        out[0]
    }

    /// The RGB a CMYK ink mix shows as.
    pub fn rgb_of(&self, cmyk: [u8; 4]) -> [u8; 3] {
        let mut out = [[0u8; 3]];
        self.from_cmyk.transform_pixels(&[cmyk], &mut out);
        out[0]
    }

    /// Ink amounts 0..=255 (C, M, Y, K).
    pub fn cmyk(&self, rgb: [u8; 3]) -> [u8; 4] {
        let mut out = [[0u8; 4]];
        self.to_cmyk.transform_pixels(&[rgb], &mut out);
        out[0]
    }

    /// Ink amounts for many colours at once.
    pub fn cmyk_many(&self, rgb: &[[u8; 3]]) -> Vec<[u8; 4]> {
        let mut out = vec![[0u8; 4]; rgb.len()];
        self.to_cmyk.transform_pixels(rgb, &mut out);
        out
    }

    /// Returns the maximum channel difference between the RGB color and its CMYK-proofed version.
    pub fn gamut_distance(&self, rgb: [u8; 3]) -> u32 {
        let proofed = self.proof(rgb);
        [rgb[0].abs_diff(proofed[0]), rgb[1].abs_diff(proofed[1]), rgb[2].abs_diff(proofed[2])]
            .into_iter()
            .max()
            .unwrap_or(0) as u32
    }

    /// Whether this color is out of the print gamut (difference exceeds threshold, default 12).
    pub fn is_out_of_gamut(&self, rgb: [u8; 3], threshold: u32) -> bool {
        self.gamut_distance(rgb) > threshold
    }

    /// The printable version of every pixel of the `coords` tiles (all with `None`).
    pub fn proof_tiles(&self, tiles: &mut TileStore, coords: Option<&[crate::tile::TileCoord]>) {
        let all: Vec<crate::tile::TileCoord>;
        let coords = match coords {
            Some(c) => c,
            None => {
                all = tiles.iter().map(|(c, _)| c).collect();
                &all
            }
        };
        for &coord in coords {
            let Some(mut tile) = tiles.get(coord).map(|t| (**t).clone()) else { continue };
            let px = tile.bytes_mut().as_chunks_mut::<4>().0;
            let straight: Vec<[u8; 3]> = px
                .iter()
                .map(|p| {
                    let s = unpremultiply(*p);
                    [s[0], s[1], s[2]]
                })
                .collect();
            let mut proofed = vec![[0u8; 3]; straight.len()];
            self.round_trip(&straight, &mut proofed);
            for (p, c) in px.iter_mut().zip(proofed) {
                if p[3] > 0 {
                    *p = premultiply([c[0], c[1], c[2], p[3]]);
                }
            }
            tiles.insert(coord, tile);
        }
    }

    /// After an edit: the pixels of the `coords` tiles that differ from
    /// `before` made printable (unchanged ones stay, so colours don't drift
    /// from edit to edit).
    pub fn proof_changed(&self, tiles: &mut TileStore, before: &TileStore, coords: &[crate::tile::TileCoord]) {
        for &coord in coords {
            let Some(mut tile) = tiles.get(coord).map(|t| (**t).clone()) else { continue };
            let old = before.get(coord);
            let px = tile.bytes_mut().as_chunks_mut::<4>().0;
            let changed: Vec<usize> =
                (0..px.len()).filter(|&i| px[i][3] > 0 && old.is_none_or(|o| o.bytes().as_chunks::<4>().0[i] != px[i])).collect();
            if changed.is_empty() {
                continue;
            }
            let straight: Vec<[u8; 3]> = changed
                .iter()
                .map(|&i| {
                    let s = unpremultiply(px[i]);
                    [s[0], s[1], s[2]]
                })
                .collect();
            let mut proofed = vec![[0u8; 3]; straight.len()];
            self.round_trip(&straight, &mut proofed);
            for (&i, c) in changed.iter().zip(proofed) {
                px[i] = premultiply([c[0], c[1], c[2], px[i][3]]);
            }
            tiles.insert(coord, tile);
        }
    }

    /// RGB → CMYK as a 3D table of [`LUT_SIZE`]³ entries (red fastest, then
    /// green, then blue), each C, M, Y, K as bytes: for showing ink channels
    /// on the GPU.
    pub fn lut(&self) -> Vec<u8> {
        let n = LUT_SIZE;
        let step = |i: usize| ((i * 255) as f32 / (n - 1) as f32).round() as u8;
        let rgb: Vec<[u8; 3]> = (0..n * n * n).map(|i| [step(i % n), step(i / n % n), step(i / (n * n))]).collect();
        self.cmyk_many(&rgb).into_iter().flatten().collect()
    }

    /// After an edit: in the pixels that changed, only the inks in `edit`
    /// (C, M, Y, K) take the new amounts; the others keep the ones from `before`.
    pub fn keep_inks(&self, tiles: &mut TileStore, before: &TileStore, coords: &[crate::tile::TileCoord], edit: [bool; 4]) {
        if edit == [true; 4] {
            return;
        }
        for &coord in coords {
            let Some(mut tile) = tiles.get(coord).map(|t| (**t).clone()) else { continue };
            let old = before.get(coord);
            let px = tile.bytes_mut().as_chunks_mut::<4>().0;
            let changed: Vec<usize> =
                (0..px.len()).filter(|&i| px[i][3] > 0 && old.is_none_or(|o| o.bytes().as_chunks::<4>().0[i] != px[i])).collect();
            if changed.is_empty() {
                continue;
            }
            let rgb = |p: [u8; 4]| {
                let s = unpremultiply(p);
                [s[0], s[1], s[2]]
            };
            let now: Vec<[u8; 3]> = changed.iter().map(|&i| rgb(px[i])).collect();
            let was: Vec<[u8; 3]> = changed.iter().map(|&i| old.map_or([255; 3], |o| rgb(o.bytes().as_chunks::<4>().0[i]))).collect();
            let (now, was) = (self.cmyk_many(&now), self.cmyk_many(&was));
            for (k, &i) in changed.iter().enumerate() {
                let ink: [u8; 4] = std::array::from_fn(|c| if edit[c] { now[k][c] } else { was[k][c] });
                let c = self.rgb_of(ink);
                px[i] = premultiply([c[0], c[1], c[2], px[i][3]]);
            }
            tiles.insert(coord, tile);
        }
    }

    /// Every colour of a layer tree made printable: pixels, vector lines and
    /// texts (drawn again), fills, gradients and tones.
    pub fn proof_layers(&self, layers: &mut [Layer], canvas: (u32, u32)) {
        let f = |c: [u8; 3]| self.proof(c);
        let ff = |c: [f32; 3]| {
            let p = self.proof(c.map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8));
            p.map(|v| v as f32 / 255.0)
        };
        for layer in layers {
            match &mut layer.kind {
                LayerKind::Raster(tiles) => self.proof_tiles(tiles, None),
                LayerKind::Vector(v) => {
                    for s in &mut v.strokes {
                        s.color = ff(s.color);
                    }
                    for t in &mut v.texts {
                        t.color = f(t.color);
                        t.outline_color = f(t.outline_color);
                        if let Some(b) = &mut t.balloon {
                            let [r, g, bl] = f([b.fill[0], b.fill[1], b.fill[2]]);
                            b.fill = [r, g, bl, b.fill[3]];
                            b.line = f(b.line);
                        }
                    }
                    crate::vector::rasterize(v, canvas);
                }
                LayerKind::Folder(folder) => self.proof_layers(&mut folder.children, canvas),
                LayerKind::Fill { color } => *color = f(*color),
                LayerKind::Gradient(g) => {
                    for c in [&mut g.from, &mut g.to] {
                        let [r, gg, b] = f([c[0], c[1], c[2]]);
                        *c = [r, gg, b, c[3]];
                    }
                }
                LayerKind::Tone(t) => t.color = f(t.color),
                LayerKind::Adjustment(_) => {}
            }
        }
    }
}

/// Standard analytical RGB → proofed print RGB simulation and gamut distance,
/// used when no hardware CMYK profile is installed on the system.
///
/// Models standard offset press gamut compression (TAC limiting and cyan/magenta dye imperfections).
pub fn analytical_proof_rgb(rgb: [u8; 3]) -> [u8; 3] {
    let r = rgb[0] as f32 / 255.0;
    let g = rgb[1] as f32 / 255.0;
    let b = rgb[2] as f32 / 255.0;

    // Convert to simple CMYK
    let k = 1.0 - r.max(g).max(b);
    let (c, m, y) = if k < 1.0 {
        ((1.0 - r - k) / (1.0 - k), (1.0 - g - k) / (1.0 - k), (1.0 - b - k) / (1.0 - k))
    } else {
        (0.0, 0.0, 0.0)
    };

    // Standard offset press gamut boundary: physical inks have lower purity than phosphor/OLED primaries.
    let c_eff = c.min(0.92);
    let m_eff = m.min(0.88);
    let y_eff = y.min(0.95);
    // Parasitic losses when overlapping primary subtractive dyes:
    let green_loss = (c * y).sqrt() * 0.18;
    let red_loss = (m * y).sqrt() * 0.10;
    let blue_loss = (c * m).sqrt() * 0.22;

    // TAC constraint (max 300% total area coverage)
    let total = c_eff + m_eff + y_eff + k;
    let factor = if total > 3.0 { 3.0 / total } else { 1.0 };
    let c_p = c_eff * factor;
    let m_p = m_eff * factor;
    let y_p = y_eff * factor;
    let k_p = (k * factor).min(1.0);

    // Convert back to simulated printed RGB, accounting for dye contamination
    let pr = ((1.0 - c_p - red_loss * 0.3) * (1.0 - k_p) * 255.0).round().clamp(0.0, 255.0) as u8;
    let pg = ((1.0 - m_p - green_loss) * (1.0 - k_p) * 255.0).round().clamp(0.0, 255.0) as u8;
    let pb = ((1.0 - y_p - blue_loss) * (1.0 - k_p) * 255.0).round().clamp(0.0, 255.0) as u8;

    [pr, pg, pb]
}

/// Evaluates print gamut for an RGB color using an optional ICC Proofer,
/// falling back to the standard analytical press model if no proofer is provided.
///
/// Returns `(is_out_of_gamut, proofed_rgb, max_channel_difference)`.
pub fn check_print_gamut(proofer: Option<&Proofer>, rgb: [u8; 3], threshold: u32) -> (bool, [u8; 3], u32) {
    let proofed = match proofer {
        Some(p) => p.proof(rgb),
        None => analytical_proof_rgb(rgb),
    };
    let diff = [rgb[0].abs_diff(proofed[0]), rgb[1].abs_diff(proofed[1]), rgb[2].abs_diff(proofed[2])]
        .into_iter()
        .max()
        .unwrap_or(0) as u32;
    (diff > threshold, proofed, diff)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A CMYK profile on this computer, if any (none on bare CI machines).
    fn any_profile() -> Option<ProfileFile> {
        find_profiles(&profile_dirs()).into_iter().next()
    }

    #[test]
    fn colours_come_back_printable() {
        let Some(p) = any_profile() else {
            eprintln!("no CMYK profile here; skipped");
            return;
        };
        let setup = load_setup(&p.path, CmykIntent::RelativeColorimetric, true).unwrap();
        let proofer = Proofer::new(&crate::color_profile::ColorProfile::Srgb.icc(), &setup).unwrap();
        // Pure RGB green is out of any press gamut: it comes back duller.
        let green = proofer.proof([0, 255, 0]);
        assert!(green[1] < 250 || green[0] > 10 || green[2] > 10, "{}: {green:?}", p.name);
        // Proofing twice moves a colour only a little (8-bit tables); edits proof only new pixels.
        let again = proofer.proof(green);
        assert!(green.iter().zip(again).all(|(a, b)| a.abs_diff(b) <= 20), "{green:?} → {again:?}");
        // Black needs ink, white none.
        assert!(proofer.cmyk([0, 0, 0])[3] > 100);
        assert!(proofer.cmyk([255, 255, 255]).iter().all(|&v| v < 10));
        assert!(cmyk_profile_name(&crate::color_profile::ColorProfile::Srgb.icc()).is_none(), "an RGB profile isn't CMYK");
        // The table: white is the first-to-last corner, needs no ink.
        let lut = proofer.lut();
        assert_eq!(lut.len(), LUT_SIZE.pow(3) * 4);
        assert!(lut[lut.len() - 4..].iter().all(|&v| v < 10), "white: {:?}", &lut[lut.len() - 4..]);
        // Painting dark blue over white with only magenta editable: no cyan, yellow or black arrives.
        let white = TileStore::from_rgba(64, 64, &[255, 255, 255, 255].repeat(64 * 64));
        let mut tiles = TileStore::from_rgba(64, 64, &[20, 30, 120, 255].repeat(64 * 64));
        proofer.keep_inks(&mut tiles, &white, &[crate::tile::TileCoord::new(0, 0)], [false, true, false, false]);
        let p = tiles.pixel(5, 5);
        let ink = proofer.cmyk([p[0], p[1], p[2]]);
        assert!(ink[0] < 40 && ink[2] < 40 && ink[3] < 40 && ink[1] > 100, "{}: {p:?} → {ink:?}", p.len());
    }

    #[test]
    fn test_analytical_and_print_gamut_checks() {
        // Neutral gray (128, 128, 128) is well within any press gamut
        let (out, proofed, diff) = check_print_gamut(None, [128, 128, 128], 12);
        assert!(!out, "neutral gray is in gamut");
        assert!(diff <= 12, "diff {diff}");
        assert_eq!(proofed, [128, 128, 128]);

        // Pure saturated green [0, 255, 0] is heavily out of CMYK gamut
        let (out_green, proof_green, diff_green) = check_print_gamut(None, [0, 255, 0], 12);
        assert!(out_green, "pure green is out of gamut");
        assert!(diff_green > 12, "diff {diff_green}");
        assert!(proof_green[1] < 255);

        // Pure cyan [0, 255, 255] is also out of gamut
        let (out_cyan, _proof_cyan, diff_cyan) = check_print_gamut(None, [0, 255, 255], 12);
        assert!(out_cyan, "pure cyan is out of gamut");
        assert!(diff_cyan > 12);
    }
}
