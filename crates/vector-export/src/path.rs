//! Bezier paths and their styling, the common currency of tracing and SVG.

/// A point in canvas pixels (y down).
pub type Point = [f32; 2];

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Segment {
    MoveTo(Point),
    LineTo(Point),
    /// Two control points, then the end point.
    CubicTo(Point, Point, Point),
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillRule {
    NonZero,
    /// Holes in traced shapes come out as separate contours inside the outer one.
    EvenOdd,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StrokeStyle {
    /// sRGB, 0..=1.
    pub color: [f32; 3],
    pub width: f32,
    pub opacity: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VectorPath {
    pub segments: Vec<Segment>,
    /// sRGB fill colour, 0..=1.
    pub fill: Option<[f32; 3]>,
    pub fill_opacity: f32,
    pub fill_rule: FillRule,
    pub stroke: Option<StrokeStyle>,
}

impl VectorPath {
    pub fn filled(segments: Vec<Segment>, color: [f32; 3], opacity: f32) -> Self {
        Self { segments, fill: Some(color), fill_opacity: opacity, fill_rule: FillRule::EvenOdd, stroke: None }
    }

    pub fn stroked(segments: Vec<Segment>, stroke: StrokeStyle) -> Self {
        Self { segments, fill: None, fill_opacity: 1.0, fill_rule: FillRule::NonZero, stroke: Some(stroke) }
    }

    pub fn is_empty(&self) -> bool {
        !self.segments.iter().any(|s| !matches!(s, Segment::MoveTo(_) | Segment::Close))
    }

    /// Number of closed contours.
    pub fn contours(&self) -> usize {
        self.segments.iter().filter(|s| matches!(s, Segment::Close)).count()
    }

    /// The SVG `d` attribute, coordinates rounded to `decimals`.
    pub fn to_svg_data(&self, decimals: usize) -> String {
        let n = |v: f32| {
            let s = format!("{v:.decimals$}");
            let s = if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s };
            if s == "-0" { "0".to_string() } else { s }
        };
        let p = |p: Point| format!("{} {}", n(p[0]), n(p[1]));
        let parts: Vec<String> = self
            .segments
            .iter()
            .map(|s| match *s {
                Segment::MoveTo(a) => format!("M{}", p(a)),
                Segment::LineTo(a) => format!("L{}", p(a)),
                Segment::CubicTo(a, b, c) => format!("C{} {} {}", p(a), p(b), p(c)),
                Segment::Close => "Z".into(),
            })
            .collect();
        parts.join("")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_compact_path_data() {
        let path = VectorPath::filled(
            vec![Segment::MoveTo([0.0, 0.0]), Segment::LineTo([10.5, -0.0001]), Segment::CubicTo([1.0, 2.0], [3.0, 4.25], [5.0, 6.0]), Segment::Close],
            [0.0; 3],
            1.0,
        );
        assert_eq!(path.to_svg_data(2), "M0 0L10.5 0C1 2 3 4.25 5 6Z");
        assert_eq!(path.contours(), 1);
        assert!(!path.is_empty());
    }
}
