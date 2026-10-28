//! Panel contents and window chrome: menu, status bar, tool strip, colour,
//! sub tools, tool properties, navigator, layers and the New Canvas dialog.

use std::ops::RangeInclusive;
use crate::i18n::tr;

use eframe::egui::{self, ecolor::Hsva, Align2, Color32, Sense, Stroke, Vec2};
use painter_core::fill_tool::{GapPreset, ReferenceSource};
use painter_core::{
    AntiAlias, BlendMode, BrushMode, GradientShape, LayerKind, PressureCurve, SelectionOp, SmoothingMethod, TILE_BYTES,
};

use crate::app::PainterApp;
use crate::curve::curve_editor;
use crate::icons::{icon_button, Icon};
use crate::theme;
use crate::tools::Tool;
use crate::view::{MAX_ZOOM, MIN_ZOOM};
use crate::brush_ui::{group_differs, reset_group, PropertyGroup};
use crate::widgets::{self, color_wheel, prop_percent, prop_percent_reset, prop_slider, prop_slider_reset, segmented};

/// The layer colour a layer gets when it is switched on (a light blue, as in Clip Studio).
const DEFAULT_LAYER_COLOR: [u8; 3] = [0, 150, 255];

/// Which pressure curve the editor shows.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CurveTarget {
    Size,
    Density,
}

