//! Canvas crop box and the selection clipboard.
//!
//! **Crop** is non-destructive: [`apply_crop`] moves the canvas window over
//! the layers (shifting their pixels, masks, vector points and gradient
//! handles) and resizes it; pixels outside the new canvas stay in their tiles
//! and come back if the canvas grows again.
//!
//! **Clipboard**: [`copy`] lifts the selected pixels of a layer into a
//! [`FloatingSelection`]; [`erase_selected`] finishes a cut; [`paste_tiles`]
//! puts a floating selection on a new layer, in place or centred on the view.

use painter_core::{Document, Layer, LayerKind, Tile, TileCoord, TileStore, TILE_SIZE};

use crate::matrix::Point;

/// Crop box aspect constraint.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AspectRatio {
    Free,
    /// Width : height.
    Ratio(f64, f64),
}

impl AspectRatio {
    pub const WIDESCREEN: AspectRatio = AspectRatio::Ratio(16.0, 9.0);
    pub const SQUARE: AspectRatio = AspectRatio::Ratio(1.0, 1.0);
    /// ISO 216 portrait (210 × 297 mm).
    pub const A4: AspectRatio = AspectRatio::Ratio(210.0, 297.0);

    pub fn presets() -> [(&'static str, AspectRatio); 4] {
        [("Free", AspectRatio::Free), ("16:9", AspectRatio::WIDESCREEN), ("1:1", AspectRatio::SQUARE), ("A4", AspectRatio::A4)]
    }

    fn ratio(self) -> Option<f64> {
        match self {
            AspectRatio::Free => None,
            AspectRatio::Ratio(w, h) if w > 0.0 && h > 0.0 => Some(w / h),
            AspectRatio::Ratio(..) => None,
        }
    }
}

/// The 8 resize handles of a box, clockwise from the top-left corner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoxHandle {
    TopLeft,
    Top,
    TopRight,
    Right,
    BottomRight,
    Bottom,
    BottomLeft,
    Left,
}

impl BoxHandle {
    pub const ALL: [BoxHandle; 8] = [
        BoxHandle::TopLeft,
        BoxHandle::Top,
        BoxHandle::TopRight,
        BoxHandle::Right,
        BoxHandle::BottomRight,
        BoxHandle::Bottom,
        BoxHandle::BottomLeft,
        BoxHandle::Left,
    ];

    /// Which edges the handle moves: (x side, y side), -1 = min, 1 = max, 0 = none.
    pub fn sides(self) -> (i8, i8) {
        match self {
            BoxHandle::TopLeft => (-1, -1),
            BoxHandle::Top => (0, -1),
            BoxHandle::TopRight => (1, -1),
            BoxHandle::Right => (1, 0),
            BoxHandle::BottomRight => (1, 1),
            BoxHandle::Bottom => (0, 1),
            BoxHandle::BottomLeft => (-1, 1),
            BoxHandle::Left => (-1, 0),
        }
    }

