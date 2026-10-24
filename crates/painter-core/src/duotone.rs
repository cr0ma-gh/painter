//! Duotone (Photoshop's Image → Mode → Duotone): a grey image printed with
//! one to four inks (monotone, duotone, tritone, quadtone). Each ink has a
//! colour and a curve from the grey's darkness to how much of it is laid
//! down; the inks print over each other.
//!
//! The document keeps its grey pixels; the canvas shows them through the
//! inks, and exports carry the inked colours.

use serde::{Deserialize, Serialize};

use crate::brush::PressureCurve;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DuotoneInk {
    pub name: String,
    pub color: [u8; 3],
    /// Darkness of the grey (0 white, 1 black) → ink coverage.
    pub curve: PressureCurve,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Duotone {
    /// One to four, the first printed first.
    pub inks: Vec<DuotoneInk>,
}

impl Default for Duotone {
    fn default() -> Self {
        Self::presets().swap_remove(1).1
    }
}

fn ink(name: &str, color: [u8; 3], curve: &[[f32; 2]]) -> DuotoneInk {
    DuotoneInk { name: name.into(), color, curve: PressureCurve::from_points(curve.to_vec()) }
}

const LINEAR: [[f32; 2]; 2] = [[0.0, 0.0], [1.0, 1.0]];

impl Duotone {
    /// Monotone, Duotone, Tritone or Quadtone.
    pub fn kind_name(&self) -> &'static str {
        match self.inks.len() {
            0 | 1 => "Monotone",
            2 => "Duotone",
            3 => "Tritone",
            _ => "Quadtone",
        }
    }

    /// Ready-made ink sets.
    pub fn presets() -> Vec<(&'static str, Duotone)> {
        // Black kept out of the light greys, so the colour shows there.
        let black_shadows = [[0.0, 0.0], [0.4, 0.12], [0.75, 0.55], [1.0, 1.0]];
        vec![
            ("Black (monotone)", Duotone { inks: vec![ink("Black", [0, 0, 0], &LINEAR)] }),
            (
                "Sepia",
                Duotone {
                    inks: vec![
                        ink("Black", [0, 0, 0], &black_shadows),
                        ink("Brown", [160, 110, 60], &[[0.0, 0.0], [0.5, 0.65], [1.0, 0.85]]),
                    ],
                },
            ),
            (
                "Warm red",
                Duotone {
                    inks: vec![
                        ink("Black", [0, 0, 0], &black_shadows),
                        ink("Warm red", [230, 70, 50], &[[0.0, 0.0], [0.5, 0.6], [1.0, 0.8]]),
                    ],
                },
            ),
            (
                "Blue",
                Duotone {
                    inks: vec![
                        ink("Black", [0, 0, 0], &black_shadows),
                        ink("Blue", [30, 90, 170], &[[0.0, 0.0], [0.5, 0.6], [1.0, 0.85]]),
                    ],
                },
            ),
            ("Cyanotype", Duotone { inks: vec![ink("Prussian blue", [18, 52, 110], &[[0.0, 0.0], [1.0, 1.0]])] }),
            (
                "Tritone (black, brown, tan)",
                Duotone {
                    inks: vec![
                        ink("Black", [0, 0, 0], &[[0.0, 0.0], [0.55, 0.08], [0.8, 0.5], [1.0, 1.0]]),
                        ink("Brown", [130, 80, 45], &[[0.0, 0.0], [0.5, 0.45], [1.0, 0.75]]),
                        ink("Tan", [215, 175, 120], &[[0.0, 0.0], [0.3, 0.45], [1.0, 0.6]]),
                    ],
                },
            ),
        ]
    }

    /// The printed colour of grey `v` (0 black, 255 white).
    pub fn render(&self, v: u8) -> [u8; 3] {
        let dark = 1.0 - v as f32 / 255.0;
        let mut out = [1.0f32; 3];
        for ink in &self.inks {
            let k = ink.curve.eval(dark);
            for (o, c) in out.iter_mut().zip(ink.color) {
                *o *= 1.0 - k * (1.0 - c as f32 / 255.0);
            }
        }
        out.map(|c| (c * 255.0 + 0.5).clamp(0.0, 255.0) as u8)
    }

    /// `render` for every grey.
    pub fn table(&self) -> Vec<[u8; 3]> {
        (0..=255u8).map(|v| self.render(v)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inks_print_over_each_other() {
        let mono = &Duotone::presets()[0].1;
        for v in [0u8, 77, 128, 255] {
            assert_eq!(mono.render(v), [v; 3], "black alone is the grey itself");
        }
        let sepia = Duotone::default();
        assert_eq!(sepia.kind_name(), "Duotone");
        assert_eq!(sepia.render(255), [255; 3], "no ink on white");
        let mid = sepia.render(150);
        assert!(mid[0] > mid[1] && mid[1] > mid[2], "brownish: {mid:?}");
        assert!(sepia.render(0).iter().all(|&c| c < 10), "black in the shadows");
        // Darker greys print darker.
        let t = sepia.table();
        let sum = |c: &[u8; 3]| c.iter().map(|&v| v as u32).sum::<u32>();
        assert!(t.windows(2).all(|w| sum(&w[0]) <= sum(&w[1]) + 1));
    }

    #[test]
    fn duotones_survive_a_save() {
        for (_, d) in Duotone::presets() {
            let back: Duotone = serde_json::from_str(&serde_json::to_string(&d).unwrap()).unwrap();
            assert_eq!(back, d);
        }
    }
}