impl PainterApp {
    pub(crate) fn menu_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("menu").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button(tr("File"), |ui| {
                    if ui.add(egui::Button::new(tr("New Canvas...")).shortcut_text("Ctrl+N")).clicked() {
                        self.open_new_canvas();
                        ui.close_menu();
                    }
                    if ui
                        .add(egui::Button::new(tr("New from Clipboard")).shortcut_text("Ctrl+Alt+N"))
                        .on_hover_text(tr("A document the size of the copied picture or selection"))
                        .clicked()
                    {
                        ui.close_menu();
                        self.new_from_clipboard();
                    }
                    if ui.button(tr("New Comic Page / Storyboard...")).clicked() {
                        self.comic_wizard = Some(crate::comic_ui::ComicWizard::open());
                        ui.close_menu();
                    }
                    if ui.add(egui::Button::new(tr("Open...")).shortcut_text("Ctrl+O")).clicked() {
                        ui.close_menu();
                        self.open_dialog();
                    }
                    if ui.button(tr("Shared Session...")).on_hover_text(tr("Draw together with others: host this document or join one")).clicked() {
                        ui.close_menu();
                        self.show_editor(ui_workspace::EditorType::Session);
                    }
                    // The files opened or saved lately.
                    ui.add_enabled_ui(!self.recent.files.is_empty(), |ui| {
                        ui.menu_button(tr("Open Recent"), |ui| {
                            let mut open = None;
                            for f in self.recent.files.iter().take(15) {
                                let exists = f.exists();
                                let r = ui.add_enabled(exists, egui::Button::new(f.name())).on_hover_text(&f.path);
                                if r.clicked() {
                                    open = Some(std::path::PathBuf::from(&f.path));
                                }
                            }
                            ui.separator();
                            if ui.button(tr("Clear Recent Files")).clicked() {
                                ui.close_menu();
                                self.recent.clear();
                            }
                            if let Some(path) = open {
                                ui.close_menu();
                                self.open_any(path);
                            }
                        });
                    });
                    // iPadOS: the app's own folder in the Files app.
                    if crate::files::documents_dir().is_some() {
                        ui.menu_button(tr("Documents"), |ui| {
                            let docs = crate::files::documents();
                            if docs.is_empty() {
                                ui.weak(tr("Empty. Copy projects, images, models or brushes into Painter's folder in the Files app."));
                            }
                            for path in docs {
                                let name = path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                                if ui.button(name).clicked() {
                                    ui.close_menu();
                                    self.open_any(path);
                                }
                            }
                        });
                    }
                    ui.separator();
                    if ui.add(egui::Button::new(tr("Save")).shortcut_text("Ctrl+S")).clicked() {
                        ui.close_menu();
                        self.save();
                    }
                    if ui.add(egui::Button::new(tr("Save As...")).shortcut_text("Ctrl+Shift+S")).clicked() {
                        ui.close_menu();
                        self.save_as();
                    }
                    if ui.add_enabled(self.dirty_count() > 0, egui::Button::new(tr("Save All"))).clicked() {
                        ui.close_menu();
                        self.save_all();
                    }
                    ui.separator();
                    if ui.add(egui::Button::new(tr("Close")).shortcut_text("Ctrl+W")).clicked() {
                        ui.close_menu();
                        let id = self.docs.active_id();
                        self.request_close_tab(id);
                    }
                    if ui.button(tr("Close All")).clicked() {
                        ui.close_menu();
                        self.close_all_documents();
                    }
                    ui.separator();
                    ui.menu_button(tr("Import"), |ui| {
                        if ui.button(tr("PNG as Layer...")).clicked() {
                            ui.close_menu();
                            self.import_png_layer();
                        }
                        if ui.button(tr("From Scanner or Camera...")).clicked() {
                            ui.close_menu();
                            self.scanner = Some(crate::dialogs::ScannerDialog::open());
                        }
                        ui.separator();
                        if ui.button(tr("Brushes...")).clicked() {
                            ui.close_menu();
                            self.import_brushes_dialog();
                        }
                    });
                    ui.menu_button(tr("Export"), |ui| {
                        if ui.button(tr("PNG...")).clicked() {
                            ui.close_menu();
                            self.export_png();
                        }
                        if ui.button(tr("PSD...")).clicked() {
                            ui.close_menu();
                            self.export_psd();
                        }
                        if ui.button(tr("Krita (.kra)...")).clicked() {
                            ui.close_menu();
                            self.export_layered("kra");
                        }
                        if ui.button(tr("OpenRaster (.ora)...")).clicked() {
                            ui.close_menu();
                            self.export_layered("ora");
                        }
                        if ui.button(tr("Clip Studio (.clip)...")).clicked() {
                            ui.close_menu();
                            self.export_clip();
                        }
                        if ui.button(tr("Layer / Canvas as SVG...")).clicked() {
                            ui.close_menu();
                            self.svg_export = Some(crate::dialogs::SvgExportDialog::default());
                        }
                        ui.separator();
                        if ui.button(tr("All Brushes...")).clicked() {
                            ui.close_menu();
                            self.export_brushes(None);
                        }
                    });
                    ui.separator();
                    if ui.add(egui::Button::new(tr("Print...")).shortcut_text("Ctrl+P")).clicked() {
                        ui.close_menu();
                        self.open_print();
                    }
                    if cfg!(any(windows, target_os = "linux")) {
                        ui.separator();
                        if ui.button(tr("Set Up File Types...")).on_hover_text(tr("Thumbnails and Open with in the file manager")).clicked() {
                            ui.close_menu();
                            self.file_types_open = true;
                        }
                    }
                    if !cfg!(target_os = "ios") {
                        ui.separator();
                        if ui.add(egui::Button::new(tr("Quit")).shortcut_text("Ctrl+Q")).clicked() {
                            ui.close_menu();
                            self.quit_requested = true;
                        }
                    }
                });
                ui.menu_button(tr("Edit"), |ui| {
                    let undo = egui::Button::new(tr("Undo")).shortcut_text("Ctrl+Z");
                    if ui.add_enabled(self.history.can_undo(), undo).clicked() {
                        self.undo();
                        ui.close_menu();
                    }
                    let redo = egui::Button::new(tr("Redo")).shortcut_text("Ctrl+Shift+Z");
                    if ui.add_enabled(self.history.can_redo(), redo).clicked() {
                        self.redo();
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button(tr("Image Size...")).on_hover_text(tr("Resize the whole image; Nearest neighbour keeps pixels sharp")).clicked() {
                        ui.close_menu();
                        self.image_size = Some(crate::dialogs::ImageSizeDialog::open(self.doc.canvas(), self.last_resize_filter));
                    }
                    self.mode_menu(ui);
                    ui.separator();
                    let clear = egui::Button::new(tr("Clear Selected")).shortcut_text("Del");
                    if ui.add_enabled(self.doc.selection.is_some(), clear).clicked() {
                        self.clear_selected();
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button(tr("Define Brush Preset...")).on_hover_text(tr("Create a custom brush tip from the selection or active layer")).clicked() {
                        ui.close_menu();
                        self.define_brush_preset_dialog();
                    }
                    if ui.button(tr("Define Pattern...")).on_hover_text(tr("Create a pattern from the selection or active layer")).clicked() {
                        ui.close_menu();
                        self.define_pattern();
                    }
                    ui.separator();
                    ui.checkbox(&mut self.tools.paste_match_dpi, tr("Paste Keeps Print Size"))
                        .on_hover_text(tr("Between documents at different resolutions, pasted and copied layers are resampled to the same size on paper"));
                });
                ui.menu_button(tr("Select"), |ui| {
                    if ui.add(egui::Button::new(tr("All")).shortcut_text("Ctrl+A")).clicked() {
                        self.select_all();
                        ui.close_menu();
                    }
                    if ui.add(egui::Button::new(tr("Deselect")).shortcut_text("Ctrl+D")).clicked() {
                        self.deselect();
                        ui.close_menu();
                    }
                    if ui.add(egui::Button::new(tr("Invert")).shortcut_text("Ctrl+Shift+I")).clicked() {
                        self.invert_selection();
                        ui.close_menu();
                    }
                });
                ui.menu_button(tr("Layer"), |ui| {
                    for (label, action) in [
                        ("New Raster Layer", PainterApp::add_raster_layer as fn(&mut PainterApp)),
                        ("New Vector Layer", PainterApp::add_vector_layer),
                        ("New Folder", PainterApp::add_folder),
                        ("New Gradient Layer", PainterApp::add_gradient_layer),
                        ("New 3D Layer from Model...", PainterApp::new_scene_layer_dialog),
                        ("New 3D Layer from Perspective Ruler", PainterApp::new_scene_layer_from_ruler),
                        ("New File Object Layer...", PainterApp::new_file_object_dialog),
                        ("New Tone Layer (from selection)", PainterApp::add_tone_layer),
                    ] {
                        if ui.button(label).clicked() {
                            action(self);
                            ui.close_menu();
                        }
                    }
                    ui.menu_button(tr("New Correction Layer"), |ui| {
                        for adjustment in painter_core::Adjustment::defaults() {
                            if ui.button(tr(adjustment.name())).clicked() {
                                self.add_adjustment_layer(adjustment);
                                ui.close_menu();
                            }
                        }
                    });
                    ui.separator();
                    let has_mask = self.doc.active_layer().is_some_and(|l| l.mask.is_some());
                    if ui.add_enabled(!has_mask, egui::Button::new(tr("Add Layer Mask"))).clicked() {
                        self.add_mask();
                        ui.close_menu();
                    }
                    if ui.add_enabled(has_mask, egui::Button::new(tr("View Mask (Grayscale)"))).clicked() {
                        self.toggle_mask_view(painter_render::MaskViewMode::Grayscale);
                        ui.close_menu();
                    }
                    if ui.add_enabled(has_mask, egui::Button::new(tr("Rubylith Mask Overlay (\\)"))).clicked() {
                        self.toggle_mask_view(painter_render::MaskViewMode::Rubylith);
                        ui.close_menu();
                    }
                    if ui.add_enabled(has_mask, egui::Button::new(tr("Enable / Disable Mask"))).clicked() {
                        self.toggle_mask_enabled();
                        ui.close_menu();
                    }
                    if ui.add_enabled(has_mask, egui::Button::new(tr("Delete Layer Mask"))).clicked() {
                        self.delete_mask();
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button(tr("Delete Layer")).clicked() {
                        self.delete_active_layer();
                        ui.close_menu();
                    }
                    ui.separator();
                    let others: Vec<(usize, String)> =
                        (0..self.docs.slots.len()).filter(|&i| i != self.docs.active).map(|i| (i, self.tab_title(i))).collect();
                    ui.add_enabled_ui(!others.is_empty(), |ui| {
                        ui.menu_button(tr("Copy Layer to Document"), |ui| {
                            for (i, name) in &others {
                                if ui.button(name).clicked() {
                                    let layer = self.doc.active;
                                    self.transfer_layer_to(*i, layer);
                                    ui.close_menu();
                                }
                            }
                        });
                    })
                    .response
                    .on_hover_text(tr("Or drag the layer from the Layers panel onto a document tab"));
                });
                ui.menu_button(tr("Filter"), |ui| {
                    for filter in crate::adjust_ui::filter_presets(self.doc.canvas()) {
                        if ui.button(format!("{}...", filter.name())).clicked() {
                            ui.close_menu();
                            self.open_filter(filter);
                        }
                    }
                    ui.separator();
                    ui.small(tr("On the edited layer, inside the selection"));
                });
                ui.menu_button(tr("View"), |ui| {
                    ui.checkbox(&mut self.ws.bars.tool_strip, tr("Tool Strip")).on_hover_text(tr("The tools on the left"));
                    ui.checkbox(&mut self.ws.bars.command_bar, tr("Command Bar")).on_hover_text(tr("The icon buttons under the menu"));
                    ui.checkbox(&mut self.ws.chrome.workspace_tabs, tr("Workspace Bar"))
                        .on_hover_text(tr("The workspace tabs (Painting, Inking…); hidden, switch workspaces from View > Workspace"));
                    // The interface language.
                    ui.menu_button(tr("Language"), |ui| {
                        let chosen = self.ws.chrome.language;
                        let mut pick = None;
                        let system = crate::i18n::system_language();
                        if ui.radio(chosen.is_empty(), format!("{} ({system})", tr("System default"))).clicked() {
                            pick = Some(String::new());
                        }
                        for (code, name) in crate::i18n::languages() {
                            if ui.radio(chosen.as_str() == code, name).clicked() {
                                pick = Some(code);
                            }
                        }
                        ui.separator();
                        ui.small(tr("Translations are .po files: see locales/README.md"));
                        if let Some(code) = pick {
                            self.ws.chrome.language = code.as_str().into();
                            let used = crate::i18n::set_language(&code);
                            self.status = format!("{} {used}", tr("Interface language:"));
                            ui.close_menu();
                        }
                    });
                    // Switching workspaces without the bar.
                    ui.menu_button(tr("Workspace"), |ui| {
                        let mut pick = None;
                        for (i, w) in self.ws.set.workspaces.iter().enumerate() {
                            if ui.radio(i == self.ws.set.active, crate::i18n::tr_str(&w.name)).clicked() {
                                pick = Some(i);
                            }
                        }
                        if let Some(i) = pick {
                            self.ws.set.activate(i);
                            ui.close_menu();
                        }
                    });
                    ui.checkbox(&mut self.ws.chrome.clock, tr("Clock and Battery")).on_hover_text(tr("The time and the battery's charge in the canvas's corner"));
                    ui.menu_button(tr("Screen Mode"), |ui| {
                        let ctx = ui.ctx().clone();
                        for (mode, label) in [(0u8, "Standard"), (1, "Full Screen with Menus"), (2, "Full Screen")] {
                            if ui.radio(self.screen_mode == mode, label).clicked() {
                                ui.close_menu();
                                self.set_screen_mode(&ctx, mode);
                            }
                        }
                        ui.small(tr("F cycles them, Shift+F goes back"));
                    });
                    ui.separator();
                    if ui.add(egui::Button::new(tr("Fit to Window")).shortcut_text("0")).clicked() {
                        self.fit_pending = true;
                        ui.close_menu();
                    }
                    if ui.button(tr("Actual Pixels (100%)")).clicked() {
                        self.view.set_zoom(1.0);
                        ui.close_menu();
                    }
                    if ui.button(tr("Reset Rotation")).clicked() {
                        self.view.set_rotation(0.0);
                        ui.close_menu();
                    }
                    if ui.checkbox(&mut self.view.flip, tr("Flip Horizontally")).clicked() {
                        ui.close_menu();
                    }
                    ui.separator();
                    let mut proof = self.proof_colors;
                    if ui.add(egui::Checkbox::new(&mut proof, tr("Proof Colors (CMYK)"))).on_hover_text(tr("Ctrl+Y")).clicked() {
                        self.proof_colors = proof;
                        self.update_proofer();
                    }
                    let mut gamut = self.gamut_warning;
                    if ui.add(egui::Checkbox::new(&mut gamut, tr("Gamut Warning"))).on_hover_text(tr("Ctrl+Shift+Y: highlight unprintable colours")).clicked() {
                        self.gamut_warning = gamut;
                        self.update_proofer();
                    }
                    if ui
                        .checkbox(&mut self.tools.pencil_only, tr("Draw With the Pen Only"))
                        .on_hover_text(tr("Fingers move the view instead of painting (iPad: Apple Pencil draws, fingers navigate)"))
                        .changed()
                        && self.ws.palm.mode != crate::palm::PalmMode::Off
                    {
                        // With palm rejection on, this is its "Stylus Only" mode.
                        self.ws.palm.mode = if self.tools.pencil_only { crate::palm::PalmMode::StylusOnly } else { crate::palm::PalmMode::Standard };
                    }
                    if ui.button(tr("Input & Tablet...")).on_hover_text(tr("Palm rejection, drawing hand and touch sensitivity")).clicked() {
                        self.palm_open = true;
                        ui.close_menu();
                    }
                    ui.menu_button(tr("Performance Profile"), |ui| self.performance_ui(ui))
                        .response
                        .on_hover_text(tr("How much quality is traded for GPU memory and RAM"));
                    ui.checkbox(&mut self.modifier_keys.shown, tr("On-Screen Modifier Keys"))
                        .on_hover_text(tr("Ctrl, Shift and Alt buttons for touch screens: tap to hold a key, tap again to release it"));
                    ui.separator();
                    let mut grid = self.doc.rulers.grid.visible;
                    if ui.add(egui::Checkbox::new(&mut grid, tr("Grid"))).on_hover_text(tr("Ctrl+'")).clicked() {
                        self.doc.rulers.grid.visible = grid;
                    }
                    let mut snap = self.doc.rulers.snapping;
                    if ui.add(egui::Checkbox::new(&mut snap, tr("Snap to Rulers"))).on_hover_text(tr("Ctrl+1")).clicked() {
                        self.toggle_ruler_snapping();
                    }
                });
                ui.menu_button(tr("Window"), |ui| {
                    for editor in ui_workspace::EditorType::ALL.into_iter().filter(|e| *e != ui_workspace::EditorType::Canvas) {
                        let mut open = self.editor_open(editor);
                        if ui.checkbox(&mut open, tr(editor.title())).clicked() {
                            self.toggle_editor(editor, ui.ctx());
                        }
                    }
                    ui.separator();
                    if ui.button(tr("Reset Workspace Layout")).clicked() {
                        self.ws.set.current_mut().reset();
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.add(egui::Button::new(tr("Command Palette...")).shortcut_text("F3")).clicked() {
                        ui.close_menu();
                        self.open_palette();
                    }
                    if ui.add(egui::Button::new(tr("Zen Mode")).shortcut_text("Tab / F11")).clicked() {
                        ui.close_menu();
                        self.toggle_zen();
                    }
                    if ui.button(tr("Theme Editor...")).clicked() {
                        ui.close_menu();
                        self.ws.theme_editor = true;
                    }
                    let mut follow = self.ws.themes.follow_system;
                    if ui
                        .checkbox(&mut follow, tr("Theme Follows the System"))
                        .on_hover_text(tr("Light or dark with the system's mode; off: the theme stays as picked"))
                        .changed()
                    {
                        self.ws.themes.follow_system = follow;
                        self.ws.themes.seen_system = None;
                    }
                    ui.menu_button(tr("Interface Size"), |ui| {
                        ui.set_min_width(220.0);
                        self.ui_scale_controls(ui);
                    });
                });
                ui.menu_button(tr("Help"), |ui| {
                    if ui.button(tr("Welcome Screen")).clicked() {
                        ui.close_menu();
                        self.welcome_open = true;
                    }
                    if ui.button(tr("Check for Updates...")).clicked() {
                        ui.close_menu();
                        self.check_for_updates();
                    }
                    if ui.button(tr("Report a Bug...")).clicked() {
                        ui.close_menu();
                        crate::crash_report::report_bug(ui.ctx());
                    }
                    if ui.button(tr("View Crash Logs...")).clicked() {
                        ui.close_menu();
                        self.crash_reports.show();
                    }
                    if ui.button(tr("Reset Settings...")).clicked() {
                        ui.close_menu();
                        self.reset_dialog = Some(Default::default());
                    }
                    if ui.button(tr("About Painter")).clicked() {
                        ui.close_menu();
                        self.about_open = true;
                    }
                    ui.separator();
                    ui.hyperlink_to("Website", crate::welcome::WEBSITE);
                    ui.hyperlink_to("Source on GitHub", crate::welcome::REPOSITORY);
                });
            });
        });
    }

    pub(crate) fn status_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(format!("{} × {} px", self.doc.width, self.doc.height));
                // Resolution: what the canvas measures on paper; changing it keeps the pixels.
                let (w_mm, h_mm) = self.doc.size_mm();
                ui.menu_button(format!("{} dpi", self.doc.dpi), |ui| {
                    ui.label(format!("{w_mm:.1} × {h_mm:.1} mm on paper"));
                    ui.horizontal(|ui| {
                        if crate::print_ui::dpi_edit(ui, "status_dpi", &mut self.doc.dpi) {
                            self.dirty = true;
                        }
                    });
                    ui.small(tr("Saved in the project and written into PNG and PSD exports.\nPixels aren't resampled."));
                })
                .response
                .on_hover_text(format!("{w_mm:.1} × {h_mm:.1} mm at {} dpi", self.doc.dpi));
                ui.label(self.doc.mode.short()).on_hover_text(format!("{}, {} (Edit > Mode)", self.doc.mode.name(), self.doc.profile.name()));
                ui.separator();
                ui.label(format!("{:.0}%", self.view.zoom * 100.0));
                ui.separator();
                ui.label(format!("{:.0}°", self.view.rotation.to_degrees()));
                ui.separator();
                let tiles = self.doc.tile_count();
                ui.label(format!("{tiles} tiles · {:.1} MB", (tiles * TILE_BYTES) as f64 / 1_048_576.0))
                    .on_hover_text(tr("Painted layer tiles held in memory"));
                if let Some((slots, bytes)) = self.gpu_stats {
                    ui.separator();
                    ui.label(format!("GPU atlas {slots} tiles / {:.0} MB", bytes as f64 / 1_048_576.0))
                        .on_hover_text(tr("Shared GPU tile atlas: grows with painted area, not layer count"));
                }
                ui.separator();
                ui.label(self.tablet.describe());
                if self.edit_mask {
                    ui.separator();
                    ui.colored_label(Color32::from_rgb(120, 200, 255), "Editing mask");
                }
                ui.separator();
                ui.label(&self.status);
            });
        });
    }

    pub(crate) fn color_panel(&mut self, ui: &mut egui::Ui) {
        let size = ui.available_width().clamp(120.0, 260.0);
        ui.vertical_centered(|ui| {
            color_wheel(ui, &mut self.color, size);
        });
        ui.horizontal(|ui| {
            self.color_chips(ui, 26.0);
            if widgets::transparent_chip(ui, self.transparent, 22.0).clicked() {
                self.toggle_transparent();
            }
            if icon_button(ui, Icon::SwapColors, 14.0, false, "Swap main and sub colour (X)").clicked() {
                std::mem::swap(&mut self.color, &mut self.sub_color);
            }
            if icon_button(ui, Icon::DefaultColors, 14.0, false, "Black and white").clicked() {
                self.color = Hsva::new(0.0, 0.0, 0.0, 1.0);
                self.sub_color = Hsva::new(0.0, 0.0, 1.0, 1.0);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut main = Color32::from(self.color);
                if ui.color_edit_button_srgba(&mut main).on_hover_text(tr("Edit numerically")).changed() {
                    self.color = Hsva::from(main);
                }
                // The mode's own numbers: CMYK inks (and the press's warning), Lab, the table's index.
                if !self.mode_readout(ui) {
                    let [r, g, b, _] = self.color.to_srgba_unmultiplied();
                    if self.doc.mode == painter_core::color_mode::ColorMode::Rgb {
                        let (out_of_gamut, printable, _) = painter_core::cmyk::check_print_gamut(
                            self.proofer.active_or_rgb_proofer(),
                            [r, g, b],
                            12,
                        );
                        if out_of_gamut {
                            let warn = ui.add(egui::Button::new(egui::RichText::new(tr("⚠")).color(Color32::from_rgb(240, 190, 90))).frame(false));
                            if warn.on_hover_text(tr("Out of print gamut (CMYK): colour will print duller. Click to snap to the closest printable colour")).clicked() {
                                self.color = Color32::from_rgb(printable[0], printable[1], printable[2]).into();
                            }
                        }
                    }
                    ui.label(egui::RichText::new(format!("#{r:02X}{g:02X}{b:02X}")).monospace().color(theme::text_dim()));
                }
            });
        });
    }

    /// Clip Studio's Brush Size palette: common sizes as dots, click to use.
    pub(crate) fn brush_size_panel(&mut self, ui: &mut egui::Ui) {
        const SIZES: [f32; 40] = [
            0.7, 1.0, 1.5, 2.0, 2.5, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 10.0, 12.0, 15.0, 17.0, 20.0, 25.0, 30.0, 40.0, 50.0, 60.0, 70.0,
            80.0, 100.0, 120.0, 150.0, 170.0, 200.0, 250.0, 300.0, 400.0, 500.0, 600.0, 700.0, 800.0, 1000.0, 1200.0, 1500.0, 1700.0,
            2000.0,
        ];
        let cell = Vec2::new(36.0, 44.0);
        let columns = ((ui.available_width() + 1.0) / (cell.x + 1.0)).floor().max(1.0) as usize;
        let current = self.current_brush().size;
        let mut picked = None;
        for row in SIZES.chunks(columns) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing = Vec2::splat(1.0);
                for &size in row {
                    let (rect, response) = ui.allocate_exact_size(cell, Sense::click());
                    let selected = (current - size).abs() < 0.05;
                    let fill = if selected {
                        theme::selected()
                    } else if response.hovered() {
                        theme::hover()
                    } else {
                        theme::well()
                    };
                    ui.painter().rect_filled(rect, 0.0, fill);
                    // Dot diameter grows with the size, capped to the cell.
                    let d = (size.sqrt() * 1.9).clamp(1.5, 26.0);
                    let dot = egui::pos2(rect.center().x, rect.top() + 16.0);
                    ui.painter().circle_filled(dot, d * 0.5, theme::text());
                    let label = if size < 10.0 && size.fract() != 0.0 { format!("{size:.1}") } else { format!("{size:.0}") };
                    ui.painter().text(
                        egui::pos2(rect.center().x, rect.bottom() - 2.0),
                        Align2::CENTER_BOTTOM,
                        label,
                        egui::FontId::proportional(10.0),
                        theme::text(),
                    );
                    if response.clicked() {
                        picked = Some(size);
                    }
                }
            });
        }
        if let Some(size) = picked {
            self.current_brush_mut().size = size;
        }
    }

    /// Clip Studio's Tool Group: the toolbar group's brushes as stroke previews.
    pub(crate) fn subtool_panel(&mut self, ui: &mut egui::Ui) {
        let group = self.brush_group;
        widgets::group_header(ui, group.icon(), group.label());
        ui.add_space(2.0);
        // Quick search: the group's brushes whose name has every typed word.
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let (glyph, _) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::hover());
            crate::icons::paint_glyph(ui.painter(), glyph, 12.0, crate::icons::Glyph::Search, theme::text_dim());
            let clear_w = if self.brush_filter.is_empty() { 0.0 } else { 20.0 };
            let field = egui::TextEdit::singleline(&mut self.brush_filter)
                .hint_text(tr("Search brushes"))
                .desired_width((ui.available_width() - clear_w).max(40.0));
            let edit = ui.add(field);
            if edit.has_focus() && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
                self.brush_filter.clear();
                edit.surrender_focus();
            } else if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.brush_filter.clear();
            }
            if !self.brush_filter.is_empty() && ui.small_button("×").on_hover_text(tr("Clear the search")).clicked() {
                self.brush_filter.clear();
            }
        });
        let footer = 30.0;
        let list_height = (ui.available_height() - footer).max(60.0);
        let in_group = self.brushes_in(group);
        let renaming = self.brush_rename.as_ref().map(|r| r.0);
        let indices: Vec<usize> = in_group
            .iter()
            .copied()
            .filter(|&i| Some(i) == renaming || crate::brush_ui::name_matches(&self.brushes[i].settings.name, &self.brush_filter))
            .collect();
        let mut picked = None;
        // What a brush's right-click menu asked for: (brush, 0 reset, 1 duplicate, 2 delete, 3 rename).
        let mut menu: Option<(usize, u8)> = None;
        // A rename finished: the new name, or None when cancelled.
        let mut rename_end: Option<Option<String>> = None;
        let builtin: Vec<String> = crate::app::default_brushes().into_iter().map(|b| b.settings.name).collect();
        egui::Frame::new().fill(theme::well()).show(ui, |ui| {
            ui.set_min_height(list_height);
            egui::ScrollArea::vertical().max_height(list_height).auto_shrink([false, false]).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                if indices.is_empty() && !in_group.is_empty() {
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new(tr("No brush in this group matches the search.")).color(theme::text_dim()));
                } else if indices.is_empty() {
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(tr("No brushes in this group yet. Import some with the button below.")).color(theme::text_dim()),
                    );
                }
                let total = self.brushes.len();
                for &i in &indices {
                    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 38.0), Sense::click());
                    let selected = i == self.brush && self.tool.paints();
                    let fill = if selected {
                        theme::selected()
                    } else if response.hovered() {
                        theme::hover()
                    } else {
                        theme::well()
                    };
                    let painter = ui.painter_at(rect);
                    painter.rect_filled(rect, 0.0, fill);
                    painter.hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0_f32, theme::line()));
                    let preset = &self.brushes[i];
                    let texture = self.previews.get(ui.ctx(), i, &preset.settings, total);
                    let size = texture.size_vec2();
                    let shown =
                        egui::Rect::from_min_size(rect.left_top() + Vec2::new(8.0, 1.0), size.min(rect.size() - Vec2::new(16.0, 2.0)));
                    let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(shown.width() / size.x, 1.0));
                    painter.image(texture.id(), shown, uv, theme::text());
                    let name = &preset.settings.name;
                    let galley = painter.layout_no_wrap(name.clone(), egui::FontId::proportional(12.0), theme::strong());
                    let at = rect.right_bottom() - Vec2::new(galley.size().x + 6.0, galley.size().y + 2.0);
                    // A backing so the name stays legible over the stroke.
                    painter.rect_filled(
                        egui::Rect::from_min_size(at - Vec2::new(3.0, 0.0), galley.size() + Vec2::new(6.0, 0.0)),
                        2.0,
                        fill.gamma_multiply(0.85),
                    );
                    painter.galley(at, galley, theme::strong());
                    // Badges for a textured, dual-tip, sprayed or imported brush, top right.
                    let badges = crate::brush_ui::badges(preset);
                    crate::brush_ui::paint_badges(&painter, rect.right() - 6.0, rect.top() + 8.0, &badges);
                    let mut hints: Vec<String> = badges.iter().map(|b| tr(b.hint()).to_string()).collect();
                    let missing = preset.unsupported_features();
                    if !missing.is_empty() {
                        hints.push(format!("{}: {}", tr("Not rendered yet"), missing.join(", ")));
                    }
                    let response = if hints.is_empty() { response } else { response.on_hover_text(hints.join("\n")) };
                    if response.clicked() {
                        picked = Some(i);
                    }
                    if response.double_clicked() {
                        menu = Some((i, 3));
                    }
                    // The name field over the row while renaming: Enter or a click elsewhere keeps it, Esc cancels.
                    if let Some((_, text, focused)) = self.brush_rename.as_mut().filter(|r| r.0 == i) {
                        let field = egui::Rect::from_min_max(rect.left_top() + Vec2::new(6.0, 8.0), rect.right_bottom() - Vec2::new(6.0, 8.0));
                        let edit = ui.put(field, egui::TextEdit::singleline(text).font(egui::FontId::proportional(12.0)));
                        if !*focused {
                            edit.request_focus();
                            *focused = true;
                        }
                        if ui.input(|inp| inp.key_pressed(egui::Key::Escape)) {
                            rename_end = Some(None);
                        } else if edit.lost_focus() {
                            rename_end = Some(Some(text.clone()));
                        }
                    }
                    let is_builtin = builtin.contains(&self.brushes[i].settings.name);
                    response.context_menu(|ui| {
                        if ui
                            .add_enabled(is_builtin, egui::Button::new(tr("Reset to Default")))
                            .on_disabled_hover_text(tr("Imported brushes have no default to go back to"))
                            .clicked()
                        {
                            menu = Some((i, 0));
                            ui.close_menu();
                        }
                        if ui.button(tr("Rename")).clicked() {
                            menu = Some((i, 3));
                            ui.close_menu();
                        }
                        if ui.button(tr("Duplicate")).clicked() {
                            menu = Some((i, 1));
                            ui.close_menu();
                        }
                        if ui.button(tr("Delete")).clicked() {
                            menu = Some((i, 2));
                            ui.close_menu();
                        }
                    });
                }
            });
        });
        if let Some(i) = picked {
            // The stamps and the history brush stay in hand: the click picks their tip.
            let tool = self.tool;
            self.select_brush(i);
            if matches!(tool, Tool::Stamp(_)) {
                self.tool = tool;
            }
        }
        match menu {
            Some((i, 0)) => self.reset_brush(i),
            Some((i, 1)) => self.duplicate_brush(i),
            Some((i, 2)) => self.delete_brush(i),
            Some((i, 3)) => self.brush_rename = Some((i, self.brushes[i].settings.name.clone(), false)),
            _ => {}
        }
        if let Some(end) = rename_end {
            if let (Some(name), Some((i, _, _))) = (end, self.brush_rename.take()) {
                self.rename_brush(i, &name);
            }
            self.brush_rename = None;
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let import = icon_button(
                ui,
                Icon::Open,
                16.0,
                false,
                "Import brushes: Clip Studio .sut/.todb, Photoshop .abr, Painter .rustbrush (right-click: default brushes)",
            );
            if import.clicked() {
                self.import_brushes_dialog();
            }
            let add_custom = icon_button(
                ui,
                Icon::Add,
                16.0,
                false,
                tr("Define Brush Preset from selection or layer..."),
            );
            if add_custom.clicked() {
                self.define_brush_preset_dialog();
            }
            import.context_menu(|ui| {
                if ui.button(tr("Define Brush Preset from selection or layer...")).clicked() {
                    self.define_brush_preset_dialog();
                    ui.close_menu();
                }
                ui.separator();
                if ui.button(tr("Add Back Missing Default Brushes")).clicked() {
                    self.restore_default_brushes();
                    ui.close_menu();
                }
                if ui.button(tr("Reset Built-in Brushes")).on_hover_text(tr("Edited built-in brushes back to default; imported brushes stay")).clicked() {
                    self.reset_builtin_brushes();
                    ui.close_menu();
                }
                if ui.button(tr("Reset Whole Brush List…")).on_hover_text(tr("Only the default brushes: imported ones are removed")).clicked() {
                    self.confirm_brush_reset = true;
                    ui.close_menu();
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                ui.add_enabled_ui(self.brushes.len() > 1, |ui| {
                    if icon_button(ui, Icon::Delete, 16.0, false, "Delete brush").clicked() {
                        self.delete_brush(self.brush);
                    }
                });
                if icon_button(ui, Icon::SaveAs, 16.0, false, "Export the brush as .rustbrush (right-click: all)").clicked() {
                    self.export_brushes(Some(self.brush));
                }
                if icon_button(ui, Icon::Duplicate, 16.0, false, "Duplicate brush").clicked() {
                    self.duplicate_brush(self.brush);
                }
                if icon_button(ui, Icon::Text, 16.0, false, "Rename brush (or double-click it)").clicked() {
                    self.brush_rename = Some((self.brush, self.brushes[self.brush].settings.name.clone(), false));
                }
            });
        });
        if !self.import_warnings.is_empty() {
            let mut dismiss = false;
            egui::CollapsingHeader::new(format!("Last import: {} fallbacks", self.import_warnings.len()))
                .id_salt("import_warnings")
                .show(ui, |ui| {
                    for warning in &self.import_warnings {
                        ui.small(warning);
                    }
                    dismiss = ui.small_button(tr("Dismiss")).clicked();
                });
            if dismiss {
                self.import_warnings.clear();
            }
        }
    }

    /// Settings of the current tool (brush settings for the brush).
    pub(crate) fn tool_property_panel(&mut self, ui: &mut egui::Ui) {
        match self.tool {
            Tool::Brush => self.brush_settings(ui),
            Tool::Ruler => {
                widgets::group_header(ui, Icon::Path, tr("Rulers and Guides"));
                self.ruler_panel(ui);
            }
            Tool::Fill => {
                widgets::group_header(ui, Icon::Bucket, tr("Fill"));
                let fill = &mut self.tools.fill;
                ui.horizontal(|ui| {
                    ui.label(tr("Close gaps"));
                    for preset in GapPreset::ALL {
                        if ui.selectable_label(fill.gap == preset.pixels(), preset.label()).clicked() {
                            fill.gap = preset.pixels();
                        }
                    }
                });
                let mut gap = fill.gap as f32;
                if prop_slider(ui, "Gap size", &mut gap, 0.0..=40.0, " px", 0, false) {
                    fill.gap = gap.round() as _;
                }
                let mut tolerance = fill.tolerance as f32;
                if prop_slider(ui, "Tolerance", &mut tolerance, 0.0..=255.0, "", 0, false) {
                    fill.tolerance = tolerance.round() as _;
                }
                let mut expand = fill.expand as f32;
                if prop_slider(ui, "Expand", &mut expand, -10.0..=10.0, " px", 0, false) {
                    fill.expand = expand.round() as _;
                }
                ui.small(tr("Expand slips the colour under the lines so no white halo is left; below 0 it keeps clear of them."));
                ui.checkbox(&mut fill.contiguous, tr("Contiguous"));
                let reference = &mut self.tools.fill_reference;
                ui.horizontal(|ui| {
                    ui.label(tr("Refer to"));
                    egui::ComboBox::from_id_salt("fill_reference").selected_text(tr(reference.source.label())).show_ui(ui, |ui| {
                        for source in ReferenceSource::ALL {
                            ui.selectable_value(&mut reference.source, source, tr(source.label()));
                        }
                    });
                });
                if reference.source != ReferenceSource::EditedLayer {
                    ui.checkbox(&mut reference.include_hidden, tr("Include hidden layers"));
                    ui.checkbox(&mut reference.exclude_locked, tr("Ignore locked layers"));
                }
                if reference.source == ReferenceSource::ReferenceLayers {
                    let marked = self.doc.reference_layer_count();
                    ui.small(match marked {
                        0 => "No layer is marked yet: tick Reference in the layer properties (e.g. on the inks).".to_string(),
                        n => format!("Reads {n} reference layer{}.", if n == 1 { "" } else { "s" }),
                    });
                }
                ui.label(tr("Fills the edited layer or mask with the main colour, inside the selection."));
            }
            Tool::Gradient => {
                widgets::group_header(ui, Icon::Gradient, tr("Gradient"));
                egui::ComboBox::from_label(tr("Shape")).selected_text(tr(self.tools.gradient_shape.name())).show_ui(ui, |ui| {
                    for shape in GradientShape::ALL {
                        ui.selectable_value(&mut self.tools.gradient_shape, shape, tr(shape.name()));
                    }
                });
                ui.checkbox(&mut self.tools.gradient_to_transparent, tr("Main colour to transparent"));
                if !self.tools.gradient_to_transparent {
                    ui.label(tr("Main colour to sub colour"));
                }
                ui.label(tr("Drag on the canvas. On a gradient layer this edits the layer; elsewhere it paints."));
            }
            Tool::Select(shape) => {
                widgets::group_header(ui, Tool::Select(shape).icon(), tr("Selection"));
                ui.horizontal_wrapped(|ui| {
                    for op in SelectionOp::ALL {
                        ui.selectable_value(&mut self.tools.selection_op, op, tr(op.name()));
                    }
                });
                ui.label(tr("Shift: add · Alt: subtract · Shift+Alt: intersect. Click to deselect."));
                if matches!(shape, crate::tools::SelectShape::Magnetic | crate::tools::SelectShape::Object) {
                    self.magnetic_panel(ui, shape);
                }
                if shape == crate::tools::SelectShape::Magic {
                    // Auto Select reads the same boundaries as the fill bucket.
                    let magic = &mut self.tools.magic;
                    let mut tolerance = magic.tolerance as f32;
                    if prop_slider(ui, "Tolerance", &mut tolerance, 0.0..=255.0, "", 0, false) {
                        magic.tolerance = tolerance.round() as _;
                    }
                    let mut gap = magic.gap as f32;
                    if prop_slider(ui, "Close gaps", &mut gap, 0.0..=40.0, " px", 0, false) {
                        magic.gap = gap.round() as _;
                    }
                    let mut expand = magic.expand as f32;
                    if prop_slider(ui, "Expand", &mut expand, -10.0..=10.0, " px", 0, false) {
                        magic.expand = expand.round() as _;
                    }
                    ui.checkbox(&mut magic.contiguous, tr("Contiguous"));
                    let reference = &mut self.tools.fill_reference;
                    ui.horizontal(|ui| {
                        ui.label(tr("Refer to"));
                        egui::ComboBox::from_id_salt("magic_reference").selected_text(tr(reference.source.label())).show_ui(ui, |ui| {
                            for source in ReferenceSource::ALL {
                                ui.selectable_value(&mut reference.source, source, tr(source.label()));
                            }
                        });
                    });
                    ui.small(tr("Shared with the fill bucket."));
                }
                ui.horizontal(|ui| {
                    if ui.button(tr("All")).clicked() {
                        self.select_all();
                    }
                    if ui.button(tr("Deselect")).clicked() {
                        self.deselect();
                    }
                    if ui.button(tr("Invert")).clicked() {
                        self.invert_selection();
                    }
                });
            }
            Tool::Transform => {
                widgets::group_header(ui, Icon::Transform, tr("Transform"));
                match &mut self.transform {
                    Some(session) => {
                        for mode in transform_tools::TransformMode::ALL {
                            if ui.selectable_label(session.gizmo.mode == mode, tr(mode.name())).clicked() {
                                session.gizmo.set_mode(mode);
                                session.revision += 1;
                            }
                        }
                        ui.small(tr("Drag the handles on the canvas. Shift keeps proportions and snaps rotation to 15°; Alt scales about the pivot."));
                        ui.small(tr("Enter applies, Esc cancels."));
                    }
                    None => {
                        ui.label(tr("Click the canvas (or press Ctrl+T) to transform the active layer, or the selection on it."));
                    }
                }
            }
            Tool::Scene3D => {
                widgets::group_header(ui, Icon::Transform3d, tr("3D Layer"));
                self.scene_panel(ui);
            }
            Tool::Frame => {
                widgets::group_header(ui, Icon::Grid, tr("Panel Frames"));
                self.frame_tool_panel(ui);
            }
            Tool::Crop => {
                widgets::group_header(ui, Icon::Crop, tr("Crop"));
                ui.label(
                    tr("Drag the box or its handles; drag outside it to draw a new one. The bar under the canvas sets the aspect ratio."),
                );
                ui.small(tr("Pixels outside the new canvas are kept and come back if it grows. Enter applies, Esc cancels."));
            }
            Tool::Eyedropper => {
                widgets::group_header(ui, Icon::ColorPicker, tr("Eyedropper"));
                self.eyedropper_panel(ui);
            }
            Tool::VectorEdit => {
                widgets::group_header(ui, Icon::Curve, tr("Vector Points"));
                self.vector_edit_panel(ui);
            }
            Tool::Shape => {
                widgets::group_header(ui, Icon::Shapes, tr("Shape"));
                self.shape_panel(ui);
            }
            Tool::Text(kind) => {
                widgets::group_header(ui, self.tool.icon(), self.tool.name());
                self.text_panel(ui, kind);
            }
            Tool::Stamp(kind) => {
                widgets::group_header(ui, self.tool.icon(), self.tool.name());
                self.stamp_panel(ui, kind);
                ui.separator();
                self.brush_settings(ui);
            }
            Tool::Hand => {
                widgets::group_header(ui, Icon::Move, tr("Hand"));
                ui.label(tr("Drag to pan. Wheel zooms, Shift + wheel rotates, Ctrl + 0 fits."));
                ui.label(tr("Hold Space for the hand with any tool."));
            }
            Tool::Zoom => {
                widgets::group_header(ui, Icon::Zoom, tr("Zoom"));
                ui.label(tr("Click zooms in, Alt + click zooms out. Drag right to zoom in, left to zoom out."));
                ui.label(tr("With any tool: Ctrl + Space + click zooms in, Alt + Space + click zooms out."));
                ui.horizontal_wrapped(|ui| {
                    if ui.button(tr("Zoom In")).clicked() {
                        self.zoom_view_step(true);
                    }
                    if ui.button(tr("Zoom Out")).clicked() {
                        self.zoom_view_step(false);
                    }
                    if ui.button(tr("Fit to Window")).clicked() {
                        self.fit_pending = true;
                    }
                    if ui.button("100%").clicked() {
                        self.view.set_zoom(1.0);
                    }
                });
                ui.label(format!("{:.1}%", self.view.zoom * 100.0));
            }
            Tool::Rotate => {
                widgets::group_header(ui, Icon::RotateRight, tr("Rotate View"));
                ui.label(tr("Drag round the centre to turn the view."));
                ui.label(tr("With any tool: Shift + Space + drag, or Shift + wheel. - and ^ turn by 5°."));
                let mut degrees = self.view.rotation.to_degrees();
                if ui.add(egui::Slider::new(&mut degrees, -180.0..=180.0).suffix("°")).changed() {
                    self.view.set_rotation(degrees.to_radians());
                }
                ui.horizontal_wrapped(|ui| {
                    if ui.button(tr("Rotate View Left")).clicked() {
                        self.rotate_view_step(-1.0);
                    }
                    if ui.button(tr("Rotate View Right")).clicked() {
                        self.rotate_view_step(1.0);
                    }
                    if ui.button(tr("Reset Rotation")).clicked() {
                        self.view.set_rotation(0.0);
                    }
                });
                ui.checkbox(&mut self.view.flip, tr("Flip Horizontally"));
            }
        }
    }

    fn brush_settings(&mut self, ui: &mut egui::Ui) {
        // Header: the brush's stroke on a light strip, its name on top.
        let total = self.brushes.len();
        let index = self.brush;
        let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 30.0), Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 3.0, Color32::from_gray(205));
        let texture = self.previews.get(ui.ctx(), index, &self.brushes[index].settings, total);
        let size = texture.size_vec2();
        let size = size * (rect.height() / size.y.max(1.0)).min(1.0);
        let shown = egui::Rect::from_min_size(rect.left_top() + Vec2::new(rect.width() * 0.25, (rect.height() - size.y) / 2.0), size);
        painter.image(
            texture.id(),
            shown,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            Color32::from_gray(20),
        );
        let name = self.brushes[index].settings.name.clone();
        let galley = painter.layout_no_wrap(name, egui::FontId::proportional(12.5), Color32::WHITE);
        let tag = egui::Rect::from_min_size(rect.left_top() + Vec2::new(3.0, 3.0), galley.size() + Vec2::new(8.0, 2.0));
        painter.rect_filled(tag, 2.0, Color32::from_gray(70).gamma_multiply(0.9));
        painter.galley(tag.min + Vec2::new(4.0, 1.0), galley, Color32::WHITE);
        ui.add_space(2.0);

        // The wrench opens Sub Tool Detail (Advanced Tool Settings); Reset puts the whole brush back.
        let base = crate::brush_ui::baseline_of(&mut self.brush_baselines, &self.brushes, index).clone();
        let mut reset_all = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            let detail = tr("Sub Tool Detail: every setting by category, with a live preview");
            if icon_button(ui, Icon::Options, 14.0, self.advanced_tool.is_some(), detail).clicked() {
                self.open_advanced_tool_settings();
            }
            if ui.small_button(tr("Advanced Tool Settings...")).on_hover_text(tr("Every setting by category; export for Photoshop or Clip Studio")).clicked() {
                self.open_advanced_tool_settings();
            }
            let edited = crate::brush_ui::preset_differs(&self.brushes[index], &base);
            if ui
                .add_enabled(edited, egui::Button::new(tr("Reset")).small())
                .on_hover_text(tr("Back to this brush's preset settings"))
                .on_disabled_hover_text(tr("The brush is as its preset"))
                .clicked()
            {
                reset_all = true;
            }
        });
        if reset_all {
            self.reset_brush_to_baseline(index, base.clone());
        }
        let missing = self.brushes[index].unsupported_features();
        if !missing.is_empty() {
            ui.colored_label(Color32::from_rgb(240, 190, 90), format!("{}: {}", tr("Imported features not painted yet"), missing.join(", ")));
        }
        egui::CollapsingHeader::new(tr("Tip preview")).id_salt("tip_preview").default_open(false).show(ui, |ui| {
            self.tip_preview(ui);
            ui.add_space(2.0);
            if ui.button(tr("Define Tip from Canvas...")).on_hover_text(tr("Create a new brush tip preset sampled from the active selection or layer")).clicked() {
                self.define_brush_preset_dialog();
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            ui.toggle_value(&mut self.ws.chrome.brush_crosshair, tr("Crosshair")).on_hover_text(tr("Off: only the brush outline shows where you paint"));
            ui.toggle_value(&mut self.tools.smart_shape, tr("Smart shape")).on_hover_text(
                tr("Draw a line, circle, rectangle or triangle and keep the pen still at the end: it becomes the clean shape, drawn with this brush"),
            );
        });
        // Which groups differ from the preset: their headers get a live reset button.
        let differs = |group| group_differs(&self.brushes[index], &base, group);
        let edited = [
            differs(PropertyGroup::General),
            differs(PropertyGroup::Tip),
            differs(PropertyGroup::Correction),
            differs(PropertyGroup::Dynamics),
            differs(PropertyGroup::Curves),
        ];
        let mut reset = None;
        let vector_eraser = &mut self.tools.vector_erase;
        let long = self.doc.width.max(self.doc.height).max(1) as f32;
        let brush = &mut self.brushes[index].settings;
        let b = &base.settings;
        let brush_px = &mut self.brush_px;

        if property_section(ui, "brush_general", "General", true, edited[0], |ui| {
            // Size in pixels, or (as Clip Studio's option) a share of the canvas that follows the document.
            match brush.size_relative {
                Some(mut f) => {
                    let mut pct = f * 100.0;
                    let changed = match b.size_relative {
                        Some(d) => prop_slider_reset(ui, "Brush Size", &mut pct, d * 100.0, 0.01..=25.0, "%", 2, true),
                        None => prop_slider(ui, "Brush Size", &mut pct, 0.01..=25.0, "%", 2, true),
                    };
                    if changed {
                        f = pct / 100.0;
                        brush.size_relative = Some(f);
                        brush.size = brush.size_on(long);
                        *brush_px = Some((index, brush.size, long));
                    }
                }
                None => {
                    prop_slider_reset(ui, "Brush Size", &mut brush.size, b.size, 0.5..=2000.0, "", 1, true);
                }
            }
            ui.horizontal(|ui| {
                let mut relative = brush.size_relative.is_some();
                let hint = tr("The size is a share of the canvas: the brush looks the same on a small or a large document, and after resizing it");
                if ui.checkbox(&mut relative, tr("Size relative to the canvas")).on_hover_text(hint).changed() {
                    brush.set_relative(relative, long);
                    *brush_px = relative.then_some((index, brush.size, long));
                }
                if brush.size_relative.is_some() {
                    ui.label(egui::RichText::new(format!("= {:.1} px", brush.size)).small().color(theme::text_dim()));
                }
            });
            if brush.pressure_size {
                prop_percent_reset(ui, "Minimum size", &mut brush.min_size_ratio, b.min_size_ratio, 0.0..=100.0);
            }
            prop_percent_reset(ui, "Opacity", &mut brush.opacity, b.opacity, 0.0..=100.0);
            // Clip Studio's brush blending mode: how the stroke mixes with the layer's pixels.
            if brush.mode != BrushMode::Erase {
                egui::ComboBox::from_label(tr("Blending")).selected_text(tr(brush.blend.name())).show_ui(ui, |ui| {
                    for blend in painter_core::BrushBlend::ALL {
                        let r = ui.selectable_value(&mut brush.blend, blend, tr(blend.name()));
                        if blend == painter_core::BrushBlend::Behind {
                            r.on_hover_text(tr("Paints only where the layer is still transparent: under the lines, on the same layer"));
                        }
                    }
                });
            }
            prop_percent_reset(ui, "Density", &mut brush.density, b.density, 0.0..=100.0);
            // What pen pressure changes, as two toggles on one row.
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 3.0;
                ui.label(egui::RichText::new(tr("Pressure:")).small());
                ui.toggle_value(&mut brush.pressure_size, tr("Size")).on_hover_text(tr("Size follows pressure"));
                ui.toggle_value(&mut brush.pressure_density, tr("Density")).on_hover_text(tr("Density follows pressure"));
            });
        }) {
            reset = Some(PropertyGroup::General);
        }

        if property_section(ui, "brush_tip", "Tip", true, edited[1], |ui| {
            prop_percent_reset(ui, "Spacing", &mut brush.spacing, b.spacing, 1.0..=200.0);
            prop_percent_reset(ui, "Hardness", &mut brush.hardness, b.hardness, 0.0..=100.0);
            ui.horizontal(|ui| {
                ui.label(tr("Anti-aliasing"));
                ui.add_space((ui.available_width() - 4.0 * 25.0).max(0.0));
                let options: Vec<(AntiAlias, &str)> = AntiAlias::ALL.iter().map(|aa| (*aa, aa.name())).collect();
                segmented(ui, &mut brush.antialias, &options, Vec2::new(24.0, 20.0), |painter, rect, aa, _| {
                    let soft = aa.width();
                    if soft <= 0.0 {
                        // Hard edge: a stair-stepped disc.
                        for (dx, dy, w, h) in [(-2.0, -6.0, 4.0, 12.0), (-4.0, -4.0, 8.0, 8.0), (-6.0, -2.0, 12.0, 4.0)] {
                            let cell = egui::Rect::from_min_size(rect.center() + Vec2::new(dx, dy), Vec2::new(w, h));
                            painter.rect_filled(cell, 0.0, Color32::from_gray(220));
                        }
                    } else {
                        // Softer rims for stronger anti-aliasing.
                        let steps = (soft * 3.0).ceil() as i32;
                        for k in (0..=steps).rev() {
                            let t = k as f32 / steps.max(1) as f32;
                            let color = Color32::from_gray(220).gamma_multiply(1.0 - t * 0.85);
                            painter.circle_filled(rect.center(), 5.0 + t * soft, color);
                        }
                    }
                });
            });
            if brush.mode == BrushMode::Erase {
                egui::ComboBox::from_label(tr("Vector layers")).selected_text(tr(vector_eraser.name())).show_ui(ui, |ui| {
                    for mode in crate::tools::VectorErase::ALL {
                        ui.selectable_value(vector_eraser, mode, tr(mode.name()));
                    }
                });
            }
        }) {
            reset = Some(PropertyGroup::Tip);
        }

        if property_section(ui, "brush_correction", "Correction", true, edited[2], |ui| {
            let mut stabilization = brush.stabilization as f32;
            if prop_slider_reset(ui, "Stabilization", &mut stabilization, b.stabilization as f32, 0.0..=100.0, "", 0, false) {
                brush.stabilization = stabilization.round() as u32;
            }
            egui::ComboBox::from_label(tr("Smoothing")).selected_text(tr(brush.smoothing.name())).show_ui(ui, |ui| {
                for method in SmoothingMethod::ALL {
                    ui.selectable_value(&mut brush.smoothing, method, tr(method.name()));
                }
            });
        }) {
            reset = Some(PropertyGroup::Correction);
        }

        if property_section(ui, "brush_dynamics", "Dynamics", false, edited[3], |ui| {
            dynamics_row(ui, "Size", &mut brush.size_dynamics, false);
            dynamics_row(ui, "Density", &mut brush.density_dynamics, false);
            dynamics_row(ui, "Opacity (per dab)", &mut brush.opacity_dynamics, true);
            dynamics_row(ui, "Tip thickness", &mut brush.thickness_dynamics, true);
            ui.label(tr("Tip rotation"));
            ui.indent("rotation", |ui| {
                ui.checkbox(&mut brush.rotation.follow_direction, tr("Follows stroke direction"));
                ui.checkbox(&mut brush.rotation.follow_tilt, tr("Follows the pen's lean"))
                    .on_hover_text(tr("Turns the tip towards where the pen leans (tablets with tilt, Apple Pencil)"));
                ui.horizontal(|ui| {
                    ui.label(tr("Random flip"));
                    ui.checkbox(&mut brush.rotation.random_flip_x, tr("horizontal"));
                    ui.checkbox(&mut brush.rotation.random_flip_y, tr("vertical"));
                });
                let rotation = &mut brush.rotation;
                for (source, curve) in [
                    ("with pressure", &mut rotation.pressure),
                    ("with speed", &mut rotation.velocity),
                    ("with tilt", &mut rotation.tilt),
                ] {
                    preset_buttons(ui, source, curve, &rotation_presets());
                }
                percent_slider(ui, &mut brush.rotation.jitter, "Jitter", 0.0..=100.0);
            });
            ui.label(tr("Tilt"));
            percent_slider(ui, &mut brush.tilt_side, "Pencil side", 0.0..=100.0);
            ui.small(tr("As the pen leans the tip lies down along it, like a pencil's side: wider, flatter, lighter."));
            ui.small(tr("Tilt comes from WinTab or XInput2 tablets and the Apple Pencil; speed is measured on the canvas."));
        }) {
            reset = Some(PropertyGroup::Dynamics);
        }

        let curve_target = &mut self.curve_target;
        if property_section(ui, "pressure_curve_section", "Pressure curve", false, edited[4], |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(curve_target, CurveTarget::Size, tr("Size"));
                ui.selectable_value(curve_target, CurveTarget::Density, tr("Density"));
            });
            let curve = match curve_target {
                CurveTarget::Size => &mut brush.size_curve,
                CurveTarget::Density => &mut brush.density_curve,
            };
            ui.horizontal(|ui| {
                for (label, gamma) in [("Soft", 0.6), ("Linear", 1.0), ("Firm", 1.7)] {
                    if ui.small_button(label).clicked() {
                        *curve = PressureCurve::gamma(gamma);
                    }
                }
            });
            curve_editor(ui, "pressure_curve", curve);
            ui.small(tr("Drag points · double-click to add · right-click to remove"));
        }) {
            reset = Some(PropertyGroup::Curves);
        }

        if let Some(group) = reset {
            if reset_group(&mut self.brushes[index], &base, group) {
                self.brushes[index].apply_tip();
            }
            if group == PropertyGroup::General {
                // The canvas-relative size, if any, follows the preset again.
                self.brush_px = None;
            }
        }
    }

    pub(crate) fn navigator_panel(&mut self, ui: &mut egui::Ui) {
        self.navigator_thumbnail(ui);
        let mut zoom_pct = self.view.zoom * 100.0;
        if prop_slider(ui, "Zoom", &mut zoom_pct, MIN_ZOOM * 100.0..=MAX_ZOOM * 100.0, "%", 0, true) {
            self.view.set_zoom(zoom_pct / 100.0);
        }
        let mut degrees = self.view.rotation.to_degrees();
        if prop_slider(ui, "Rotation", &mut degrees, -180.0..=180.0, "°", 0, false) {
            self.view.set_rotation(degrees.to_radians());
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            if icon_button(ui, Icon::ZoomFit, 16.0, false, "Fit to window").clicked() {
                self.fit_pending = true;
            }
            if icon_button(ui, Icon::ZoomOriginal, 16.0, false, "Actual pixels (100%)").clicked() {
                self.view.set_zoom(1.0);
            }
            if icon_button(ui, Icon::Reset, 16.0, false, "Reset rotation").clicked() {
                self.view.set_rotation(0.0);
            }
        });
    }

    pub(crate) fn layers_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            if icon_button(ui, Icon::Layer, 16.0, false, "New raster layer").clicked() {
                self.add_raster_layer();
            }
            if icon_button(ui, Icon::Path, 16.0, false, "New vector layer").clicked() {
                self.add_vector_layer();
            }
            if icon_button(ui, Icon::GroupLayer, 16.0, false, "New folder").clicked() {
                self.add_folder();
            }
            if icon_button(ui, Icon::LayerMask, 16.0, false, "Add layer mask (from the selection when there is one)").clicked() {
                self.add_mask();
            }
            if icon_button(ui, Icon::Duplicate, 16.0, false, "Duplicate the selected layers (Ctrl+J)").clicked() {
                self.duplicate_selected_layers();
            }
            // Select all / none.
            let (total, selected) = (self.doc.rows_top_down().len(), self.selected_layers().len());
            let mut all = selected == total && total > 1;
            let hint = if selected > 1 { format!("{selected} layers selected: untick for only the active one") } else { "Select every layer".to_string() };
            if ui.checkbox(&mut all, tr("")).on_hover_text(hint).changed() {
                self.layer_selection = if all { self.doc.rows_top_down().iter().map(|(id, _)| *id).collect() } else { vec![self.doc.active] };
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                if icon_button(ui, Icon::Delete, 16.0, false, "Delete the selected layers").clicked() {
                    self.delete_selected_layers();
                }
                if icon_button(ui, Icon::Down, 16.0, false, "Move the selected layers down").clicked() {
                    self.move_selected_layers(false);
                }
                if icon_button(ui, Icon::Up, 16.0, false, "Move the selected layers up").clicked() {
                    self.move_selected_layers(true);
                }
                // Thumbnail size: small, large, none.
                let sizes = [tr("Thumbnails: off"), tr("Thumbnails: small"), tr("Thumbnails: large")];
                let now = self.ws.chrome.layer_thumbs.min(2) as usize;
                if icon_button(ui, Icon::Template, 16.0, now > 0, sizes[now]).clicked() {
                    self.ws.chrome.layer_thumbs = ((now + 1) % 3) as u8;
                }
                // What the thumbnails frame: the layer's content, or the whole canvas.
                if now > 0 {
                    use ui_workspace::LayerThumbMode as M;
                    let mode = self.ws.chrome.layer_thumb_mode;
                    let (icon, hint, next) = match mode {
                        M::Content => (Icon::ZoomFit, tr("Thumbnails: show layer content"), M::Canvas),
                        M::Canvas => (Icon::ZoomOriginal, tr("Thumbnails: show entire canvas"), M::Content),
                    };
                    if icon_button(ui, icon, 16.0, mode == M::Content, hint).clicked() {
                        self.ws.chrome.layer_thumb_mode = next;
                    }
                }
            });
        });
        // Comic pages: the panels, in stacking order, with their clip and border.
        self.panels_section(ui);

        let mut changed = false;
        let mut select = None;
        // Layers that own rulers, and whether any of them is shown.
        let mut ruler_layers: std::collections::HashMap<u64, bool> = std::collections::HashMap::new();
        for r in &self.doc.rulers.rulers {
            if let Some(owner) = r.layer {
                *ruler_layers.entry(owner).or_insert(false) |= r.visible;
            }
        }
        let mut toggle_rulers = None;
        let active = self.doc.active;
        let rows = self.doc.rows_top_down();
        // Thumbnails, as Clip Studio's (none, small, large).
        let thumb_side = match self.ws.chrome.layer_thumbs {
            0 => None,
            1 => Some(30.0),
            _ => Some(48.0),
        };
        if let Some(side) = thumb_side {
            let ids: Vec<painter_core::LayerId> = rows.iter().map(|(id, _)| *id).collect();
            let ctx = ui.ctx().clone();
            // Pixels for the screen's density, so they stay sharp.
            self.update_layer_thumbs(&ctx, &ids, side * ctx.pixels_per_point());
        }
        let row_h = thumb_side.map_or(24.0, |s: f32| s + 6.0);
        let multi = self.selected_layers();
        let modifiers = ui.input(|i| i.modifiers);
        let mut drop: Option<(painter_core::LayerId, painter_core::layer_ops::LayerDrop)> = None;
        let mut eye_all: Option<bool> = None;
        let mut menu_action: Option<(painter_core::LayerId, &str)> = None;
        // A layer being renamed in place (double-click): its id, the text, whether the field is new.
        let mut renaming = self.layer_rename.take();
        let mut renamed: Option<(painter_core::LayerId, String)> = None;
        // Checkbox clicks: toggle the layer in the selection (Shift: the range).
        let mut checked: Option<(painter_core::LayerId, bool)> = None;
        let mut set_edit_mask: Option<bool> = None;
        let mut toggle_mask_mode: Option<painter_render::MaskViewMode> = None;
        egui::Frame::new().fill(theme::well()).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for (id, depth) in rows {
                let Some(layer) = self.doc.find_mut(id) else {
                    continue;
                };
                let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), row_h), Sense::click_and_drag());
                // Dragged within the list it moves (above, below, or into a folder);
                // onto a document tab or a reference view it's copied there.
                if response.drag_started() {
                    egui::DragAndDrop::set_payload(ui.ctx(), crate::document_manager::LayerDrag { layer: id });
                }
                if response.dragged() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                }
                let is_folder = matches!(layer.kind, LayerKind::Folder(_));
                let zone = |y: f32| {
                    use painter_core::layer_ops::LayerDrop;
                    let f = (y - rect.top()) / rect.height();
                    if is_folder && (0.25..0.75).contains(&f) {
                        LayerDrop::Into(id)
                    } else if f < 0.5 {
                        LayerDrop::Above(id)
                    } else {
                        LayerDrop::Below(id)
                    }
                };
                let dragged_here = egui::DragAndDrop::payload::<crate::document_manager::LayerDrag>(ui.ctx())
                    .filter(|p| p.layer != id && response.contains_pointer());
                if let Some(p) = response.dnd_release_payload::<crate::document_manager::LayerDrag>() {
                    if p.layer != id {
                        if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
                            drop = Some((p.layer, zone(pos.y)));
                        }
                    }
                }
                let selected = multi.contains(&id);
                let fill = if id == active {
                    theme::selected()
                } else if selected {
                    theme::selected().gamma_multiply(0.6)
                } else if response.hovered() {
                    theme::hover()
                } else {
                    theme::well()
                };
                ui.painter().rect_filled(rect, 0.0, fill);
                ui.painter().hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0_f32, theme::line()));
                // Where a dragged layer would land.
                if dragged_here.is_some() {
                    if let Some(pos) = ui.input(|i| i.pointer.hover_pos()) {
                        let accent = Stroke::new(2.0_f32, theme::accent());
                        match zone(pos.y) {
                            painter_core::layer_ops::LayerDrop::Into(_) => {
                                ui.painter().rect_stroke(rect.shrink(1.0), 2.0, accent, egui::StrokeKind::Inside);
                            }
                            painter_core::layer_ops::LayerDrop::Above(_) => {
                                ui.painter().hline(rect.x_range(), rect.top() + 1.0, accent);
                            }
                            painter_core::layer_ops::LayerDrop::Below(_) => {
                                ui.painter().hline(rect.x_range(), rect.bottom() - 1.0, accent);
                            }
                        }
                    }
                }

                // Eye toggle.
                let eye = egui::Rect::from_min_size(rect.left_top(), Vec2::new(26.0, rect.height()));
                let eye_response = ui.interact(eye, response.id.with("eye"), Sense::click());
                let eye_tint = if layer.visible { theme::text() } else { theme::text_dim().gamma_multiply(0.5) };
                Icon::Visible.paint(ui, eye, 14.0, eye_tint);
                if eye_response.on_hover_text(tr("Show / hide (all the selected layers, when this one is selected)")).clicked() {
                    if selected && multi.len() > 1 {
                        eye_all = Some(!layer.visible);
                    } else {
                        layer.visible = !layer.visible;
                        changed = true;
                    }
                }
                ui.painter().vline(eye.right(), rect.y_range(), Stroke::new(1.0_f32, theme::line()));

                // Selection checkbox.
                let check = egui::Rect::from_min_size(egui::pos2(eye.right() + 2.0, rect.top()), Vec2::new(22.0, rect.height()));
                let check_response = ui.interact(check, response.id.with("check"), Sense::click());
                let boxed = egui::Rect::from_center_size(check.center(), Vec2::splat(13.0));
                if selected {
                    ui.painter().rect_filled(boxed, 3.0, theme::accent());
                    let (a, b, c) = (boxed.left_center() + Vec2::new(3.0, 0.0), boxed.center_bottom() + Vec2::new(-1.0, -3.0), boxed.right_top() + Vec2::new(-3.0, 3.0));
                    ui.painter().line_segment([a, b], Stroke::new(1.8_f32, Color32::WHITE));
                    ui.painter().line_segment([b, c], Stroke::new(1.8_f32, Color32::WHITE));
                } else {
                    let ring = if check_response.hovered() { theme::text() } else { theme::text_dim() };
                    ui.painter().rect_stroke(boxed, 3.0, Stroke::new(1.2_f32, ring), egui::StrokeKind::Inside);
                }
                if check_response.on_hover_text(tr("Select or deselect this layer (Shift: the range from the active one)")).clicked() {
                    checked = Some((id, modifiers.shift));
                }

                let mut x = check.right() + 4.0 + depth as f32 * 12.0;
                if layer.clip {
                    ui.painter().text(
                        egui::pos2(x, rect.center().y),
                        Align2::LEFT_CENTER,
                        "↳",
                        egui::FontId::proportional(12.0),
                        theme::text_dim(),
                    );
                    x += 12.0;
                }
                let kind_icon = match &layer.kind {
                    LayerKind::Raster(_) | LayerKind::Fill { .. } => Icon::Layer,
                    LayerKind::Vector(_) => Icon::Path,
                    LayerKind::Folder(_) => Icon::GroupLayer,
                    LayerKind::Gradient(_) => Icon::GradientLinear,
                    LayerKind::Tone(_) => Icon::Transparency,
                    LayerKind::Adjustment(_) => Icon::Curve,
                };
                match (thumb_side, self.layer_thumbs.get(id)) {
                    // The layer's thumbnail (its content, or the canvas in its shape), with a small badge for vector layers.
                    (Some(side), Some(tex)) => {
                        let s = tex.size_vec2();
                        let fit = Vec2::new(s.x, s.y) * (side / s.x.max(s.y));
                        let frame = egui::Rect::from_min_size(egui::pos2(x, rect.top() + 3.0), Vec2::new(side, side));
                        let shown = egui::Rect::from_center_size(frame.center(), fit);
                        ui.painter().image(tex.id(), shown, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                        ui.painter().rect_stroke(shown, 0.0, Stroke::new(1.0_f32, theme::line()), egui::StrokeKind::Outside);
                        if matches!(layer.kind, LayerKind::Vector(_)) {
                            let badge = egui::Rect::from_min_size(shown.right_bottom() - Vec2::splat(12.0), Vec2::splat(12.0));
                            ui.painter().rect_filled(badge, 2.0, theme::panel());
                            Icon::Path.paint(ui, badge, 10.0, theme::text());
                        }
                        if id == active && !self.edit_mask {
                            ui.painter().rect_stroke(shown.expand(1.0), 0.0, Stroke::new(1.8_f32, Color32::WHITE), egui::StrokeKind::Outside);
                        }
                        let hit_content = ui.interact(shown, response.id.with("thumb_content"), Sense::click());
                        if hit_content.on_hover_text(tr("Layer pixels: click to edit")).clicked() {
                            select = Some(id);
                            set_edit_mask = Some(false);
                        }
                        x += side + 4.0;

                        // Dual thumbnail: render mask thumbnail if the layer has a mask
                        if let Some(mask) = &mut layer.mask {
                            let mframe = egui::Rect::from_min_size(egui::pos2(x, rect.top() + 3.0), Vec2::new(side, side));
                            let mshown = if let Some(mtex) = self.layer_thumbs.get_mask(id) {
                                let ms = mtex.size_vec2();
                                let mfit = Vec2::new(ms.x, ms.y) * (side / ms.x.max(ms.y));
                                let mshown = egui::Rect::from_center_size(mframe.center(), mfit);
                                ui.painter().image(mtex.id(), mshown, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                                mshown
                            } else {
                                ui.painter().rect_filled(mframe, 0.0, Color32::WHITE);
                                mframe
                            };
                            ui.painter().rect_stroke(mshown, 0.0, Stroke::new(1.0_f32, theme::line()), egui::StrokeKind::Outside);
                            if !mask.enabled {
                                let red = Stroke::new(2.0_f32, Color32::from_rgb(230, 45, 45));
                                ui.painter().line_segment([mshown.left_top(), mshown.right_bottom()], red);
                                ui.painter().line_segment([mshown.left_bottom(), mshown.right_top()], red);
                            }
                            if self.mask_view_mode != painter_render::MaskViewMode::Off && id == active {
                                let dot = if self.mask_view_mode == painter_render::MaskViewMode::Grayscale { Color32::from_rgb(100, 180, 255) } else { Color32::from_rgb(255, 80, 100) };
                                ui.painter().circle_filled(mshown.right_top() + Vec2::new(-4.0, 4.0), 3.5, dot);
                            }
                            if id == active && self.edit_mask {
                                ui.painter().rect_stroke(mshown.expand(1.0), 0.0, Stroke::new(1.8_f32, Color32::WHITE), egui::StrokeKind::Outside);
                            }
                            let hit_mask = ui.interact(mshown, response.id.with("thumb_mask"), Sense::click());
                            if hit_mask.on_hover_text(tr("Layer mask: click to edit, Alt-click to view mask, Shift-click to disable")).clicked() {
                                select = Some(id);
                                if modifiers.alt {
                                    toggle_mask_mode = Some(painter_render::MaskViewMode::Grayscale);
                                } else if modifiers.shift {
                                    mask.enabled = !mask.enabled;
                                    changed = true;
                                } else {
                                    set_edit_mask = Some(true);
                                }
                            }
                            x += side + 4.0;
                        }
                        x += 2.0;
                    }
                    (Some(side), None) => {
                        kind_icon.paint(ui, egui::Rect::from_min_size(egui::pos2(x, rect.top()), Vec2::new(side, rect.height())), 16.0, theme::text_dim());
                        x += side + 6.0;
                    }
                    (None, _) => {
                        kind_icon.paint(
                            ui,
                            egui::Rect::from_min_size(egui::pos2(x, rect.top()), Vec2::new(18.0, rect.height())),
                            14.0,
                            theme::text_dim(),
                        );
                        x += 22.0;
                    }
                }
                let name_color = if !layer.visible || layer.draft { theme::text_dim() } else { theme::strong() };
                match &mut renaming {
                    // Renaming: a text field where the name was. Enter or clicking away keeps it, Esc doesn't.
                    Some((rid, text, fresh)) if *rid == id => {
                        let field = egui::Rect::from_min_max(egui::pos2(x - 2.0, rect.top() + 1.0), egui::pos2(rect.right() - 6.0, rect.bottom() - 3.0));
                        let edit = ui.put(field, egui::TextEdit::singleline(text).font(egui::FontId::proportional(12.5)).margin(egui::vec2(2.0, 0.0)));
                        if *fresh {
                            edit.request_focus();
                            *fresh = false;
                        }
                        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            renaming = None;
                        } else if edit.lost_focus() {
                            let name = text.trim().to_string();
                            if !name.is_empty() && name != layer.name {
                                renamed = Some((id, name));
                            }
                            renaming = None;
                        }
                    }
                    _ => {
                        ui.painter().text(egui::pos2(x, rect.center().y), Align2::LEFT_CENTER, &layer.name, egui::FontId::proportional(12.5), name_color);
                    }
                }
                // State badges on the right: mask, locks.
                let mut right = rect.right() - 4.0;
                let mut badge = |icon: Icon, on: bool| {
                    let r = egui::Rect::from_min_max(egui::pos2(right - 18.0, rect.top()), egui::pos2(right, rect.bottom()));
                    icon.paint(ui, r, 13.0, if on { theme::text() } else { theme::text_dim() });
                    right -= 20.0;
                };
                if layer.draft {
                    badge(Icon::Pencil, true);
                }
                if layer.locked {
                    badge(Icon::Lock, true);
                }
                if layer.alpha_locked {
                    badge(Icon::LockAlpha, true);
                }
                if let Some(mask) = &mut layer.mask {
                    let r = egui::Rect::from_min_max(egui::pos2(right - 18.0, rect.top()), egui::pos2(right, rect.bottom()));
                    let tint = if mask.enabled {
                        if id == active && self.edit_mask { Color32::from_rgb(120, 200, 255) } else { theme::text() }
                    } else {
                        Color32::from_rgb(230, 45, 45)
                    };
                    Icon::LayerMask.paint(ui, r, 13.0, tint);
                    if !mask.enabled {
                        let red = Stroke::new(1.5_f32, Color32::from_rgb(230, 45, 45));
                        ui.painter().line_segment([r.left_top() + Vec2::new(3.0, 3.0), r.right_bottom() - Vec2::new(3.0, 3.0)], red);
                        ui.painter().line_segment([r.left_bottom() + Vec2::new(3.0, -3.0), r.right_top() + Vec2::new(-3.0, 3.0)], red);
                    }
                    let hit = ui.interact(r, response.id.with("mask_badge"), Sense::click());
                    if hit.on_hover_text(tr("Layer mask: click to edit, Alt-click to view mask, Shift-click to disable")).clicked() {
                        select = Some(id);
                        if modifiers.alt {
                            toggle_mask_mode = Some(painter_render::MaskViewMode::Grayscale);
                        } else if modifiers.shift {
                            mask.enabled = !mask.enabled;
                            changed = true;
                        } else {
                            set_edit_mask = Some(!self.edit_mask);
                        }
                    }
                    right -= 20.0;
                }
                if let Some(&shown) = ruler_layers.get(&id.0) {
                    let r = egui::Rect::from_min_max(egui::pos2(right - 18.0, rect.top()), egui::pos2(right, rect.bottom()));
                    let tint = if shown { Color32::from_rgb(176, 96, 255) } else { theme::text_dim() };
                    Icon::Path.paint(ui, r, 13.0, tint);
                    let hit = ui.interact(r, response.id.with("rulers"), Sense::click());
                    if hit.on_hover_text(tr("This layer has rulers: click to show or hide them")).clicked() {
                        toggle_rulers = Some(id);
                    }
                }
                // Opacity and blend, when not 100 % Normal.
                if layer.opacity < 0.995 || layer.blend != BlendMode::Normal {
                    let mut detail = String::new();
                    if layer.opacity < 0.995 {
                        detail.push_str(&format!("{:.0}%", layer.opacity * 100.0));
                    }
                    if layer.blend != BlendMode::Normal {
                        detail.push_str(&format!(" {}", layer.blend.name()));
                    }
                    ui.painter().text(egui::pos2(right - 2.0, rect.center().y), Align2::RIGHT_CENTER, detail.trim(), egui::FontId::proportional(10.0), theme::text_dim());
                }
                if response.clicked() {
                    select = Some(id);
                }
                if response.double_clicked() {
                    renaming = Some((id, layer.name.clone(), true));
                }
                let draft = layer.draft;
                response.context_menu(|ui| {
                    let mut item = |label: &str, what: &'static str| {
                        if ui.button(label).clicked() {
                            menu_action = Some((id, what));
                            ui.close_menu();
                        }
                    };
                    item("Duplicate (Ctrl+J)", "duplicate");
                    item("Group into Folder (Ctrl+G)", "group");
                    item("Delete", "delete");
                    item(if draft { "Not a Draft" } else { "Set as Draft (left out of exports)" }, "draft");
                    item("Show / Hide", "visible");
                    item("Lock / Unlock", "lock");
                    item("Reference / Not", "reference");
                    item("Layer Colour on / off", "layer_color");
                    if layer.mask.is_some() {
                        item(if self.mask_view_mode == painter_render::MaskViewMode::Grayscale { "Exit Mask View" } else { "View Mask (Grayscale)" }, "mask_view_gray");
                        item(if self.mask_view_mode == painter_render::MaskViewMode::Rubylith { "Exit Rubylith Overlay" } else { "Rubylith Mask Overlay (\\)" }, "mask_view_ruby");
                        item(if layer.mask.as_ref().is_some_and(|m| m.enabled) { "Disable Layer Mask" } else { "Enable Layer Mask" }, "mask_toggle");
                        item("Delete Layer Mask", "delete_mask");
                    } else {
                        item("Add Layer Mask", "add_mask");
                    }
                });
            }
        });
        // Mass and structural changes, once the list is drawn.
        if let Some(visible) = eye_all {
            self.set_on_selected_layers(|l| l.visible = visible);
        }
        if let Some((dragged, to)) = drop {
            self.drop_layers(dragged, to);
        }
        self.layer_rename = renaming;
        if let Some((id, name)) = renamed {
            self.next_label = Some("Rename Layer".into());
            self.checkpoint();
            if let Some(layer) = self.doc.find_mut(id) {
                layer.name = name;
            }
        }
        if let Some((id, what)) = menu_action {
            // The menu acts on the selection when its row is selected, else on that row.
            if !self.selected_layers().contains(&id) {
                self.click_layer(id, egui::Modifiers::NONE);
            }
            let first =
                self.doc.find(id).map(|l| (l.draft, l.visible, l.locked, l.reference, l.layer_color.is_some())).unwrap_or_default();
            match what {
                "duplicate" => self.duplicate_selected_layers(),
                "group" => self.group_selected_layers(),
                "delete" => self.delete_selected_layers(),
                "draft" => self.set_on_selected_layers(|l| l.draft = !first.0),
                "visible" => self.set_on_selected_layers(|l| l.visible = !first.1),
                "lock" => self.set_on_selected_layers(|l| l.locked = !first.2),
                "reference" => self.set_on_selected_layers(|l| l.reference = !first.3),
                "layer_color" => self.set_on_selected_layers(|l| {
                    if l.has_pixels() {
                        l.layer_color = (!first.4).then_some(DEFAULT_LAYER_COLOR);
                    }
                }),
                "mask_view_gray" => self.toggle_mask_view(painter_render::MaskViewMode::Grayscale),
                "mask_view_ruby" => self.toggle_mask_view(painter_render::MaskViewMode::Rubylith),
                "mask_toggle" => self.toggle_mask_enabled(),
                "add_mask" => self.add_mask(),
                "delete_mask" => self.delete_mask(),
                _ => {}
            }
        }
        if let Some((id, range)) = checked {
            let m = if range { egui::Modifiers::SHIFT } else { egui::Modifiers::CTRL };
            self.click_layer(id, m);
        }
        if let Some(id) = toggle_rulers {
            let show = !ruler_layers.get(&id.0).copied().unwrap_or(true);
            for r in self.doc.rulers.rulers.iter_mut().filter(|r| r.layer == Some(id.0)) {
                r.visible = show;
            }
            self.dirty = true;
        } else if let Some(id) = select {
            self.click_layer(id, modifiers);
        }
        if let Some(em) = set_edit_mask {
            self.edit_mask = em;
        }
        if let Some(mode) = toggle_mask_mode {
            self.toggle_mask_view(mode);
        }

        ui.separator();
        // Several layers: opacity and blend mode for all of them.
        let multi = self.selected_layers();
        if multi.len() > 1 {
            ui.label(egui::RichText::new(format!("{} layers selected", multi.len())).strong());
            let (mut opacity, blend) = self.doc.find(self.doc.active).map_or((1.0, BlendMode::Normal), |l| (l.opacity, l.blend));
            let mut new_blend = None;
            egui::ComboBox::from_label(tr("Blend (all)")).selected_text(tr(blend.name())).show_ui(ui, |ui| {
                for mode in BlendMode::ALL {
                    if ui.selectable_label(mode == blend, tr(mode.name())).clicked() {
                        new_blend = Some(mode);
                    }
                }
            });
            if let Some(mode) = new_blend {
                self.set_on_selected_layers(|l| l.blend = mode);
            }
            if prop_percent(ui, "Opacity (all)", &mut opacity, 0.0..=100.0) {
                for id in &multi {
                    if let Some(l) = self.doc.find_mut(*id) {
                        l.opacity = opacity;
                    }
                }
                self.recomposite = true;
                self.dirty = true;
            }
            ui.horizontal_wrapped(|ui| {
                if ui.button(tr("Duplicate")).clicked() {
                    self.duplicate_selected_layers();
                }
                if ui.button(tr("Group")).clicked() {
                    self.group_selected_layers();
                }
                if ui.button(tr("Draft on / off")).clicked() {
                    let on = !self.doc.find(self.doc.active).is_some_and(|l| l.draft);
                    self.set_on_selected_layers(|l| l.draft = on);
                }
                if ui.button(tr("Delete")).clicked() {
                    self.delete_selected_layers();
                }
            });
            ui.separator();
        }
        let mut delete_mask = false;
        if let Some(layer) = self.doc.find_mut(self.doc.active) {
            // Blend mode and opacity on one row (rename: double-click the layer).
            let pass_through = matches!(&layer.kind, LayerKind::Folder(f) if f.pass_through) && layer.mask.is_none();
            ui.add_enabled_ui(!pass_through, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    egui::ComboBox::from_id_salt("layer_blend").width(110.0).selected_text(tr(layer.blend.name())).show_ui(ui, |ui| {
                        for mode in BlendMode::ALL {
                            changed |= ui.selectable_value(&mut layer.blend, mode, tr(mode.name())).changed();
                        }
                    });
                    let mut pct = layer.opacity * 100.0;
                    let r = ui.add(egui::DragValue::new(&mut pct).range(0.0..=100.0).speed(0.5).max_decimals(0).suffix("%"));
                    if r.on_hover_text(tr("Opacity: drag sideways or type")).changed() {
                        layer.opacity = pct / 100.0;
                        changed = true;
                    }
                });
            });
            // Locks and switches as small toggles on one row.
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(2.0, 2.0);
                let toggle = |ui: &mut egui::Ui, on: &mut bool, icon: Icon, hint: &str| -> bool {
                    let r = icon_button(ui, icon, 14.0, *on, hint);
                    if r.clicked() {
                        *on = !*on;
                        return true;
                    }
                    false
                };
                toggle(ui, &mut layer.locked, Icon::Lock, "Lock the layer");
                if layer.has_pixels() && !layer.is_vector() {
                    toggle(ui, &mut layer.alpha_locked, Icon::LockAlpha, "Lock transparency: paint only where there are pixels");
                }
                let small = |ui: &mut egui::Ui, on: &mut bool, label: &str, hint: &str| ui.toggle_value(on, egui::RichText::new(label).small()).on_hover_text(hint).changed();
                changed |= small(ui, &mut layer.clip, "Clip", "Clip to the layer below");
                changed |= small(ui, &mut layer.reference, "Ref", "Reference: the fill bucket can read its boundaries from it");
                changed |= small(ui, &mut layer.draft, "Draft", "Shown while you work, left out of exports and prints (sketches, notes, guides)");
                // Layer colour (Clip Studio): the layer shown in one colour, on screen only.
                if layer.has_pixels() {
                    let mut on = layer.layer_color.is_some();
                    let hint = "Layer colour: the layer's lines in one colour (light pixels stay white) to tell layers apart. \
                                On screen only: saving and exporting keep the real colours";
                    if small(ui, &mut on, "Colour", hint) {
                        layer.layer_color = on.then_some(DEFAULT_LAYER_COLOR);
                        changed = true;
                    }
                    if let Some(color) = &mut layer.layer_color {
                        changed |= ui.color_edit_button_srgb(color).changed();
                    }
                }
            });
            match &mut layer.kind {
                LayerKind::Folder(folder) => {
                    changed |= ui.checkbox(&mut folder.pass_through, tr("Pass-through blending")).changed();
                }
                LayerKind::Fill { color } => {
                    ui.horizontal(|ui| {
                        ui.label(tr("Paper colour"));
                        changed |= ui.color_edit_button_srgb(color).changed();
                    });
                }
                LayerKind::Gradient(g) => {
                    ui.horizontal(|ui| {
                        ui.label(tr("From"));
                        changed |= ui.color_edit_button_srgba_unmultiplied(&mut g.from).changed();
                        ui.label(tr("To"));
                        changed |= ui.color_edit_button_srgba_unmultiplied(&mut g.to).changed();
                    });
                    egui::ComboBox::from_label(tr("Shape")).selected_text(tr(g.shape.name())).show_ui(ui, |ui| {
                        for shape in GradientShape::ALL {
                            changed |= ui.selectable_value(&mut g.shape, shape, tr(shape.name())).changed();
                        }
                    });
                    ui.small(tr("Drag with the Gradient tool to place it."));
                }
                LayerKind::Tone(t) => {
                    ui.horizontal(|ui| {
                        ui.label(tr("Ink"));
                        changed |= ui.color_edit_button_srgb(&mut t.color).changed();
                    });
                    changed |= ui.add(egui::Slider::new(&mut t.cell, 3.0..=40.0).text(tr("Dot pitch")).suffix(" px")).changed();
                    changed |= ui.add(egui::Slider::new(&mut t.angle, 0.0..=90.0).text(tr("Angle")).suffix("°")).changed();
                    changed |= percent_slider(ui, &mut t.density, "Density", 0.0..=100.0);
                }
                LayerKind::Vector(v) => {
                    ui.label(match (v.strokes.len(), v.texts.len()) {
                        (s, 0) => format!("{s} strokes"),
                        (0, t) => format!("{t} texts"),
                        (s, t) => format!("{s} strokes, {t} texts"),
                    });
                }
                LayerKind::Adjustment(a) => {
                    changed |= crate::adjust_ui::adjustment_editor(ui, a);
                }
                LayerKind::Raster(_) => {}
            }

            // The mask (added from the icon row above).
            match &mut layer.mask {
                Some(mask) => {
                    ui.horizontal_wrapped(|ui| {
                        changed |= ui.checkbox(&mut mask.enabled, tr("Mask")).changed();
                        ui.toggle_value(&mut self.edit_mask, tr("Edit mask"))
                            .on_hover_text(tr("Brush hides, eraser reveals, fill/gradient hide"));
                        delete_mask = ui.small_button(tr("Delete")).on_hover_text(tr("Delete the mask")).clicked();
                    });
                }
                None => self.edit_mask = false,
            }
        }
        if delete_mask {
            self.delete_mask();
        }
        if changed {
            self.recomposite = true;
            self.dirty = true;
        }
        self.file_object_panel(ui);
    }
}

