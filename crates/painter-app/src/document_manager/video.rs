//! Videos on the reference board: decoded on a background thread, frame by
//! frame, and shown at their own pace, looping.
//!
//! - Windows: Media Foundation, which plays what the system plays (MP4 / H.264,
//!   HEVC with its extension, WMV, AVI, MOV with the codecs installed…).
//! - Elsewhere: the `ffmpeg` program when it's installed (on the PATH), which
//!   plays nearly everything.
//!
//! Frames come scaled down to [`MAX_SIDE`] (a reference doesn't need more),
//! as premultiplied RGBA.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{sync_channel, Receiver, Sender, SyncSender, TryRecvError};

use eframe::egui::ColorImage;

/// Longest side of decoded frames.
pub(crate) const MAX_SIDE: u32 = 1280;

/// Video files the board takes.
pub(crate) const VIDEO_EXTENSIONS: [&str; 8] = ["mp4", "m4v", "mov", "webm", "mkv", "avi", "wmv", "mpg"];

pub(crate) fn is_video_file(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| VIDEO_EXTENSIONS.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

/// A video's size (as decoded) and length.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct VideoInfo {
    pub width: u32,
    pub height: u32,
    pub duration_ms: u64,
}

/// A decoded frame. `ms` keeps counting across loops, so playback time only grows.
pub(crate) struct Frame {
    pub generation: u64,
    pub ms: u64,
    pub image: ColorImage,
}

enum Command {
    /// Go to this time (ms), for this generation of frames.
    Seek(u64, u64),
}

/// The size `w`×`h` comes out at, longest side at most [`MAX_SIDE`], even sizes.
fn scaled(w: u32, h: u32) -> (u32, u32) {
    let k = (MAX_SIDE as f32 / w.max(h).max(1) as f32).min(1.0);
    let even = |v: f32| ((v.round() as u32) / 2 * 2).max(2);
    (even(w as f32 * k), even(h as f32 * k))
}

/// One decoder: reads frames from a position on.
trait Decoder {
    fn info(&self) -> VideoInfo;
    /// Moves to `ms`.
    fn seek(&mut self, ms: u64) -> Result<(), String>;
    /// The next frame and its time (ms), or `None` at the end.
    fn next(&mut self) -> Result<Option<(u64, ColorImage)>, String>;
}

fn open(path: &Path) -> Result<Box<dyn Decoder>, String> {
    #[cfg(windows)]
    {
        mf::MediaFoundation::open(path).map(|d| Box::new(d) as Box<dyn Decoder>)
    }
    #[cfg(not(windows))]
    {
        ffmpeg::Ffmpeg::open(path).map(|d| Box::new(d) as Box<dyn Decoder>)
    }
}

/// Reads a video's size and length.
pub(crate) fn probe(path: &Path) -> Result<VideoInfo, String> {
    Ok(open(path)?.info())
}

/// A video playing on the board.
pub(crate) struct VideoStream {
    /// Known once the decoder has opened the file.
    pub info: Option<VideoInfo>,
    info_rx: Receiver<VideoInfo>,
    commands: Sender<Command>,
    frames: Receiver<Result<Frame, String>>,
    generation: u64,
    /// The next frame, waiting for its time.
    next: Option<Frame>,
}

impl VideoStream {
    /// Opens and decodes `path` on a thread of its own.
    pub(crate) fn start(path: PathBuf) -> Self {
        let (commands, command_rx) = std::sync::mpsc::channel();
        // A few frames ahead: decoding waits while they aren't shown.
        let (frame_tx, frames) = sync_channel(3);
        let (info_tx, info_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || decode_loop(&path, command_rx, frame_tx, info_tx));
        Self { info: None, info_rx, commands, frames, generation: 0, next: None }
    }

    /// Restarts from `ms` (frames of the old position are dropped).
    pub(crate) fn seek(&mut self, ms: u64) {
        self.generation += 1;
        self.next = None;
        let _ = self.commands.send(Command::Seek(ms, self.generation));
    }

    /// The latest frame due by playback time `ms`, if a new one is.
    pub(crate) fn frame_for(&mut self, ms: u64) -> Result<Option<ColorImage>, String> {
        if let Ok(info) = self.info_rx.try_recv() {
            self.info = Some(info);
        }
        let mut shown = None;
        loop {
            let frame = match self.next.take() {
                Some(f) => f,
                None => match self.frames.try_recv() {
                    Ok(Ok(f)) => f,
                    Ok(Err(e)) => return Err(e),
                    Err(TryRecvError::Empty) => return Ok(shown),
                    Err(TryRecvError::Disconnected) => return Err("the video decoder stopped".into()),
                },
            };
            if frame.generation != self.generation {
                continue;
            }
            if frame.ms > ms {
                self.next = Some(frame);
                return Ok(shown);
            }
            shown = Some(frame.image);
        }
    }
}

