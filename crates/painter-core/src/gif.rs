//! GIF reading (GIF87a / GIF89a, from the format's specification): every
//! frame composed onto the full canvas as straight RGBA, with its delay.
//! Handles global and local palettes, transparency, interlacing and the
//! three disposal methods, so animated GIFs play as browsers show them.

/// One composed frame: the whole canvas, straight RGBA, and how long it shows.
#[derive(Clone, Debug, PartialEq)]
pub struct GifFrame {
    pub rgba: Vec<u8>,
    /// Milliseconds (GIFs saying 0 or 10 ms get 100 ms, as browsers do).
    pub delay_ms: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Gif {
    pub width: u32,
    pub height: u32,
    pub frames: Vec<GifFrame>,
}

impl Gif {
    pub fn is_animated(&self) -> bool {
        self.frames.len() > 1
    }

    /// The whole loop's length in milliseconds.
    pub fn duration_ms(&self) -> u64 {
        self.frames.iter().map(|f| f.delay_ms as u64).sum()
    }

    /// The frame showing `ms` into the (looping) animation.
    pub fn frame_at(&self, ms: u64) -> usize {
        let total = self.duration_ms().max(1);
        let mut t = ms % total;
        for (i, f) in self.frames.iter().enumerate() {
            if t < f.delay_ms as u64 {
                return i;
            }
            t -= f.delay_ms as u64;
        }
        self.frames.len().saturating_sub(1)
    }
}

pub fn is_gif(bytes: &[u8]) -> bool {
    bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")
}

/// Most frames read (a safety limit for huge animations).
const MAX_FRAMES: usize = 2000;

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn u8(&mut self) -> Result<u8, String> {
        let b = *self.bytes.get(self.at).ok_or("truncated GIF")?;
        self.at += 1;
        Ok(b)
    }
    fn u16(&mut self) -> Result<u16, String> {
        Ok(self.u8()? as u16 | (self.u8()? as u16) << 8)
    }
    fn take(&mut self, n: usize) -> Result<&[u8], String> {
        let s = self.bytes.get(self.at..self.at + n).ok_or("truncated GIF")?;
        self.at += n;
        Ok(s)
    }
    fn palette(&mut self, packed: u8) -> Result<Vec<[u8; 3]>, String> {
        let n = 2usize << (packed & 7);
        Ok(self.take(n * 3)?.as_chunks::<3>().0.to_vec())
    }
    /// Data sub-blocks up to the terminator, joined.
    fn blocks(&mut self) -> Result<Vec<u8>, String> {
        let mut out = Vec::new();
        loop {
            let n = self.u8()? as usize;
            if n == 0 {
                return Ok(out);
            }
            out.extend_from_slice(self.take(n)?);
        }
    }
    fn skip_blocks(&mut self) -> Result<(), String> {
        loop {
            let n = self.u8()? as usize;
            if n == 0 {
                return Ok(());
            }
            self.take(n)?;
        }
    }
}

/// LZW-decodes `data` (minimum code size `min`) into at most `count` indices.
fn lzw(data: &[u8], min: u8, count: usize) -> Result<Vec<u8>, String> {
    if !(1..=11).contains(&min) {
        return Err("bad GIF code size".into());
    }
    let clear = 1usize << min;
    let end = clear + 1;
    // Each code: its prefix code and last byte; lengths for unrolling.
    let mut prefix = vec![0u16; 4096];
    let mut suffix = vec![0u8; 4096];
    let mut length = vec![0u16; 4096];
    for i in 0..clear {
        suffix[i] = i as u8;
        length[i] = 1;
    }
    let mut out = Vec::with_capacity(count);
    let (mut size, mut next) = (min as u32 + 1, end + 1);
    let mut prev: Option<usize> = None;
    let (mut acc, mut bits) = (0u32, 0u32);
    let mut stack = Vec::with_capacity(4096);
    for &byte in data {
        acc |= (byte as u32) << bits;
        bits += 8;
        while bits >= size {
            let code = (acc & ((1 << size) - 1)) as usize;
            acc >>= size;
            bits -= size;
            if code == clear {
                size = min as u32 + 1;
                next = end + 1;
                prev = None;
                continue;
            }
            if code == end {
                return Ok(out);
            }
            let Some(p) = prev else {
                if code >= clear {
                    return Err("bad GIF data".into());
                }
                out.push(code as u8);
                prev = Some(code);
                continue;
            };
            // The code's string (or, if it isn't defined yet, prev's plus its own first byte).
            let known = code < next;
            if !known && code != next {
                return Err("bad GIF code".into());
            }
            let mut c = if known { code } else { p };
            stack.clear();
            while c >= clear {
                stack.push(suffix[c]);
                c = prefix[c] as usize;
            }
            stack.push(suffix[c]);
            let first = c as u8;
            out.extend(stack.iter().rev());
            if !known {
                out.push(first);
            }
            if next < 4096 {
                prefix[next] = p as u16;
                suffix[next] = first;
                length[next] = length[p] + 1;
                next += 1;
                if next == 1 << size && size < 12 {
                    size += 1;
                }
            }
            prev = Some(code);
            if out.len() >= count {
                out.truncate(count);
                return Ok(out);
            }
        }
    }
    Ok(out)
}

