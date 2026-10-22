//! Extended Color Selector, after Krita's: a hue ring or bar with a shape for
//! the other two coordinates (triangle, square, diamond, wheel, rectangles),
//! in one of four colour models: HSV, HSL, HSI and HSY′ (luma). HSI and HSY′
//! are "extended": their saturation is the chroma over the most the gamut
//! allows at that hue and intensity / luma, so every point is a real sRGB
//! colour and the whole gamut is reachable (as Krita's selector does).
//!
//! Written from how Krita's selector behaves, not its code. The picture is
//! drawn on the CPU into a texture whenever the hue, the shape or the size
//! changes; the main colour follows every drag.

use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, Vec2};
use serde::{Deserialize, Serialize};

use crate::app::PainterApp;

/// The colour model of the shape's two coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) enum ColorModel {
    #[default]
    Hsv,
    Hsl,
    Hsi,
    Hsy,
}

impl ColorModel {
    pub(crate) const ALL: [ColorModel; 4] = [ColorModel::Hsv, ColorModel::Hsl, ColorModel::Hsi, ColorModel::Hsy];

    fn name(self) -> &'static str {
        match self {
            ColorModel::Hsv => "HSV",
            ColorModel::Hsl => "HSL",
            ColorModel::Hsi => "HSI",
            ColorModel::Hsy => "HSY′",
        }
    }

    /// The third coordinate's name.
    fn third(self) -> &'static str {
        match self {
            ColorModel::Hsv => "Value",
            ColorModel::Hsl => "Lightness",
            ColorModel::Hsi => "Intensity",
            ColorModel::Hsy => "Luma",
        }
    }
}

/// The selector's layout.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) enum GamutShape {
    /// Hue ring around a triangle (black, white, the hue); always HSV.
    #[default]
    RingTriangle,
    /// Hue ring around a square: saturation across, the third coordinate up.
    RingSquare,
    /// Hue ring around a diamond: the third coordinate up, saturation out from the middle.
    RingDiamond,
    /// Hue around, saturation out; the third coordinate on a bar above.
    Wheel,
    /// Hue on a bar above a square of saturation × the third coordinate.
    SquareHueBar,
    /// Hue across, the third coordinate up; saturation on a bar above.
    HueThird,
    /// Hue across, saturation up; the third coordinate on a bar above.
    HueSaturation,
}

impl GamutShape {
    pub(crate) const ALL: [GamutShape; 7] = [
        GamutShape::RingTriangle,
        GamutShape::RingSquare,
        GamutShape::RingDiamond,
        GamutShape::Wheel,
        GamutShape::SquareHueBar,
        GamutShape::HueThird,
        GamutShape::HueSaturation,
    ];

    fn name(self) -> &'static str {
        match self {
            GamutShape::RingTriangle => "Ring and triangle",
            GamutShape::RingSquare => "Ring and square",
            GamutShape::RingDiamond => "Ring and diamond",
            GamutShape::Wheel => "Wheel",
            GamutShape::SquareHueBar => "Square and hue bar",
            GamutShape::HueThird => "Hue × value",
            GamutShape::HueSaturation => "Hue × saturation",
        }
    }

    fn has_bar(self) -> bool {
        matches!(self, GamutShape::Wheel | GamutShape::SquareHueBar | GamutShape::HueThird | GamutShape::HueSaturation)
    }

    fn has_ring(self) -> bool {
        matches!(self, GamutShape::RingTriangle | GamutShape::RingSquare | GamutShape::RingDiamond)
    }

    /// The triangle is HSV whatever the model (as in Krita).
    fn model(self, model: ColorModel) -> ColorModel {
        if self == GamutShape::RingTriangle {
            ColorModel::Hsv
        } else {
            model
        }
    }
}

// ---- colour models ---------------------------------------------------------------------

/// The pure colour of hue `h` (0..1): the most saturated, brightest one.
fn hue_rgb(h: f32) -> [f32; 3] {
    let h6 = h.rem_euclid(1.0) * 6.0;
    let x = 1.0 - (h6 % 2.0 - 1.0).abs();
    match h6 as u32 {
        0 => [1.0, x, 0.0],
        1 => [x, 1.0, 0.0],
        2 => [0.0, 1.0, x],
        3 => [0.0, x, 1.0],
        4 => [x, 0.0, 1.0],
        _ => [1.0, 0.0, x],
    }
}

