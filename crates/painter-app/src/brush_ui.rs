//! Helpers for the brush panels: the Sub Tool name filter, the small badges
//! that tell a textured or dual-tip brush at a glance, the reset glyph next to
//! a property, and the preset values a reset goes back to.

use std::collections::HashMap;

use brush_io::{BrushPreset, PresetSource};
use eframe::egui::{self, Color32, Response, Sense, Stroke, Ui, Vec2};
use painter_core::TipShape;

use crate::theme;

/// A feature of a brush worth showing on its row in the Sub Tool list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BrushBadge {
    /// A sampled (image) tip instead of the round one.
    Tip,
    /// A flattened tip (thickness below 100%), e.g. a calligraphy pen.
    Flat,
    /// A paper texture the ink follows.
    Texture,
    /// A second tip masking every dab (dual brush / 2-brush shape).
    Dual,
    /// Dabs sprayed around the stroke.
    Spray,
    /// Imported from Clip Studio.
    ClipStudio,
    /// Imported from Photoshop.
    Photoshop,
    /// Imported from Krita.
    Krita,
    /// Imported from MyPaint or GIMP.
    OtherApp,
}

impl BrushBadge {
    /// The badge's short text, drawn on the row.
    pub(crate) fn label(self) -> &'static str {
        match self {
            BrushBadge::Tip => "TIP",
            BrushBadge::Flat => "FLAT",
            BrushBadge::Texture => "TEX",
            BrushBadge::Dual => "2×",
            BrushBadge::Spray => "SPR",
            BrushBadge::ClipStudio => "CSP",
            BrushBadge::Photoshop => "ABR",
            BrushBadge::Krita => "KRA",
            BrushBadge::OtherApp => "EXT",
        }
    }

    /// What the badge means, for the row's tooltip.
    pub(crate) fn hint(self) -> &'static str {
        match self {
            BrushBadge::Tip => "Image tip",
            BrushBadge::Flat => "Flattened tip",
            BrushBadge::Texture => "Paper texture",
            BrushBadge::Dual => "Second tip (dual brush)",
            BrushBadge::Spray => "Spraying effect",
            BrushBadge::ClipStudio => "Imported from Clip Studio",
            BrushBadge::Photoshop => "Imported from Photoshop",
            BrushBadge::Krita => "Imported from Krita",
            BrushBadge::OtherApp => "Imported from another program",
        }
    }

    /// The badge's tint: features in warm colours, the source in grey.
    pub(crate) fn color(self) -> Color32 {
        match self {
            BrushBadge::Tip | BrushBadge::Flat => Color32::from_rgb(90, 150, 220),
            BrushBadge::Texture => Color32::from_rgb(200, 150, 70),
            BrushBadge::Dual => Color32::from_rgb(170, 110, 210),
            BrushBadge::Spray => Color32::from_rgb(90, 170, 120),
            BrushBadge::ClipStudio | BrushBadge::Photoshop | BrushBadge::Krita | BrushBadge::OtherApp => Color32::from_gray(120),
        }
    }
}

/// The badges of a brush, features first, then where it came from.
pub(crate) fn badges(preset: &BrushPreset) -> Vec<BrushBadge> {
    let s = &preset.settings;
    let mut out = Vec::new();
    if preset.tip.is_some() || matches!(s.tip.shape, TipShape::Sampled(_)) {
        out.push(BrushBadge::Tip);
    } else if s.tip.thickness < 0.99 {
        out.push(BrushBadge::Flat);
    }
    if preset.texture.is_some() || s.texture.is_some() {
        out.push(BrushBadge::Texture);
    }
    if preset.dual.is_some() || s.dual.is_some() {
        out.push(BrushBadge::Dual);
    }
    if s.scatter.is_on() {
        out.push(BrushBadge::Spray);
    }
    match preset.source {
        PresetSource::Native => {}
        PresetSource::ClipStudio { .. } => out.push(BrushBadge::ClipStudio),
        PresetSource::Photoshop { .. } => out.push(BrushBadge::Photoshop),
        PresetSource::Krita { .. } => out.push(BrushBadge::Krita),
        PresetSource::MyPaint | PresetSource::Gimp => out.push(BrushBadge::OtherApp),
    }
    out
}

