//! OpenRaster (.ora), the layered format shared by MyPaint, Krita, GIMP and
//! others: a ZIP with a stored `mimetype`, `stack.xml` describing the layer
//! tree (first child = topmost), one PNG per layer in `data/`, the composite
//! `mergedimage.png` and `Thumbnails/thumbnail.png`.
//!
//! What OpenRaster can't hold is flattened on save, and listed as warnings:
//! masks and clipping are baked into the layer's pixels, fill / gradient /
//! tone and vector layers are saved as their pixels, correction layers are
//! left out.

use crate::blend::BlendMode;
use crate::document::Document;
use crate::interchange::{self as ix, xml_escape};
use crate::layer::{clip_targets, ClipTo, Folder, Layer, LayerId, LayerKind};
use crate::tile::{TileCoord, TileStore, TILE_SIZE};
use crate::zipfile::{ZipReader, ZipWriter};

pub const MIMETYPE: &str = "image/openraster";

/// Multiplies `tiles`' alpha by `base`'s (clipping baked in).
pub(crate) fn bake_clip(tiles: &mut TileStore, base: &TileStore) {
    let coords: Vec<TileCoord> = tiles.iter().map(|(c, _)| c).collect();
    for coord in coords {
        let b = base.get(coord).cloned();
        let tile = tiles.get_or_create_mut(coord);
        for y in 0..TILE_SIZE {
            for x in 0..TILE_SIZE {
                let a = b.as_ref().map_or(0, |b| b.pixel(x, y)[3]) as u32;
                let p = tile.pixel(x, y);
                tile.set_pixel(x, y, p.map(|v| ((v as u32 * a + 127) / 255) as u8));
            }
        }
    }
}

/// A layer's pixels ready for a flat-layer format: its own pixels, mask and
/// clipping (against `clip_base`) baked in. `None` for folders and correction layers.
pub(crate) fn flat_pixels(layer: &Layer, clip_base: Option<&Layer>, canvas: (u32, u32)) -> Option<TileStore> {
    let mut tiles = ix::layer_pixels(layer, canvas, true)?;
    if let Some(base) = clip_base.and_then(|b| ix::layer_pixels(b, canvas, true)) {
        bake_clip(&mut tiles, &base);
    }
    Some(tiles)
}

struct Writer<'a> {
    doc: &'a Document,
    zip: ZipWriter,
    xml: String,
    count: usize,
    warnings: Vec<String>,
}

