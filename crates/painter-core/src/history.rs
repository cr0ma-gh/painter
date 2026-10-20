//! Snapshot undo/redo.
//!
//! A snapshot clones the layer tree. Tiles are `Arc`-shared, so a snapshot
//! costs one pointer per tile plus layer metadata, not pixel data. The canvas
//! size is part of it, so cropping undoes too.

use crate::document::Document;
use crate::layer::{Layer, LayerId};
use crate::tile::TileStore;

#[derive(Clone)]
struct Snapshot {
    size: (u32, u32),
    layers: Vec<Layer>,
    active: LayerId,
    selection: Option<TileStore>,
}

impl Snapshot {
    fn of(doc: &Document) -> Self {
        Self { size: doc.canvas(), layers: doc.layers.clone(), active: doc.active, selection: doc.selection.clone() }
    }

    fn restore(self, doc: &mut Document) {
        (doc.width, doc.height) = self.size;
        doc.layers = self.layers;
        doc.active = self.active;
        doc.selection = self.selection;
    }
}

pub struct History {
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    limit: usize,
}

impl History {
    pub fn new(limit: usize) -> Self {
        Self { undo: Vec::new(), redo: Vec::new(), limit: limit.max(1) }
    }

    /// Records the state *before* an edit.
    pub fn push(&mut self, doc: &Document) {
        self.undo.push(Snapshot::of(doc));
        if self.undo.len() > self.limit {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    pub fn undo(&mut self, doc: &mut Document) -> bool {
        let Some(snapshot) = self.undo.pop() else {
            return false;
        };
        self.redo.push(Snapshot::of(doc));
        snapshot.restore(doc);
        true
    }

    pub fn redo(&mut self, doc: &mut Document) -> bool {
        let Some(snapshot) = self.redo.pop() else {
            return false;
        };
        self.undo.push(Snapshot::of(doc));
        snapshot.restore(doc);
        true
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undo_redo_round_trip() {
        let mut doc = Document::new(10, 10);
        let mut history = History::new(10);
        history.push(&doc);
        doc.find_mut(doc.active).unwrap().name = "Renamed".into();
        assert!(history.undo(&mut doc));
        assert_eq!(doc.active_layer().unwrap().name, "Layer 1");
        assert!(history.redo(&mut doc));
        assert_eq!(doc.active_layer().unwrap().name, "Renamed");
        assert!(!history.can_redo());

        // The canvas size comes back too (crop).
        history.push(&doc);
        doc.width = 4;
        assert!(history.undo(&mut doc));
        assert_eq!(doc.canvas(), (10, 10));
    }
}