/// Whether a brush name matches the Sub Tool search: every word of `query`
/// appears in it, ignoring case. An empty query matches everything.
pub(crate) fn name_matches(name: &str, query: &str) -> bool {
    let name = name.to_lowercase();
    query.split_whitespace().all(|word| name.contains(&word.to_lowercase()))
}

/// Paints `badges` right to left, ending at `right` on the row's centre line
/// `y`. Returns the left edge of the leftmost badge.
pub(crate) fn paint_badges(painter: &egui::Painter, right: f32, y: f32, badges: &[BrushBadge]) -> f32 {
    let font = egui::FontId::monospace(8.5);
    let mut x = right;
    for badge in badges.iter().rev() {
        let galley = painter.layout_no_wrap(badge.label().to_string(), font.clone(), Color32::WHITE);
        let size = galley.size() + Vec2::new(6.0, 2.0);
        let rect = egui::Rect::from_min_size(egui::pos2(x - size.x, y - size.y / 2.0), size);
        painter.rect_filled(rect, 3.0, badge.color().gamma_multiply(0.9));
        painter.galley(rect.min + Vec2::new(3.0, 1.0), galley, Color32::WHITE);
        x = rect.left() - 3.0;
    }
    x
}

/// A small round-arrow button that puts a value back to the preset's.
/// Drawn dimmed and inert when the value is already the preset's.
pub(crate) fn reset_glyph(ui: &mut Ui, changed: bool, hint: &str) -> Response {
    let side = 16.0;
    let sense = if changed { Sense::click() } else { Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(side), sense);
    let color = if !changed {
        theme::text_dim().gamma_multiply(0.35)
    } else if response.hovered() {
        theme::accent()
    } else {
        theme::text()
    };
    let painter = ui.painter();
    let c = rect.center();
    let r = 4.5;
    // An open circle, like a reload arrow, with the head at its top-left end.
    let points: Vec<egui::Pos2> = (0..=16)
        .map(|k| {
            let a = std::f32::consts::PI * (0.75 + 1.5 * k as f32 / 16.0);
            c + Vec2::new(a.cos(), a.sin()) * r
        })
        .collect();
    let end = points[0];
    painter.add(egui::Shape::line(points, Stroke::new(1.4_f32, color)));
    painter.line_segment([end, end + Vec2::new(0.0, -3.5)], Stroke::new(1.4_f32, color));
    painter.line_segment([end, end + Vec2::new(3.5, 0.0)], Stroke::new(1.4_f32, color));
    if changed {
        response.on_hover_text(hint)
    } else {
        response
    }
}

/// The settings a brush's reset buttons go back to: a built-in brush's
/// defaults, or, for an imported brush, the preset as it was first seen this
/// session (imported brushes keep no other copy of their original values).
#[derive(Default)]
pub(crate) struct PresetBaselines {
    by_name: HashMap<String, BrushPreset>,
    /// Brush count the map was filled for; an import or delete clears it.
    brushes: usize,
}

impl PresetBaselines {
    /// The baseline of `current`; `builtin` gives its built-in default, if any (only asked the first time).
    pub(crate) fn get(&mut self, current: &BrushPreset, builtin: impl FnOnce() -> Option<BrushPreset>, total: usize) -> &BrushPreset {
        if total != self.brushes {
            self.by_name.retain(|_, b| b.source == PresetSource::Native);
            self.brushes = total;
        }
        self.by_name.entry(current.settings.name.clone()).or_insert_with(|| builtin().unwrap_or_else(|| current.clone()))
    }
}

/// [`PresetBaselines::get`] for brush `index` of `brushes`, looking up the
/// built-in default by name the first time.
pub(crate) fn baseline_of<'a>(baselines: &'a mut PresetBaselines, brushes: &[BrushPreset], index: usize) -> &'a BrushPreset {
    let current = &brushes[index];
    let builtin = || crate::app::default_brushes().into_iter().find(|d| d.settings.name == current.settings.name);
    baselines.get(current, builtin, brushes.len())
}

/// A group of brush properties a reset button puts back together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PropertyGroup {
    /// Size, opacity, blending, density and what pressure drives.
    General,
    /// Spacing, hardness, anti-aliasing, tip image, thickness and angle.
    Tip,
    /// Speed, tilt and random changes; tip rotation; pencil side.
    Dynamics,
    /// The size and density pressure curves.
    Curves,
    /// Paper texture and second tip.
    Texture,
    /// Stabilisation, smoothing, starting taper.
    Correction,
    /// Spraying effect and colour jitter.
    Effects,
}

