//! Vector panel frames: shapes, borders, splitting with gutters, reading
//! order, snapping and the frame tool's handles.
//!
//! A panel is a polygon in canvas pixels (clockwise on screen). Rectangles,
//! diagonal cuts and free polygons are all the same thing, so the split,
//! mask and border code has one path.

use serde::{Deserialize, Serialize};

use crate::page_preset::{Binding, GutterMetrics, Point, RectPx};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PanelId(pub u32);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BorderStyle {
    /// 0 or `visible == false` = borderless.
    pub width: f64,
    pub color: [u8; 3],
    pub antialias: bool,
    /// Dash and gap lengths in pixels; `None` = solid.
    pub dash: Option<[f64; 2]>,
    pub visible: bool,
}

impl Default for BorderStyle {
    fn default() -> Self {
        Self { width: 6.0, color: [0, 0, 0], antialias: true, dash: None, visible: true }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Panel {
    pub id: PanelId,
    /// Clockwise polygon in canvas pixels.
    pub shape: Vec<Point>,
    pub border: BorderStyle,
    /// Runs past the inner frame to the bleed edge (no border on that side
    /// in print); the shape already includes the bleed.
    pub full_bleed: bool,
}

impl Panel {
    pub fn rect(id: PanelId, [x, y, w, h]: RectPx) -> Self {
        Self { id, shape: vec![[x, y], [x + w, y], [x + w, y + h], [x, y + h]], border: BorderStyle::default(), full_bleed: false }
    }

    pub fn bounds(&self) -> RectPx {
        bounds(&self.shape)
    }

    pub fn area(&self) -> f64 {
        signed_area(&self.shape).abs()
    }

    pub fn contains(&self, p: Point) -> bool {
        let n = self.shape.len();
        let mut odd = false;
        for i in 0..n {
            let (a, b) = (self.shape[i], self.shape[(i + n - 1) % n]);
            if (a[1] > p[1]) != (b[1] > p[1]) && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0] {
                odd = !odd;
            }
        }
        odd
    }

    /// Shapes in `f32` for `painter_core::selection::polygon_mask`.
    pub fn shape_f32(&self) -> Vec<[f32; 2]> {
        self.shape.iter().map(|p| [p[0] as f32, p[1] as f32]).collect()
    }
}

pub fn bounds(points: &[Point]) -> RectPx {
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for p in points {
        (x0, y0, x1, y1) = (x0.min(p[0]), y0.min(p[1]), x1.max(p[0]), y1.max(p[1]));
    }
    [x0, y0, x1 - x0, y1 - y0]
}

pub fn signed_area(points: &[Point]) -> f64 {
    let n = points.len();
    (0..n).map(|i| points[i][0] * points[(i + 1) % n][1] - points[(i + 1) % n][0] * points[i][1]).sum::<f64>() * 0.5
}

/// A cut through a panel: the line through `a` and `b` (canvas pixels).
/// Diagonal cuts are ordinary cuts with a slanted line.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cut {
    pub a: Point,
    pub b: Point,
}

impl Cut {
    /// A horizontal cut at `t` (0..=1) of the panel's height.
    pub fn horizontal(panel: &Panel, t: f64) -> Cut {
        let [x, y, w, h] = panel.bounds();
        Cut { a: [x - 1.0, y + h * t], b: [x + w + 1.0, y + h * t] }
    }

    /// A vertical cut at `t` (0..=1) of the panel's width.
    pub fn vertical(panel: &Panel, t: f64) -> Cut {
        let [x, y, w, h] = panel.bounds();
        Cut { a: [x + w * t, y - 1.0], b: [x + w * t, y + h + 1.0] }
    }

    fn direction(&self) -> Point {
        [self.b[0] - self.a[0], self.b[1] - self.a[1]]
    }
}

