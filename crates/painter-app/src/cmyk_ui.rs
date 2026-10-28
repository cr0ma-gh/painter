//! CMYK documents (Edit → Mode → CMYK Color…, or New Canvas), through Little
//! CMS: the colours stay printable with the chosen press profile, the colour
//! panel shows C M Y K and warns about colours the press can't print, and PSD
//! exports are CMYK files. See `painter_core::cmyk`.

use std::path::PathBuf;
use crate::i18n::tr;
use std::sync::Arc;

use eframe::egui::{self, Color32, RichText};
use painter_core::cmyk::{self, CmykIntent, CmykSetup, ProfileFile, Proofer};
use painter_core::color_mode::{self, ColorMode};

use crate::app::PainterApp;
use crate::theme;

/// The Convert to CMYK dialog.
#[derive(Clone)]
pub(crate) struct CmykDialog {
    profiles: Vec<ProfileFile>,
    chosen: usize,
    intent: CmykIntent,
    black_point: bool,
    error: Option<String>,
}

#[derive(Clone, PartialEq)]
struct DisplayKey {
    mode: ColorMode,
    shown: [bool; 4],
    duotone: Option<painter_core::duotone::Duotone>,
    proof: bool,
    gamut: bool,
}

/// The proofer for the document's current setup, rebuilt when it changes.
#[derive(Default)]
pub(crate) struct ProoferCache {
    key: Option<(usize, CmykIntent, bool, painter_core::color_profile::ColorProfile)>,
    pub proofer: Option<Proofer>,
    /// Fallback or default press proofer for RGB soft-proofing / gamut warnings.
    rgb_proofer_key: Option<(String, painter_core::color_profile::ColorProfile)>,
    pub rgb_proofer: Option<Proofer>,
    /// Its RGB → CMYK table for showing ink channels, with a key that changes with it.
    pub lut: Option<(u64, Arc<Vec<u8>>)>,
    built: u64,
    /// What the display table was made for: mode, channels shown, inks, proof colors, gamut warning.
    display_key: Option<DisplayKey>,
    /// How Duotone, Lab and Multichannel documents show, or RGB proof / gamut warning.
    pub display: Option<(u64, Arc<Vec<u8>>)>,
}

impl ProoferCache {
    pub(crate) fn active_or_rgb_proofer(&self) -> Option<&Proofer> {
        self.proofer.as_ref().or(self.rgb_proofer.as_ref())
    }
}

/// Profiles copied in with Load Profile, kept with Painter's settings.
fn imported_dir() -> Option<PathBuf> {
    crate::files::app_data_dir().map(|d| d.join("profiles"))
}

/// Every CMYK profile here: the system's, Adobe's, and imported ones.
pub(crate) fn all_profiles() -> Vec<ProfileFile> {
    let mut dirs = cmyk::profile_dirs();
    dirs.extend(imported_dir());
    cmyk::find_profiles(&dirs)
}

impl CmykDialog {
    pub(crate) fn new(current: Option<&CmykSetup>) -> Self {
        let profiles = all_profiles();
        // The document's profile, else FOGRA39 or SWOP when they're here.
        let chosen = current
            .and_then(|c| profiles.iter().position(|p| p.name == c.name))
            .or_else(|| profiles.iter().position(|p| p.name.contains("FOGRA39")))
            .or_else(|| profiles.iter().position(|p| p.name.contains("SWOP")))
            .unwrap_or(0);
        Self {
            profiles,
            chosen,
            intent: current.map_or(CmykIntent::RelativeColorimetric, |c| c.intent),
            black_point: current.is_none_or(|c| c.black_point),
            error: None,
        }
    }

