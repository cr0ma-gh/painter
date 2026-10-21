//! The reference board (like PureRef): pictures laid out freely on an
//! endless surface (moved, scaled, rotated, mirrored, faded, shown in grey)
//! and ink marks drawn over them to point things out. A mark drawn over a
//! picture belongs to it and follows it when it moves, scales or turns.
//!
//! Board units are pixels at 100 % zoom. A picture's local coordinates are
//! its own pixels from its centre, before mirroring; marks on a picture are
//! stored in them.
//!
//! The board is saved as a ZIP (`.refboard`): `board.json` (layout and marks)
//! and the pictures' original files under `images/`. Pictures of open
//! documents are live and aren't saved.

use std::sync::Arc;

use painter_core::zipfile::{ZipReader, ZipWriter};
use serde::{Deserialize, Serialize};

pub(crate) const EXTENSION: &str = "refboard";
const FORMAT: &str = "painter-reference-board";
const VERSION: u32 = 1;

pub(crate) type ItemId = u64;

/// What a picture shows.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Source {
    /// An image file's bytes (PNG, JPEG, BMP), kept as they came.
    Image { name: String, ext: String, bytes: Arc<Vec<u8>> },
    /// A live picture of another open document (by its tab's slot id).
    Document { slot: u64 },
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Item {
    pub(crate) id: ItemId,
    pub(crate) source: Source,
    /// Its size in pixels.
    pub(crate) size: [f32; 2],
    /// Centre, in board units.
    pub(crate) pos: [f32; 2],
    pub(crate) scale: f32,
    /// Radians, clockwise on screen.
    pub(crate) rotation: f32,
    /// Mirrored left-right.
    pub(crate) flip: bool,
    pub(crate) opacity: f32,
    pub(crate) grey: bool,
    /// Can't be selected by clicking on the board (unlock from the menu).
    pub(crate) locked: bool,
}

impl Item {
    /// A local point (pixels from the centre, unmirrored) on the board.
    pub(crate) fn to_board(&self, [x, y]: [f32; 2]) -> [f32; 2] {
        let x = if self.flip { -x } else { x } * self.scale;
        let y = y * self.scale;
        let (s, c) = self.rotation.sin_cos();
        [self.pos[0] + x * c - y * s, self.pos[1] + x * s + y * c]
    }

    /// A board point in local coordinates.
    pub(crate) fn to_local(&self, [bx, by]: [f32; 2]) -> [f32; 2] {
        let (dx, dy) = (bx - self.pos[0], by - self.pos[1]);
        let (s, c) = self.rotation.sin_cos();
        let (x, y) = ((dx * c + dy * s) / self.scale, (-dx * s + dy * c) / self.scale);
        [if self.flip { -x } else { x }, y]
    }

    pub(crate) fn contains(&self, p: [f32; 2]) -> bool {
        let [x, y] = self.to_local(p);
        x.abs() <= self.size[0] / 2.0 && y.abs() <= self.size[1] / 2.0
    }

    /// Corners on the board: top left, top right, bottom right, bottom left (as shown).
    pub(crate) fn corners(&self) -> [[f32; 2]; 4] {
        let [w, h] = [self.size[0] / 2.0, self.size[1] / 2.0];
        let f = if self.flip { -1.0 } else { 1.0 };
        [[-w * f, -h], [w * f, -h], [w * f, h], [-w * f, h]].map(|p| self.to_board(p))
    }

    /// The axis-aligned box around it on the board: min and max.
    pub(crate) fn bounds(&self) -> ([f32; 2], [f32; 2]) {
        let c = self.corners();
        let min = [c.iter().map(|p| p[0]).fold(f32::MAX, f32::min), c.iter().map(|p| p[1]).fold(f32::MAX, f32::min)];
        let max = [c.iter().map(|p| p[0]).fold(f32::MIN, f32::max), c.iter().map(|p| p[1]).fold(f32::MIN, f32::max)];
        (min, max)
    }
}