/// A collapsible group of Tool Property with a reset button in its header,
/// live while `edited` (the group differs from the preset). Returns whether
/// the reset button was clicked.
fn property_section(ui: &mut egui::Ui, id: &str, title: &'static str, open: bool, edited: bool, body: impl FnOnce(&mut egui::Ui)) -> bool {
    let mut reset = false;
    let id = ui.make_persistent_id(id);
    egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, open)
        .show_header(ui, |ui| {
            ui.label(egui::RichText::new(tr(title)).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                reset = crate::brush_ui::reset_glyph(ui, edited, tr("Back to the preset's values for this group")).clicked();
            });
        })
        .body(body);
    reset
}

/// Slider showing a 0..1 value as a percentage. Returns whether it changed.
pub(crate) fn percent_slider(ui: &mut egui::Ui, value: &mut f32, label: &str, range: RangeInclusive<f32>) -> bool {
    let mut pct = *value * 100.0;
    let changed = ui.add(egui::Slider::new(&mut pct, range).text(label).suffix("%")).changed();
    if changed {
        *value = pct / 100.0;
    }
    changed
}

/// Speed and tilt responses offered for a dynamic, as (label, curve).
fn response_presets() -> [(&'static str, Option<PressureCurve>); 3] {
    [
        ("Off", None),
        ("Less", Some(PressureCurve::from_points(vec![[0.0, 1.0], [1.0, 0.3]]))),
        ("More", Some(PressureCurve::from_points(vec![[0.0, 0.3], [1.0, 1.0]]))),
    ]
}

/// Rotation responses: none, up to a quarter turn, up to a full turn.
pub(crate) fn rotation_presets() -> [(&'static str, Option<PressureCurve>); 3] {
    [("Off", None), ("90°", Some(PressureCurve::from_points(vec![[0.0, 0.0], [1.0, 0.25]]))), ("360°", Some(PressureCurve::linear()))]
}

/// A labelled row of curve presets; marks a curve that matches none (e.g. imported).
pub(crate) fn preset_buttons(ui: &mut egui::Ui, label: &str, curve: &mut Option<PressureCurve>, presets: &[(&str, Option<PressureCurve>)]) {
    ui.horizontal(|ui| {
        ui.label(label);
        for (name, preset) in presets {
            if ui.selectable_label(curve == preset, *name).clicked() {
                *curve = preset.clone();
            }
        }
        if curve.is_some() && !presets.iter().any(|(_, p)| p == curve) {
            ui.small(tr("(imported)"));
        }
    });
}

/// One row of pressure / speed / tilt / jitter controls for a brush parameter.
pub(crate) fn dynamics_row(ui: &mut egui::Ui, label: &str, dynamics: &mut painter_core::brush::InputDynamics, with_pressure: bool) {
    ui.label(label);
    ui.indent(label, |ui| {
        let presets = response_presets();
        if with_pressure {
            preset_buttons(ui, "with pressure", &mut dynamics.pressure, &presets);
        }
        preset_buttons(ui, "with speed", &mut dynamics.velocity, &presets);
        preset_buttons(ui, "with tilt", &mut dynamics.tilt, &presets);
        percent_slider(ui, &mut dynamics.jitter, "Jitter", 0.0..=100.0);
    });
}
