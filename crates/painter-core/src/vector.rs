//! Vector layers: strokes are kept as input points and re-rasterised on change.

use crate::brush::Dab;
use crate::layer::{VectorLayer, VectorStroke};
use crate::raster::paint_stroke;
use crate::stroke::StrokeEngine;
use crate::tile::TileStore;

/// The dabs a recorded stroke produces (deterministic replay).
pub fn stroke_dabs(stroke: &VectorStroke) -> Vec<Dab> {
    let mut engine = StrokeEngine::with_seed(&stroke.settings, stroke.seed);
    let mut dabs = Vec::new();
    for &point in &stroke.points {
        dabs.extend(engine.add(point));
    }
    dabs.extend(engine.finish());
    dabs
}

/// Rebuilds the layer cache from its strokes.
pub fn rasterize(layer: &mut VectorLayer, canvas: (u32, u32)) {
    let mut cache = TileStore::new();
    for stroke in &layer.strokes {
        paint_stroke(&mut cache, &stroke_dabs(stroke), stroke.color, &stroke.settings, canvas, false, None);
    }
    layer.cache = cache;
}

/// Vector eraser: removes every stroke touched by the eraser dabs and
/// re-rasterises. Returns whether anything was removed.
pub fn erase_touching(layer: &mut VectorLayer, eraser: &[Dab], canvas: (u32, u32)) -> bool {
    if eraser.is_empty() {
        return false;
    }
    let bounds = |dabs: &[Dab]| {
        dabs.iter().fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |b, d| {
            [b[0].min(d.x - d.radius), b[1].min(d.y - d.radius), b[2].max(d.x + d.radius), b[3].max(d.y + d.radius)]
        })
    };
    let eb = bounds(eraser);
    let before = layer.strokes.len();
    layer.strokes.retain(|stroke| {
        let dabs = stroke_dabs(stroke);
        let sb = bounds(&dabs);
        if sb[0] > eb[2] || sb[2] < eb[0] || sb[1] > eb[3] || sb[3] < eb[1] {
            return true;
        }
        !dabs.iter().any(|d| eraser.iter().any(|e| (d.x - e.x).hypot(d.y - e.y) < d.radius + e.radius))
    });
    let changed = layer.strokes.len() != before;
    if changed {
        rasterize(layer, canvas);
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brush::BrushSettings;
    use crate::smoothing::StrokePoint;

    fn stroke(y: f32) -> VectorStroke {
        VectorStroke {
            points: (0..20).map(|i| StrokePoint::new(5.0 + i as f32 * 4.0, y, 1.0)).collect(),
            settings: BrushSettings { stabilization: 0, ..BrushSettings::g_pen() },
            color: [0.0, 0.0, 0.0],
            seed: 0,
        }
    }

    #[test]
    fn eraser_removes_only_touched_strokes() {
        let mut layer = VectorLayer { strokes: vec![stroke(10.0), stroke(60.0)], cache: TileStore::new() };
        rasterize(&mut layer, (100, 100));
        assert!(layer.cache.pixel(40, 10)[3] > 200);
        let eraser = [Dab { x: 40.0, y: 12.0, radius: 3.0, alpha: 1.0, angle: 0.0, thickness: 1.0, mirror: false }];
        assert!(erase_touching(&mut layer, &eraser, (100, 100)));
        assert_eq!(layer.strokes.len(), 1);
        assert_eq!(layer.cache.pixel(40, 10)[3], 0);
        assert!(layer.cache.pixel(40, 60)[3] > 200);
        assert!(!erase_touching(&mut layer, &eraser, (100, 100)));
    }
}