/// Hue (0..1) and chroma of an RGB colour.
fn hue_chroma([r, g, b]: [f32; 3]) -> (f32, f32) {
    let max = r.max(g).max(b);
    let c = max - r.min(g).min(b);
    let h = if c <= 1e-6 {
        0.0
    } else if max == r {
        ((g - b) / c).rem_euclid(6.0) / 6.0
    } else if max == g {
        ((b - r) / c + 2.0) / 6.0
    } else {
        ((r - g) / c + 4.0) / 6.0
    };
    (h, c)
}

/// The weights of HSI's intensity and HSY′'s luma (Rec. 601).
fn weights(model: ColorModel) -> [f32; 3] {
    match model {
        ColorModel::Hsy => [0.299, 0.587, 0.114],
        _ => [1.0 / 3.0; 3],
    }
}

fn weighted(w: [f32; 3], c: [f32; 3]) -> f32 {
    w[0] * c[0] + w[1] * c[1] + w[2] * c[2]
}

/// The most chroma hue `h` can have at weighted brightness `y` inside sRGB.
fn max_chroma(w: [f32; 3], h: f32, y: f32) -> f32 {
    // Colours of hue h and chroma c: y + c · (base − weighted(base)).
    let base = hue_rgb(h);
    let lb = weighted(w, base);
    let mut c = f32::MAX;
    for b in base {
        let d = b - lb;
        if d > 1e-6 {
            c = c.min((1.0 - y) / d);
        } else if d < -1e-6 {
            c = c.min(y / -d);
        }
    }
    if c == f32::MAX {
        0.0
    } else {
        c.max(0.0)
    }
}

/// sRGB (0..1) of `[hue, saturation, third]` in `model`.
pub(crate) fn to_rgb(model: ColorModel, [h, s, x]: [f32; 3]) -> [f32; 3] {
    let (s, x) = (s.clamp(0.0, 1.0), x.clamp(0.0, 1.0));
    let base = hue_rgb(h);
    match model {
        ColorModel::Hsv => base.map(|b| x * (1.0 - s * (1.0 - b))),
        ColorModel::Hsl => {
            let c = (1.0 - (2.0 * x - 1.0).abs()) * s;
            base.map(|b| (x + c * (b - 0.5)).clamp(0.0, 1.0))
        }
        ColorModel::Hsi | ColorModel::Hsy => {
            let w = weights(model);
            let c = s * max_chroma(w, h, x);
            let lb = weighted(w, base);
            base.map(|b| (x + c * (b - lb)).clamp(0.0, 1.0))
        }
    }
}

/// `[hue, saturation, third]` in `model` of an sRGB colour; `hue` is kept for greys.
pub(crate) fn from_rgb(model: ColorModel, rgb: [f32; 3], hue: f32) -> [f32; 3] {
    let (h, c) = hue_chroma(rgb);
    let h = if c <= 1e-4 { hue } else { h };
    let max = rgb[0].max(rgb[1]).max(rgb[2]);
    let min = rgb[0].min(rgb[1]).min(rgb[2]);
    match model {
        ColorModel::Hsv => [h, if max > 0.0 { c / max } else { 0.0 }, max],
        ColorModel::Hsl => {
            let l = (max + min) / 2.0;
            let d = 1.0 - (2.0 * l - 1.0).abs();
            [h, if d > 1e-6 { (c / d).min(1.0) } else { 0.0 }, l]
        }
        ColorModel::Hsi | ColorModel::Hsy => {
            let w = weights(model);
            let y = weighted(w, rgb);
            let m = max_chroma(w, h, y);
            [h, if m > 1e-6 { (c / m).min(1.0) } else { 0.0 }, y]
        }
    }
}

// ---- geometry --------------------------------------------------------------------------

/// Which part a point is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Ring,
    Shape,
    Bar,
}

/// Where the parts are in a `w` × `h` selector.
struct Layout {
    centre: Pos2,
    /// Ring: outer and inner radius; wheel: radius (inner 0).
    outer: f32,
    inner: f32,
    /// The bar across the top, if any.
    bar: Option<Rect>,
    /// The rectangle of a square / rectangle shape, if any.
    rect: Rect,
}

const BAR: f32 = 14.0;

