//! Thumbnails of Painter projects (and Krita / OpenRaster files) in Windows
//! Explorer: a COM thumbnail provider (`IThumbnailProvider`, initialised with
//! the file's stream) that shows the PNG thumbnail the file carries.
//!
//! The app installs this DLL for the current user (no administrator rights)
//! from File → Set Up File Types. Explorer loads it in its thumbnail process;
//! it only reads the file it's handed.
//!
//! The platform-independent part (finding the thumbnail in a ZIP) is tested
//! everywhere.

use std::io::Read;

/// The provider's class id; registered under `HKCU\Software\Classes\CLSID`.
pub const CLSID: u128 = 0x5a1c_9a3e_2f4b_4c8e_9d3a_7e61_2b0c_7f41;

/// Entries that hold a thumbnail: Painter, OpenRaster, Krita (then the full picture).
const THUMBNAIL_ENTRIES: [&str; 4] = ["thumbnail.png", "Thumbnails/thumbnail.png", "preview.png", "mergedimage.png"];

fn u16_at(b: &[u8], at: usize) -> Option<usize> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?) as usize)
}

fn u32_at(b: &[u8], at: usize) -> Option<usize> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?) as usize)
}

/// One entry of a ZIP file (stored or deflated), from its central directory.
pub fn zip_entry(zip: &[u8], wanted: &str) -> Option<Vec<u8>> {
    // The end of central directory record, searched from the end (a comment may follow it).
    let eocd = (0..zip.len().saturating_sub(21)).rev().take(65_557).find(|&i| zip[i..].starts_with(b"PK\x05\x06"))?;
    let count = u16_at(zip, eocd + 10)?;
    let mut at = u32_at(zip, eocd + 16)?;
    for _ in 0..count {
        if !zip.get(at..)?.starts_with(b"PK\x01\x02") {
            return None;
        }
        let method = u16_at(zip, at + 10)?;
        let packed = u32_at(zip, at + 20)?;
        let size = u32_at(zip, at + 24)?;
        let (name_len, extra_len, comment_len) = (u16_at(zip, at + 28)?, u16_at(zip, at + 30)?, u16_at(zip, at + 32)?);
        let local = u32_at(zip, at + 42)?;
        let name = zip.get(at + 46..at + 46 + name_len)?;
        if name == wanted.as_bytes() {
            let data_at = local + 30 + u16_at(zip, local + 26)? + u16_at(zip, local + 28)?;
            let data = zip.get(data_at..data_at + packed)?;
            return match method {
                0 => Some(data.to_vec()),
                8 => {
                    let mut out = Vec::with_capacity(size.min(64 << 20));
                    flate2::read::DeflateDecoder::new(data).take(64 << 20).read_to_end(&mut out).ok()?;
                    Some(out)
                }
                _ => None,
            };
        }
        at += 46 + name_len + extra_len + comment_len;
    }
    None
}

/// The thumbnail a file carries, as straight RGBA scaled to fit `size`: (width, height, pixels).
pub fn thumbnail(file: &[u8], size: u32) -> Option<(u32, u32, Vec<u8>)> {
    let png = THUMBNAIL_ENTRIES.iter().find_map(|name| zip_entry(file, name))?;
    let img = image::load_from_memory_with_format(&png, image::ImageFormat::Png).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    let scale = (size as f32 / w.max(h).max(1) as f32).min(1.0);
    let (tw, th) = (((w as f32 * scale).round() as u32).max(1), ((h as f32 * scale).round() as u32).max(1));
    let img = if (tw, th) == (w, h) { img } else { image::imageops::resize(&img, tw, th, image::imageops::FilterType::Triangle) };
    Some((tw, th, img.into_raw()))
}

#[cfg(windows)]
mod com {
    use std::cell::RefCell;
    use std::ffi::c_void;