fn decode_loop(path: &Path, commands: Receiver<Command>, frames: SyncSender<Result<Frame, String>>, info: Sender<VideoInfo>) {
    let mut decoder = match open(path) {
        Ok(d) => d,
        Err(e) => {
            let _ = frames.send(Err(e));
            return;
        }
    };
    let _ = info.send(decoder.info());
    let duration = decoder.info().duration_ms.max(1);
    let (mut generation, mut base) = (0u64, 0u64);
    loop {
        // Commands first; the last seek wins.
        loop {
            match commands.try_recv() {
                Ok(Command::Seek(ms, g)) => {
                    generation = g;
                    base = ms - ms % duration;
                    if let Err(e) = decoder.seek(ms % duration) {
                        let _ = frames.send(Err(e));
                        return;
                    }
                }
                Err(TryRecvError::Empty) => break,
                // The board let go of the video.
                Err(TryRecvError::Disconnected) => return,
            }
        }
        match decoder.next() {
            Ok(Some((ms, image))) => {
                if frames.send(Ok(Frame { generation, ms: base + ms, image })).is_err() {
                    return;
                }
            }
            // The end: loop.
            Ok(None) => {
                base += duration;
                if let Err(e) = decoder.seek(0) {
                    let _ = frames.send(Err(e));
                    return;
                }
            }
            Err(e) => {
                let _ = frames.send(Err(e));
                return;
            }
        }
    }
}

/// Straight RGBA (alpha 255) as a texture image.
fn image(w: u32, h: u32, rgba: Vec<u8>) -> ColorImage {
    ColorImage::from_rgba_premultiplied([w as usize, h as usize], &rgba)
}

/// Nearest-neighbour scaling of BGRX / RGBA rows (`stride` bytes apart) to `w`×`h` RGBA.
#[cfg(any(windows, test))]
#[allow(clippy::too_many_arguments)]
fn resample(src: &[u8], sw: u32, sh: u32, stride: usize, bgr: bool, bottom_up: bool, w: u32, h: u32) -> Vec<u8> {
    let mut out = vec![255u8; (w * h * 4) as usize];
    for y in 0..h {
        let sy = (y as u64 * sh as u64 / h as u64) as usize;
        let row = if bottom_up { sh as usize - 1 - sy } else { sy };
        for x in 0..w {
            let sx = (x as u64 * sw as u64 / w as u64) as usize;
            let at = row * stride + sx * 4;
            let Some(px) = src.get(at..at + 4) else { continue };
            let o = ((y * w + x) * 4) as usize;
            let (r, b) = if bgr { (px[2], px[0]) } else { (px[0], px[2]) };
            out[o..o + 3].copy_from_slice(&[r, px[1], b]);
        }
    }
    out
}

#[cfg(windows)]
mod mf {
    use super::*;
    use windows::core::{GUID, HSTRING, PROPVARIANT};
    use windows::Win32::Media::MediaFoundation::*;
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

    const STREAM: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;

    pub(super) struct MediaFoundation {
        reader: IMFSourceReader,
        size: (u32, u32),
        out: (u32, u32),
        stride: i32,
        duration_ms: u64,
        /// Seeking lands on the keyframe before: frames before this time are skipped.
        skip_before: u64,
    }