/// An ink mark: a polyline drawn over the board.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Mark {
    /// In the picture's local coordinates when `on` is set, else board units.
    pub(crate) points: Vec<[f32; 2]>,
    /// Straight sRGBA.
    pub(crate) color: [u8; 4],
    /// Line width, in the same units as the points.
    pub(crate) width: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) on: Option<ItemId>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Board {
    /// Bottom to top.
    pub(crate) items: Vec<Item>,
    /// Drawn over every picture, in order.
    pub(crate) marks: Vec<Mark>,
    next_id: ItemId,
}

/// Distance from `p` to the segment `a`–`b`.
fn segment_distance(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let (abx, aby) = (b[0] - a[0], b[1] - a[1]);
    let len2 = abx * abx + aby * aby;
    let t = if len2 > 0.0 { (((p[0] - a[0]) * abx + (p[1] - a[1]) * aby) / len2).clamp(0.0, 1.0) } else { 0.0 };
    let (x, y) = (a[0] + abx * t - p[0], a[1] + aby * t - p[1]);
    (x * x + y * y).sqrt()
}

impl Board {
    pub(crate) fn find(&self, id: ItemId) -> Option<&Item> {
        self.items.iter().find(|i| i.id == id)
    }

    pub(crate) fn find_mut(&mut self, id: ItemId) -> Option<&mut Item> {
        self.items.iter_mut().find(|i| i.id == id)
    }

    /// Adds a picture of `size` pixels centred at `at`, on top; returns its id.
    pub(crate) fn add(&mut self, source: Source, size: [f32; 2], at: [f32; 2]) -> ItemId {
        self.next_id += 1;
        let id = self.next_id;
        self.items.push(Item {
            id,
            source,
            size,
            pos: at,
            scale: 1.0,
            rotation: 0.0,
            flip: false,
            opacity: 1.0,
            grey: false,
            locked: false,
        });
        id
    }

    /// The topmost unlocked picture at `p`.
    pub(crate) fn item_at(&self, p: [f32; 2]) -> Option<ItemId> {
        self.items.iter().rev().find(|i| !i.locked && i.contains(p)).map(|i| i.id)
    }

    /// The topmost picture at `p`, locked or not (marks drawn there belong to it).
    pub(crate) fn any_item_at(&self, p: [f32; 2]) -> Option<ItemId> {
        self.items.iter().rev().find(|i| i.contains(p)).map(|i| i.id)
    }

    /// Removes pictures and the marks on them.
    pub(crate) fn remove(&mut self, ids: &[ItemId]) {
        self.items.retain(|i| !ids.contains(&i.id));
        self.marks.retain(|m| m.on.is_none_or(|on| !ids.contains(&on)));
    }

    /// Puts `ids` above (or below) every other picture, keeping their order.
    pub(crate) fn restack(&mut self, ids: &[ItemId], to_front: bool) {
        let (moved, rest): (Vec<Item>, Vec<Item>) = std::mem::take(&mut self.items).into_iter().partition(|i| ids.contains(&i.id));
        self.items = if to_front { rest.into_iter().chain(moved).collect() } else { moved.into_iter().chain(rest).collect() };
    }

    /// Copies of `ids`, offset by `offset`, with their marks; returns the new ids.
    pub(crate) fn duplicate(&mut self, ids: &[ItemId], offset: [f32; 2]) -> Vec<ItemId> {
        let originals: Vec<Item> = self.items.iter().filter(|i| ids.contains(&i.id)).cloned().collect();
        let mut new_ids = Vec::new();
        for mut item in originals {
            let old = item.id;
            self.next_id += 1;
            item.id = self.next_id;
            item.pos = [item.pos[0] + offset[0], item.pos[1] + offset[1]];
            let marks: Vec<Mark> =
                self.marks.iter().filter(|m| m.on == Some(old)).cloned().map(|m| Mark { on: Some(item.id), ..m }).collect();
            new_ids.push(item.id);
            self.items.push(item);
            self.marks.extend(marks);
        }
        new_ids
    }

    /// A mark's points on the board.
    pub(crate) fn mark_points(&self, mark: &Mark) -> Vec<[f32; 2]> {
        match mark.on.and_then(|id| self.find(id)) {
            Some(item) => mark.points.iter().map(|p| item.to_board(*p)).collect(),
            None => mark.points.clone(),
        }
    }

    /// A mark's line width on the board.
    pub(crate) fn mark_width(&self, mark: &Mark) -> f32 {
        mark.width * mark.on.and_then(|id| self.find(id)).map_or(1.0, |i| i.scale)
    }