impl PropertyGroup {
    /// Every group, in panel order.
    pub(crate) const ALL: [PropertyGroup; 7] = [
        PropertyGroup::General,
        PropertyGroup::Tip,
        PropertyGroup::Dynamics,
        PropertyGroup::Curves,
        PropertyGroup::Texture,
        PropertyGroup::Correction,
        PropertyGroup::Effects,
    ];
}

/// Copies `group`'s properties from `from` into `to`; returns whether the tip
/// fields changed (the caller then calls [`BrushPreset::apply_tip`]).
pub(crate) fn reset_group(to: &mut BrushPreset, from: &BrushPreset, group: PropertyGroup) -> bool {
    let (s, f) = (&mut to.settings, &from.settings);
    match group {
        PropertyGroup::General => {
            s.size = f.size;
            s.size_relative = f.size_relative;
            s.min_size_ratio = f.min_size_ratio;
            s.opacity = f.opacity;
            s.blend = f.blend;
            s.density = f.density;
            s.pressure_size = f.pressure_size;
            s.pressure_density = f.pressure_density;
            false
        }
        PropertyGroup::Tip => {
            s.spacing = f.spacing;
            s.hardness = f.hardness;
            s.antialias = f.antialias;
            let tip_changed = to.tip != from.tip || to.thickness != from.thickness || to.angle != from.angle;
            to.tip = from.tip.clone();
            to.thickness = from.thickness;
            to.angle = from.angle;
            tip_changed
        }
        PropertyGroup::Dynamics => {
            s.size_dynamics = f.size_dynamics.clone();
            s.density_dynamics = f.density_dynamics.clone();
            s.opacity_dynamics = f.opacity_dynamics.clone();
            s.thickness_dynamics = f.thickness_dynamics.clone();
            s.rotation = f.rotation.clone();
            s.tilt_side = f.tilt_side;
            false
        }
        PropertyGroup::Curves => {
            s.size_curve = f.size_curve.clone();
            s.density_curve = f.density_curve.clone();
            false
        }
        PropertyGroup::Texture => {
            let tip_changed = to.texture != from.texture || to.dual != from.dual;
            to.texture = from.texture.clone();
            to.dual = from.dual.clone();
            tip_changed
        }
        PropertyGroup::Correction => {
            s.stabilization = f.stabilization;
            s.smoothing = f.smoothing;
            s.taper_start = f.taper_start;
            false
        }
        PropertyGroup::Effects => {
            s.scatter = f.scatter;
            s.color_jitter = f.color_jitter;
            false
        }
    }
}

/// Whether `group`'s properties differ between `a` and `b`.
pub(crate) fn group_differs(a: &BrushPreset, b: &BrushPreset, group: PropertyGroup) -> bool {
    let (s, f) = (&a.settings, &b.settings);
    match group {
        PropertyGroup::General => {
            // A canvas-relative size has its pixel size follow the document: compare the share.
            (s.size != f.size && (s.size_relative.is_none() || f.size_relative.is_none()))
                || s.size_relative != f.size_relative
                || s.min_size_ratio != f.min_size_ratio
                || s.opacity != f.opacity
                || s.blend != f.blend
                || s.density != f.density
                || s.pressure_size != f.pressure_size
                || s.pressure_density != f.pressure_density
        }
        PropertyGroup::Tip => {
            s.spacing != f.spacing
                || s.hardness != f.hardness
                || s.antialias != f.antialias
                || a.tip != b.tip
                || a.thickness != b.thickness
                || a.angle != b.angle
        }
        PropertyGroup::Dynamics => {
            s.size_dynamics != f.size_dynamics
                || s.density_dynamics != f.density_dynamics
                || s.opacity_dynamics != f.opacity_dynamics
                || s.thickness_dynamics != f.thickness_dynamics
                || s.rotation != f.rotation
                || s.tilt_side != f.tilt_side
        }
        PropertyGroup::Curves => s.size_curve != f.size_curve || s.density_curve != f.density_curve,
        PropertyGroup::Texture => a.texture != b.texture || a.dual != b.dual,
        PropertyGroup::Correction => s.stabilization != f.stabilization || s.smoothing != f.smoothing || s.taper_start != f.taper_start,
        PropertyGroup::Effects => s.scatter != f.scatter || s.color_jitter != f.color_jitter,
    }
}