impl Writer<'_> {
    fn list(&mut self, layers: &[Layer], depth: usize) -> Result<(), String> {
        let clips = clip_targets(layers);
        for (i, layer) in layers.iter().enumerate().rev() {
            let base = match clips[i] {
                ClipTo::Layer(id) => layers.iter().find(|l| l.id == id),
                _ => None,
            };
            self.layer(layer, base, matches!(clips[i], ClipTo::Skip), depth)?;
        }
        Ok(())
    }

    fn common(&self, layer: &Layer, hidden: bool) -> String {
        format!(
            r#"name="{}" opacity="{:.4}" visibility="{}" composite-op="{}"{}{}"#,
            xml_escape(&layer.name),
            layer.opacity.clamp(0.0, 1.0),
            if layer.visible && !hidden { "visible" } else { "hidden" },
            ix::ora_op(layer.blend),
            if layer.id == self.doc.active { r#" selected="true""# } else { "" },
            if layer.locked { r#" edit-locked="true""# } else { "" },
        )
    }

    fn layer(&mut self, layer: &Layer, clip_base: Option<&Layer>, hidden: bool, depth: usize) -> Result<(), String> {
        let pad = " ".repeat(depth + 1);
        match &layer.kind {
            LayerKind::Folder(f) => {
                if layer.active_mask().is_some() {
                    self.warnings.push(format!("“{}”: folder masks aren't supported by OpenRaster and were dropped", layer.name));
                }
                let isolation = if f.pass_through { "auto" } else { "isolate" };
                let mut common = self.common(layer, hidden);
                if f.pass_through {
                    // Our pass-through folders ignore their own blend mode and opacity.
                    common = format!(
                        r#"name="{}" opacity="1.0000" visibility="{}" composite-op="svg:src-over""#,
                        xml_escape(&layer.name),
                        if layer.visible && !hidden { "visible" } else { "hidden" }
                    );
                }
                self.xml += &format!("{pad}<stack {common} isolation=\"{isolation}\" x=\"0\" y=\"0\">\n");
                self.list(&f.children, depth + 1)?;
                self.xml += &format!("{pad}</stack>\n");
            }
            LayerKind::Adjustment(_) => {
                self.warnings.push(format!("“{}”: correction layers aren't supported by OpenRaster and were left out", layer.name));
            }
            _ => {
                let canvas = self.doc.canvas();
                let tiles = flat_pixels(layer, clip_base, canvas).unwrap_or_default();
                if layer.clip && clip_base.is_some() {
                    self.warnings.push(format!("“{}”: clipping was baked into the pixels", layer.name));
                }
                let (x, y, w, h, rgba) = match ix::bounds(&tiles, canvas) {
                    Some((x, y, w, h)) => (x, y, w, h, ix::crop_rgba(&tiles, x, y, w, h)),
                    None => (0, 0, 1, 1, vec![0; 4]),
                };
                let src = format!("data/layer{}.png", self.count);
                self.count += 1;
                self.zip.add(&src, &ix::encode_png(w, h, &rgba)?, false);
                let common = self.common(layer, hidden);
                self.xml += &format!("{pad}<layer {common} src=\"{src}\" x=\"{x}\" y=\"{y}\"/>\n");
            }
        }
        Ok(())
    }
}

/// The document as an OpenRaster file, and what couldn't be kept as is.
pub fn write(doc: &Document) -> Result<(Vec<u8>, Vec<String>), String> {
    let mut zip = ZipWriter::new();
    zip.add("mimetype", MIMETYPE.as_bytes(), false);
    let mut w = Writer { doc, zip, xml: String::new(), count: 0, warnings: Vec::new() };
    w.xml = format!(
        "<?xml version='1.0' encoding='UTF-8'?>\n<image version=\"0.0.5\" w=\"{}\" h=\"{}\" xres=\"{}\" yres=\"{}\">\n <stack>\n",
        doc.width, doc.height, doc.dpi, doc.dpi
    );
    w.list(&doc.layers, 1)?;
    w.xml += " </stack>\n</image>\n";
    let (merged, thumb) = ix::merged_and_thumbnail(doc, 256)?;
    let Writer { mut zip, xml, mut warnings, .. } = w;
    zip.add("stack.xml", xml.as_bytes(), true);
    zip.add("mergedimage.png", &merged, false);
    zip.add("Thumbnails/thumbnail.png", &thumb, false);
    warnings.dedup();
    Ok((zip.finish(), warnings))
}

pub fn save(doc: &Document, path: &std::path::Path) -> Result<Vec<String>, String> {
    let (bytes, warnings) = write(doc)?;
    std::fs::write(path, bytes).map_err(|e| e.to_string())?;
    Ok(warnings)
}

struct Reader<'a> {
    zip: &'a ZipReader,
    canvas: (u32, u32),
    next: u64,
    active: Option<LayerId>,
    warnings: Vec<String>,
}

fn attr<'a>(n: roxmltree::Node<'a, '_>, name: &str) -> Option<&'a str> {
    n.attribute(name)
}

fn num<T: std::str::FromStr>(n: roxmltree::Node, name: &str) -> Option<T> {
    n.attribute(name).and_then(|v| v.trim().parse().ok())
}