    /// The handle's position on an axis-aligned `(x, y, w, h)` box.
    pub fn position(self, [x, y, w, h]: [f64; 4]) -> Point {
        let (sx, sy) = self.sides();
        [x + w * (sx as f64 + 1.0) * 0.5, y + h * (sy as f64 + 1.0) * 0.5]
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CropBox {
    /// `(x, y, w, h)` in canvas pixels; may extend past the canvas (to grow it).
    pub rect: [f64; 4],
    /// Degrees, clockwise. Shown and edited; [`apply_crop`] doesn't resample yet.
    pub rotation: f64,
    pub aspect: AspectRatio,
}

impl CropBox {
    pub fn full(canvas: (u32, u32)) -> Self {
        Self { rect: [0.0, 0.0, canvas.0 as f64, canvas.1 as f64], rotation: 0.0, aspect: AspectRatio::Free }
    }

    /// Drags `handle` to `to`, keeping the opposite side fixed and the aspect
    /// ratio when one is set. The box never inverts or drops below 1 px.
    pub fn drag(&mut self, handle: BoxHandle, to: Point) {
        let [x, y, w, h] = self.rect;
        let (mut x0, mut y0, mut x1, mut y1) = (x, y, x + w, y + h);
        let (sx, sy) = handle.sides();
        match sx {
            -1 => x0 = to[0].min(x1 - 1.0),
            1 => x1 = to[0].max(x0 + 1.0),
            _ => {}
        }
        match sy {
            -1 => y0 = to[1].min(y1 - 1.0),
            1 => y1 = to[1].max(y0 + 1.0),
            _ => {}
        }
        if let Some(r) = self.aspect.ratio() {
            let (mut nw, mut nh) = (x1 - x0, y1 - y0);
            // Edge handles drive one dimension; corners follow the larger change.
            if sy == 0 || (sx != 0 && nw / r >= nh) {
                nh = nw / r;
            } else {
                nw = nh * r;
            }
            match sx {
                -1 => x0 = x1 - nw,
                1 => x1 = x0 + nw,
                _ => {
                    let cx = (x0 + x1) * 0.5;
                    (x0, x1) = (cx - nw * 0.5, cx + nw * 0.5);
                }
            }
            match sy {
                -1 => y0 = y1 - nh,
                1 => y1 = y0 + nh,
                _ => {
                    let cy = (y0 + y1) * 0.5;
                    (y0, y1) = (cy - nh * 0.5, cy + nh * 0.5);
                }
            }
        }
        self.rect = [x0, y0, x1 - x0, y1 - y0];
    }

    /// Applies an aspect ratio, keeping the centre and the area's width.
    pub fn set_aspect(&mut self, aspect: AspectRatio) {
        self.aspect = aspect;
        if let Some(r) = aspect.ratio() {
            let [x, y, w, h] = self.rect;
            let (cy, nh) = (y + h * 0.5, w / r);
            self.rect = [x, cy - nh * 0.5, w, nh];
        }
    }

    /// The rule-of-thirds guides as line segments.
    pub fn thirds(&self) -> [(Point, Point); 4] {
        let [x, y, w, h] = self.rect;
        let v = |t: f64| ([x + w * t, y], [x + w * t, y + h]);
        let hz = |t: f64| ([x, y + h * t], [x + w, y + h * t]);
        [v(1.0 / 3.0), v(2.0 / 3.0), hz(1.0 / 3.0), hz(2.0 / 3.0)]
    }

    /// The canvas change this box describes, in whole pixels.
    pub fn plan(&self) -> CropPlan {
        // Size from the rounded width and height (not rounded edges), so a
        // 1:1 box stays square.
        let [x, y, w, h] = self.rect;
        CropPlan { offset: [x.round() as i32, y.round() as i32], size: (w.round().max(1.0) as u32, h.round().max(1.0) as u32) }
    }
}

/// New canvas size and where its top-left corner was on the old canvas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CropPlan {
    pub offset: [i32; 2],
    pub size: (u32, u32),
}

/// Moves the canvas window: every layer shifts by `-offset`, the canvas takes
/// the new size. Nothing is cut away. Vector layers are re-rasterised.
pub fn apply_crop(doc: &mut Document, plan: CropPlan) {
    let (dx, dy) = (-plan.offset[0], -plan.offset[1]);
    let canvas = (plan.size.0.clamp(1, painter_core::MAX_CANVAS_SIZE), plan.size.1.clamp(1, painter_core::MAX_CANVAS_SIZE));
    fn walk(layers: &mut [Layer], dx: i32, dy: i32, canvas: (u32, u32)) {
        for layer in layers {
            if let Some(mask) = &mut layer.mask {
                mask.tiles = shift(&mask.tiles, dx, dy);
            }
            match &mut layer.kind {
                LayerKind::Raster(tiles) => *tiles = shift(tiles, dx, dy),
                LayerKind::Vector(v) => {
                    for stroke in &mut v.strokes {
                        for p in &mut stroke.points {
                            p.x += dx as f32;
                            p.y += dy as f32;
                        }
                    }
                    for t in &mut v.texts {
                        t.transform(|[x, y]| [x + dx as f32, y + dy as f32], 1.0);
                    }
                    painter_core::vector::rasterize(v, canvas);
                }
                LayerKind::Folder(folder) => walk(&mut folder.children, dx, dy, canvas),
                LayerKind::Gradient(g) => {
                    for p in [&mut g.start, &mut g.end] {
                        p[0] += dx as f32;
                        p[1] += dy as f32;
                    }
                }
                LayerKind::Fill { .. } | LayerKind::Tone(_) | LayerKind::Adjustment(_) => {}
            }
        }
    }
    walk(&mut doc.layers, dx, dy, canvas);
    if let Some(selection) = &doc.selection {
        doc.selection = Some(shift(selection, dx, dy));
    }
    doc.width = canvas.0;
    doc.height = canvas.1;
}

/// A copy of `tiles` moved by whole pixels. Whole-tile moves just rename tiles.
pub fn shift(tiles: &TileStore, dx: i32, dy: i32) -> TileStore {
    let t = TILE_SIZE as i32;
    if dx % t == 0 && dy % t == 0 {
        let mut out = TileStore::new();
        for (coord, tile) in tiles.iter() {
            out.insert(TileCoord::new(coord.x + dx / t, coord.y + dy / t), (**tile).clone());
        }
        return out;
    }
    let mut out = TileStore::new();
    for (coord, tile) in tiles.iter() {
        let (ox, oy) = coord.origin();
        for y in 0..TILE_SIZE {
            for x in 0..TILE_SIZE {
                let px = tile.pixel(x, y);
                if px[3] == 0 {
                    continue;
                }
                put(&mut out, ox + x as i32 + dx, oy + y as i32 + dy, px);
            }
        }
    }
    out
}

fn put(store: &mut TileStore, x: i32, y: i32, px: [u8; 4]) {
    let coord = TileCoord::containing(x, y);
    let (ox, oy) = coord.origin();
    let i = (((y - oy) as u32 * TILE_SIZE + (x - ox) as u32) * 4) as usize;
    store.get_or_create_mut(coord).bytes_mut()[i..i + 4].copy_from_slice(&px);
}

/// Pixels lifted off a layer: premultiplied RGBA8, row-major.
#[derive(Debug, Clone, PartialEq)]
pub struct FloatingSelection {
    /// Top-left corner on the canvas it was copied from.
    pub origin: [i32; 2],
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Copies the selected part of `tiles` (all of it without a selection),
/// weighting each pixel by the selection's coverage. `None` when nothing is there.
pub fn copy(tiles: &TileStore, selection: Option<&TileStore>, canvas: (u32, u32)) -> Option<FloatingSelection> {
    let (w, h) = (canvas.0 as i32, canvas.1 as i32);
    let coverage = |x: i32, y: i32| selection.map_or(255, |s| s.pixel(x, y)[3]);
    let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    let mut pixels = Vec::new();
    for (coord, tile) in tiles.iter() {
        let (ox, oy) = coord.origin();
        for ty in 0..TILE_SIZE {
            for tx in 0..TILE_SIZE {
                let (x, y) = (ox + tx as i32, oy + ty as i32);
                if x < 0 || y < 0 || x >= w || y >= h {
                    continue;
                }
                let px = tile.pixel(tx, ty);
                let c = coverage(x, y) as u32;
                if px[3] == 0 || c == 0 {
                    continue;
                }
                let px = px.map(|v| ((v as u32 * c + 127) / 255) as u8);
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
                pixels.push((x, y, px));
            }
        }
    }
    if pixels.is_empty() {
        return None;
    }
    let (width, height) = ((x1 - x0 + 1) as u32, (y1 - y0 + 1) as u32);
    let mut rgba = vec![0u8; (width * height * 4) as usize];
    for (x, y, px) in pixels {
        let i = (((y - y0) as u32 * width + (x - x0) as u32) * 4) as usize;
        rgba[i..i + 4].copy_from_slice(&px);
    }
    Some(FloatingSelection { origin: [x0, y0], width, height, rgba })
}

/// The rest of a cut: removes the selected coverage from `tiles`.
pub fn erase_selected(tiles: &TileStore, selection: Option<&TileStore>) -> TileStore {
    let Some(selection) = selection else {
        return TileStore::new();
    };
    let mut out = tiles.clone();
    for (coord, _) in tiles.iter() {
        let (ox, oy) = coord.origin();
        let tile: &mut Tile = out.get_or_create_mut(coord);
        for ty in 0..TILE_SIZE {
            for tx in 0..TILE_SIZE {
                let keep = 255 - selection.pixel(ox + tx as i32, oy + ty as i32)[3] as u32;
                let i = ((ty * TILE_SIZE + tx) * 4) as usize;
                for v in &mut tile.bytes_mut()[i..i + 4] {
                    *v = ((*v as u32 * keep + 127) / 255) as u8;
                }
            }
        }
    }
    out
}

/// Where a paste goes: Ctrl+V centres on the view, Ctrl+Shift+V keeps the origin.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PasteMode {
    Centered { view_center: Point },
    InPlace,
}

/// The floating selection as layer tiles at its paste position.
pub fn paste_tiles(floating: &FloatingSelection, mode: PasteMode) -> TileStore {
    let origin = match mode {
        PasteMode::InPlace => floating.origin,
        PasteMode::Centered { view_center } => [
            (view_center[0] - floating.width as f64 * 0.5).round() as i32,
            (view_center[1] - floating.height as f64 * 0.5).round() as i32,
        ],
    };
    let mut out = TileStore::new();
    for y in 0..floating.height {
        for x in 0..floating.width {
            let i = ((y * floating.width + x) * 4) as usize;
            let px = [floating.rgba[i], floating.rgba[i + 1], floating.rgba[i + 2], floating.rgba[i + 3]];
            if px[3] > 0 {
                put(&mut out, origin[0] + x as i32, origin[1] + y as i32, px);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x0: u32, y0: u32, size: u32, canvas: u32) -> TileStore {
        let rgba: Vec<u8> = (0..canvas * canvas)
            .flat_map(|i| {
                let (x, y) = (i % canvas, i / canvas);
                if (x0..x0 + size).contains(&x) && (y0..y0 + size).contains(&y) { [200, 0, 0, 255] } else { [0; 4] }
            })
            .collect();
        TileStore::from_rgba(canvas, canvas, &rgba)
    }

    #[test]
    fn crop_box_keeps_aspect_and_bounds() {
        let mut b = CropBox::full((400, 300));
        b.set_aspect(AspectRatio::SQUARE);
        assert_eq!(b.rect[2], b.rect[3]);
        b.drag(BoxHandle::BottomRight, [500.0, 260.0]);
        assert!((b.rect[2] - b.rect[3]).abs() < 1e-9, "square kept: {:?}", b.rect);
        b.aspect = AspectRatio::Free;
        b.drag(BoxHandle::Left, [10_000.0, 0.0]);
        assert!(b.rect[2] >= 1.0, "never inverts");
        let thirds = CropBox::full((300, 90)).thirds();
        assert_eq!(thirds[0].0, [100.0, 0.0]);
        assert_eq!(thirds[3].0, [0.0, 60.0]);
        assert_eq!(BoxHandle::Right.position([0.0, 0.0, 10.0, 20.0]), [10.0, 10.0]);
    }

    #[test]
    fn crop_moves_the_canvas_without_losing_pixels() {
        let mut doc = Document::new(128, 128);
        let active = doc.active;
        doc.find_mut(active).unwrap().kind = LayerKind::Raster(square(10, 10, 5, 128));
        apply_crop(&mut doc, CropPlan { offset: [8, 9], size: (50, 40) });
        assert_eq!(doc.canvas(), (50, 40));
        let tiles = doc.find(active).unwrap().tiles().unwrap();
        assert_eq!(tiles.pixel(2, 1), [200, 0, 0, 255]);
        assert_eq!(tiles.pixel(1, 1)[3], 0);
        // Growing back restores the original placement.
        apply_crop(&mut doc, CropPlan { offset: [-8, -9], size: (128, 128) });
        assert_eq!(doc.find(active).unwrap().tiles().unwrap().pixel(10, 10), [200, 0, 0, 255]);
        // Whole-tile shifts take the fast path and agree.
        let t = square(3, 4, 2, 64);
        assert_eq!(shift(&t, 64, 0).pixel(67, 4), t.pixel(3, 4));
    }

    #[test]
    fn copy_cut_and_paste() {
        let tiles = square(10, 10, 4, 64);
        // Select the left half of the square.
        let selection = square(0, 0, 12, 64);
        let floating = copy(&tiles, Some(&selection), (64, 64)).unwrap();
        assert_eq!((floating.origin, floating.width, floating.height), ([10, 10], 2, 2));
        let rest = erase_selected(&tiles, Some(&selection));
        assert_eq!(rest.pixel(10, 10)[3], 0);
        assert_eq!(rest.pixel(12, 12)[3], 255);
        let placed = paste_tiles(&floating, PasteMode::InPlace);
        assert_eq!(placed.pixel(11, 11), [200, 0, 0, 255]);
        let centred = paste_tiles(&floating, PasteMode::Centered { view_center: [32.0, 32.0] });
        assert_eq!(centred.pixel(31, 31)[3], 255);
        assert!(copy(&TileStore::new(), None, (64, 64)).is_none());
    }
}