    use windows::core::{implement, IUnknown, Interface, GUID, HRESULT};
    use windows::Win32::Foundation::{BOOL, CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_FAIL, E_POINTER, S_FALSE};
    use windows::Win32::Graphics::Gdi::{CreateDIBSection, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP};
    use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl, IStream};
    use windows::Win32::UI::Shell::PropertiesSystem::{IInitializeWithStream, IInitializeWithStream_Impl};
    use windows::Win32::UI::Shell::{IThumbnailProvider, IThumbnailProvider_Impl, WTSAT_ARGB, WTS_ALPHATYPE};

    /// Files bigger than this aren't read (a thumbnail is near the start anyway).
    const MAX_FILE: usize = 1 << 30;

    #[implement(IInitializeWithStream, IThumbnailProvider)]
    struct Provider {
        file: RefCell<Option<Vec<u8>>>,
    }

    impl IInitializeWithStream_Impl for Provider_Impl {
        fn Initialize(&self, stream: Option<&IStream>, _mode: u32) -> windows::core::Result<()> {
            let stream = stream.ok_or(windows::core::Error::from(E_POINTER))?;
            let mut file = Vec::new();
            let mut chunk = vec![0u8; 1 << 16];
            loop {
                let mut read = 0u32;
                let hr = unsafe { stream.Read(chunk.as_mut_ptr().cast(), chunk.len() as u32, Some(&mut read)) };
                if hr.is_err() {
                    return Err(hr.into());
                }
                if read == 0 {
                    break;
                }
                file.extend_from_slice(&chunk[..read as usize]);
                if file.len() > MAX_FILE {
                    return Err(E_FAIL.into());
                }
            }
            *self.file.borrow_mut() = Some(file);
            Ok(())
        }
    }

    impl IThumbnailProvider_Impl for Provider_Impl {
        fn GetThumbnail(&self, cx: u32, bitmap: *mut HBITMAP, alpha: *mut WTS_ALPHATYPE) -> windows::core::Result<()> {
            if bitmap.is_null() || alpha.is_null() {
                return Err(E_POINTER.into());
            }
            let file = self.file.borrow();
            let (w, h, rgba) = file.as_deref().and_then(|f| super::thumbnail(f, cx)).ok_or(windows::core::Error::from(E_FAIL))?;
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w as i32,
                    // Negative: rows top to bottom.
                    biHeight: -(h as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut c_void = std::ptr::null_mut();
            let hbitmap = unsafe { CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0)? };
            if bits.is_null() {
                return Err(E_FAIL.into());
            }
            // BGRA, straight alpha.
            let out = unsafe { std::slice::from_raw_parts_mut(bits as *mut u8, (w * h * 4) as usize) };
            for (o, p) in out.as_chunks_mut::<4>().0.iter_mut().zip(rgba.as_chunks::<4>().0) {
                o.copy_from_slice(&[p[2], p[1], p[0], p[3]]);
            }
            unsafe {
                *bitmap = hbitmap;
                *alpha = WTSAT_ARGB;
            }
            Ok(())
        }
    }

    #[implement(IClassFactory)]
    struct Factory;

    impl IClassFactory_Impl for Factory_Impl {
        fn CreateInstance(&self, outer: Option<&IUnknown>, iid: *const GUID, object: *mut *mut c_void) -> windows::core::Result<()> {
            if object.is_null() {
                return Err(E_POINTER.into());
            }
            unsafe { *object = std::ptr::null_mut() };
            if outer.is_some() {
                return Err(CLASS_E_NOAGGREGATION.into());
            }
            let provider: IUnknown = Provider { file: RefCell::new(None) }.into();
            unsafe { provider.query(iid, object).ok() }
        }

        fn LockServer(&self, _lock: BOOL) -> windows::core::Result<()> {
            Ok(())
        }
    }

    /// COM's entry point: the class factory for our provider.
    #[no_mangle]
    pub unsafe extern "system" fn DllGetClassObject(clsid: *const GUID, iid: *const GUID, object: *mut *mut c_void) -> HRESULT {
        if clsid.is_null() || iid.is_null() || object.is_null() {
            return E_POINTER;
        }
        *object = std::ptr::null_mut();
        if *clsid != GUID::from_u128(super::CLSID) {
            return CLASS_E_CLASSNOTAVAILABLE;
        }
        let factory: IClassFactory = Factory.into();
        factory.query(iid, object)
    }

    /// Explorer may unload us when it likes: nothing is kept between calls.
    #[no_mangle]
    pub extern "system" fn DllCanUnloadNow() -> HRESULT {
        S_FALSE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ZIP with one stored and one deflated entry, written by hand.
    fn zip(entries: &[(&str, &[u8], bool)]) -> Vec<u8> {
        use std::io::Write;
        let (mut out, mut central) = (Vec::new(), Vec::new());
        for (name, data, deflate) in entries {
            let body = if *deflate {
                let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
                e.write_all(data).unwrap();
                e.finish().unwrap()
            } else {
                data.to_vec()
            };
            let method: u16 = if *deflate { 8 } else { 0 };
            let offset = out.len() as u32;
            out.extend(b"PK\x03\x04");
            out.extend([20, 0, 0, 0]);
            out.extend(method.to_le_bytes());
            out.extend([0; 8]); // time, date, crc (not checked here)
            out.extend((body.len() as u32).to_le_bytes());
            out.extend((data.len() as u32).to_le_bytes());
            out.extend((name.len() as u16).to_le_bytes());
            out.extend([0, 0]);
            out.extend(name.as_bytes());
            out.extend(&body);
            central.extend(b"PK\x01\x02");
            central.extend([20, 0, 20, 0, 0, 0]);
            central.extend(method.to_le_bytes());
            central.extend([0; 8]);
            central.extend((body.len() as u32).to_le_bytes());
            central.extend((data.len() as u32).to_le_bytes());
            central.extend((name.len() as u16).to_le_bytes());
            central.extend([0; 12]);
            central.extend(offset.to_le_bytes());
            central.extend(name.as_bytes());
        }
        let cd_at = out.len() as u32;
        out.extend(&central);
        out.extend(b"PK\x05\x06");
        out.extend([0; 4]);
        out.extend((entries.len() as u16).to_le_bytes());
        out.extend((entries.len() as u16).to_le_bytes());
        out.extend((central.len() as u32).to_le_bytes());
        out.extend(cd_at.to_le_bytes());
        out.extend([0, 0]);
        out
    }

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        image::RgbaImage::from_pixel(w, h, image::Rgba([10, 20, 30, 255]))
            .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();
        bytes
    }

    #[test]
    fn finds_thumbnails_in_projects_krita_and_openraster() {
        let painter = zip(&[("thumbnail.png", &png(256, 128), false), ("document.json", b"{}", true)]);
        assert_eq!(thumbnail(&painter, 64).map(|t| (t.0, t.1)), Some((64, 32)));
        let ora = zip(&[("mimetype", b"image/openraster", false), ("Thumbnails/thumbnail.png", &png(100, 200), true)]);
        let (w, h, px) = thumbnail(&ora, 256).unwrap();
        assert_eq!((w, h), (100, 200), "never enlarged");
        assert_eq!(&px[0..4], &[10, 20, 30, 255]);
        assert!(thumbnail(b"not a zip at all", 64).is_none());
    }
}