fn layout(shape: GamutShape, size: Vec2) -> Layout {
    let top = if shape.has_bar() { BAR + 6.0 } else { 0.0 };
    let bar = shape.has_bar().then(|| Rect::from_min_size(Pos2::new(2.0, 1.0), Vec2::new(size.x - 4.0, BAR)));
    let area = Rect::from_min_max(Pos2::new(0.0, top), Pos2::new(size.x, size.y));
    let side = area.width().min(area.height());
    let centre = area.center();
    let outer = side / 2.0 - 2.0;
    match shape {
        GamutShape::Wheel => Layout { centre, outer, inner: 0.0, bar, rect: area },
        GamutShape::SquareHueBar => {
            Layout { centre, outer, inner: 0.0, bar, rect: Rect::from_center_size(centre, Vec2::splat(side - 4.0)) }
        }
        GamutShape::HueThird | GamutShape::HueSaturation => Layout { centre, outer, inner: 0.0, bar, rect: area.shrink(2.0) },
        _ => {
            let inner = outer * 0.8;
            // Squares inscribed in the ring.
            let half = (inner - 4.0) / std::f32::consts::SQRT_2;
            Layout { centre, outer, inner, bar, rect: Rect::from_center_size(centre, Vec2::splat(half * 2.0)) }
        }
    }
}

/// Screen angle (y down) of hue `h` on rings and wheels: red at the left, yellow up-left, cyan right.
fn hue_angle(h: f32) -> f32 {
    std::f32::consts::PI + h * std::f32::consts::TAU
}

fn angle_hue(a: f32) -> f32 {
    ((a - std::f32::consts::PI) / std::f32::consts::TAU).rem_euclid(1.0)
}

/// The triangle's corners: black at the top, white bottom-left, the hue bottom-right.
fn triangle(l: &Layout) -> [Pos2; 3] {
    let r = l.inner - 4.0;
    let at = |deg: f32| l.centre + Vec2::angled(deg.to_radians()) * r;
    [at(-90.0), at(150.0), at(30.0)]
}

/// The part under point `p`, if any.
fn part_at(shape: GamutShape, l: &Layout, p: Pos2) -> Option<Part> {
    if l.bar.is_some_and(|b| b.expand(3.0).contains(p)) {
        return Some(Part::Bar);
    }
    let d = (p - l.centre).length();
    if shape.has_ring() {
        if d >= l.inner - 2.0 && d <= l.outer + 4.0 {
            return Some(Part::Ring);
        }
        return (d < l.inner).then_some(Part::Shape);
    }
    match shape {
        GamutShape::Wheel => (d <= l.outer + 4.0).then_some(Part::Shape),
        _ => l.rect.expand(4.0).contains(p).then_some(Part::Shape),
    }
}

/// The colour coordinates (h, s, x) at point `p` of `part`, from `hsx` (what the point doesn't set).
fn pick(shape: GamutShape, l: &Layout, part: Part, p: Pos2, [h, s, x]: [f32; 3]) -> [f32; 3] {
    let across = |r: Rect| ((p.x - r.left()) / r.width()).clamp(0.0, 1.0);
    let up = |r: Rect| (1.0 - (p.y - r.top()) / r.height()).clamp(0.0, 1.0);
    match (part, shape) {
        (Part::Ring, _) => [angle_hue((p - l.centre).angle()), s, x],
        (Part::Bar, GamutShape::SquareHueBar) => [across(l.bar.unwrap()), s, x],
        (Part::Bar, GamutShape::HueThird) => [h, across(l.bar.unwrap()), x],
        (Part::Bar, _) => [h, s, across(l.bar.unwrap())],
        (Part::Shape, GamutShape::RingTriangle) => {
            let [k, w, c] = triangle(l);
            // Barycentric weights of black, white and the hue, clamped into the triangle.
            let area = (w - k).x * (c - k).y - (c - k).x * (w - k).y;
            let mut bw = ((c - p).x * (k - p).y - (k - p).x * (c - p).y) / area;
            let mut bc = ((k - p).x * (w - p).y - (w - p).x * (k - p).y) / area;
            bw = bw.max(0.0);
            bc = bc.max(0.0);
            let sum = bw + bc;
            if sum > 1.0 {
                bw /= sum;
                bc /= sum;
            }
            let v = bw + bc;
            [h, if v > 1e-6 { bc / v } else { s }, v]
        }
        (Part::Shape, GamutShape::RingSquare | GamutShape::SquareHueBar) => [h, across(l.rect), up(l.rect)],
        (Part::Shape, GamutShape::RingDiamond) => {
            let r = l.inner - 4.0;
            let (dx, dy) = ((p.x - l.centre.x) / r, (p.y - l.centre.y) / r);
            let third = (0.5 - dy / 2.0).clamp(0.0, 1.0);
            let half = (1.0 - dy.abs()).max(1e-3);
            [h, ((dx + half) / (2.0 * half)).clamp(0.0, 1.0), third]
        }
        (Part::Shape, GamutShape::Wheel) => {
            let d = p - l.centre;
            [angle_hue(d.angle()), (d.length() / l.outer).clamp(0.0, 1.0), x]
        }
        (Part::Shape, GamutShape::HueThird) => [across(l.rect).min(0.9999), s, up(l.rect)],
        (Part::Shape, GamutShape::HueSaturation) => [across(l.rect).min(0.9999), up(l.rect), x],
    }
}

