//! Layer management on a document, for several layers at once: moving them
//! above, below or into another layer (drag and drop), duplicating them
//! (folders with their contents, fresh ids), gathering them into a new
//! folder, deleting them, and a copy of the document without its draft layers
//! (for exports).
//!
//! A selection may name a folder and layers inside it; the operations act on
//! its "roots" (the selected layers not inside another selected folder), in
//! the panel's top-down order, and keep that order.

use crate::document::{find_in, locate_mut, Document};
use crate::layer::{Layer, LayerId, LayerKind};

/// Where dropped layers go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayerDrop {
    /// Just above this layer, in its folder.
    Above(LayerId),
    /// Just below this layer, in its folder.
    Below(LayerId),
    /// At the top of this folder.
    Into(LayerId),
}

impl Document {
    /// Whether `id` is `ancestor` or inside it.
    pub fn contains_in(&self, ancestor: LayerId, id: LayerId) -> bool {
        ancestor == id
            || self.find(ancestor).is_some_and(|a| match &a.kind {
                LayerKind::Folder(f) => find_in(&f.children, id).is_some(),
                _ => false,
            })
    }

    /// The selected layers not inside another selected folder, top first.
    pub fn selection_roots(&self, ids: &[LayerId]) -> Vec<LayerId> {
        self.rows_top_down()
            .into_iter()
            .map(|(id, _)| id)
            .filter(|id| ids.contains(id) && !ids.iter().any(|other| other != id && self.contains_in(*other, *id)))
            .collect()
    }

    /// Detaches a layer (with its contents) from the tree.
    fn take_layer(&mut self, id: LayerId) -> Option<Layer> {
        let (list, i) = locate_mut(&mut self.layers, id)?;
        Some(list.remove(i))
    }

    /// Puts a detached layer back at `at`; gives it back if `at` doesn't exist.
    fn place_layer(&mut self, layer: Layer, at: LayerDrop) -> Result<(), Box<Layer>> {
        match at {
            LayerDrop::Above(t) | LayerDrop::Below(t) => {
                let Some((list, i)) = locate_mut(&mut self.layers, t) else { return Err(Box::new(layer)) };
                let i = if matches!(at, LayerDrop::Above(_)) { i + 1 } else { i };
                list.insert(i, layer);
                Ok(())
            }
            LayerDrop::Into(f) => match self.find_mut(f).map(|l| &mut l.kind) {
                Some(LayerKind::Folder(folder)) => {
                    folder.children.push(layer);
                    Ok(())
                }
                _ => Err(Box::new(layer)),
            },
        }
    }

    /// Moves `ids` (see [`Document::selection_roots`]) to `to`, keeping
    /// their order. Refused when the target is one of them or inside one.
    pub fn move_layers(&mut self, ids: &[LayerId], to: LayerDrop) -> bool {
        let roots = self.selection_roots(ids);
        let target = match to {
            LayerDrop::Above(t) | LayerDrop::Below(t) | LayerDrop::Into(t) => t,
        };
        if roots.is_empty() || roots.iter().any(|r| self.contains_in(*r, target)) || self.find(target).is_none() {
            return false;
        }
        if matches!(to, LayerDrop::Into(_)) && !self.find(target).is_some_and(Layer::is_folder) {
            return false;
        }
        let taken: Vec<Layer> = roots.iter().filter_map(|id| self.take_layer(*id)).collect();
        // `taken` is top first. Above / into: place the lowest first, each next
        // one above it. Below: the highest first, each next one below it.
        let mut anchor = to;
        let order: Vec<Layer> = match to {
            LayerDrop::Below(_) => taken,
            _ => taken.into_iter().rev().collect(),
        };
        for layer in order {
            let id = layer.id;
            if let Err(layer) = self.place_layer(layer, anchor) {
                // Can't happen (checked above); keep the layer rather than lose it.
                self.layers.push(*layer);
            }
            anchor = match to {
                LayerDrop::Below(_) => LayerDrop::Below(id),
                _ => LayerDrop::Above(id),
            };
            if let LayerDrop::Into(_) = to {
                // Inside the folder now: the next one goes above it.
                anchor = LayerDrop::Above(id);
            }
        }
        true
    }

    /// A deep copy of `layer` with fresh ids from this document; records old → new ids.
    fn copy_with_new_ids(&mut self, layer: &Layer, ids: &mut Vec<(LayerId, LayerId)>) -> Layer {
        let mut copy = layer.clone();
        copy.id = self.alloc_id();
        ids.push((layer.id, copy.id));
        if let (LayerKind::Folder(src), LayerKind::Folder(dst)) = (&layer.kind, &mut copy.kind) {
            dst.children = src.children.iter().map(|c| self.copy_with_new_ids(c, ids)).collect();
        }
        copy
    }

    /// Duplicates `ids` (folders with their contents), each copy just above
    /// its original and named "… copy". Returns every old → new id (contents
    /// included); the copy of the active layer becomes active.
    pub fn duplicate_layers(&mut self, ids: &[LayerId]) -> Vec<(LayerId, LayerId)> {
        let mut map = Vec::new();
        for id in self.selection_roots(ids) {
            let Some(original) = self.find(id).cloned() else { continue };
            let mut copy = self.copy_with_new_ids(&original, &mut map);
            copy.name = format!("{} copy", original.name);
            let _ = self.place_layer(copy, LayerDrop::Above(id));
        }
        if let Some(&(_, new)) = map.iter().find(|(old, _)| *old == self.active) {
            self.active = new;
        }
        map
    }