/// Keeps the part of `polygon` where `n · (p − origin) ≥ offset`
/// (Sutherland–Hodgman against one half-plane).
fn clip_half_plane(polygon: &[Point], origin: Point, n: Point, offset: f64) -> Vec<Point> {
    let side = |p: Point| n[0] * (p[0] - origin[0]) + n[1] * (p[1] - origin[1]) - offset;
    let mut out = Vec::new();
    let count = polygon.len();
    for i in 0..count {
        let (p, q) = (polygon[i], polygon[(i + 1) % count]);
        let (sp, sq) = (side(p), side(q));
        if sp >= 0.0 {
            out.push(p);
        }
        if (sp >= 0.0) != (sq >= 0.0) {
            let t = sp / (sp - sq);
            out.push([p[0] + (q[0] - p[0]) * t, p[1] + (q[1] - p[1]) * t]);
        }
    }
    out
}

/// Splits `panel` along `cut`, leaving `gutter` pixels between the halves
/// (measured perpendicular to the cut). The first half keeps the panel's id
/// and comes first in reading order of a horizontal (top) or vertical (left)
/// cut; the second gets `new_id`. `None` when the cut misses the panel or
/// leaves a sliver.
pub fn split(panel: &Panel, cut: Cut, gutter: f64, new_id: PanelId) -> Option<(Panel, Panel)> {
    let d = cut.direction();
    let len = d[0].hypot(d[1]);
    if len < 1e-9 {
        return None;
    }
    // Normal pointing to the "first" side: up for horizontal cuts, left for vertical ones.
    let mut n = [d[1] / len, -d[0] / len];
    if n[1] > 1e-9 || (n[1].abs() <= 1e-9 && n[0] > 0.0) {
        n = [-n[0], -n[1]];
    }
    let half = gutter.max(0.0) * 0.5;
    let first = clip_half_plane(&panel.shape, cut.a, n, half);
    let second = clip_half_plane(&panel.shape, cut.a, [-n[0], -n[1]], half);
    let min_area = 4.0;
    if first.len() < 3 || second.len() < 3 || signed_area(&first).abs() < min_area || signed_area(&second).abs() < min_area {
        return None;
    }
    let a = Panel { shape: first, ..panel.clone() };
    let b = Panel { id: new_id, shape: second, ..panel.clone() };
    Some((a, b))
}

/// Panels in reading order: tiers top to bottom (panels whose vertical
/// centres overlap the tier share it), then left to right or right to left.
pub fn reading_order(panels: &[Panel], binding: Binding) -> Vec<PanelId> {
    let mut items: Vec<(PanelId, RectPx)> = panels.iter().map(|p| (p.id, p.bounds())).collect();
    items.sort_by(|a, b| a.1[1].total_cmp(&b.1[1]));
    let mut tiers: Vec<Vec<(PanelId, RectPx)>> = Vec::new();
    for item in items {
        let centre = item.1[1] + item.1[3] * 0.5;
        match tiers.last_mut() {
            Some(tier) if tier.iter().any(|(_, r)| centre >= r[1] && centre <= r[1] + r[3]) => tier.push(item),
            _ => tiers.push(vec![item]),
        }
    }
    tiers
        .into_iter()
        .flat_map(|mut tier| {
            tier.sort_by(|a, b| {
                let (ca, cb) = (a.1[0] + a.1[2] * 0.5, b.1[0] + b.1[2] * 0.5);
                if binding == Binding::RightToLeft {
                    cb.total_cmp(&ca)
                } else {
                    ca.total_cmp(&cb)
                }
            });
            tier.into_iter().map(|(id, _)| id)
        })
        .collect()
}

/// What a point or axis can snap to.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SnapTargets {
    /// Vertical lines (x) and horizontal lines (y): panel edges, frames, margins.
    pub xs: Vec<f64>,
    pub ys: Vec<f64>,
    /// Panel corners.
    pub points: Vec<Point>,
    /// Guide grid spacing in pixels (0 = off) and origin.
    pub grid: f64,
    pub grid_origin: Point,
}

impl SnapTargets {
    /// Edges and corners of `panels` (except `skip`), plus the page frames.
    pub fn collect(panels: &[Panel], skip: Option<PanelId>, frames: &[RectPx], gutter: &GutterMetrics) -> Self {
        let mut t = SnapTargets::default();
        for p in panels.iter().filter(|p| Some(p.id) != skip) {
            let n = p.shape.len();
            for i in 0..n {
                let (a, b) = (p.shape[i], p.shape[(i + 1) % n]);
                t.points.push(a);
                if (a[0] - b[0]).abs() < 0.5 {
                    // A vertical edge: snap to it, and to a gutter away from it.
                    t.xs.extend([a[0], a[0] - gutter.horizontal, a[0] + gutter.horizontal]);
                }
                if (a[1] - b[1]).abs() < 0.5 {
                    t.ys.extend([a[1], a[1] - gutter.vertical, a[1] + gutter.vertical]);
                }
            }
        }
        for f in frames {
            t.xs.extend([f[0], f[0] + f[2]]);
            t.ys.extend([f[1], f[1] + f[3]]);
        }
        t
    }

