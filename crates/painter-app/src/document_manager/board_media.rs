//! Moving pictures on the reference board: animated GIFs and videos. Each
//! plays in a loop on its own clock; Space (or the right-click menu) pauses
//! the selected ones, ← / → step a video five seconds, Home restarts.

use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::Duration;

use eframe::egui::{self, Color32, ColorImage, TextureHandle, TextureId, TextureOptions};

use super::board::{Board, Item, ItemId, Source};
use super::video::VideoStream;

/// Longest side of an animated GIF's frames, and most frames kept.
const ANIMATION_SIZE: u32 = 1024;
const MAX_FRAMES: usize = 400;

/// When an item's media time started, or where it was paused.
#[derive(Clone, Copy, Debug)]
struct Clock {
    /// Seconds (egui time) at which media time 0 was.
    origin: f64,
    paused_at: Option<u64>,
}

impl Clock {
    fn ms(&self, now: f64) -> u64 {
        self.paused_at.unwrap_or(((now - self.origin).max(0.0) * 1000.0) as u64)
    }
}

struct Animation {
    /// The bytes and look it was made from.
    source: usize,
    grey: bool,
    frames: Vec<(TextureHandle, u32)>,
    total_ms: u64,
}

impl Animation {
    fn frame_at(&self, ms: u64) -> &TextureHandle {
        let mut t = ms % self.total_ms.max(1);
        for (texture, delay) in &self.frames {
            if t < *delay as u64 {
                return texture;
            }
            t -= *delay as u64;
        }
        &self.frames[self.frames.len() - 1].0
    }
}

struct AnimationJob {
    source: usize,
    grey: bool,
    rx: Receiver<Option<Vec<(ColorImage, u32)>>>,
}

struct Video {
    path: String,
    stream: VideoStream,
    texture: Option<TextureHandle>,
    grey: bool,
    failed: bool,
}

#[derive(Default)]
pub(crate) struct Media {
    clocks: HashMap<ItemId, Clock>,
    animations: HashMap<ItemId, Animation>,
    jobs: HashMap<ItemId, AnimationJob>,
    videos: HashMap<ItemId, Video>,
    /// egui time of the last update, for keys.
    now: f64,
}

/// Grey by luminance, keeping alpha (premultiplied stays premultiplied).
fn to_grey(image: &mut ColorImage) {
    for c in &mut image.pixels {
        let [r, g, b, a] = c.to_array();
        let l = ((r as u32 * 299 + g as u32 * 587 + b as u32 * 114) / 1000) as u8;
        *c = Color32::from_rgba_premultiplied(l, l, l, a);
    }
}

/// An animated GIF's frames, premultiplied and scaled for the screen.
fn decode_animation(bytes: &[u8], grey: bool) -> Option<Vec<(ColorImage, u32)>> {
    let gif = painter_core::gif::decode(bytes).ok()?;
    let frames = gif
        .frames
        .into_iter()
        .take(MAX_FRAMES)
        .map(|f| {
            let mut px = f.rgba;
            for p in px.as_chunks_mut::<4>().0 {
                *p = painter_core::blend::premultiply(*p);
            }
            let mut image = super::split_viewport::downscale(&px, gif.width, gif.height, ANIMATION_SIZE);
            if grey {
                to_grey(&mut image);
            }
            (image, f.delay_ms)
        })
        .collect();
    Some(frames)
}

/// Whether an item plays (an animated GIF or a video).
fn is_gif(item: &Item) -> bool {
    matches!(&item.source, Source::Image { bytes, .. } if painter_core::gif::is_gif(bytes))
}