/// Where the current colour sits in the shape, and on the bar.
fn marks(shape: GamutShape, l: &Layout, [h, s, x]: [f32; 3]) -> (Option<Pos2>, Option<Pos2>, Option<Pos2>) {
    let at = |r: Rect, u: f32, v: f32| Pos2::new(r.left() + u * r.width(), r.bottom() - v * r.height());
    let ring = shape.has_ring().then(|| l.centre + Vec2::angled(hue_angle(h)) * (l.inner + l.outer) / 2.0);
    let bar_at = |u: f32| l.bar.map(|b| Pos2::new(b.left() + u * b.width(), b.center().y));
    let (shape_mark, bar) = match shape {
        GamutShape::RingTriangle => {
            let [k, w, c] = triangle(l);
            let p = k + ((w + (c - w) * s) - k) * x;
            (Some(p), None)
        }
        GamutShape::RingSquare => (Some(at(l.rect, s, x)), None),
        GamutShape::RingDiamond => {
            let r = l.inner - 4.0;
            let dy = 1.0 - 2.0 * x;
            let half = 1.0 - dy.abs();
            (Some(l.centre + Vec2::new((s * 2.0 * half - half) * r, dy * r)), None)
        }
        GamutShape::Wheel => (Some(l.centre + Vec2::angled(hue_angle(h)) * s * l.outer), bar_at(x)),
        GamutShape::SquareHueBar => (Some(at(l.rect, s, x)), bar_at(h)),
        GamutShape::HueThird => (Some(at(l.rect, h, x)), bar_at(s)),
        GamutShape::HueSaturation => (Some(at(l.rect, h, s)), bar_at(x)),
    };
    (ring, shape_mark, bar)
}

/// The selector as a picture, `size` in physical pixels (`scale` per point).
fn render(shape: GamutShape, model: ColorModel, [h, s, x]: [f32; 3], size: Vec2, scale: f32) -> egui::ColorImage {
    let (w, ht) = ((size.x * scale).round().max(1.0) as usize, (size.y * scale).round().max(1.0) as usize);
    let l = layout(shape, size);
    let model = shape.model(model);
    let mut pixels = vec![Color32::TRANSPARENT; w * ht];
    // 2×2 samples a pixel for smooth edges.
    let offsets = [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)];
    for py in 0..ht {
        for px in 0..w {
            let mut acc = [0.0f32; 4];
            for (ox, oy) in offsets {
                let p = Pos2::new((px as f32 + ox) / scale, (py as f32 + oy) / scale);
                let Some(color) = colour_at(shape, model, &l, p, [h, s, x]) else { continue };
                for k in 0..3 {
                    acc[k] += color[k];
                }
                acc[3] += 1.0;
            }
            if acc[3] > 0.0 {
                let a = acc[3] / 4.0;
                let c = |v: f32| ((v / acc[3]).clamp(0.0, 1.0) * 255.0).round() as u8;
                pixels[py * w + px] = Color32::from_rgba_unmultiplied(c(acc[0]), c(acc[1]), c(acc[2]), (a * 255.0).round() as u8);
            }
        }
    }
    egui::ColorImage { size: [w, ht], pixels }
}