    /// `p` moved onto the nearest target within `radius` (points win over
    /// lines, lines over the grid; x and y snap independently).
    pub fn snap(&self, p: Point, radius: f64) -> Point {
        if let Some(q) = self
            .points
            .iter()
            .filter(|q| (q[0] - p[0]).hypot(q[1] - p[1]) <= radius)
            .min_by(|a, b| (a[0] - p[0]).hypot(a[1] - p[1]).total_cmp(&(b[0] - p[0]).hypot(b[1] - p[1])))
        {
            return *q;
        }
        let axis = |v: f64, lines: &[f64], origin: f64| {
            let line =
                lines.iter().copied().filter(|l| (l - v).abs() <= radius).min_by(|a, b| (a - v).abs().total_cmp(&(b - v).abs()));
            line.or_else(|| {
                (self.grid > 0.0).then(|| origin + ((v - origin) / self.grid).round() * self.grid).filter(|g| (g - v).abs() <= radius)
            })
            .unwrap_or(v)
        };
        [axis(p[0], &self.xs, self.grid_origin[0]), axis(p[1], &self.ys, self.grid_origin[1])]
    }
}

/// What the frame tool's pointer grabbed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameHandle {
    /// A corner: reshapes the panel (diagonal cuts, skewed frames).
    Vertex(PanelId, usize),
    /// An edge: moves both its corners, keeping the edge's direction.
    Edge(PanelId, usize),
    /// Inside a panel: moves it.
    Body(PanelId),
}

/// The frame tool's drawing modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameMode {
    /// Drag a rectangle.
    Rectangle,
    /// Click corners; double-click or click the first corner to close.
    Polygon,
    /// Drag a line across a panel to split it.
    Divide,
    /// Grab corners, edges or panels.
    Edit,
}

impl FrameMode {
    pub const ALL: [FrameMode; 4] = [FrameMode::Rectangle, FrameMode::Polygon, FrameMode::Divide, FrameMode::Edit];

    pub fn label(self) -> &'static str {
        match self {
            FrameMode::Rectangle => "Rectangle",
            FrameMode::Polygon => "Polygon",
            FrameMode::Divide => "Divide",
            FrameMode::Edit => "Edit",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            FrameMode::Rectangle => "Drag a rectangle to add a panel.",
            FrameMode::Polygon => "Click the corners; click the first corner again or press Enter to close. Esc cancels.",
            FrameMode::Divide => "Drag a line across a panel to split it with the gutter. Shift keeps the line straight.",
            FrameMode::Edit => "Drag corners to reshape (diagonal cuts), edges to move a side, the inside to move the panel.",
        }
    }
}

/// The handle under `p` within `radius`, topmost panel first.
pub fn hit_test(panels: &[Panel], p: Point, radius: f64) -> Option<FrameHandle> {
    for panel in panels.iter().rev() {
        let n = panel.shape.len();
        if let Some(i) = (0..n).find(|&i| (panel.shape[i][0] - p[0]).hypot(panel.shape[i][1] - p[1]) <= radius) {
            return Some(FrameHandle::Vertex(panel.id, i));
        }
        if let Some(i) = (0..n).find(|&i| segment_distance(p, panel.shape[i], panel.shape[(i + 1) % n]) <= radius) {
            return Some(FrameHandle::Edge(panel.id, i));
        }
    }
    panels.iter().rev().find(|panel| panel.contains(p)).map(|panel| FrameHandle::Body(panel.id))
}