    /// Adds a mark drawn with board points; it belongs to the picture under
    /// its first point, if any. Returns false for an empty line.
    pub(crate) fn add_mark(&mut self, points: Vec<[f32; 2]>, color: [u8; 4], width: f32) -> bool {
        let Some(first) = points.first().copied() else { return false };
        let mark = match self.any_item_at(first).and_then(|id| self.find(id)) {
            Some(item) => Mark {
                points: points.iter().map(|p| item.to_local(*p)).collect(),
                color,
                width: width / item.scale,
                on: Some(item.id),
            },
            None => Mark { points, color, width, on: None },
        };
        self.marks.push(mark);
        true
    }

    /// Removes the marks passing within `radius` of `p`; returns how many.
    pub(crate) fn erase_at(&mut self, p: [f32; 2], radius: f32) -> usize {
        let hit: Vec<bool> = self
            .marks
            .iter()
            .map(|m| {
                let pts = self.mark_points(m);
                let reach = radius + self.mark_width(m) / 2.0;
                match pts.as_slice() {
                    [only] => segment_distance(p, *only, *only) <= reach,
                    _ => pts.windows(2).any(|w| segment_distance(p, w[0], w[1]) <= reach),
                }
            })
            .collect();
        let before = self.marks.len();
        let mut i = 0;
        self.marks.retain(|_| {
            i += 1;
            !hit[i - 1]
        });
        before - self.marks.len()
    }

    /// The box around every picture and free mark, if there's anything.
    pub(crate) fn bounds(&self) -> Option<([f32; 2], [f32; 2])> {
        let mut pts: Vec<[f32; 2]> = self.items.iter().flat_map(|i| i.corners()).collect();
        pts.extend(self.marks.iter().flat_map(|m| self.mark_points(m)));
        let first = *pts.first()?;
        Some(pts.iter().fold((first, first), |(lo, hi), p| ([lo[0].min(p[0]), lo[1].min(p[1])], [hi[0].max(p[0]), hi[1].max(p[1])])))
    }

    /// Lays the pictures (`ids`, or all) out in rows, in their order, so the
    /// whole is roughly square, keeping their sizes, from the top left of
    /// where they were.
    pub(crate) fn arrange(&mut self, ids: Option<&[ItemId]>, gap: f32) {
        let chosen: Vec<usize> = (0..self.items.len()).filter(|i| ids.is_none_or(|ids| ids.contains(&self.items[*i].id))).collect();
        if chosen.is_empty() {
            return;
        }
        let boxes: Vec<([f32; 2], [f32; 2])> = chosen.iter().map(|i| self.items[*i].bounds()).collect();
        let origin = boxes.iter().fold([f32::MAX, f32::MAX], |o, (lo, _)| [o[0].min(lo[0]), o[1].min(lo[1])]);
        let area: f32 = boxes.iter().map(|(lo, hi)| (hi[0] - lo[0] + gap) * (hi[1] - lo[1] + gap)).sum();
        let widest = boxes.iter().map(|(lo, hi)| hi[0] - lo[0]).fold(0.0, f32::max);
        let row_width = area.sqrt().max(widest);
        let (mut x, mut y, mut row_h) = (0.0f32, 0.0f32, 0.0f32);
        for (k, i) in chosen.iter().enumerate() {
            let (lo, hi) = boxes[k];
            let (w, h) = (hi[0] - lo[0], hi[1] - lo[1]);
            if x > 0.0 && x + w > row_width {
                x = 0.0;
                y += row_h + gap;
                row_h = 0.0;
            }
            // Move the picture so its box starts at (x, y).
            let item = &mut self.items[*i];
            item.pos = [item.pos[0] + origin[0] + x - lo[0], item.pos[1] + origin[1] + y - lo[1]];
            x += w + gap;
            row_h = row_h.max(h);
        }
    }

    // ---- file -------------------------------------------------------------------------