    /// Gathers `ids` into a new folder where the topmost of them was; the
    /// folder becomes active. `None` when nothing is selected.
    pub fn group_layers(&mut self, ids: &[LayerId], name: &str) -> Option<LayerId> {
        let roots = self.selection_roots(ids);
        let top = *roots.first()?;
        let folder = self.alloc_id();
        self.place_layer(Layer::folder(folder, name), LayerDrop::Above(top)).ok()?;
        self.move_layers(&roots, LayerDrop::Into(folder));
        self.active = folder;
        Some(folder)
    }

    /// Deletes `ids` (folders with their contents). The last root layer stays.
    pub fn delete_layers(&mut self, ids: &[LayerId]) -> usize {
        self.selection_roots(ids).into_iter().filter(|id| self.delete_layer(*id)).count()
    }

    /// This document without its draft layers (and their contents), for
    /// exporting and printing. Layer data is shared, so this is cheap.
    pub fn without_drafts(&self) -> Document {
        fn strip(list: &mut Vec<Layer>) {
            list.retain(|l| !l.draft);
            for layer in list {
                if let LayerKind::Folder(f) = &mut layer.kind {
                    strip(&mut f.children);
                }
            }
        }
        let mut doc = self.clone();
        strip(&mut doc.layers);
        doc
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Paper, a folder "F" with "a" (bottom) and "b", then "c" and "d" on top.
    fn doc() -> (Document, [LayerId; 5]) {
        let mut doc = Document::new(64, 64);
        let ink = doc.active;
        doc.find_mut(ink).unwrap().name = "c".into();
        let f = doc.alloc_id();
        let (a, b, d) = (doc.alloc_id(), doc.alloc_id(), doc.alloc_id());
        let mut folder = Layer::folder(f, "F");
        if let LayerKind::Folder(x) = &mut folder.kind {
            x.children.push(Layer::raster(a, "a"));
            x.children.push(Layer::raster(b, "b"));
        }
        // Folder below "c".
        let i = doc.layers.iter().position(|l| l.id == ink).unwrap();
        doc.layers.insert(i, folder);
        doc.layers.push(Layer::raster(d, "d"));
        (doc, [f, a, b, ink, d])
    }

    fn names(doc: &Document) -> Vec<String> {
        doc.rows_top_down().iter().map(|(id, depth)| format!("{}{}", "-".repeat(*depth), doc.find(*id).unwrap().name)).collect()
    }

    #[test]
    fn moves_keep_order_and_refuse_cycles() {
        let (mut d, [f, a, b, c, top]) = doc();
        assert_eq!(names(&d), ["d", "c", "F", "-b", "-a", "Paper"]);
        // "d" and "c" into the folder: on top of it, still d over c.
        assert!(d.move_layers(&[c, top], LayerDrop::Into(f)));
        assert_eq!(names(&d), ["F", "-d", "-c", "-b", "-a", "Paper"]);
        // "a" above "d", "b" below "a".
        assert!(d.move_layers(&[a], LayerDrop::Above(top)));
        assert!(d.move_layers(&[b], LayerDrop::Below(a)));
        assert_eq!(names(&d), ["F", "-a", "-b", "-d", "-c", "Paper"]);
        // Two layers out, below the folder, keeping their order.
        assert!(d.move_layers(&[a, c], LayerDrop::Below(f)));
        assert_eq!(names(&d), ["F", "-b", "-d", "a", "c", "Paper"]);
        // A folder can't go into itself or next to something inside it.
        assert!(!d.move_layers(&[f], LayerDrop::Into(f)));
        assert!(!d.move_layers(&[f], LayerDrop::Above(b)));
        assert!(!d.move_layers(&[a], LayerDrop::Into(c)), "not a folder");
        // A folder and something inside it: the folder moves with its contents.
        assert!(d.move_layers(&[f, b], LayerDrop::Below(c)));
        assert_eq!(names(&d), ["a", "c", "F", "-b", "-d", "Paper"]);
    }

    #[test]
    fn duplicates_group_delete_and_drafts() {
        let (mut d, [f, a, _b, c, _top]) = doc();
        d.active = f;
        let map = d.duplicate_layers(&[f, a]);
        assert_eq!(map.len(), 3, "the folder and both children, once");
        assert_eq!(names(&d), ["d", "c", "F copy", "-b", "-a", "F", "-b", "-a", "Paper"]);
        let copy = map[0].1;
        assert_eq!(d.active, copy);
        assert!(map.iter().all(|(old, new)| old != new && d.find(*new).is_some()));
        // Copies share pixels until painted.
        assert_eq!(d.find(map[1].1).unwrap().tiles().unwrap().len(), d.find(map[1].0).unwrap().tiles().unwrap().len());

        let g = d.group_layers(&[c, copy], "Group").unwrap();
        assert_eq!(names(&d), ["d", "Group", "-c", "-F copy", "--b", "--a", "F", "-b", "-a", "Paper"]);
        assert_eq!(d.active, g);

        d.find_mut(c).unwrap().draft = true;
        d.find_mut(f).unwrap().draft = true;
        let out = d.without_drafts();
        assert_eq!(names(&out), ["d", "Group", "-F copy", "--b", "--a", "Paper"]);
        assert!(d.find(c).is_some(), "the document itself keeps them");

        assert_eq!(d.delete_layers(&[g, c]), 1, "the folder goes with its contents");
        assert_eq!(names(&d), ["d", "F", "-b", "-a", "Paper"]);
    }
}
