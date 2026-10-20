//! The SVG document model and writer. Layers become Inkscape-compatible
//! groups (`inkscape:groupmode="layer"`), so the hierarchy survives in
//! Inkscape and Illustrator. Gradients, screentone patterns and masks live in
//! `<defs>` and are referenced by id.

use std::fmt::Write;

use painter_core::BlendMode;

use crate::path::{FillRule, VectorPath};

/// What fills a rectangle.
#[derive(Debug, Clone, PartialEq)]
pub enum Paint {
    /// sRGB, 0..=1.
    Color([f32; 3]),
    /// A gradient or pattern in `<defs>`, by id.
    Ref(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum SvgNode {
    Group(SvgGroup),
    Path(VectorPath),
    /// A filled rectangle (paper, gradient and tone layers, mask backgrounds).
    Rect { x: f32, y: f32, width: f32, height: f32, paint: Paint },
    /// Draws the element with this id again (clipping masks reuse a layer's content).
    Use { href: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct SvgGroup {
    pub id: String,
    pub label: String,
    pub opacity: f32,
    pub visible: bool,
    pub blend: BlendMode,
    /// A `<mask>` in `<defs>` applied to the group, by id.
    pub mask: Option<String>,
    /// When set, the children are wrapped in an inner `<g>` with this id, so
    /// another element can `<use>` the pixels without the group's opacity.
    pub content_id: Option<String>,
    pub children: Vec<SvgNode>,
}

impl SvgGroup {
    /// A plain wrapper group (not an Inkscape layer).
    pub fn wrapper(children: Vec<SvgNode>, mask: Option<String>) -> Self {
        Self { id: String::new(), label: String::new(), opacity: 1.0, visible: true, blend: BlendMode::Normal, mask, content_id: None, children }
    }
}

/// A gradient stop: offset 0..=1, sRGB colour, opacity.
#[derive(Debug, Clone, PartialEq)]
pub struct Stop {
    pub offset: f32,
    pub color: [f32; 3],
    pub opacity: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SvgDef {
    LinearGradient { id: String, from: [f32; 2], to: [f32; 2], stops: Vec<Stop> },
    RadialGradient { id: String, center: [f32; 2], radius: f32, stops: Vec<Stop> },
    /// Dots of `radius` on a `cell` grid rotated by `angle` degrees (screentone).
    DotPattern { id: String, cell: f32, angle: f32, radius: f32, color: [f32; 3] },
    /// `alpha`: the children's alpha masks (SVG 2 `mask-type="alpha"`);
    /// otherwise their luminance does.
    Mask { id: String, alpha: bool, children: Vec<SvgNode> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct SvgDocument {
    pub width: u32,
    pub height: u32,
    pub defs: Vec<SvgDef>,
    /// Bottom to top, like the layer stack.
    pub children: Vec<SvgNode>,
    /// Coordinate precision.
    pub decimals: usize,
}

/// CSS `mix-blend-mode` for a layer blend mode.
pub fn css_blend(mode: BlendMode) -> &'static str {
    match mode {
        BlendMode::Normal => "normal",
        BlendMode::Darken => "darken",
        BlendMode::Multiply => "multiply",
        BlendMode::ColorBurn => "color-burn",
        // No CSS equivalents; the closest ones.
        BlendMode::LinearBurn => "multiply",
        BlendMode::Add => "screen",
        BlendMode::Lighten => "lighten",
        BlendMode::Screen => "screen",
        BlendMode::ColorDodge => "color-dodge",
        BlendMode::Overlay => "overlay",
        BlendMode::SoftLight => "soft-light",
        BlendMode::HardLight => "hard-light",
        BlendMode::Difference => "difference",
    }
}

fn hex(c: [f32; 3]) -> String {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", b(c[0]), b(c[1]), b(c[2]))
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// A number without trailing zeros.
fn num(v: f32) -> String {
    let s = format!("{v:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.into() }
}

impl SvgDocument {
    pub fn to_svg(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, r#"<?xml version="1.0" encoding="UTF-8"?>"#);
        let _ = writeln!(
            out,
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" xmlns:inkscape="http://www.inkscape.org/namespaces/inkscape" width="{w}" height="{h}" viewBox="0 0 {w} {h}">"#,
            w = self.width,
            h = self.height
        );
        if !self.defs.is_empty() {
            out.push_str("  <defs>\n");
            for def in &self.defs {
                self.write_def(&mut out, def);
            }
            out.push_str("  </defs>\n");
        }
        for node in &self.children {
            self.write_node(&mut out, node, 1);
        }
        out.push_str("</svg>\n");
        out
    }

    fn write_def(&self, out: &mut String, def: &SvgDef) {
        let stops = |out: &mut String, stops: &[Stop]| {
            for s in stops {
                let _ = writeln!(out, r#"      <stop offset="{}" stop-color="{}" stop-opacity="{}"/>"#, num(s.offset), hex(s.color), num(s.opacity));
            }
        };
        match def {
            SvgDef::LinearGradient { id, from, to, stops: list } => {
                let _ = writeln!(
                    out,
                    r#"    <linearGradient id="{}" gradientUnits="userSpaceOnUse" x1="{}" y1="{}" x2="{}" y2="{}">"#,
                    escape(id),
                    num(from[0]),
                    num(from[1]),
                    num(to[0]),
                    num(to[1])
                );
                stops(out, list);
                out.push_str("    </linearGradient>\n");
            }
            SvgDef::RadialGradient { id, center, radius, stops: list } => {
                let _ = writeln!(
                    out,
                    r#"    <radialGradient id="{}" gradientUnits="userSpaceOnUse" cx="{}" cy="{}" r="{}">"#,
                    escape(id),
                    num(center[0]),
                    num(center[1]),
                    num(*radius)
                );
                stops(out, list);
                out.push_str("    </radialGradient>\n");
            }
            SvgDef::DotPattern { id, cell, angle, radius, color } => {
                let _ = writeln!(
                    out,
                    r#"    <pattern id="{}" patternUnits="userSpaceOnUse" width="{c}" height="{c}" patternTransform="rotate({})"><circle cx="{h}" cy="{h}" r="{}" fill="{}"/></pattern>"#,
                    escape(id),
                    num(*angle),
                    num(*radius),
                    hex(*color),
                    c = num(*cell),
                    h = num(cell * 0.5)
                );
            }
            SvgDef::Mask { id, alpha, children } => {
                let kind = if *alpha { r#" mask-type="alpha" style="mask-type:alpha""# } else { "" };
                let (w, h) = (self.width, self.height);
                let _ = writeln!(out, r#"    <mask id="{}" maskUnits="userSpaceOnUse" x="0" y="0" width="{w}" height="{h}"{kind}>"#, escape(id));
                for child in children {
                    self.write_node(out, child, 3);
                }
                out.push_str("    </mask>\n");
            }
        }
    }

    fn write_node(&self, out: &mut String, node: &SvgNode, depth: usize) {
        let pad = "  ".repeat(depth);
        match node {
            SvgNode::Group(g) => {
                let mut attrs = String::new();
                if !g.id.is_empty() {
                    let _ = write!(attrs, r#" id="{}" inkscape:groupmode="layer" inkscape:label="{}""#, escape(&g.id), escape(&g.label));
                }
                if g.opacity < 1.0 {
                    let _ = write!(attrs, r#" opacity="{:.3}""#, g.opacity);
                }
                if !g.visible {
                    attrs.push_str(r#" display="none""#);
                }
                if g.blend != BlendMode::Normal {
                    let _ = write!(attrs, r#" style="mix-blend-mode:{}""#, css_blend(g.blend));
                }
                if let Some(mask) = &g.mask {
                    let _ = write!(attrs, r#" mask="url(#{})""#, escape(mask));
                }
                let _ = writeln!(out, "{pad}<g{attrs}>");
                let inner = match &g.content_id {
                    Some(id) => {
                        let _ = writeln!(out, r#"{pad}  <g id="{}">"#, escape(id));
                        depth + 2
                    }
                    None => depth + 1,
                };
                for child in &g.children {
                    self.write_node(out, child, inner);
                }
                if g.content_id.is_some() {
                    let _ = writeln!(out, "{pad}  </g>");
                }
                let _ = writeln!(out, "{pad}</g>");
            }
            SvgNode::Path(p) => {
                if p.is_empty() {
                    return;
                }
                let mut attrs = format!(r#"d="{}""#, p.to_svg_data(self.decimals));
                match p.fill {
                    Some(c) => {
                        let _ = write!(attrs, r#" fill="{}""#, hex(c));
                        if p.fill_opacity < 1.0 {
                            let _ = write!(attrs, r#" fill-opacity="{:.3}""#, p.fill_opacity);
                        }
                        if p.fill_rule == FillRule::EvenOdd {
                            attrs.push_str(r#" fill-rule="evenodd""#);
                        }
                    }
                    None => attrs.push_str(r#" fill="none""#),
                }
                if let Some(s) = &p.stroke {
                    let _ = write!(
                        attrs,
                        r#" stroke="{}" stroke-width="{:.2}" stroke-linecap="round" stroke-linejoin="round""#,
                        hex(s.color),
                        s.width
                    );
                    if s.opacity < 1.0 {
                        let _ = write!(attrs, r#" stroke-opacity="{:.3}""#, s.opacity);
                    }
                }
                let _ = writeln!(out, "{pad}<path {attrs}/>");
            }
            SvgNode::Rect { x, y, width, height, paint } => {
                let fill = match paint {
                    Paint::Color(c) => hex(*c),
                    Paint::Ref(id) => format!("url(#{})", escape(id)),
                };
                let _ = writeln!(out, r#"{pad}<rect x="{}" y="{}" width="{}" height="{}" fill="{fill}"/>"#, num(*x), num(*y), num(*width), num(*height));
            }
            SvgNode::Use { href } => {
                let _ = writeln!(out, r##"{pad}<use href="#{0}" xlink:href="#{0}"/>"##, escape(href));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::Segment;

    fn group(id: &str, children: Vec<SvgNode>) -> SvgGroup {
        SvgGroup {
            id: id.into(),
            label: id.into(),
            opacity: 1.0,
            visible: true,
            blend: BlendMode::Normal,
            mask: None,
            content_id: None,
            children,
        }
    }

    #[test]
    fn writes_layers_as_groups() {
        let path = VectorPath::filled(vec![Segment::MoveTo([0.0, 0.0]), Segment::LineTo([4.0, 0.0]), Segment::LineTo([4.0, 4.0]), Segment::Close], [1.0, 0.0, 0.0], 0.5);
        let doc = SvgDocument {
            width: 10,
            height: 8,
            decimals: 2,
            defs: Vec::new(),
            children: vec![
                SvgNode::Rect { x: 0.0, y: 0.0, width: 10.0, height: 8.0, paint: Paint::Color([1.0; 3]) },
                SvgNode::Group(SvgGroup {
                    label: "Ink & <lines>".into(),
                    opacity: 0.8,
                    visible: false,
                    blend: BlendMode::Multiply,
                    ..group("layer-2", vec![SvgNode::Path(path)])
                }),
            ],
        };
        let svg = doc.to_svg();
        assert!(svg.contains(r#"viewBox="0 0 10 8""#));
        assert!(svg.contains(r#"inkscape:label="Ink &amp; &lt;lines&gt;""#));
        assert!(svg.contains(r#"opacity="0.800""#) && svg.contains(r#"display="none""#));
        assert!(svg.contains("mix-blend-mode:multiply"));
        assert!(svg.contains(r##"d="M0 0L4 0L4 4Z" fill="#ff0000" fill-opacity="0.500" fill-rule="evenodd""##));
        assert!(svg.contains(r##"<rect x="0" y="0" width="10" height="8" fill="#ffffff"/>"##));
        assert!(!svg.contains("<defs>"));
        assert!(svg.trim_end().ends_with("</svg>"));
    }

    #[test]
    fn writes_defs_and_references() {
        let stops = vec![Stop { offset: 0.0, color: [1.0, 0.0, 0.0], opacity: 1.0 }, Stop { offset: 1.0, color: [0.0, 0.0, 1.0], opacity: 0.0 }];
        let doc = SvgDocument {
            width: 20,
            height: 20,
            decimals: 2,
            defs: vec![
                SvgDef::LinearGradient { id: "g".into(), from: [0.0, 0.0], to: [10.0, 0.0], stops },
                SvgDef::DotPattern { id: "t".into(), cell: 8.0, angle: 45.0, radius: 2.5, color: [0.0; 3] },
                SvgDef::Mask { id: "clip".into(), alpha: true, children: vec![SvgNode::Use { href: "content-1".into() }] },
            ],
            children: vec![
                SvgNode::Group(SvgGroup { content_id: Some("content-1".into()), ..group("layer-1", vec![SvgNode::Rect { x: 0.0, y: 0.0, width: 20.0, height: 20.0, paint: Paint::Ref("g".into()) }]) }),
                SvgNode::Group(SvgGroup::wrapper(vec![SvgNode::Group(group("layer-2", Vec::new()))], Some("clip".into()))),
            ],
        };
        let svg = doc.to_svg();
        assert!(svg.contains(r#"<linearGradient id="g" gradientUnits="userSpaceOnUse" x1="0" y1="0" x2="10" y2="0">"#));
        assert!(svg.contains(r##"<stop offset="1" stop-color="#0000ff" stop-opacity="0"/>"##));
        assert!(svg.contains(r#"patternTransform="rotate(45)"><circle cx="4" cy="4" r="2.5""#));
        assert!(svg.contains(r#"mask-type="alpha""#) && svg.contains(r##"<use href="#content-1""##));
        assert!(svg.contains(r#"fill="url(#g)""#));
        assert!(svg.contains(r#"<g id="content-1">"#));
        assert!(svg.contains(r#"<g mask="url(#clip)">"#));
    }
}