/// Whether any property group differs between `a` and `b`.
pub(crate) fn preset_differs(a: &BrushPreset, b: &BrushPreset) -> bool {
    PropertyGroup::ALL.iter().any(|&g| group_differs(a, b, g)) || a.settings.mode != b.settings.mode
}

#[cfg(test)]
mod tests {
    use super::*;
    use painter_core::BrushSettings;

    fn preset(settings: BrushSettings) -> BrushPreset {
        let mut p = BrushPreset::new(settings);
        p.apply_tip();
        p
    }

    #[test]
    fn search_matches_every_word_ignoring_case() {
        assert!(name_matches("G-pen (soft)", ""));
        assert!(name_matches("G-pen (soft)", "pen"));
        assert!(name_matches("G-pen (soft)", "SOFT g-"));
        assert!(!name_matches("G-pen (soft)", "pen hard"));
        assert!(name_matches("Ölfarbe Pinsel", "öl"));
    }

    #[test]
    fn round_native_brush_has_no_badges() {
        assert!(badges(&preset(BrushSettings::g_pen())).is_empty());
    }

    #[test]
    fn badges_show_spray_flat_tip_and_source() {
        let mut p = preset(BrushSettings::g_pen());
        p.settings.scatter = painter_core::Scatter { amount: 1.0, count: 4, count_jitter: 0.5, both_axes: true };
        p.thickness = 0.4;
        p.apply_tip();
        p.source = PresetSource::Photoshop { version: 6, sample_id: None };
        assert_eq!(badges(&p), vec![BrushBadge::Flat, BrushBadge::Spray, BrushBadge::Photoshop]);
    }

    #[test]
    fn group_reset_restores_only_that_group() {
        let base = preset(BrushSettings::g_pen());
        let mut edited = base.clone();
        edited.settings.opacity = 0.25;
        edited.settings.spacing = 1.5;
        edited.settings.stabilization = 77;
        assert!(group_differs(&edited, &base, PropertyGroup::General));
        assert!(group_differs(&edited, &base, PropertyGroup::Tip));
        assert!(!group_differs(&edited, &base, PropertyGroup::Curves));
        reset_group(&mut edited, &base, PropertyGroup::General);
        assert_eq!(edited.settings.opacity, base.settings.opacity);
        assert_eq!(edited.settings.spacing, 1.5, "other groups untouched");
        assert_eq!(edited.settings.stabilization, 77);
        assert!(!group_differs(&edited, &base, PropertyGroup::General));
    }

    #[test]
    fn badge_labels_and_hints_cover_all_variants() {
        let all_badges = [
            BrushBadge::Tip,
            BrushBadge::Flat,
            BrushBadge::Texture,
            BrushBadge::Dual,
            BrushBadge::Spray,
            BrushBadge::ClipStudio,
            BrushBadge::Photoshop,
            BrushBadge::Krita,
            BrushBadge::OtherApp,
        ];
        for b in all_badges {
            assert!(!b.label().is_empty());
            assert!(!b.hint().is_empty());
        }
    }

    #[test]
    fn preset_differs_detects_any_property_or_mode_change() {
        let base = preset(BrushSettings::g_pen());
        assert!(!preset_differs(&base, &base));

        let mut edited = base.clone();
        edited.settings.mode = painter_core::BrushMode::Erase;
        assert!(preset_differs(&edited, &base));

        let mut edited = base.clone();
        edited.settings.stabilization = 99;
        assert!(preset_differs(&edited, &base));
        reset_group(&mut edited, &base, PropertyGroup::Correction);
        assert!(!preset_differs(&edited, &base));
    }

    #[test]
    fn imported_baseline_is_the_first_seen_preset() {
        let mut baselines = PresetBaselines::default();
        let mut p = preset(BrushSettings::g_pen());
        p.settings.name = "Imported ink".into();
        p.source = PresetSource::ClipStudio { variant_id: 1 };
        let original = p.settings.opacity;
        assert_eq!(baselines.get(&p, || None, 3).settings.opacity, original);
        p.settings.opacity = 0.1;
        assert_eq!(baselines.get(&p, || None, 3).settings.opacity, original, "edits don't move the baseline");
        // A new import (count changes) forgets imported baselines: the brush may be another one now.
        assert_eq!(baselines.get(&p, || None, 4).settings.opacity, 0.1);
    }
}
