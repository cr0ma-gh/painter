//! Layer management in the app, as in Clip Studio's Layer palette: several
//! layers selected at once (Ctrl / Shift + click), dragged above, below or
//! into folders, duplicated, gathered into a folder, deleted, shown, locked
//! or marked as drafts together. Every change is one undo step.

use eframe::egui::Modifiers;
use crate::i18n::tr;
use painter_core::layer_ops::LayerDrop;
use painter_core::LayerId;

use crate::app::PainterApp;

impl PainterApp {
    /// The selected layers that still exist, top first; at least the active one.
    pub(crate) fn selected_layers(&self) -> Vec<LayerId> {
        let mut ids: Vec<LayerId> = self
            .doc
            .rows_top_down()
            .into_iter()
            .map(|(id, _)| id)
            .filter(|id| *id == self.doc.active || self.layer_selection.contains(id))
            .collect();
        if ids.is_empty() {
            ids.push(self.doc.active);
        }
        ids
    }

    /// A click on a layer row: alone, Ctrl / Cmd toggles it, Shift adds the
    /// range from the active layer.
    pub(crate) fn click_layer(&mut self, id: LayerId, modifiers: Modifiers) {
        if modifiers.command || modifiers.ctrl {
            let mut ids = self.selected_layers();
            if ids.contains(&id) && ids.len() > 1 {
                ids.retain(|x| *x != id);
                if self.doc.active == id {
                    self.doc.active = ids[0];
                }
            } else if !ids.contains(&id) {
                ids.push(id);
                self.doc.active = id;
            }
            self.layer_selection = ids;
        } else if modifiers.shift {
            let rows: Vec<LayerId> = self.doc.rows_top_down().into_iter().map(|(id, _)| id).collect();
            let (a, b) = (rows.iter().position(|x| *x == self.doc.active), rows.iter().position(|x| *x == id));
            if let (Some(a), Some(b)) = (a, b) {
                self.layer_selection = rows[a.min(b)..=a.max(b)].to_vec();
            }
            self.doc.active = id;
        } else {
            self.layer_selection = vec![id];
            if self.doc.active != id {
                self.edit_mask = false;
            }
            self.doc.active = id;
        }
    }

    /// After a structural change: one undo step was recorded before it; redraw everything.
    fn layers_changed(&mut self) {
        self.dirty = true;
        self.recomposite = true;
        self.resync_gpu();
    }

    /// Drops the selection (or `dragged` alone, if it isn't selected) at `to`.
    pub(crate) fn drop_layers(&mut self, dragged: LayerId, to: LayerDrop) {
        let selected = self.selected_layers();
        let ids = if selected.contains(&dragged) { selected } else { vec![dragged] };
        let before = self.doc.clone();
        if self.doc.move_layers(&ids, to) {
            self.history.push(&before);
            self.push_comic_history(self.comic.clone());
            self.layers_changed();
        }
    }

    pub(crate) fn duplicate_selected_layers(&mut self) {
        let ids = self.selected_layers();
        self.checkpoint();
        let map = self.doc.duplicate_layers(&ids);
        // 3D layers keep their scene; rulers follow their layer.
        for (old, new) in &map {
            if let Some(scene) = self.scenes.get(old).cloned() {
                self.scenes.insert(*new, scene);
            }
            let copies: Vec<_> = self.doc.rulers.rulers.iter().filter(|r| r.layer == Some(old.0)).cloned().collect();
            for mut r in copies {
                r.layer = Some(new.0);
                self.doc.rulers.rulers.push(r);
            }
        }
        let new: Vec<LayerId> = map.iter().map(|(_, n)| *n).filter(|n| self.doc.selection_roots(&[*n]).len() == 1).collect();
        self.layer_selection = self.doc.selection_roots(&new);
        self.status = format!("Duplicated {} layer(s)", self.doc.selection_roots(&ids).len());
        self.layers_changed();
    }

    pub(crate) fn group_selected_layers(&mut self) {
        let ids = self.selected_layers();
        self.checkpoint();
        if let Some(folder) = self.doc.group_layers(&ids, "Folder") {
            self.layer_selection = vec![folder];
            self.layers_changed();
        }
    }

    pub(crate) fn delete_selected_layers(&mut self) {
        let ids = self.selected_layers();
        let before = self.doc.clone();
        let n = self.doc.delete_layers(&ids);
        if n == 0 {
            self.status = tr("Can't delete the last layer").into();
            return;
        }
        self.history.push(&before);
        self.push_comic_history(self.comic.clone());
        self.layer_selection = vec![self.doc.active];
        self.edit_mask = false;
        self.layers_changed();
    }

    /// Moves the selected layers one step up or down, past their neighbour.
    pub(crate) fn move_selected_layers(&mut self, up: bool) {
        let ids = self.doc.selection_roots(&self.selected_layers());
        if ids.len() == 1 {
            return self.move_active_layer_id(ids[0], up);
        }
        let rows: Vec<LayerId> = self.doc.rows_top_down().into_iter().map(|(id, _)| id).collect();
        // The first row above the topmost (or below the bottommost) that isn't selected or inside a selection.
        let free = |id: &LayerId| !ids.iter().any(|s| self.doc.contains_in(*s, *id));
        let target = if up {
            let top = rows.iter().position(|r| *r == ids[0]).unwrap_or(0);
            rows[..top].iter().rev().find(|r| free(r)).map(|t| LayerDrop::Above(*t))
        } else {
            let bottom = rows.iter().rposition(|r| ids.iter().any(|s| self.doc.contains_in(*s, *r))).unwrap_or(0);
            rows[bottom + 1..].iter().find(|r| free(r)).map(|t| LayerDrop::Below(*t))
        };
        if let Some(to) = target {
            self.drop_layers(ids[0], to);
        }
    }

    fn move_active_layer_id(&mut self, id: LayerId, up: bool) {
        let active = self.doc.active;
        self.doc.active = id;
        self.move_active_layer(up);
        self.doc.active = active;
    }

    /// Sets a flag on every selected layer (one undo step).
    pub(crate) fn set_on_selected_layers(&mut self, apply: impl Fn(&mut painter_core::Layer)) {
        self.checkpoint();
        for id in self.selected_layers() {
            if let Some(layer) = self.doc.find_mut(id) {
                apply(layer);
            }
        }
        self.recomposite = true;
    }
}
