//! Panel contents and window chrome: menu, status bar, tool strip, colour,
//! sub tools, tool properties, navigator, layers and the New Canvas dialog.

use std::ops::RangeInclusive;

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
use crate::widgets::{self, color_chips, color_wheel, prop_percent, prop_slider, segmented};

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
                ui.menu_button("File", |ui| {
                    if ui.add(egui::Button::new("New Canvas...").shortcut_text("Ctrl+N")).clicked() {
                        self.open_new_canvas();
                        ui.close_menu();
                    }
                    if ui
                        .add(egui::Button::new("New from Clipboard").shortcut_text("Ctrl+Alt+N"))
                        .on_hover_text("A document the size of the copied picture or selection")
                        .clicked()
                    {
                        ui.close_menu();
                        self.new_from_clipboard();
                    }
                    if ui.button("New Comic Page / Storyboard...").clicked() {
                        self.comic_wizard = Some(crate::comic_ui::ComicWizard::open());
                        ui.close_menu();
                    }
                    if ui.add(egui::Button::new("Open...").shortcut_text("Ctrl+O")).clicked() {
                        ui.close_menu();
                        self.open_dialog();
                    }
                    // iPadOS: the app's own folder in the Files app.
                    if crate::files::documents_dir().is_some() {
                        ui.menu_button("Documents", |ui| {
                            let docs = crate::files::documents();
                            if docs.is_empty() {
                                ui.weak("Empty. Copy projects, images, models or brushes into Painter's folder in the Files app.");
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
                    if ui.add(egui::Button::new("Save").shortcut_text("Ctrl+S")).clicked() {
                        ui.close_menu();
                        self.save();
                    }
                    if ui.add(egui::Button::new("Save As...").shortcut_text("Ctrl+Shift+S")).clicked() {
                        ui.close_menu();
                        self.save_as();
                    }
                    if ui.add_enabled(self.dirty_count() > 0, egui::Button::new("Save All")).clicked() {
                        ui.close_menu();
                        self.save_all();
                    }
                    ui.separator();
                    if ui.add(egui::Button::new("Close").shortcut_text("Ctrl+W")).clicked() {
                        ui.close_menu();
                        let id = self.docs.active_id();
                        self.request_close_tab(id);
                    }
                    if ui.button("Close All").clicked() {
                        ui.close_menu();
                        self.close_all_documents();
                    }
                    ui.separator();
                    ui.menu_button("Import", |ui| {
                        if ui.button("PNG as Layer...").clicked() {
                            ui.close_menu();
                            self.import_png_layer();
                        }
                        if ui.button("From Scanner or Camera...").clicked() {
                            ui.close_menu();
                            self.scanner = Some(crate::dialogs::ScannerDialog::open());
                        }
                        ui.separator();
                        if ui.button("Brushes...").clicked() {
                            ui.close_menu();
                            self.import_brushes_dialog();
                        }
                    });
                    ui.menu_button("Export", |ui| {
                        if ui.button("PNG...").clicked() {
                            ui.close_menu();
                            self.export_png();
                        }
                        if ui.button("PSD...").clicked() {
                            ui.close_menu();
                            self.export_psd();
                        }
                        if ui.button("Krita (.kra)...").clicked() {
                            ui.close_menu();
                            self.export_layered("kra");
                        }
                        if ui.button("OpenRaster (.ora)...").clicked() {
                            ui.close_menu();
                            self.export_layered("ora");
                        }
                        if ui.button("Layer / Canvas as SVG...").clicked() {
                            ui.close_menu();
                            self.svg_export = Some(crate::dialogs::SvgExportDialog::default());
                        }
                        ui.separator();
                        if ui.button("All Brushes...").clicked() {
                            ui.close_menu();
                            self.export_brushes(None);
                        }
                    });
                });
                ui.menu_button("Edit", |ui| {
                    let undo = egui::Button::new("Undo").shortcut_text("Ctrl+Z");
                    if ui.add_enabled(self.history.can_undo(), undo).clicked() {
                        self.undo();
                        ui.close_menu();
                    }
                    let redo = egui::Button::new("Redo").shortcut_text("Ctrl+Shift+Z");
                    if ui.add_enabled(self.history.can_redo(), redo).clicked() {
                        self.redo();
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Image Size...").on_hover_text("Resize the whole image; Nearest neighbour keeps pixels sharp").clicked() {
                        ui.close_menu();
                        self.image_size = Some(crate::dialogs::ImageSizeDialog::open(self.doc.canvas(), self.last_resize_filter));
                    }
                    ui.separator();
                    let clear = egui::Button::new("Clear Selected").shortcut_text("Del");
                    if ui.add_enabled(self.doc.selection.is_some(), clear).clicked() {
                        self.clear_selected();
                        ui.close_menu();
                    }
                    ui.separator();
                    ui.checkbox(&mut self.tools.paste_match_dpi, "Paste Keeps Print Size")
                        .on_hover_text("Between documents at different resolutions, pasted and copied layers are resampled to the same size on paper");
                });
                ui.menu_button("Select", |ui| {
                    if ui.add(egui::Button::new("All").shortcut_text("Ctrl+A")).clicked() {
                        self.select_all();
                        ui.close_menu();
                    }
                    if ui.add(egui::Button::new("Deselect").shortcut_text("Ctrl+D")).clicked() {
                        self.deselect();
                        ui.close_menu();
                    }
                    if ui.add(egui::Button::new("Invert").shortcut_text("Ctrl+Shift+I")).clicked() {
                        self.invert_selection();
                        ui.close_menu();
                    }
                });
                ui.menu_button("Layer", |ui| {
                    for (label, action) in [
                        ("New Raster Layer", PainterApp::add_raster_layer as fn(&mut PainterApp)),
                        ("New Vector Layer", PainterApp::add_vector_layer),
                        ("New Folder", PainterApp::add_folder),
                        ("New Gradient Layer", PainterApp::add_gradient_layer),
                        ("New 3D Layer from Model...", PainterApp::new_scene_layer_dialog),
                        ("New Tone Layer (from selection)", PainterApp::add_tone_layer),
                    ] {
                        if ui.button(label).clicked() {
                            action(self);
                            ui.close_menu();
                        }
                    }
                    ui.menu_button("New Correction Layer", |ui| {
                        for adjustment in painter_core::Adjustment::defaults() {
                            if ui.button(adjustment.name()).clicked() {
                                self.add_adjustment_layer(adjustment);
                                ui.close_menu();
                            }
                        }
                    });
                    ui.separator();
                    let has_mask = self.doc.active_layer().is_some_and(|l| l.mask.is_some());
                    if ui.add_enabled(!has_mask, egui::Button::new("Add Layer Mask")).clicked() {
                        self.add_mask();
                        ui.close_menu();
                    }
                    if ui.add_enabled(has_mask, egui::Button::new("Delete Layer Mask")).clicked() {
                        self.delete_mask();
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Delete Layer").clicked() {
                        self.delete_active_layer();
                        ui.close_menu();
                    }
                    ui.separator();
                    let others: Vec<(usize, String)> =
                        (0..self.docs.slots.len()).filter(|&i| i != self.docs.active).map(|i| (i, self.tab_title(i))).collect();
                    ui.add_enabled_ui(!others.is_empty(), |ui| {
                        ui.menu_button("Copy Layer to Document", |ui| {
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
                    .on_hover_text("Or drag the layer from the Layers panel onto a document tab");
                });
                ui.menu_button("Filter", |ui| {
                    for filter in crate::adjust_ui::filter_presets(self.doc.canvas()) {
                        if ui.button(format!("{}...", filter.name())).clicked() {
                            ui.close_menu();
                            self.open_filter(filter);
                        }
                    }
                    ui.separator();
                    ui.small("On the edited layer, inside the selection");
                });
                ui.menu_button("View", |ui| {
                    if ui.add(egui::Button::new("Fit to Window").shortcut_text("0")).clicked() {
                        self.fit_pending = true;
                        ui.close_menu();
                    }
                    if ui.button("Actual Pixels (100%)").clicked() {
                        self.view.set_zoom(1.0);
                        ui.close_menu();
                    }
                    if ui.button("Reset Rotation").clicked() {
                        self.view.set_rotation(0.0);
                        ui.close_menu();
                    }
                    if ui.checkbox(&mut self.view.flip, "Flip Horizontally").clicked() {
                        ui.close_menu();
                    }
                    ui.checkbox(&mut self.tools.pencil_only, "Draw With the Pen Only")
                        .on_hover_text("Fingers move the view instead of painting (iPad: Apple Pencil draws, fingers navigate)");
                    ui.checkbox(&mut self.modifier_keys.shown, "On-Screen Modifier Keys")
                        .on_hover_text("Ctrl, Shift and Alt buttons for touch screens: tap to hold a key, tap again to release it");
                    ui.separator();
                    let mut grid = self.doc.rulers.grid.visible;
                    if ui.add(egui::Checkbox::new(&mut grid, "Grid")).on_hover_text("Ctrl+'").clicked() {
                        self.doc.rulers.grid.visible = grid;
                    }
                    let mut snap = self.doc.rulers.snapping;
                    if ui.add(egui::Checkbox::new(&mut snap, "Snap to Rulers")).on_hover_text("Ctrl+1").clicked() {
                        self.toggle_ruler_snapping();
                    }
                });
                ui.menu_button("Window", |ui| {
                    for editor in ui_workspace::EditorType::ALL.into_iter().filter(|e| *e != ui_workspace::EditorType::Canvas) {
                        let mut open = self.editor_open(editor);
                        if ui.checkbox(&mut open, editor.title()).clicked() {
                            self.toggle_editor(editor, ui.ctx());
                        }
                    }
                    ui.separator();
                    if ui.button("Reset Workspace Layout").clicked() {
                        self.ws.set.current_mut().reset();
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.add(egui::Button::new("Command Palette...").shortcut_text("F3")).clicked() {
                        ui.close_menu();
                        self.open_palette();
                    }
                    if ui.add(egui::Button::new("Zen Mode").shortcut_text("Tab / F11")).clicked() {
                        ui.close_menu();
                        self.toggle_zen();
                    }
                    if ui.button("Theme Editor...").clicked() {
                        ui.close_menu();
                        self.ws.theme_editor = true;
                    }
                    ui.menu_button("Interface Size", |ui| {
                        ui.set_min_width(220.0);
                        self.ui_scale_controls(ui);
                    });
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
                    ui.small("Saved in the project and written into PNG and PSD exports.\nPixels aren't resampled.");
                })
                .response
                .on_hover_text(format!("{w_mm:.1} × {h_mm:.1} mm at {} dpi", self.doc.dpi));
                ui.separator();
                ui.label(format!("{:.0}%", self.view.zoom * 100.0));
                ui.separator();
                ui.label(format!("{:.0}°", self.view.rotation.to_degrees()));
                ui.separator();
                let tiles = self.doc.tile_count();
                ui.label(format!("{tiles} tiles · {:.1} MB", (tiles * TILE_BYTES) as f64 / 1_048_576.0))
                    .on_hover_text("Painted layer tiles held in memory");
                if let Some((slots, bytes)) = self.gpu_stats {
                    ui.separator();
                    ui.label(format!("GPU atlas {slots} tiles / {:.0} MB", bytes as f64 / 1_048_576.0))
                        .on_hover_text("Shared GPU tile atlas: grows with painted area, not layer count");
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

    /// Icon buttons under the menu: file, history and view commands.
    pub(crate) fn command_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("commands")
            .frame(egui::Frame::new().fill(theme::title()).inner_margin(egui::Margin::symmetric(6, 2)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    if icon_button(ui, Icon::New, 16.0, false, "New canvas (Ctrl+N)").clicked() {
                        self.open_new_canvas();
                    }
                    if icon_button(ui, Icon::Template, 16.0, false, "New comic page / storyboard").clicked() {
                        self.comic_wizard = Some(crate::comic_ui::ComicWizard::open());
                    }
                    if icon_button(ui, Icon::Open, 16.0, false, "Open (Ctrl+O)").clicked() {
                        self.open_dialog();
                    }
                    if icon_button(ui, Icon::Save, 16.0, false, "Save (Ctrl+S)").clicked() {
                        self.save();
                    }
                    ui.separator();
                    ui.add_enabled_ui(self.history.can_undo(), |ui| {
                        if icon_button(ui, Icon::Undo, 16.0, false, "Undo (Ctrl+Z)").clicked() {
                            self.undo();
                        }
                    });
                    ui.add_enabled_ui(self.history.can_redo(), |ui| {
                        if icon_button(ui, Icon::Redo, 16.0, false, "Redo (Ctrl+Shift+Z)").clicked() {
                            self.redo();
                        }
                    });
                    ui.separator();
                    if icon_button(ui, Icon::ZoomFit, 16.0, false, "Fit to window (0)").clicked() {
                        self.fit_pending = true;
                    }
                    if icon_button(ui, Icon::ZoomOriginal, 16.0, false, "Actual pixels (100%)").clicked() {
                        self.view.set_zoom(1.0);
                    }
                    if icon_button(ui, Icon::RotateLeft, 16.0, false, "Rotate view left (,)").clicked() {
                        self.view.set_rotation(self.view.rotation - 15f32.to_radians());
                    }
                    if icon_button(ui, Icon::RotateRight, 16.0, false, "Rotate view right (.)").clicked() {
                        self.view.set_rotation(self.view.rotation + 15f32.to_radians());
                    }
                    if icon_button(ui, Icon::Reset, 16.0, false, "Reset rotation").clicked() {
                        self.view.set_rotation(0.0);
                    }
                    ui.separator();
                    if icon_button(ui, Icon::Brushes, 16.0, false, "Import brushes (.sut, .abr, .rustbrush)").clicked() {
                        self.import_brushes_dialog();
                    }
                });
            });
    }

    pub(crate) fn color_panel(&mut self, ui: &mut egui::Ui) {
        let size = ui.available_width().clamp(120.0, 260.0);
        ui.vertical_centered(|ui| {
            color_wheel(ui, &mut self.color, size);
        });
        ui.horizontal(|ui| {
            color_chips(ui, &mut self.color, &mut self.sub_color, 26.0);
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
                if ui.color_edit_button_srgba(&mut main).on_hover_text("Edit numerically").changed() {
                    self.color = Hsva::from(main);
                }
                let [r, g, b, _] = self.color.to_srgba_unmultiplied();
                ui.label(egui::RichText::new(format!("#{r:02X}{g:02X}{b:02X}")).monospace().color(theme::text_dim()));
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
        let footer = 30.0;
        let list_height = (ui.available_height() - footer).max(60.0);
        let indices = self.brushes_in(group);
        let mut picked = None;
        egui::Frame::new().fill(theme::well()).show(ui, |ui| {
            ui.set_min_height(list_height);
            egui::ScrollArea::vertical().max_height(list_height).auto_shrink([false, false]).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                if indices.is_empty() {
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new("No brushes in this group yet. Import some with the button below.").color(theme::text_dim()),
                    );
                }
                let total = self.brushes.len();
                for &i in &indices {
                    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 38.0), Sense::click());
                    let selected = i == self.brush && self.tool == Tool::Brush;
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
                    let response = match preset.unsupported_features() {
                        missing if missing.is_empty() => response,
                        missing => response.on_hover_text(format!("Not rendered yet: {}", missing.join(", "))),
                    };
                    if response.clicked() {
                        picked = Some(i);
                    }
                }
            });
        });
        if let Some(i) = picked {
            self.select_brush(i);
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
            import.context_menu(|ui| {
                if ui.button("Add Back Missing Default Brushes").clicked() {
                    self.restore_default_brushes();
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
                    dismiss = ui.small_button("Dismiss").clicked();
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
                widgets::group_header(ui, Icon::Path, "Rulers and Guides");
                self.ruler_panel(ui);
            }
            Tool::Fill => {
                widgets::group_header(ui, Icon::Bucket, "Fill");
                let fill = &mut self.tools.fill;
                ui.horizontal(|ui| {
                    ui.label("Close gaps");
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
                ui.small("Expand slips the colour under the lines so no white halo is left; below 0 it keeps clear of them.");
                ui.checkbox(&mut fill.contiguous, "Contiguous");
                let reference = &mut self.tools.fill_reference;
                ui.horizontal(|ui| {
                    ui.label("Refer to");
                    egui::ComboBox::from_id_salt("fill_reference").selected_text(reference.source.label()).show_ui(ui, |ui| {
                        for source in ReferenceSource::ALL {
                            ui.selectable_value(&mut reference.source, source, source.label());
                        }
                    });
                });
                if reference.source != ReferenceSource::EditedLayer {
                    ui.checkbox(&mut reference.include_hidden, "Include hidden layers");
                    ui.checkbox(&mut reference.exclude_locked, "Ignore locked layers");
                }
                if reference.source == ReferenceSource::ReferenceLayers {
                    let marked = self.doc.reference_layer_count();
                    ui.small(match marked {
                        0 => "No layer is marked yet: tick Reference in the layer properties (e.g. on the inks).".to_string(),
                        n => format!("Reads {n} reference layer{}.", if n == 1 { "" } else { "s" }),
                    });
                }
                ui.label("Fills the edited layer or mask with the main colour, inside the selection.");
            }
            Tool::Gradient => {
                widgets::group_header(ui, Icon::Gradient, "Gradient");
                egui::ComboBox::from_label("Shape").selected_text(self.tools.gradient_shape.name()).show_ui(ui, |ui| {
                    for shape in GradientShape::ALL {
                        ui.selectable_value(&mut self.tools.gradient_shape, shape, shape.name());
                    }
                });
                ui.checkbox(&mut self.tools.gradient_to_transparent, "Main colour to transparent");
                if !self.tools.gradient_to_transparent {
                    ui.label("Main colour to sub colour");
                }
                ui.label("Drag on the canvas. On a gradient layer this edits the layer; elsewhere it paints.");
            }
            Tool::Select(shape) => {
                widgets::group_header(ui, Tool::Select(shape).icon(), "Selection");
                ui.horizontal_wrapped(|ui| {
                    for op in SelectionOp::ALL {
                        ui.selectable_value(&mut self.tools.selection_op, op, op.name());
                    }
                });
                ui.label("Shift: add · Alt: subtract · Shift+Alt: intersect. Click to deselect.");
                ui.horizontal(|ui| {
                    if ui.button("All").clicked() {
                        self.select_all();
                    }
                    if ui.button("Deselect").clicked() {
                        self.deselect();
                    }
                    if ui.button("Invert").clicked() {
                        self.invert_selection();
                    }
                });
            }
            Tool::Transform => {
                widgets::group_header(ui, Icon::Transform, "Transform");
                match &mut self.transform {
                    Some(session) => {
                        for mode in transform_tools::TransformMode::ALL {
                            if ui.selectable_label(session.gizmo.mode == mode, mode.name()).clicked() {
                                session.gizmo.set_mode(mode);
                                session.revision += 1;
                            }
                        }
                        ui.small("Drag the handles on the canvas. Shift keeps proportions and snaps rotation to 15°; Alt scales about the pivot.");
                        ui.small("Enter applies, Esc cancels.");
                    }
                    None => {
                        ui.label("Click the canvas (or press Ctrl+T) to transform the active layer, or the selection on it.");
                    }
                }
            }
            Tool::Scene3D => {
                widgets::group_header(ui, Icon::Transform3d, "3D Layer");
                self.scene_panel(ui);
            }
            Tool::Frame => {
                widgets::group_header(ui, Icon::Grid, "Panel Frames");
                self.frame_tool_panel(ui);
            }
            Tool::Crop => {
                widgets::group_header(ui, Icon::Crop, "Crop");
                ui.label(
                    "Drag the box or its handles; drag outside it to draw a new one. The bar under the canvas sets the aspect ratio.",
                );
                ui.small("Pixels outside the new canvas are kept and come back if it grows. Enter applies, Esc cancels.");
            }
            Tool::Eyedropper => {
                widgets::group_header(ui, Icon::ColorPicker, "Eyedropper");
                self.eyedropper_panel(ui);
            }
            Tool::Hand => {
                widgets::group_header(ui, Icon::Move, "Hand");
                ui.label("Drag to pan. Wheel zooms, , and . rotate, 0 fits.");
            }
        }
    }

    fn brush_settings(&mut self, ui: &mut egui::Ui) {
        // Header: the brush's stroke on a light strip, its name on top.
        let total = self.brushes.len();
        let index = self.brush;
        let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 40.0), Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 3.0, Color32::from_gray(205));
        let texture = self.previews.get(ui.ctx(), index, &self.brushes[index].settings, total);
        let size = texture.size_vec2();
        let shown = egui::Rect::from_min_size(rect.left_top() + Vec2::new(rect.width() * 0.25, 3.0), size);
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
        ui.add_space(4.0);

        ui.horizontal(|ui| {
            if ui.button("Advanced Tool Settings...").on_hover_text("Every setting by category; export for Photoshop or Clip Studio").clicked() {
                self.open_advanced_tool_settings();
            }
        });
        let missing = self.brushes[self.brush].unsupported_features();
        if !missing.is_empty() {
            ui.colored_label(Color32::from_rgb(240, 190, 90), format!("Imported features not painted yet: {}", missing.join(", ")));
        }
        let vector_eraser = &mut self.tools.vector_eraser_whole_strokes;
        let brush = &mut self.brushes[self.brush].settings;
        prop_slider(ui, "Brush Size", &mut brush.size, 0.5..=2000.0, "", 1, true);
        ui.checkbox(&mut brush.pressure_size, "Size follows pressure");
        if brush.pressure_size {
            prop_percent(ui, "Minimum size", &mut brush.min_size_ratio, 0.0..=100.0);
        }
        prop_percent(ui, "Opacity", &mut brush.opacity, 0.0..=100.0);
        prop_percent(ui, "Density", &mut brush.density, 0.0..=100.0);
        ui.checkbox(&mut brush.pressure_density, "Density follows pressure");
        prop_percent(ui, "Spacing", &mut brush.spacing, 1.0..=200.0);
        prop_percent(ui, "Hardness", &mut brush.hardness, 0.0..=100.0);

        ui.horizontal(|ui| {
            ui.label("Anti-aliasing");
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
            ui.checkbox(vector_eraser, "Vector layers: erase whole strokes");
        }

        ui.add_space(2.0);
        let mut stabilization = brush.stabilization as f32;
        if prop_slider(ui, "Stabilization", &mut stabilization, 0.0..=100.0, "", 0, false) {
            brush.stabilization = stabilization.round() as u32;
        }
        egui::ComboBox::from_label("Smoothing").selected_text(brush.smoothing.name()).show_ui(ui, |ui| {
            for method in SmoothingMethod::ALL {
                ui.selectable_value(&mut brush.smoothing, method, method.name());
            }
        });

        egui::CollapsingHeader::new("Dynamics").id_salt("brush_dynamics").show(ui, |ui| {
            dynamics_row(ui, "Size", &mut brush.size_dynamics, false);
            dynamics_row(ui, "Density", &mut brush.density_dynamics, false);
            dynamics_row(ui, "Opacity (per dab)", &mut brush.opacity_dynamics, true);
            dynamics_row(ui, "Tip thickness", &mut brush.thickness_dynamics, true);
            ui.label("Tip rotation");
            ui.indent("rotation", |ui| {
                ui.checkbox(&mut brush.rotation.follow_direction, "Follows stroke direction");
                ui.horizontal(|ui| {
                    ui.label("Random flip");
                    ui.checkbox(&mut brush.rotation.random_flip_x, "horizontal");
                    ui.checkbox(&mut brush.rotation.random_flip_y, "vertical");
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
            ui.small("Tilt needs a WinTab tablet; speed is measured on the canvas.");
        });

        ui.separator();
        ui.horizontal(|ui| {
            ui.label("Pressure curve:");
            ui.selectable_value(&mut self.curve_target, CurveTarget::Size, "Size");
            ui.selectable_value(&mut self.curve_target, CurveTarget::Density, "Density");
        });
        let curve = match self.curve_target {
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
        ui.small("Drag points · double-click to add · right-click to remove");
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
        let multi = self.selected_layers();
        let modifiers = ui.input(|i| i.modifiers);
        let mut drop: Option<(painter_core::LayerId, painter_core::layer_ops::LayerDrop)> = None;
        let mut eye_all: Option<bool> = None;
        let mut menu_action: Option<(painter_core::LayerId, &str)> = None;
        egui::Frame::new().fill(theme::well()).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for (id, depth) in rows {
                let Some(layer) = self.doc.find_mut(id) else {
                    continue;
                };
                let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 30.0), Sense::click_and_drag());
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
                if eye_response.on_hover_text("Show / hide (all the selected layers, when this one is selected)").clicked() {
                    if selected && multi.len() > 1 {
                        eye_all = Some(!layer.visible);
                    } else {
                        layer.visible = !layer.visible;
                        changed = true;
                    }
                }
                ui.painter().vline(eye.right(), rect.y_range(), Stroke::new(1.0_f32, theme::line()));

                let mut x = eye.right() + 6.0 + depth as f32 * 12.0;
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
                kind_icon.paint(
                    ui,
                    egui::Rect::from_min_size(egui::pos2(x, rect.top()), Vec2::new(18.0, rect.height())),
                    14.0,
                    theme::text_dim(),
                );
                x += 22.0;
                let name_color = if !layer.visible || layer.draft { theme::text_dim() } else { theme::strong() };
                ui.painter().text(
                    egui::pos2(x, rect.top() + 9.0),
                    Align2::LEFT_CENTER,
                    &layer.name,
                    egui::FontId::proportional(12.5),
                    name_color,
                );
                let detail = format!("{:.0}% {}", layer.opacity * 100.0, layer.blend.name());
                ui.painter().text(
                    egui::pos2(x, rect.bottom() - 8.0),
                    Align2::LEFT_CENTER,
                    detail,
                    egui::FontId::proportional(10.0),
                    theme::text_dim(),
                );

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
                if let Some(mask) = &layer.mask {
                    badge(Icon::LayerMask, mask.enabled);
                }
                if let Some(&shown) = ruler_layers.get(&id.0) {
                    let r = egui::Rect::from_min_max(egui::pos2(right - 18.0, rect.top()), egui::pos2(right, rect.bottom()));
                    let tint = if shown { Color32::from_rgb(176, 96, 255) } else { theme::text_dim() };
                    Icon::Path.paint(ui, r, 13.0, tint);
                    let hit = ui.interact(r, response.id.with("rulers"), Sense::click());
                    if hit.on_hover_text("This layer has rulers: click to show or hide them").clicked() {
                        toggle_rulers = Some(id);
                    }
                }
                if response.clicked() {
                    select = Some(id);
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
        if let Some((id, what)) = menu_action {
            // The menu acts on the selection when its row is selected, else on that row.
            if !self.selected_layers().contains(&id) {
                self.click_layer(id, egui::Modifiers::NONE);
            }
            let first = self.doc.find(id).map(|l| (l.draft, l.visible, l.locked, l.reference)).unwrap_or_default();
            match what {
                "duplicate" => self.duplicate_selected_layers(),
                "group" => self.group_selected_layers(),
                "delete" => self.delete_selected_layers(),
                "draft" => self.set_on_selected_layers(|l| l.draft = !first.0),
                "visible" => self.set_on_selected_layers(|l| l.visible = !first.1),
                "lock" => self.set_on_selected_layers(|l| l.locked = !first.2),
                "reference" => self.set_on_selected_layers(|l| l.reference = !first.3),
                _ => {}
            }
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

        ui.separator();
        // Several layers: opacity and blend mode for all of them.
        let multi = self.selected_layers();
        if multi.len() > 1 {
            ui.label(egui::RichText::new(format!("{} layers selected", multi.len())).strong());
            let (mut opacity, blend) = self.doc.find(self.doc.active).map_or((1.0, BlendMode::Normal), |l| (l.opacity, l.blend));
            let mut new_blend = None;
            egui::ComboBox::from_label("Blend (all)").selected_text(blend.name()).show_ui(ui, |ui| {
                for mode in BlendMode::ALL {
                    if ui.selectable_label(mode == blend, mode.name()).clicked() {
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
                if ui.button("Duplicate").clicked() {
                    self.duplicate_selected_layers();
                }
                if ui.button("Group").clicked() {
                    self.group_selected_layers();
                }
                if ui.button("Draft on / off").clicked() {
                    let on = !self.doc.find(self.doc.active).is_some_and(|l| l.draft);
                    self.set_on_selected_layers(|l| l.draft = on);
                }
                if ui.button("Delete").clicked() {
                    self.delete_selected_layers();
                }
            });
            ui.separator();
        }
        let mut add_mask = false;
        let mut delete_mask = false;
        if let Some(layer) = self.doc.find_mut(self.doc.active) {
            ui.horizontal(|ui| {
                ui.label("Name");
                ui.text_edit_singleline(&mut layer.name);
            });
            let pass_through = matches!(&layer.kind, LayerKind::Folder(f) if f.pass_through) && layer.mask.is_none();
            ui.add_enabled_ui(!pass_through, |ui| {
                egui::ComboBox::from_label("Blend").selected_text(layer.blend.name()).show_ui(ui, |ui| {
                    for mode in BlendMode::ALL {
                        changed |= ui.selectable_value(&mut layer.blend, mode, mode.name()).changed();
                    }
                });
                changed |= prop_percent(ui, "Opacity", &mut layer.opacity, 0.0..=100.0);
            });
            ui.horizontal_wrapped(|ui| {
                ui.checkbox(&mut layer.locked, "Lock");
                changed |= ui.checkbox(&mut layer.clip, "Clip to layer below").changed();
                changed |= ui.checkbox(&mut layer.reference, "Reference").on_hover_text("The fill bucket can read its boundaries from reference layers").changed();
                changed |= ui
                    .checkbox(&mut layer.draft, "Draft")
                    .on_hover_text("Shown while you work, left out of exports and prints (sketches, notes, guides)")
                    .changed();
                if layer.has_pixels() && !layer.is_vector() {
                    ui.checkbox(&mut layer.alpha_locked, "Lock transparency");
                }
            });
            match &mut layer.kind {
                LayerKind::Folder(folder) => {
                    changed |= ui.checkbox(&mut folder.pass_through, "Pass-through blending").changed();
                }
                LayerKind::Fill { color } => {
                    ui.horizontal(|ui| {
                        ui.label("Paper colour");
                        changed |= ui.color_edit_button_srgb(color).changed();
                    });
                }
                LayerKind::Gradient(g) => {
                    ui.horizontal(|ui| {
                        ui.label("From");
                        changed |= ui.color_edit_button_srgba_unmultiplied(&mut g.from).changed();
                        ui.label("To");
                        changed |= ui.color_edit_button_srgba_unmultiplied(&mut g.to).changed();
                    });
                    egui::ComboBox::from_label("Shape").selected_text(g.shape.name()).show_ui(ui, |ui| {
                        for shape in GradientShape::ALL {
                            changed |= ui.selectable_value(&mut g.shape, shape, shape.name()).changed();
                        }
                    });
                    ui.small("Drag with the Gradient tool to place it.");
                }
                LayerKind::Tone(t) => {
                    ui.horizontal(|ui| {
                        ui.label("Ink");
                        changed |= ui.color_edit_button_srgb(&mut t.color).changed();
                    });
                    changed |= ui.add(egui::Slider::new(&mut t.cell, 3.0..=40.0).text("Dot pitch").suffix(" px")).changed();
                    changed |= ui.add(egui::Slider::new(&mut t.angle, 0.0..=90.0).text("Angle").suffix("°")).changed();
                    changed |= percent_slider(ui, &mut t.density, "Density", 0.0..=100.0);
                }
                LayerKind::Vector(v) => {
                    ui.label(format!("{} strokes", v.strokes.len()));
                }
                LayerKind::Adjustment(a) => {
                    changed |= crate::adjust_ui::adjustment_editor(ui, a);
                }
                LayerKind::Raster(_) => {}
            }

            ui.separator();
            match &mut layer.mask {
                Some(mask) => {
                    ui.horizontal_wrapped(|ui| {
                        changed |= ui.checkbox(&mut mask.enabled, "Mask enabled").changed();
                        ui.toggle_value(&mut self.edit_mask, "Edit mask")
                            .on_hover_text("Brush hides, eraser reveals, fill/gradient hide");
                        delete_mask = ui.button("Delete mask").clicked();
                    });
                }
                None => {
                    self.edit_mask = false;
                    add_mask = ui.button("Add mask").on_hover_text("From the selection when there is one").clicked();
                }
            }
        }
        if add_mask {
            self.add_mask();
        }
        if delete_mask {
            self.delete_mask();
        }
        if changed {
            self.recomposite = true;
            self.dirty = true;
        }
    }
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
            ui.small("(imported)");
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