    /// The profile picker, intent and black point compensation; returns the setup when there's one.
    pub(crate) fn ui(&mut self, ui: &mut egui::Ui) {
        if self.profiles.is_empty() {
            ui.label(tr("No CMYK profile found on this computer."));
            ui.small(tr("Load one (.icc / .icm) from your printer or print shop, or from Adobe's or ECI's free downloads."));
        } else {
            let shown = self.profiles.get(self.chosen).map_or("", |p| p.name.as_str()).to_string();
            egui::ComboBox::from_label(tr("Profile")).selected_text(shown).width(260.0).show_ui(ui, |ui| {
                for (i, p) in self.profiles.iter().enumerate() {
                    ui.selectable_value(&mut self.chosen, i, &p.name).on_hover_text(p.path.display().to_string());
                }
            });
        }
        #[cfg(not(any(target_os = "ios", target_os = "android")))]
        if ui.button(tr("Load Profile...")).on_hover_text(tr("Copies a CMYK .icc profile into Painter's settings")).clicked() {
            if let Some(path) = rfd::FileDialog::new().add_filter("ICC profile", &["icc", "icm"]).pick_file() {
                self.import(&path);
            }
        }
        egui::ComboBox::from_label(tr("Intent")).selected_text(tr(self.intent.name())).show_ui(ui, |ui| {
            for i in CmykIntent::ALL {
                ui.selectable_value(&mut self.intent, i, tr(i.name()));
            }
        });
        ui.checkbox(&mut self.black_point, tr("Black point compensation"));
        if let Some(e) = &self.error {
            ui.colored_label(Color32::from_rgb(240, 120, 100), e);
        }
    }

    fn import(&mut self, path: &std::path::Path) {
        let Ok(bytes) = std::fs::read(path) else {
            self.error = Some("Couldn't read the file".into());
            return;
        };
        let Some(name) = cmyk::cmyk_profile_name(&bytes) else {
            self.error = Some("That isn't a CMYK profile".into());
            return;
        };
        if let Some(dir) = imported_dir() {
            let _ = std::fs::create_dir_all(&dir);
            let file = dir.join(path.file_name().unwrap_or_default());
            if std::fs::write(&file, &bytes).is_ok() {
                self.profiles = all_profiles();
                self.chosen = self.profiles.iter().position(|p| p.name == name).unwrap_or(0);
                self.error = None;
            }
        }
    }

    pub(crate) fn setup(&self) -> Option<CmykSetup> {
        let p = self.profiles.get(self.chosen)?;
        cmyk::load_setup(&p.path, self.intent, self.black_point).ok()
    }
}

impl PainterApp {
    /// The RGB profile the document's numbers mean (sRGB for grey documents).
    fn rgb_icc(&self) -> Vec<u8> {
        let p = if self.doc.profile.is_gray() { painter_core::color_profile::ColorProfile::Srgb } else { self.doc.profile };
        p.icc()
    }

    fn ensure_rgb_proofer(&mut self) {
        if self.doc.mode != ColorMode::Rgb {
            return;
        }
        let profile_name = self.doc.cmyk.as_ref().map(|c| c.name.clone()).unwrap_or_else(|| "default".to_string());
        let key = (profile_name, self.doc.profile);
        if self.proofer.rgb_proofer_key.as_ref() == Some(&key) {
            return;
        }
        self.proofer.rgb_proofer_key = Some(key);
        self.proofer.rgb_proofer = None;

        let setup = if let Some(setup) = &self.doc.cmyk {
            Some(setup.clone())
        } else {
            let profiles = all_profiles();
            let chosen = profiles.iter().position(|p| p.name.contains("FOGRA39"))
                .or_else(|| profiles.iter().position(|p| p.name.contains("SWOP")))
                .or_else(|| (!profiles.is_empty()).then_some(0));
            chosen.and_then(|idx| cmyk::load_setup(&profiles[idx].path, CmykIntent::RelativeColorimetric, true).ok())
        };

        if let Some(setup) = setup {
            if let Ok(p) = Proofer::new(&self.rgb_icc(), &setup) {
                self.proofer.rgb_proofer = Some(p);
            }
        }
    }

    /// Keeps the proofer in step with the document's CMYK setup. Every frame.
    pub(crate) fn update_proofer(&mut self) {
        self.sync_channels();
        self.ensure_rgb_proofer();
        self.update_display_lut();
        let key = match (&self.doc.cmyk, self.doc.mode) {
            (Some(c), ColorMode::Cmyk) => Some((Arc::as_ptr(&c.icc) as usize, c.intent, c.black_point, self.doc.profile)),
            _ => None,
        };
        if key == self.proofer.key {
            return;
        }
        self.proofer.key = key;
        self.proofer.proofer = match (&self.doc.cmyk, key) {
            (Some(setup), Some(_)) => match Proofer::new(&self.rgb_icc(), setup) {
                Ok(p) => Some(p),
                Err(e) => {
                    self.status = format!("The CMYK profile couldn't be used: {e}");
                    None
                }
            },
            _ => None,
        };
        // The ink table for the Channels view (about 36 000 conversions, once per setup).
        self.proofer.built += 1;
        let built = self.proofer.built;
        self.proofer.lut = self.proofer.proofer.as_ref().map(|p| (built, Arc::new(p.lut())));
    }

