//! Mapping the document onto GPU planes.

use painter_core::{Document, TileStore};

use crate::atlas::Plane;
use crate::canvas::Upload;

/// Every plane the document currently has.
pub fn live_planes(doc: &Document) -> Vec<Plane> {
    let mut planes: Vec<Plane> = doc.raster_layer_ids().into_iter().map(Plane::Color).collect();
    planes.extend(doc.masked_layer_ids().into_iter().map(Plane::Mask));
    if doc.selection.is_some() {
        planes.push(Plane::Selection);
    }
    planes
}

/// Clears `plane` and re-uploads every tile of `tiles`.
pub fn plane_uploads(plane: Plane, tiles: &TileStore) -> Vec<Upload> {
    let mut uploads = vec![Upload::Clear(plane)];
    uploads.extend(tiles.iter().map(|(coord, tile)| Upload::Tile { plane, coord, tile: Some(tile.clone()) }));
    uploads
}

/// A full re-upload of the document (after load, undo, ...).
pub fn full_upload(doc: &Document) -> Vec<Upload> {
    let mut uploads = Vec::new();
    for plane in live_planes(doc) {
        let tiles = match plane {
            Plane::Color(id) => doc.find(id).and_then(|l| l.tiles()),
            Plane::Mask(id) => doc.find(id).and_then(|l| l.mask.as_ref()).map(|m| &m.tiles),
            Plane::Selection => doc.selection.as_ref(),
        };
        if let Some(tiles) = tiles {
            uploads.extend(plane_uploads(plane, tiles));
        }
    }
    uploads
}
