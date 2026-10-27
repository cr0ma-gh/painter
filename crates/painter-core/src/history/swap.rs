//! The history's swap file: tiles of old undo steps, deflated, on disk.
//!
//! Each tile is one record. A tile shared by several steps (or several
//! coordinates) is written once and counted; the record goes when the last
//! step that names it is undone or forgotten. Tiles read back are shared with
//! any copy still alive in RAM, so paging in never duplicates pixels.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use crate::tile::{Tile, TILE_BYTES};

/// A tile's place in the swap file.
pub(super) type RecordId = u64;

struct Record {
    offset: u64,
    len: u64,
    /// Pages (step, store, coordinate) naming this record.
    refs: usize,
}

/// The tile's address: it identifies the tile while a strong or weak `Arc`
/// to it lives (the allocation can't be reused until then).
pub(super) fn key(tile: &Arc<Tile>) -> usize {
    Arc::as_ptr(tile) as usize
}

pub(super) struct SwapFile {
    dir: PathBuf,
    /// Opened on the first write; `Mutex` so `&self` can read it back.
    file: Option<(PathBuf, Mutex<File>)>,
    /// A write or the file's creation failed: keep everything in RAM from now on.
    broken: bool,
    end: u64,
    /// Space freed by forgotten records, `(offset, len)`.
    holes: Vec<(u64, u64)>,
    records: HashMap<RecordId, Record>,
    next_id: RecordId,
    /// Tiles in RAM known to be on disk already, so they're written once.
    by_tile: HashMap<usize, (Weak<Tile>, RecordId)>,
    /// The tile in RAM read from (or written to) a record, if still alive.
    by_record: HashMap<RecordId, Weak<Tile>>,
}

impl SwapFile {
    pub(super) fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            file: None,
            broken: false,
            end: 0,
            holes: Vec::new(),
            records: HashMap::new(),
            next_id: 0,
            by_tile: HashMap::new(),
            by_record: HashMap::new(),
        }
    }

    /// Where the next swap file goes; one already open stays where it is.
    pub(super) fn set_dir(&mut self, dir: PathBuf) {
        self.dir = dir;
        if self.records.is_empty() {
            self.close();
        }
        self.broken = false;
    }

    pub(super) fn is_broken(&self) -> bool {
        self.broken
    }

    /// Bytes of tiles on disk, compressed.
    pub(super) fn bytes(&self) -> u64 {
        self.records.values().map(|r| r.len).sum()
    }

    /// Writes `tile` (or counts one more page of it, when already on disk).
    /// `None` when the disk fails: the tile has to stay in RAM.
    pub(super) fn store(&mut self, tile: &Arc<Tile>) -> Option<RecordId> {
        if let Some(&(_, id)) = self.by_tile.get(&key(tile)) {
            if let Some(record) = self.records.get_mut(&id) {
                record.refs += 1;
                return Some(id);
            }
            self.by_tile.remove(&key(tile));
        }
        if self.broken {
            return None;
        }
        let Some(id) = self.write(tile.bytes()) else {
            self.broken = true;
            return None;
        };
        self.by_tile.insert(key(tile), (Arc::downgrade(tile), id));
        self.by_record.insert(id, Arc::downgrade(tile));
        Some(id)
    }

    fn write(&mut self, bytes: &[u8]) -> Option<RecordId> {
        let mut deflate = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
        deflate.write_all(bytes).ok()?;
        let data = deflate.finish().ok()?;
        let len = data.len() as u64;
        if self.file.is_none() {
            self.file = Some(self.create().ok()?);
        }
        let offset = match self.holes.iter().position(|&(_, l)| l >= len) {
            Some(i) => {
                let (offset, l) = self.holes[i];
                if l == len {
                    self.holes.swap_remove(i);
                } else {
                    self.holes[i] = (offset + len, l - len);
                }
                offset
            }
            None => {
                self.end += len;
                self.end - len
            }
        };
        let (_, file) = self.file.as_ref()?;
        let mut file = file.lock().ok()?;
        file.seek(SeekFrom::Start(offset)).ok()?;
        file.write_all(&data).ok()?;
        let id = self.next_id;
        self.next_id += 1;
        self.records.insert(id, Record { offset, len, refs: 1 });
        Some(id)
    }

    fn create(&self) -> std::io::Result<(PathBuf, Mutex<File>)> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        std::fs::create_dir_all(&self.dir)?;
        let name = format!("{}-{}.swap", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
        let path = self.dir.join(name);
        let file = OpenOptions::new().read(true).write(true).create_new(true).open(&path)?;
        Ok((path, Mutex::new(file)))
    }

    /// The tile of record `id`: the copy still in RAM, else read from disk.
    pub(super) fn read(&self, id: RecordId) -> Option<Arc<Tile>> {
        if let Some(tile) = self.by_record.get(&id).and_then(Weak::upgrade) {
            return Some(tile);
        }
        let record = self.records.get(&id)?;
        let mut data = vec![0; record.len as usize];
        {
            let (_, file) = self.file.as_ref()?;
            let mut file = file.lock().ok()?;
            file.seek(SeekFrom::Start(record.offset)).ok()?;
            file.read_exact(&mut data).ok()?;
        }
        let mut tile = Tile::empty();
        let mut inflate = flate2::read::DeflateDecoder::new(data.as_slice());
        inflate.read_exact(tile.bytes_mut()).ok()?;
        debug_assert_eq!(tile.bytes().len(), TILE_BYTES);
        Some(Arc::new(tile))
    }

    /// Pages record `id` back in, for good: the tile, and one page fewer naming it.
    pub(super) fn take(&mut self, id: RecordId) -> Option<Arc<Tile>> {
        let tile = self.read(id);
        if let Some(tile) = &tile {
            self.by_tile.insert(key(tile), (Arc::downgrade(tile), id));
            self.by_record.insert(id, Arc::downgrade(tile));
        }
        self.release(id);
        tile
    }

    /// One page fewer names record `id`; the last one frees its space.
    pub(super) fn release(&mut self, id: RecordId) {
        let Some(record) = self.records.get_mut(&id) else { return };
        record.refs -= 1;
        if record.refs > 0 {
            return;
        }
        let record = self.records.remove(&id).expect("record just seen");
        if let Some(weak) = self.by_record.remove(&id) {
            let k = weak.as_ptr() as usize;
            if self.by_tile.get(&k).is_some_and(|(_, r)| *r == id) {
                self.by_tile.remove(&k);
            }
        }
        if self.records.is_empty() {
            // Nothing left on disk: start the file over.
            self.holes.clear();
            self.end = 0;
            if let Some((_, file)) = &self.file {
                if let Ok(file) = file.lock() {
                    let _ = file.set_len(0);
                }
            }
        } else {
            self.holes.push((record.offset, record.len));
        }
    }

    /// Forgets tiles no longer alive in RAM (they stay on disk).
    pub(super) fn prune(&mut self) {
        self.by_tile.retain(|_, (weak, _)| weak.strong_count() > 0);
        self.by_record.retain(|_, weak| weak.strong_count() > 0);
    }

    /// Forgets every record and deletes the file.
    pub(super) fn close(&mut self) {
        self.records.clear();
        self.holes.clear();
        self.by_tile.clear();
        self.by_record.clear();
        self.end = 0;
        if let Some((path, file)) = self.file.take() {
            drop(file);
            let _ = std::fs::remove_file(path);
        }
    }
}

impl Drop for SwapFile {
    fn drop(&mut self) {
        self.close();
    }
}