    /// The display table, rebuilt when the mode, the channels shown or the inks change,
    /// or when Proof Colors (CMYK) / Gamut Warning are active.
    fn update_display_lut(&mut self) {
        let shown = self.channels.shown();
        let duotone = if self.doc.mode == ColorMode::Duotone { self.doc.duotone.clone() } else { None };
        let proof = self.proof_colors;
        let gamut = self.gamut_warning;
        let key = Some(DisplayKey {
            mode: self.doc.mode,
            shown,
            duotone,
            proof,
            gamut,
        });
        if key == self.proofer.display_key {
            return;
        }
        self.proofer.display_key = key;
        self.proofer.built += 1;
        let built = self.proofer.built;
        if proof || gamut {
            let lut = color_mode::print_proof_lut(self.proofer.active_or_rgb_proofer(), gamut);
            self.proofer.display = Some((built, Arc::new(lut)));
        } else {
            self.proofer.display = color_mode::display_lut(&self.doc, shown).map(|lut| (built, Arc::new(lut)));
        }
    }

    /// Edit → Mode → CMYK Color: converts the document with the dialog's setup.
    pub(crate) fn convert_to_cmyk(&mut self, setup: CmykSetup) {
        let proofer = match Proofer::new(&self.rgb_icc(), &setup) {
            Ok(p) => p,
            Err(e) => {
                self.status = format!("The CMYK profile couldn't be used: {e}");
                return;
            }
        };
        let started = std::time::Instant::now();
        self.next_label = Some("Mode: CMYK Color".into());
        self.checkpoint();
        // A Duotone's inks are its colours.
        color_mode::ink_duotone(&mut self.doc);
        self.doc.palette.clear();
        let canvas = self.doc.canvas();
        proofer.proof_layers(&mut self.doc.layers, canvas);
        if self.doc.profile.is_gray() {
            self.doc.profile = painter_core::color_profile::ColorProfile::Srgb;
        }
        self.status = format!("CMYK Color with {} in {} ms: colours are kept printable", setup.name, started.elapsed().as_millis());
        self.doc.cmyk = Some(setup);
        self.doc.mode = ColorMode::Cmyk;
        self.resync_gpu();
    }

    pub(crate) fn cmyk_window(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.cmyk_dialog.take() else { return };
        let (mut open, mut ok, mut cancel) = (true, false, false);
        egui::Window::new(tr("Convert to CMYK"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(tr("Colours become what this press profile can print (Little CMS)."));
                d.ui(ui);
                ui.small(tr("Layers stay; undo brings the colours back. PSD exports are CMYK files with the profile."));
                ui.horizontal(|ui| {
                    ok = ui.add_enabled(d.setup().is_some(), egui::Button::new(tr("OK"))).clicked();
                    cancel = ui.button(tr("Cancel")).clicked();
                });
            });
        if ok {
            if let Some(setup) = d.setup() {
                self.convert_to_cmyk(setup);
            }
        } else if !cancel && open {
            self.cmyk_dialog = Some(d);
        }
    }

    /// The main colour in C M Y K, with a warning when the press can't print it.
    pub(crate) fn cmyk_readout(&mut self, ui: &mut egui::Ui) -> bool {
        let Some(p) = &self.proofer.proofer else { return false };
        let [r, g, b, _] = self.color.to_srgba_unmultiplied();
        let ink = p.cmyk([r, g, b]).map(|v| (v as f32 / 2.55).round() as u32);
        let printable = p.proof([r, g, b]);
        let off = [r, g, b].iter().zip(printable).map(|(a, b)| a.abs_diff(b) as u32).max().unwrap_or(0);
        let text = format!("C{} M{} Y{} K{}", ink[0], ink[1], ink[2], ink[3]);
        ui.label(RichText::new(text).monospace().small().color(theme::text_dim()));
        if off > 12 {
            let warn = ui.add(egui::Button::new(RichText::new(tr("⚠")).color(Color32::from_rgb(240, 190, 90))).frame(false));
            if warn.on_hover_text(tr("Out of the press's gamut: it prints duller. Click to use the printable colour")).clicked() {
                self.color = Color32::from_rgb(printable[0], printable[1], printable[2]).into();
            }
        }
        true
    }
}