/// Rows of an interlaced image in the order they're stored.
fn interlaced_rows(height: usize) -> Vec<usize> {
    let mut rows = Vec::with_capacity(height);
    for (start, step) in [(0, 8), (4, 8), (2, 4), (1, 2)] {
        rows.extend((start..height).step_by(step));
    }
    rows
}

/// Reads a GIF: every frame composed onto the canvas.
pub fn decode(bytes: &[u8]) -> Result<Gif, String> {
    if !is_gif(bytes) {
        return Err("not a GIF".into());
    }
    let mut r = Reader { bytes, at: 6 };
    let (width, height) = (r.u16()? as usize, r.u16()? as usize);
    if width == 0 || height == 0 || width * height > 64 << 20 {
        return Err("GIF size out of range".into());
    }
    let packed = r.u8()?;
    let _background = r.u8()?;
    let _aspect = r.u8()?;
    let global = if packed & 0x80 != 0 { Some(r.palette(packed)?) } else { None };
    let mut canvas = vec![0u8; width * height * 4];
    let mut frames = Vec::new();
    // From the last graphic control extension.
    let (mut delay, mut transparent, mut disposal) = (0u16, None::<u8>, 0u8);
    loop {
        match r.u8()? {
            0x21 => {
                let label = r.u8()?;
                if label == 0xF9 {
                    let block = r.blocks()?;
                    if block.len() >= 4 {
                        disposal = (block[0] >> 2) & 7;
                        delay = block[1] as u16 | (block[2] as u16) << 8;
                        transparent = (block[0] & 1 != 0).then_some(block[3]);
                    }
                } else {
                    r.skip_blocks()?;
                }
            }
            0x2C => {
                let (x, y, w, h) = (r.u16()? as usize, r.u16()? as usize, r.u16()? as usize, r.u16()? as usize);
                let packed = r.u8()?;
                let local = if packed & 0x80 != 0 { Some(r.palette(packed)?) } else { None };
                let palette = local.as_ref().or(global.as_ref()).ok_or("GIF without a palette")?;
                let min = r.u8()?;
                let data = r.blocks()?;
                let indices = lzw(&data, min, w * h)?;
                let previous = (disposal == 3).then(|| canvas.clone());
                let rows: Vec<usize> = if packed & 0x40 != 0 { interlaced_rows(h) } else { (0..h).collect() };
                for (k, &row) in rows.iter().enumerate() {
                    let cy = y + row;
                    if cy >= height {
                        continue;
                    }
                    for col in 0..w {
                        let cx = x + col;
                        let Some(&index) = indices.get(k * w + col) else { break };
                        if cx >= width || Some(index) == transparent {
                            continue;
                        }
                        let [red, green, blue] = palette.get(index as usize).copied().unwrap_or([0, 0, 0]);
                        let at = (cy * width + cx) * 4;
                        canvas[at..at + 4].copy_from_slice(&[red, green, blue, 255]);
                    }
                }
                let delay_ms = match delay as u32 * 10 {
                    0 | 10 => 100,
                    ms => ms,
                };
                frames.push(GifFrame { rgba: canvas.clone(), delay_ms });
                // What the next frame starts from.
                match disposal {
                    2 => {
                        for row in y..(y + h).min(height) {
                            for col in x..(x + w).min(width) {
                                let at = (row * width + col) * 4;
                                canvas[at..at + 4].fill(0);
                            }
                        }
                    }
                    3 => {
                        if let Some(previous) = previous {
                            canvas = previous;
                        }
                    }
                    _ => {}
                }
                (delay, transparent, disposal) = (0, None, 0);
                if frames.len() >= MAX_FRAMES {
                    break;
                }
            }
            0x3B => break,
            // Junk after the last frame: keep what was read.
            _ if !frames.is_empty() => break,
            other => return Err(format!("bad GIF block {other:#x}")),
        }
    }
    if frames.is_empty() {
        return Err("GIF without frames".into());
    }
    Ok(Gif { width: width as u32, height: height as u32, frames })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// LZW with a clear code often enough that codes never widen (fine for tests).
    fn lzw_encode(indices: &[u8], min: u8) -> Vec<u8> {
        let clear = 1u32 << min;
        let size = min as u32 + 1;
        let mut out = Vec::new();
        let (mut acc, mut bits) = (0u32, 0u32);
        let mut put = |code: u32, out: &mut Vec<u8>| {
            acc |= code << bits;
            bits += size;
            while bits >= 8 {
                out.push(acc as u8);
                acc >>= 8;
                bits -= 8;
            }
        };
        let run = (1usize << min) - 3;
        for chunk in indices.chunks(run.max(1)) {
            put(clear, &mut out);
            for &i in chunk {
                put(i as u32, &mut out);
            }
        }
        put(clear + 1, &mut out);
        if bits > 0 {
            out.push(acc as u8);
        }
        out
    }

    fn sub_blocks(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for c in data.chunks(255) {
            out.push(c.len() as u8);
            out.extend_from_slice(c);
        }
        out.push(0);
        out
    }

    /// A GIF of `frames` (indices into a 4-colour palette), each with a delay
    /// in centiseconds and a disposal method; index 3 is transparent.
    pub(crate) fn encode(w: u16, h: u16, frames: &[(Vec<u8>, u16, u8)]) -> Vec<u8> {
        let mut g = b"GIF89a".to_vec();
        g.extend(w.to_le_bytes());
        g.extend(h.to_le_bytes());
        g.extend([0x81, 0, 0]); // global palette of 4
        g.extend([255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0]);
        for (indices, delay, disposal) in frames {
            g.extend([0x21, 0xF9, 4, (disposal << 2) | 1]);
            g.extend(delay.to_le_bytes());
            g.extend([3, 0]);
            g.push(0x2C);
            g.extend([0, 0, 0, 0]);
            g.extend(w.to_le_bytes());
            g.extend(h.to_le_bytes());
            g.push(0);
            g.push(2);
            g.extend(sub_blocks(&lzw_encode(indices, 2)));
        }
        g.push(0x3B);
        g
    }

    #[test]
    fn frames_compose_with_transparency_and_delays() {
        // Frame 1: red left half, green right; frame 2: blue over the left half only.
        let first: Vec<u8> = (0..16).map(|i| if i % 4 < 2 { 0 } else { 1 }).collect();
        let second: Vec<u8> = (0..16).map(|i| if i % 4 < 2 { 2 } else { 3 }).collect();
        let bytes = encode(4, 4, &[(first, 5, 1), (second, 0, 1)]);
        let gif = decode(&bytes).unwrap();
        assert_eq!((gif.width, gif.height, gif.frames.len()), (4, 4, 2));
        assert_eq!(&gif.frames[0].rgba[0..4], &[255, 0, 0, 255]);
        assert_eq!(&gif.frames[0].rgba[8..12], &[0, 255, 0, 255]);
        // Transparent pixels keep what was under them.
        assert_eq!(&gif.frames[1].rgba[0..4], &[0, 0, 255, 255]);
        assert_eq!(&gif.frames[1].rgba[8..12], &[0, 255, 0, 255]);
        assert_eq!(gif.frames[0].delay_ms, 50);
        assert_eq!(gif.frames[1].delay_ms, 100, "0 means the browsers' 100 ms");
        assert_eq!((gif.frame_at(0), gif.frame_at(60), gif.frame_at(160)), (0, 1, 0));
    }

    #[test]
    fn real_lzw_codes_widen() {
        // A long run makes the dictionary grow past 8 codes: widths 3 → 4 → 5 bits.
        // Encoded by a standard encoder (bytes from a known-good GIF writer).
        let indices: Vec<u8> = (0..300).map(|i| ((i / 7) % 4) as u8).collect();
        let packed = standard_lzw(&indices, 2);
        assert_eq!(lzw(&packed, 2, indices.len()).unwrap(), indices);
    }

    /// A textbook LZW encoder (variable width, dictionary up to 4096).
    fn standard_lzw(data: &[u8], min: u8) -> Vec<u8> {
        use std::collections::HashMap;
        let clear = 1u32 << min;
        let mut dict: HashMap<Vec<u8>, u32> = (0..clear).map(|i| (vec![i as u8], i)).collect();
        let (mut next, mut size) = (clear + 2, min as u32 + 1);
        let mut out = Vec::new();
        let (mut acc, mut bits) = (0u64, 0u32);
        let mut put = |code: u32, size: u32, out: &mut Vec<u8>| {
            acc |= (code as u64) << bits;
            bits += size;
            while bits >= 8 {
                out.push(acc as u8);
                acc >>= 8;
                bits -= 8;
            }
        };
        put(clear, size, &mut out);
        let mut w: Vec<u8> = Vec::new();
        for &c in data {
            let mut wc = w.clone();
            wc.push(c);
            if dict.contains_key(&wc) {
                w = wc;
            } else {
                put(dict[&w], size, &mut out);
                if next < 4096 {
                    dict.insert(wc, next);
                    next += 1;
                    if next > (1 << size) && size < 12 {
                        size += 1;
                    }
                }
                w = vec![c];
            }
        }
        put(dict[&w], size, &mut out);
        put(clear + 1, size, &mut out);
        if bits > 0 {
            out.push(acc as u8);
        }
        out
    }
}