    impl MediaFoundation {
        pub(super) fn open(path: &Path) -> Result<Self, String> {
            let err = |e: windows::core::Error| format!("{}: {}", path.display(), e.message());
            unsafe {
                // Already initialised on this thread is fine.
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                MFStartup(MF_VERSION, MFSTARTUP_FULL).map_err(err)?;
                let mut attributes = None;
                MFCreateAttributes(&mut attributes, 1).map_err(err)?;
                let attributes = attributes.ok_or("no attributes")?;
                // Lets the reader convert decoded YUV to RGB for us.
                attributes.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1).map_err(err)?;
                let url = HSTRING::from(path.as_os_str());
                let reader = MFCreateSourceReaderFromURL(&url, &attributes).map_err(err)?;
                reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false).map_err(err)?;
                reader.SetStreamSelection(STREAM, true).map_err(|_| format!("{}: no video in the file", path.display()))?;
                let wanted = MFCreateMediaType().map_err(err)?;
                wanted.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(err)?;
                wanted.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32).map_err(err)?;
                reader.SetCurrentMediaType(STREAM, None, &wanted).map_err(err)?;
                let current = reader.GetCurrentMediaType(STREAM).map_err(err)?;
                let packed = current.GetUINT64(&MF_MT_FRAME_SIZE).map_err(err)?;
                let size = ((packed >> 32) as u32, packed as u32);
                let stride = current.GetUINT32(&MF_MT_DEFAULT_STRIDE).map(|s| s as i32).unwrap_or(size.0 as i32 * 4);
                let duration = reader.GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION).ok();
                let duration_ms = duration.as_ref().and_then(|d| u64::try_from(d).ok()).unwrap_or(0) / 10_000;
                Ok(Self { reader, size, out: scaled(size.0, size.1), stride, duration_ms, skip_before: 0 })
            }
        }
    }

    impl Decoder for MediaFoundation {
        fn info(&self) -> VideoInfo {
            VideoInfo { width: self.out.0, height: self.out.1, duration_ms: self.duration_ms }
        }

        fn seek(&mut self, ms: u64) -> Result<(), String> {
            self.skip_before = ms;
            let at = PROPVARIANT::from(ms as i64 * 10_000);
            unsafe { self.reader.SetCurrentPosition(&GUID::zeroed(), &at) }.map_err(|e| e.message())
        }

        fn next(&mut self) -> Result<Option<(u64, ColorImage)>, String> {
            unsafe {
                loop {
                    let (mut flags, mut time, mut sample) = (0u32, 0i64, None);
                    self.reader
                        .ReadSample(STREAM, 0, None, Some(&mut flags), Some(&mut time), Some(&mut sample))
                        .map_err(|e| e.message())?;
                    if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                        return Ok(None);
                    }
                    // Gaps and format changes come without a sample.
                    let Some(sample) = sample else { continue };
                    let ms = (time.max(0) / 10_000) as u64;
                    // Up to the time asked for (a frame early is fine).
                    if ms + 45 < self.skip_before {
                        continue;
                    }
                    let buffer = sample.ConvertToContiguousBuffer().map_err(|e| e.message())?;
                    let (mut data, mut len) = (std::ptr::null_mut(), 0u32);
                    buffer.Lock(&mut data, None, Some(&mut len)).map_err(|e| e.message())?;
                    let bytes = std::slice::from_raw_parts(data, len as usize);
                    let (w, h) = self.size;
                    let rgba =
                        resample(bytes, w, h, self.stride.unsigned_abs() as usize, true, self.stride < 0, self.out.0, self.out.1);
                    let _ = buffer.Unlock();
                    return Ok(Some((ms, image(self.out.0, self.out.1, rgba))));
                }
            }
        }
    }
}

#[cfg(not(windows))]
mod ffmpeg {
    use super::*;
    use std::io::Read;
    use std::process::{Child, Command, Stdio};

    pub(super) struct Ffmpeg {
        path: PathBuf,
        info: VideoInfo,
        fps: f64,
        child: Option<Child>,
        /// Time of the next frame read, and how many have been read since the seek.
        start_ms: u64,
        read: u64,
    }

    /// "Duration: 00:01:02.50" and "1920x1080" and "29.97 fps" from ffmpeg's report.
    fn parse_report(text: &str) -> Option<(u32, u32, u64, f64)> {
        let d = text.split("Duration: ").nth(1)?.split(',').next()?.trim();
        let parts: Vec<f64> = d.split(':').filter_map(|p| p.parse().ok()).collect();
        let duration = parts.iter().fold(0.0, |acc, v| acc * 60.0 + v);
        let video = text.lines().find(|l| l.contains("Video:"))?;
        // "1920x1080" (not a codec tag like "0x31637661").
        let size = video.split([',', ' ']).filter(|t| !t.starts_with("0x")).find_map(|t| {
            let (w, h) = t.split_once('x')?;
            let (w, h) = (w.parse::<u32>().ok()?, h.trim_end_matches(|c: char| !c.is_ascii_digit()).parse::<u32>().ok()?);
            (w > 0 && h > 0 && w <= 16384 && h <= 16384).then_some((w, h))
        })?;
        let fps = video.split(',').find_map(|t| t.trim().strip_suffix(" fps")?.parse::<f64>().ok()).unwrap_or(25.0);
        Some((size.0, size.1, (duration * 1000.0) as u64, fps))
    }