/// Applies a drag of `delta` to `panel` (in its state when the drag began).
pub fn drag_handle(panel: &Panel, handle: FrameHandle, delta: Point) -> Panel {
    let mut out = panel.clone();
    let n = out.shape.len();
    let shift = |p: &mut Point| {
        p[0] += delta[0];
        p[1] += delta[1];
    };
    match handle {
        FrameHandle::Vertex(_, i) => shift(&mut out.shape[i]),
        FrameHandle::Edge(_, i) => {
            shift(&mut out.shape[i]);
            shift(&mut out.shape[(i + 1) % n]);
        }
        FrameHandle::Body(_) => out.shape.iter_mut().for_each(shift),
    }
    out
}

pub fn segment_distance(p: Point, a: Point, b: Point) -> f64 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 { (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
    (p[0] - a[0] - t * dx).hypot(p[1] - a[1] - t * dy)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square() -> Panel {
        Panel::rect(PanelId(1), [0.0, 0.0, 100.0, 100.0])
    }

    #[test]
    fn splits_keep_the_gutter() {
        let (top, bottom) = split(&square(), Cut::horizontal(&square(), 0.5), 10.0, PanelId(2)).unwrap();
        assert_eq!(top.id, PanelId(1));
        assert_eq!(bottom.id, PanelId(2));
        assert_eq!(top.bounds(), [0.0, 0.0, 100.0, 45.0]);
        assert_eq!(bottom.bounds(), [0.0, 55.0, 100.0, 45.0]);
        let (left, right) = split(&square(), Cut::vertical(&square(), 0.3), 4.0, PanelId(2)).unwrap();
        assert!((left.bounds()[2] - 28.0).abs() < 1e-9 && (right.bounds()[0] - 32.0).abs() < 1e-9);
        // A diagonal cut leaves two trapezoids with a perpendicular gap.
        let (a, b) = split(&square(), Cut { a: [0.0, 30.0], b: [100.0, 70.0] }, 6.0, PanelId(2)).unwrap();
        assert!(a.area() + b.area() < 10_000.0 && a.area() > 3000.0 && b.area() > 3000.0);
        assert!(split(&square(), Cut::horizontal(&square(), 0.0), 10.0, PanelId(2)).is_none(), "sliver");
    }

    #[test]
    fn reading_order_follows_binding() {
        let panels = vec![
            Panel::rect(PanelId(1), [0.0, 0.0, 45.0, 40.0]),
            Panel::rect(PanelId(2), [55.0, 0.0, 45.0, 40.0]),
            Panel::rect(PanelId(3), [0.0, 50.0, 100.0, 50.0]),
        ];
        assert_eq!(reading_order(&panels, Binding::LeftToRight), [PanelId(1), PanelId(2), PanelId(3)]);
        assert_eq!(reading_order(&panels, Binding::RightToLeft), [PanelId(2), PanelId(1), PanelId(3)]);
    }

    #[test]
    fn snapping_and_handles() {
        let panels = vec![Panel::rect(PanelId(1), [10.0, 10.0, 50.0, 50.0])];
        let targets = SnapTargets {
            grid: 25.0,
            ..SnapTargets::collect(&panels, None, &[[0.0, 0.0, 200.0, 200.0]], &GutterMetrics::uniform(4.0))
        };
        assert_eq!(targets.snap([61.0, 11.5], 3.0), [60.0, 10.0], "corner");
        assert_eq!(targets.snap([65.5, 120.0], 3.0), [64.0, 120.0], "a gutter away from the edge");
        assert_eq!(targets.snap([101.0, 149.0], 3.0), [100.0, 150.0], "grid");
        assert_eq!(targets.snap([90.0, 90.0], 3.0), [90.0, 90.0], "nothing near");

        assert_eq!(hit_test(&panels, [60.5, 59.0], 3.0), Some(FrameHandle::Vertex(PanelId(1), 2)));
        assert_eq!(hit_test(&panels, [35.0, 11.0], 3.0), Some(FrameHandle::Edge(PanelId(1), 0)));
        assert_eq!(hit_test(&panels, [30.0, 30.0], 3.0), Some(FrameHandle::Body(PanelId(1))));
        // Dragging one corner makes a diagonal edge.
        let moved = drag_handle(&panels[0], FrameHandle::Vertex(PanelId(1), 1), [0.0, 20.0]);
        assert_eq!(moved.shape[1], [60.0, 30.0]);
        assert_eq!(moved.shape[2], [60.0, 60.0]);
    }
}
