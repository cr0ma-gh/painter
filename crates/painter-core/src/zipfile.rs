//! A small ZIP reader and writer for container formats (OpenRaster, Krita,
//! brush bundles): stored and deflated entries, as in the PKWARE APPNOTE.
//! The writer can store an entry uncompressed, as those formats require for
//! their leading `mimetype` file. No ZIP64, encryption or spanning.

use std::io::{Read, Write};

#[derive(Clone, Debug)]
struct Entry {
    name: String,
    method: u16,
    /// Offset of the local header.
    offset: usize,
    compressed: usize,
    size: usize,
}

/// A ZIP archive held in memory.
pub struct ZipReader {
    data: Vec<u8>,
    entries: Vec<Entry>,
}

fn u16_at(d: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(i..i + 2)?.try_into().ok()?))
}

fn u32_at(d: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(i..i + 4)?.try_into().ok()?))
}

impl ZipReader {
    pub fn new(data: Vec<u8>) -> Result<Self, String> {
        // End of central directory: the last "PK\x05\x06" (a comment may follow it).
        let eocd = (0..data.len().saturating_sub(21))
            .rev()
            .take(65_557)
            .find(|&i| data[i..].starts_with(&[0x50, 0x4b, 0x05, 0x06]))
            .ok_or("not a ZIP archive")?;
        let count = u16_at(&data, eocd + 10).ok_or("truncated ZIP")? as usize;
        let mut at = u32_at(&data, eocd + 16).ok_or("truncated ZIP")? as usize;
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            if u32_at(&data, at) != Some(0x0201_4b50) {
                return Err("damaged ZIP directory".into());
            }
            let method = u16_at(&data, at + 10).ok_or("truncated ZIP")?;
            let compressed = u32_at(&data, at + 20).ok_or("truncated ZIP")? as usize;
            let size = u32_at(&data, at + 24).ok_or("truncated ZIP")? as usize;
            let name_len = u16_at(&data, at + 28).ok_or("truncated ZIP")? as usize;
            let extra_len = u16_at(&data, at + 30).ok_or("truncated ZIP")? as usize;
            let comment_len = u16_at(&data, at + 32).ok_or("truncated ZIP")? as usize;
            let offset = u32_at(&data, at + 42).ok_or("truncated ZIP")? as usize;
            let name = String::from_utf8_lossy(data.get(at + 46..at + 46 + name_len).ok_or("truncated ZIP")?).into_owned();
            entries.push(Entry { name, method, offset, compressed, size });
            at += 46 + name_len + extra_len + comment_len;
        }
        Ok(Self { data, entries })
    }

    pub fn open(path: &std::path::Path) -> Result<Self, String> {
        Self::new(std::fs::read(path).map_err(|e| e.to_string())?)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|e| e.name.as_str())
    }

    /// An entry's contents.
    pub fn read(&self, name: &str) -> Option<Vec<u8>> {
        let e = self.entries.iter().find(|e| e.name == name)?;
        let d = &self.data;
        if u32_at(d, e.offset)? != 0x0403_4b50 {
            return None;
        }
        let start = e.offset + 30 + u16_at(d, e.offset + 26)? as usize + u16_at(d, e.offset + 28)? as usize;
        let raw = d.get(start..start + e.compressed)?;
        match e.method {
            0 => Some(raw.to_vec()),
            8 => {
                let mut out = Vec::with_capacity(e.size);
                flate2::read::DeflateDecoder::new(raw).read_to_end(&mut out).ok()?;
                Some(out)
            }
            _ => None,
        }
    }
}

/// Builds a ZIP archive in memory.
#[derive(Default)]
pub struct ZipWriter {
    out: Vec<u8>,
    central: Vec<u8>,
    count: u16,
}