/// The colour drawn at point `p` (selector points), or `None` off the parts.
fn colour_at(shape: GamutShape, model: ColorModel, l: &Layout, p: Pos2, [h, s, x]: [f32; 3]) -> Option<[f32; 3]> {
    if let Some(bar) = l.bar {
        if bar.contains(p) {
            let u = ((p.x - bar.left()) / bar.width()).clamp(0.0, 1.0);
            return Some(match shape {
                // Pure hues, like the ring.
                GamutShape::SquareHueBar => hue_rgb(u),
                GamutShape::HueThird => to_rgb(model, [h, u, x]),
                _ => to_rgb(model, [h, s, u]),
            });
        }
    }
    let d = p - l.centre;
    let r = d.length();
    if shape.has_ring() && r >= l.inner && r <= l.outer {
        return Some(hue_rgb(angle_hue(d.angle())));
    }
    match shape {
        GamutShape::RingTriangle => {
            let [k, w, c] = triangle(l);
            let area = (w - k).x * (c - k).y - (c - k).x * (w - k).y;
            let bk = ((w - p).x * (c - p).y - (c - p).x * (w - p).y) / area;
            let bw = ((c - p).x * (k - p).y - (k - p).x * (c - p).y) / area;
            let bc = 1.0 - bk - bw;
            if bk < 0.0 || bw < 0.0 || bc < 0.0 {
                return None;
            }
            let v = bw + bc;
            Some(to_rgb(ColorModel::Hsv, [h, if v > 1e-6 { bc / v } else { 0.0 }, v]))
        }
        GamutShape::RingDiamond => {
            let rr = l.inner - 4.0;
            let (dx, dy) = (d.x / rr, d.y / rr);
            if dx.abs() + dy.abs() > 1.0 {
                return None;
            }
            let half = (1.0 - dy.abs()).max(1e-3);
            Some(to_rgb(model, [h, ((dx + half) / (2.0 * half)).clamp(0.0, 1.0), (0.5 - dy / 2.0).clamp(0.0, 1.0)]))
        }
        GamutShape::Wheel => (r <= l.outer).then(|| to_rgb(model, [angle_hue(d.angle()), r / l.outer, x])),
        _ => {
            if !l.rect.contains(p) {
                return None;
            }
            let u = (p.x - l.rect.left()) / l.rect.width();
            let v = 1.0 - (p.y - l.rect.top()) / l.rect.height();
            Some(match shape {
                GamutShape::HueThird => to_rgb(model, [u, s, v]),
                GamutShape::HueSaturation => to_rgb(model, [u, v, x]),
                _ => to_rgb(model, [h, u, v]),
            })
        }
    }
}

// ---- the panel ---------------------------------------------------------------------------

/// The selector's state between frames.
#[derive(Default)]
pub(crate) struct GamutSelector {
    /// The colour in the current model, kept exactly while dragging.
    hsx: [f32; 3],
    /// The main colour last set or seen (sRGB bytes), to notice changes from elsewhere.
    seen: Option<[u8; 3]>,
    dragging: Option<Part>,
    picture: Option<(String, egui::TextureHandle)>,
    thumbs: Vec<(String, egui::TextureHandle)>,
}

fn rgb_bytes(c: [f32; 3]) -> [u8; 3] {
    c.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
}

fn marker(painter: &egui::Painter, at: Pos2, radius: f32) {
    painter.circle_stroke(at, radius + 1.0, Stroke::new(1.5_f32, Color32::BLACK));
    painter.circle_stroke(at, radius, Stroke::new(1.5_f32, Color32::WHITE));
}