    impl Ffmpeg {
        pub(super) fn open(path: &Path) -> Result<Self, String> {
            let report = Command::new("ffmpeg")
                .args(["-hide_banner", "-i"])
                .arg(path)
                .stdin(Stdio::null())
                .output()
                .map_err(|_| "videos need ffmpeg here: install it (it plays nearly every format) and try again".to_string())?;
            let text = String::from_utf8_lossy(&report.stderr);
            let (w, h, duration_ms, fps) =
                parse_report(&text).ok_or_else(|| format!("{}: no video ffmpeg can read", path.display()))?;
            let (ow, oh) = scaled(w, h);
            let mut me = Self {
                path: path.to_path_buf(),
                info: VideoInfo { width: ow, height: oh, duration_ms },
                fps,
                child: None,
                start_ms: 0,
                read: 0,
            };
            me.seek(0)?;
            Ok(me)
        }
    }

    impl Drop for Ffmpeg {
        fn drop(&mut self) {
            if let Some(mut c) = self.child.take() {
                let _ = c.kill();
                let _ = c.wait();
            }
        }
    }

    impl Decoder for Ffmpeg {
        fn info(&self) -> VideoInfo {
            self.info
        }

        fn seek(&mut self, ms: u64) -> Result<(), String> {
            if let Some(mut c) = self.child.take() {
                let _ = c.kill();
                let _ = c.wait();
            }
            let scale = format!("scale={}:{}", self.info.width, self.info.height);
            let child = Command::new("ffmpeg")
                .args(["-v", "error", "-ss", &format!("{:.3}", ms as f64 / 1000.0), "-i"])
                .arg(&self.path)
                .args(["-an", "-vf", &scale, "-f", "rawvideo", "-pix_fmt", "rgba", "-"])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| format!("ffmpeg: {e}"))?;
            self.child = Some(child);
            (self.start_ms, self.read) = (ms, 0);
            Ok(())
        }

        fn next(&mut self) -> Result<Option<(u64, ColorImage)>, String> {
            let Some(child) = &mut self.child else { return Ok(None) };
            let Some(out) = child.stdout.as_mut() else { return Ok(None) };
            let (w, h) = (self.info.width, self.info.height);
            let mut rgba = vec![0u8; (w * h * 4) as usize];
            if out.read_exact(&mut rgba).is_err() {
                return Ok(None);
            }
            let ms = self.start_ms + (self.read as f64 * 1000.0 / self.fps.max(1.0)) as u64;
            self.read += 1;
            Ok(Some((ms, image(w, h, rgba))))
        }
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn reads_ffmpeg_reports() {
            let text = "  Duration: 00:01:02.50, start: 0.000000, bitrate: 1205 kb/s\n  Stream #0:0(und): Video: h264 (High) (avc1 / 0x31637661), yuv420p(tv, bt709), 1920x1080 [SAR 1:1 DAR 16:9], 1000 kb/s, 29.97 fps, 29.97 tbr";
            assert_eq!(super::parse_report(text), Some((1920, 1080, 62_500, 29.97)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_scale_down_to_even_sizes() {
        assert_eq!(scaled(1920, 1080), (1280, 720));
        assert_eq!(scaled(640, 360), (640, 360));
        assert_eq!(scaled(1081, 1921), (720, 1280));
        // BGRX bottom-up rows become RGBA top-down.
        let src = [1, 2, 3, 0, 4, 5, 6, 0]; // 1×2: bottom row first
        assert_eq!(resample(&src, 1, 2, 4, true, true, 1, 2), [6, 5, 4, 255, 3, 2, 1, 255]);
    }
}

/// `PAINTER_VIDEO_PROBE=clip.mp4 cargo test -p painter-app video_probe -- --ignored`
/// decodes a real file and saves its first frame and one from the middle
/// beside it (`.first.png`, `.middle.png`).
#[cfg(test)]
#[test]
#[ignore]
fn video_probe() {
    let path = PathBuf::from(std::env::var("PAINTER_VIDEO_PROBE").expect("PAINTER_VIDEO_PROBE"));
    let mut d = open(&path).unwrap();
    let info = d.info();
    eprintln!("{info:?}");
    let save = |img: &ColorImage, suffix: &str| {
        let rgba: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
        image::RgbaImage::from_raw(img.size[0] as u32, img.size[1] as u32, rgba).unwrap().save(path.with_extension(suffix)).unwrap();
    };
    let (t0, first) = d.next().unwrap().unwrap();
    save(&first, "first.png");
    d.seek(info.duration_ms / 2).unwrap();
    let (t1, middle) = d.next().unwrap().unwrap();
    save(&middle, "middle.png");
    let mut count = 2;
    while d.next().unwrap().is_some() {
        count += 1;
    }
    eprintln!("first at {t0} ms, middle at {t1} ms, {count} frames read");
    assert!(t1 + 200 >= info.duration_ms / 2);
}