impl ZipWriter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `name`; `compress` deflates it, otherwise it's stored as is.
    pub fn add(&mut self, name: &str, data: &[u8], compress: bool) {
        let crc = crc32fast::hash(data);
        let (method, body) = if compress {
            let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(6));
            enc.write_all(data).expect("writing to memory");
            (8u16, enc.finish().expect("writing to memory"))
        } else {
            (0u16, data.to_vec())
        };
        let offset = self.out.len() as u32;
        // Bit 11: the name is UTF-8.
        let flags: u16 = if name.is_ascii() { 0 } else { 0x0800 };
        let (time, date) = (0u16, 0x21u16); // 1980-01-01 00:00
        let mut local = Vec::with_capacity(30 + name.len());
        local.extend(0x0403_4b50u32.to_le_bytes());
        local.extend(20u16.to_le_bytes());
        local.extend(flags.to_le_bytes());
        local.extend(method.to_le_bytes());
        local.extend(time.to_le_bytes());
        local.extend(date.to_le_bytes());
        local.extend(crc.to_le_bytes());
        local.extend((body.len() as u32).to_le_bytes());
        local.extend((data.len() as u32).to_le_bytes());
        local.extend((name.len() as u16).to_le_bytes());
        local.extend(0u16.to_le_bytes());
        local.extend(name.as_bytes());
        self.out.extend(local);
        self.out.extend(&body);

        let c = &mut self.central;
        c.extend(0x0201_4b50u32.to_le_bytes());
        c.extend(20u16.to_le_bytes()); // made by
        c.extend(20u16.to_le_bytes()); // needed
        c.extend(flags.to_le_bytes());
        c.extend(method.to_le_bytes());
        c.extend(time.to_le_bytes());
        c.extend(date.to_le_bytes());
        c.extend(crc.to_le_bytes());
        c.extend((body.len() as u32).to_le_bytes());
        c.extend((data.len() as u32).to_le_bytes());
        c.extend((name.len() as u16).to_le_bytes());
        c.extend([0u8; 12]); // extra and comment lengths, disk, internal and external attributes
        c.extend(offset.to_le_bytes());
        c.extend(name.as_bytes());
        self.count += 1;
    }

    pub fn finish(mut self) -> Vec<u8> {
        let start = self.out.len() as u32;
        let size = self.central.len() as u32;
        self.out.extend(&self.central);
        self.out.extend(0x0605_4b50u32.to_le_bytes());
        self.out.extend([0u8; 4]);
        self.out.extend(self.count.to_le_bytes());
        self.out.extend(self.count.to_le_bytes());
        self.out.extend(size.to_le_bytes());
        self.out.extend(start.to_le_bytes());
        self.out.extend(0u16.to_le_bytes());
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_stored_and_deflated_entries() {
        let mut w = ZipWriter::new();
        w.add("mimetype", b"image/openraster", false);
        let big: Vec<u8> = (0..50_000u32).map(|i| (i % 7) as u8).collect();
        w.add("data/layer0.png", &big, true);
        w.add("città.txt", "ciao".as_bytes(), true);
        let bytes = w.finish();
        // The first entry is stored, its contents right after the 38-byte header + name.
        assert_eq!(&bytes[30..38], b"mimetype");
        assert_eq!(&bytes[38..54], b"image/openraster");
        let r = ZipReader::new(bytes).unwrap();
        assert_eq!(r.names().collect::<Vec<_>>(), ["mimetype", "data/layer0.png", "città.txt"]);
        assert_eq!(r.read("data/layer0.png").unwrap(), big);
        assert_eq!(r.read("città.txt").unwrap(), b"ciao");
        assert!(r.read("missing").is_none());
        assert!(ZipReader::new(b"not a zip".to_vec()).is_err());
    }

    #[test]
    fn reads_archives_from_the_zip_crate() {
        // The project format is written by the `zip` crate: check we read its archives.
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            z.start_file("a.txt", zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored))
                .unwrap();
            z.write_all(b"hello").unwrap();
            z.finish().unwrap();
        }
        let r = ZipReader::new(buf.into_inner()).unwrap();
        assert_eq!(r.read("a.txt").unwrap(), b"hello");
    }
}