impl PainterApp {
    /// Extended Color Selector: Krita's shapes and colour models.
    pub(crate) fn gamut_selector_panel(&mut self, ui: &mut egui::Ui) {
        let (shape, model) = (self.colors.data.gamut_shape, self.colors.data.gamut_model);
        let space = shape.model(model);
        // Follow the main colour when something else changed it.
        let [r, g, b, _] = self.color.to_srgba_unmultiplied();
        let current = [r, g, b];
        let sel = &mut self.gamut;
        if sel.seen != Some(current) {
            let hue = sel.hsx[0];
            sel.hsx = from_rgb(space, current.map(|v| v as f32 / 255.0), hue);
            sel.seen = Some(current);
        }

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(format!("{} · {}", space.name(), shape.name())).small());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.menu_button("☰", |ui| self.gamut_menu(ui)).response.on_hover_text("Colour model and shape");
            });
        });

        let width = ui.available_width().clamp(120.0, 320.0);
        let height = if shape.has_bar() && !matches!(shape, GamutShape::HueThird | GamutShape::HueSaturation) {
            width + BAR + 6.0
        } else if matches!(shape, GamutShape::HueThird | GamutShape::HueSaturation) {
            width * 0.75 + BAR + 6.0
        } else {
            width
        };
        let size = Vec2::new(width, height);
        let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
        let l = layout(shape, size);
        let sel = &mut self.gamut;

        // Drags: the part pressed on keeps the drag.
        if let Some(pos) = response.interact_pointer_pos() {
            let local = (pos - rect.min).to_pos2();
            if response.drag_started() || response.clicked() || sel.dragging.is_none() {
                sel.dragging = part_at(shape, &l, local);
            }
            if let Some(part) = sel.dragging {
                sel.hsx = pick(shape, &l, part, local, sel.hsx);
                let rgb = to_rgb(space, sel.hsx);
                let bytes = rgb_bytes(rgb);
                self.color = egui::ecolor::Hsva::from_srgb(bytes);
                sel.seen = Some(bytes);
            }
        }
        if !response.dragged() && !response.is_pointer_button_down_on() {
            sel.dragging = None;
        }

        // The picture changes with the hue (ring shapes and the square), or the bar's value.
        let scale = ui.ctx().pixels_per_point();
        let [h, s, x] = sel.hsx;
        let q = |v: f32| (v * 512.0).round() as i32;
        let key = match shape {
            GamutShape::Wheel => format!("{shape:?}{space:?}{}x{}@{scale}:{}", width, height, q(x)),
            GamutShape::HueThird => format!("{shape:?}{space:?}{}x{}@{scale}:{}", width, height, q(s)),
            GamutShape::HueSaturation => format!("{shape:?}{space:?}{}x{}@{scale}:{}", width, height, q(x)),
            _ => format!("{shape:?}{space:?}{}x{}@{scale}:{}", width, height, q(h)),
        };
        if sel.picture.as_ref().is_none_or(|(k, _)| *k != key) {
            let image = render(shape, model, [h, s, x], size, scale);
            match &mut sel.picture {
                Some((k, handle)) => {
                    handle.set(image, egui::TextureOptions::LINEAR);
                    *k = key;
                }
                None => sel.picture = Some((key, ui.ctx().load_texture("gamut-selector", image, egui::TextureOptions::LINEAR))),
            }
        }
        let painter = ui.painter_at(rect.expand(4.0));
        if let Some((_, tex)) = &sel.picture {
            painter.image(tex.id(), rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
        }
        let (ring, inner, bar) = marks(shape, &l, sel.hsx);
        for (m, radius) in [(ring, 5.0), (inner, 5.0), (bar, 4.0)] {
            if let Some(p) = m {
                marker(&painter, rect.min + p.to_vec2(), radius);
            }
        }

        ui.horizontal(|ui| {
            let rgb = to_rgb(space, sel.hsx);
            let (chip, _) = ui.allocate_exact_size(Vec2::new(28.0, 18.0), Sense::hover());
            let c = rgb_bytes(rgb);
            ui.painter().rect_filled(chip, 3.0, Color32::from_rgb(c[0], c[1], c[2]));
            ui.label(
                egui::RichText::new(format!(
                    "H {:.0}°  S {:.0}%  {} {:.0}%  #{:02X}{:02X}{:02X}",
                    sel.hsx[0] * 360.0,
                    sel.hsx[1] * 100.0,
                    &space.third()[..1],
                    sel.hsx[2] * 100.0,
                    c[0],
                    c[1],
                    c[2]
                ))
                .small()
                .monospace(),
            );
        });
    }

    /// The ☰ menu: colour model, and the shapes as thumbnails.
    fn gamut_menu(&mut self, ui: &mut egui::Ui) {
        ui.label("Colour model");
        ui.horizontal(|ui| {
            for m in ColorModel::ALL {
                ui.radio_value(&mut self.colors.data.gamut_model, m, m.name());
            }
        });
        ui.small("HSI and HSY′ stretch saturation to the whole gamut; the triangle is always HSV.");
        ui.separator();
        ui.label("Shape");
        let model = self.colors.data.gamut_model;
        let scale = ui.ctx().pixels_per_point();
        let hsx = [self.gamut.hsx[0], 1.0, 0.5];
        ui.horizontal_wrapped(|ui| {
            ui.set_max_width(300.0);
            for shape in GamutShape::ALL {
                let size = if shape.has_bar() { Vec2::new(64.0, 64.0 + BAR / 2.0) } else { Vec2::splat(64.0) };
                let key = format!("{shape:?}{model:?}{}", (hsx[0] * 64.0).round());
                let thumbs = &mut self.gamut.thumbs;
                let id = match thumbs.iter().find(|(k, _)| *k == key) {
                    Some((_, t)) => t.id(),
                    None => {
                        let image = render(shape, model, hsx, size, scale);
                        let t = ui.ctx().load_texture("gamut-thumb", image, egui::TextureOptions::LINEAR);
                        let id = t.id();
                        thumbs.retain(|(k, _)| !k.starts_with(&format!("{shape:?}")));
                        thumbs.push((key, t));
                        id
                    }
                };
                let on = self.colors.data.gamut_shape == shape;
                let button = egui::Button::image(egui::Image::new((id, size))).selected(on);
                if ui.add(button).on_hover_text(shape.name()).clicked() {
                    self.colors.data.gamut_shape = shape;
                    ui.close_menu();
                }
            }
        });
        // The model or shape may have changed: re-read the colour in the new space.
        self.gamut.seen = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_round_trip_inside_the_gamut() {
        for model in ColorModel::ALL {
            for rgb in [[0.8, 0.2, 0.1], [0.1, 0.5, 0.9], [0.3, 0.3, 0.3], [1.0, 1.0, 0.0], [0.05, 0.6, 0.2]] {
                let hsx = from_rgb(model, rgb, 0.0);
                let back = to_rgb(model, hsx);
                for k in 0..3 {
                    assert!((back[k] - rgb[k]).abs() < 1e-3, "{model:?} {rgb:?} -> {hsx:?} -> {back:?}");
                }
            }
            // Every point of the extended models is a real colour, and full saturation touches the gamut.
            for h in [0.0, 0.1, 0.4, 0.7] {
                for y in [0.1, 0.5, 0.9] {
                    let c = to_rgb(model, [h, 1.0, y]);
                    assert!(c.iter().all(|v| (-1e-4..=1.0 + 1e-4).contains(v)));
                    if matches!(model, ColorModel::Hsi | ColorModel::Hsy) {
                        assert!(c.iter().any(|v| *v < 1e-3 || *v > 1.0 - 1e-3), "{model:?} at the edge: {c:?}");
                        assert!((weighted(weights(model), c) - y).abs() < 1e-3, "brightness kept");
                    }
                }
            }
        }
        // Greys keep the hue they had.
        assert_eq!(from_rgb(ColorModel::Hsv, [0.5; 3], 0.3)[0], 0.3);
    }

    #[test]
    fn picking_and_marking_agree() {
        let size = Vec2::new(240.0, 240.0 + BAR + 6.0);
        for shape in GamutShape::ALL {
            let l = layout(shape, size);
            let hsx = [0.3, 0.6, 0.7];
            let (_, inner, bar) = marks(shape, &l, hsx);
            let p = inner.unwrap();
            assert_eq!(part_at(shape, &l, p), Some(Part::Shape), "{shape:?}");
            let back = pick(shape, &l, Part::Shape, p, hsx);
            for k in 0..3 {
                assert!((back[k] - hsx[k]).abs() < 0.02, "{shape:?}: {hsx:?} -> {back:?}");
            }
            if let Some(b) = bar {
                let back = pick(shape, &l, Part::Bar, b, hsx);
                assert!(back.iter().zip(hsx).all(|(a, b)| (a - b).abs() < 0.02), "{shape:?} bar");
            }
        }
        // The ring gives the hue under the pointer.
        let l = layout(GamutShape::RingSquare, Vec2::splat(240.0));
        let p = l.centre + Vec2::angled(hue_angle(0.25)) * (l.inner + l.outer) / 2.0;
        assert_eq!(part_at(GamutShape::RingSquare, &l, p), Some(Part::Ring));
        assert!((pick(GamutShape::RingSquare, &l, Part::Ring, p, [0.0; 3])[0] - 0.25).abs() < 1e-3);
    }
}