impl Reader<'_> {
    fn id(&mut self) -> LayerId {
        self.next += 1;
        LayerId(self.next)
    }

    /// The children of a `<stack>`, bottom to top.
    fn stack(&mut self, node: roxmltree::Node) -> Vec<Layer> {
        let mut out: Vec<Layer> = node.children().filter(|c| c.is_element()).filter_map(|c| self.node(c)).collect();
        out.reverse();
        out
    }

    fn node(&mut self, n: roxmltree::Node) -> Option<Layer> {
        let name = attr(n, "name").unwrap_or("Layer").to_string();
        let op = attr(n, "composite-op").unwrap_or("svg:src-over");
        let blend = ix::from_ora_op(op).unwrap_or_else(|| {
            if op != "svg:src-over" {
                self.warnings.push(format!("“{name}”: blend mode {op} isn't supported, using Normal"));
            }
            BlendMode::Normal
        });
        let opacity = num::<f32>(n, "opacity").unwrap_or(1.0).clamp(0.0, 1.0);
        let id = self.id();
        let mut layer = match n.tag_name().name() {
            "stack" => {
                let children = self.stack(n);
                let mut folder = Layer::folder(id, name.clone());
                let isolate = attr(n, "isolation") == Some("isolate") || blend != BlendMode::Normal || opacity < 1.0;
                folder.kind = LayerKind::Folder(Folder { children, pass_through: !isolate });
                folder
            }
            "layer" => {
                let src = attr(n, "src")?;
                let mut tiles = TileStore::new();
                match self.zip.read(src).map(|b| ix::decode_png(&b)) {
                    Some(Ok((w, h, rgba))) => {
                        let (x, y) = (num::<i32>(n, "x").unwrap_or(0), num::<i32>(n, "y").unwrap_or(0));
                        ix::blit_rgba(&mut tiles, x, y, w, h, &rgba);
                        clip_to_canvas(&mut tiles, self.canvas);
                    }
                    _ => self.warnings.push(format!("“{name}”: couldn't read {src} (only PNG layers are supported)")),
                }
                Layer::raster_with(id, name, tiles)
            }
            _ => return None,
        };
        layer.blend = blend;
        layer.opacity = opacity;
        layer.visible = attr(n, "visibility") != Some("hidden");
        layer.locked = attr(n, "edit-locked") == Some("true");
        if attr(n, "selected") == Some("true") && !layer.is_folder() {
            self.active = Some(layer.id);
        }
        Some(layer)
    }
}

/// Drops tiles wholly outside the canvas.
pub(crate) fn clip_to_canvas(tiles: &mut TileStore, canvas: (u32, u32)) {
    let outside: Vec<TileCoord> = tiles
        .iter()
        .map(|(c, _)| c)
        .filter(|c| {
            let (ox, oy) = c.origin();
            ox + TILE_SIZE as i32 <= 0 || oy + TILE_SIZE as i32 <= 0 || ox >= canvas.0 as i32 || oy >= canvas.1 as i32
        })
        .collect();
    for c in outside {
        tiles.remove(c);
    }
}

/// The first raster layer (top-down), for when nothing is marked selected.
fn first_raster(layers: &[Layer]) -> Option<LayerId> {
    layers.iter().rev().find_map(|l| match &l.kind {
        LayerKind::Folder(f) => first_raster(&f.children),
        LayerKind::Raster(_) => Some(l.id),
        _ => None,
    })
}

/// Reads an OpenRaster file; also returns what couldn't be read as is.
pub fn read(data: Vec<u8>) -> Result<(Document, Vec<String>), String> {
    let zip = ZipReader::new(data)?;
    let xml = zip.read("stack.xml").ok_or("not an OpenRaster file (no stack.xml)")?;
    let xml = String::from_utf8(xml).map_err(|_| "stack.xml isn't UTF-8")?;
    let tree = roxmltree::Document::parse(&xml).map_err(|e| format!("stack.xml: {e}"))?;
    let image = tree.root_element();
    if image.tag_name().name() != "image" {
        return Err("stack.xml: no <image>".into());
    }
    let (w, h) = (num::<u32>(image, "w").ok_or("stack.xml: no width")?, num::<u32>(image, "h").ok_or("stack.xml: no height")?);
    let mut r = Reader { zip: &zip, canvas: (w, h), next: 0, active: None, warnings: Vec::new() };
    let layers = image.children().find(|c| c.has_tag_name("stack")).map(|s| r.stack(s)).unwrap_or_default();
    let active = r.active.or_else(|| first_raster(&layers)).unwrap_or(LayerId(0));
    let mut doc = Document::from_parts(w, h, layers, active, r.next + 1);
    if doc.layers.is_empty() {
        let id = doc.alloc_id();
        doc.layers.push(Layer::raster(id, "Layer 1"));
        doc.active = id;
    }
    if let Some(dpi) = num::<f32>(image, "xres").filter(|d| *d >= 1.0) {
        doc.dpi = dpi.round() as u32;
    }
    Ok((doc, r.warnings))
}