impl Media {
    /// Keeps decoders and textures in step with the board; returns a message
    /// for the status bar when something can't play.
    pub(crate) fn update(&mut self, ctx: &egui::Context, board: &Board) -> Option<String> {
        let now = ctx.input(|i| i.time);
        self.now = now;
        let mut message = None;
        let alive = |id: &ItemId| board.find(*id).is_some();
        self.clocks.retain(|id, _| alive(id));
        self.animations.retain(|id, _| alive(id));
        self.jobs.retain(|id, _| alive(id));
        self.videos
            .retain(|id, v| board.find(*id).is_some_and(|i| matches!(&i.source, Source::Video { path, .. } if *path == v.path)));
        let mut playing = false;
        for item in &board.items {
            let moving = is_gif(item) || matches!(item.source, Source::Video { .. });
            if !moving {
                continue;
            }
            let clock = *self.clocks.entry(item.id).or_insert(Clock { origin: now, paused_at: None });
            playing |= clock.paused_at.is_none();
            let ms = clock.ms(now);
            match &item.source {
                Source::Image { bytes, .. } => {
                    let source = Arc::as_ptr(bytes) as usize;
                    let current = self.animations.get(&item.id).is_some_and(|a| a.source == source && a.grey == item.grey);
                    if current {
                        continue;
                    }
                    if let Some(job) = self.jobs.get(&item.id) {
                        if job.source == source && job.grey == item.grey {
                            match job.rx.try_recv() {
                                Ok(Some(frames)) if frames.len() > 1 => {
                                    let total_ms = frames.iter().map(|f| f.1 as u64).sum();
                                    let frames = frames
                                        .into_iter()
                                        .enumerate()
                                        .map(|(k, (img, d))| {
                                            (ctx.load_texture(format!("board-anim-{}-{k}", item.id), img, TextureOptions::LINEAR), d)
                                        })
                                        .collect();
                                    self.animations.insert(item.id, Animation { source, grey: item.grey, frames, total_ms });
                                    self.jobs.remove(&item.id);
                                }
                                // A still GIF: the ordinary picture shows it.
                                Ok(_) | Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                                    self.jobs.remove(&item.id);
                                    self.animations
                                        .insert(item.id, Animation { source, grey: item.grey, frames: Vec::new(), total_ms: 0 });
                                }
                                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                            }
                            continue;
                        }
                    }
                    let (tx, rx) = channel();
                    let (bytes, grey) = (bytes.clone(), item.grey);
                    std::thread::spawn(move || {
                        let _ = tx.send(decode_animation(&bytes, grey));
                    });
                    self.jobs.insert(item.id, AnimationJob { source, grey: item.grey, rx });
                }
                Source::Video { path, .. } => {
                    let video = self.videos.entry(item.id).or_insert_with(|| Video {
                        path: path.clone(),
                        stream: VideoStream::start(path.into()),
                        texture: None,
                        grey: item.grey,
                        failed: false,
                    });
                    if video.failed {
                        continue;
                    }
                    match video.stream.frame_for(ms) {
                        Ok(Some(mut image)) => {
                            if item.grey {
                                to_grey(&mut image);
                            }
                            video.grey = item.grey;
                            match &mut video.texture {
                                Some(t) => t.set(image, TextureOptions::LINEAR),
                                None => {
                                    video.texture =
                                        Some(ctx.load_texture(format!("board-video-{}", item.id), image, TextureOptions::LINEAR))
                                }
                            }
                        }
                        Ok(None) => {}
                        Err(e) => {
                            video.failed = true;
                            message = Some(format!("Video {path}: {e}"));
                        }
                    }
                }
                Source::Document { .. } => {}
            }
        }
        if playing || !self.jobs.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(15));
        }
        message
    }

    /// What to draw for a moving item now (None: the ordinary picture, or not ready).
    pub(crate) fn texture(&self, item: &Item) -> Option<TextureId> {
        let ms = self.clocks.get(&item.id)?.ms(self.now);
        match &item.source {
            Source::Video { .. } => self.videos.get(&item.id)?.texture.as_ref().map(|t| t.id()),
            _ => {
                let a = self.animations.get(&item.id).filter(|a| !a.frames.is_empty())?;
                Some(a.frame_at(ms).id())
            }
        }
    }

    /// For the progress line under a moving item: how far through its loop, and whether it plays.
    pub(crate) fn progress(&self, item: &Item) -> Option<(f32, bool)> {
        let clock = self.clocks.get(&item.id)?;
        let ms = clock.ms(self.now);
        let total = match &item.source {
            Source::Video { .. } => self.videos.get(&item.id)?.stream.info?.duration_ms,
            _ => self.animations.get(&item.id).filter(|a| !a.frames.is_empty())?.total_ms,
        };
        Some(((ms % total.max(1)) as f32 / total.max(1) as f32, clock.paused_at.is_none()))
    }

    pub(crate) fn is_moving(&self, id: ItemId) -> bool {
        self.clocks.contains_key(&id)
    }

    pub(crate) fn is_paused(&self, id: ItemId) -> bool {
        self.clocks.get(&id).is_some_and(|c| c.paused_at.is_some())
    }

    /// Pauses or plays every moving one of `ids`.
    pub(crate) fn toggle(&mut self, ids: &[ItemId]) {
        let now = self.now;
        // All follow the first: a mixed selection pauses together.
        let pause = ids.iter().find_map(|id| self.clocks.get(id)).is_some_and(|c| c.paused_at.is_none());
        for id in ids {
            if let Some(c) = self.clocks.get_mut(id) {
                let ms = c.ms(now);
                if pause {
                    c.paused_at = Some(ms);
                } else {
                    c.origin = now - ms as f64 / 1000.0;
                    c.paused_at = None;
                }
            }
        }
    }

    /// Moves `ids` to `ms` into their loop (or by `delta` ms from where they are).
    pub(crate) fn seek(&mut self, ids: &[ItemId], to: Option<u64>, delta: i64) {
        let now = self.now;
        for id in ids {
            let Some(c) = self.clocks.get_mut(id) else { continue };
            let current = c.ms(now);
            let ms = to.unwrap_or_else(|| (current as i64 + delta).max(0) as u64);
            match &mut c.paused_at {
                Some(p) => *p = ms,
                None => c.origin = now - ms as f64 / 1000.0,
            }
            if let Some(v) = self.videos.get_mut(id) {
                v.stream.seek(ms);
            }
        }
    }
}