    /// The board as a `.refboard` file. Pictures of open documents are left out.
    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        let mut zip = ZipWriter::new();
        let mut items = Vec::new();
        for item in &self.items {
            let Source::Image { name, ext, bytes } = &item.source else { continue };
            let file = format!("images/{}.{ext}", item.id);
            zip.add(&file, bytes, false);
            items.push(ItemFile {
                id: item.id,
                name: name.clone(),
                file,
                size: item.size,
                pos: item.pos,
                scale: item.scale,
                rotation: item.rotation,
                flip: item.flip,
                opacity: item.opacity,
                grey: item.grey,
                locked: item.locked,
            });
        }
        let kept: Vec<ItemId> = items.iter().map(|i| i.id).collect();
        let marks = self.marks.iter().filter(|m| m.on.is_none_or(|on| kept.contains(&on))).cloned().collect();
        let file = BoardFile { format: FORMAT.into(), version: VERSION, next_id: self.next_id, items, marks };
        zip.add("board.json", &serde_json::to_vec_pretty(&file).expect("plain data"), true);
        zip.finish()
    }

    pub(crate) fn from_bytes(bytes: Vec<u8>) -> Result<Self, String> {
        let zip = ZipReader::new(bytes)?;
        let json = zip.read("board.json").ok_or("not a reference board")?;
        let file: BoardFile = serde_json::from_slice(&json).map_err(|e| e.to_string())?;
        if file.format != FORMAT {
            return Err("not a reference board".into());
        }
        if file.version > VERSION {
            return Err(format!("made by a newer Painter (board version {})", file.version));
        }
        let mut board = Board { next_id: file.next_id, ..Board::default() };
        for i in file.items {
            let Some(bytes) = zip.read(&i.file) else { continue };
            let ext = i.file.rsplit('.').next().unwrap_or("png").to_string();
            board.next_id = board.next_id.max(i.id);
            board.items.push(Item {
                id: i.id,
                source: Source::Image { name: i.name, ext, bytes: Arc::new(bytes) },
                size: i.size,
                pos: i.pos,
                scale: i.scale.max(1e-4),
                rotation: i.rotation,
                flip: i.flip,
                opacity: i.opacity.clamp(0.0, 1.0),
                grey: i.grey,
                locked: i.locked,
            });
        }
        let ids: Vec<ItemId> = board.items.iter().map(|i| i.id).collect();
        board.marks = file.marks.into_iter().filter(|m| m.on.is_none_or(|on| ids.contains(&on))).collect();
        Ok(board)
    }
}

#[derive(Serialize, Deserialize)]
struct ItemFile {
    id: ItemId,
    name: String,
    file: String,
    size: [f32; 2],
    pos: [f32; 2],
    scale: f32,
    rotation: f32,
    #[serde(default)]
    flip: bool,
    #[serde(default = "one")]
    opacity: f32,
    #[serde(default)]
    grey: bool,
    #[serde(default)]
    locked: bool,
}

fn one() -> f32 {
    1.0
}

#[derive(Serialize, Deserialize)]
struct BoardFile {
    format: String,
    version: u32,
    next_id: ItemId,
    items: Vec<ItemFile>,
    marks: Vec<Mark>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(board: &mut Board, w: f32, h: f32, at: [f32; 2]) -> ItemId {
        board.add(Source::Image { name: "a".into(), ext: "png".into(), bytes: Arc::new(vec![1, 2, 3]) }, [w, h], at)
    }

    fn close(a: [f32; 2], b: [f32; 2]) -> bool {
        (a[0] - b[0]).abs() < 1e-3 && (a[1] - b[1]).abs() < 1e-3
    }

    #[test]
    fn transforms_round_trip_and_hit_test() {
        let mut b = Board::default();
        let id = image(&mut b, 100.0, 50.0, [10.0, 20.0]);
        let item = b.find_mut(id).unwrap();
        item.scale = 2.0;
        item.rotation = std::f32::consts::FRAC_PI_2;
        item.flip = true;
        let item = b.find(id).unwrap().clone();
        // Local (50, 0), the right edge of the picture: mirrored, doubled, turned a quarter.
        assert!(close(item.to_board([50.0, 0.0]), [10.0, 20.0 - 100.0]));
        for p in [[3.0, -7.0], [-40.0, 20.0]] {
            assert!(close(item.to_local(item.to_board(p)), p));
        }
        assert!(b.item_at([10.0, 20.0 + 90.0]).is_some(), "inside the turned picture");
        assert!(b.item_at([10.0 + 90.0, 20.0]).is_none());
        let (lo, hi) = item.bounds();
        assert!(close(lo, [-40.0, -80.0]) && close(hi, [60.0, 120.0]));
    }