pub fn load(path: &std::path::Path) -> Result<(Document, Vec<String>), String> {
    read(std::fs::read(path).map_err(|e| e.to_string())?)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::io::flatten;

    /// Paper, a red square at (70, 10), a folder holding a half-opaque
    /// multiply blue layer, and a clipped layer over the red square.
    pub(crate) fn sample() -> Document {
        let mut doc = Document::new(160, 100);
        let red = doc.active;
        {
            let l = doc.find_mut(red).unwrap();
            l.name = "Red & <ink>".into();
            let t = l.tiles_mut().unwrap();
            for y in 10..40 {
                for x in 70..100 {
                    let c = TileCoord::containing(x, y);
                    let (ox, oy) = c.origin();
                    t.get_or_create_mut(c).set_pixel((x - ox) as u32, (y - oy) as u32, [200, 0, 0, 255]);
                }
            }
        }
        let clipped = doc.alloc_id();
        let mut c = Layer::raster(clipped, "Shade");
        {
            let t = c.tiles_mut().unwrap();
            for y in 0..100 {
                for x in 60..90 {
                    let coord = TileCoord::containing(x, y);
                    let (ox, oy) = coord.origin();
                    t.get_or_create_mut(coord).set_pixel((x - ox) as u32, (y - oy) as u32, [0, 0, 0, 255]);
                }
            }
        }
        c.clip = true;
        c.opacity = 0.5;
        doc.layers.push(c);
        let f = doc.alloc_id();
        let b = doc.alloc_id();
        let mut folder = Layer::folder(f, "Folder");
        let mut blue = Layer::raster(b, "Blue");
        blue.blend = BlendMode::Multiply;
        blue.opacity = 0.5;
        {
            let t = blue.tiles_mut().unwrap();
            for y in 50..90 {
                for x in 5..140 {
                    let coord = TileCoord::containing(x, y);
                    let (ox, oy) = coord.origin();
                    t.get_or_create_mut(coord).set_pixel((x - ox) as u32, (y - oy) as u32, [0, 0, 255, 255]);
                }
            }
        }
        if let LayerKind::Folder(x) = &mut folder.kind {
            x.children.push(blue);
            x.pass_through = false;
        }
        doc.layers.push(folder);
        doc.dpi = 144;
        doc
    }

    fn close(a: &[u8], b: &[u8]) -> bool {
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.abs_diff(*y) <= 2)
    }

    #[test]
    fn round_trips_the_picture_and_the_tree() {
        let doc = sample();
        let (bytes, warnings) = write(&doc).unwrap();
        assert_eq!(&bytes[30..38], b"mimetype");
        assert!(warnings.iter().any(|w| w.contains("clipping")), "{warnings:?}");
        let (back, warnings) = read(bytes).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!((back.width, back.height, back.dpi), (160, 100, 144));
        let names: Vec<String> =
            back.rows_top_down().iter().map(|(id, d)| format!("{}{}", "-".repeat(*d), back.find(*id).unwrap().name)).collect();
        assert_eq!(names, ["Folder", "-Blue", "Shade", "Red & <ink>", "Paper"]);
        let blue = back.find(back.rows_top_down()[1].0).unwrap();
        assert_eq!((blue.blend, blue.opacity), (BlendMode::Multiply, 0.5));
        assert!(back.find(back.rows_top_down()[0].0).unwrap().is_isolated_folder());
        assert_eq!(back.find(back.active).unwrap().name, "Red & <ink>");
        assert!(close(&flatten(&back), &flatten(&doc)), "same picture");
    }

    #[test]
    fn rejects_other_archives() {
        let mut z = ZipWriter::new();
        z.add("a.txt", b"x", false);
        assert!(read(z.finish()).is_err());
    }
}