    #[test]
    fn marks_follow_their_picture_and_erase() {
        let mut b = Board::default();
        let id = image(&mut b, 100.0, 100.0, [0.0, 0.0]);
        assert!(b.add_mark(vec![[0.0, 0.0], [10.0, 0.0]], [255, 0, 0, 255], 4.0));
        assert!(b.add_mark(vec![[500.0, 500.0], [510.0, 500.0]], [0, 0, 255, 255], 4.0));
        assert_eq!(b.marks[0].on, Some(id));
        assert_eq!(b.marks[1].on, None);
        // Moving and scaling the picture carries its mark.
        let item = b.find_mut(id).unwrap();
        item.pos = [100.0, 0.0];
        item.scale = 2.0;
        let pts = b.mark_points(&b.marks[0]);
        assert!(close(pts[1], [120.0, 0.0]) && (b.mark_width(&b.marks[0]) - 8.0).abs() < 1e-4);
        assert_eq!(b.erase_at([505.0, 503.0], 2.0), 1);
        assert_eq!(b.marks.len(), 1);
        // Duplicating copies the marks; removing takes them away.
        let copy = b.duplicate(&[id], [300.0, 0.0])[0];
        assert_eq!(b.marks.iter().filter(|m| m.on == Some(copy)).count(), 1);
        b.remove(&[id]);
        assert_eq!(b.marks.len(), 1);
        assert!(!b.add_mark(Vec::new(), [0; 4], 1.0));
    }

    #[test]
    fn stacking_locking_and_arranging() {
        let mut b = Board::default();
        let a = image(&mut b, 100.0, 100.0, [0.0, 0.0]);
        let c = image(&mut b, 100.0, 100.0, [20.0, 0.0]);
        assert_eq!(b.item_at([10.0, 0.0]), Some(c));
        b.restack(&[a], true);
        assert_eq!(b.item_at([10.0, 0.0]), Some(a));
        b.find_mut(a).unwrap().locked = true;
        assert_eq!(b.item_at([10.0, 0.0]), Some(c), "locked pictures let clicks through");
        assert_eq!(b.any_item_at([10.0, 0.0]), Some(a));

        let mut b = Board::default();
        for k in 0..4 {
            image(&mut b, 100.0, 50.0, [k as f32 * 7.0, 0.0]);
        }
        b.arrange(None, 10.0);
        let boxes: Vec<_> = b.items.iter().map(|i| i.bounds()).collect();
        for (i, x) in boxes.iter().enumerate() {
            for y in &boxes[i + 1..] {
                let apart = x.1[0] <= y.0[0] || y.1[0] <= x.0[0] || x.1[1] <= y.0[1] || y.1[1] <= x.0[1];
                assert!(apart, "no overlaps after arranging");
            }
        }
        let (lo, hi) = b.bounds().unwrap();
        assert!(hi[0] - lo[0] < 400.0, "more than one row");
    }

    #[test]
    fn saves_and_loads() {
        let mut b = Board::default();
        let id = image(&mut b, 30.0, 40.0, [5.0, 6.0]);
        b.find_mut(id).unwrap().grey = true;
        b.add(Source::Document { slot: 9 }, [10.0, 10.0], [0.0, 0.0]);
        b.add_mark(vec![[0.0, 0.0], [1.0, 1.0]], [1, 2, 3, 255], 2.0);
        b.add_mark(vec![[900.0, 0.0]], [1, 2, 3, 255], 2.0);
        let back = Board::from_bytes(b.to_bytes()).unwrap();
        assert_eq!(back.items.len(), 1, "live documents aren't saved");
        assert_eq!(back.items[0], b.items[0]);
        assert_eq!(back.marks.len(), 1, "the mark on the document went with it");
        assert!(Board::from_bytes(b"nope".to_vec()).is_err());
        // New ids don't reuse saved ones.
        let mut back = back;
        assert!(image(&mut back, 1.0, 1.0, [0.0, 0.0]) > id);
    }
}
