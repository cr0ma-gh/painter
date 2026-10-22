//! Checks the built DLL the way Explorer uses it, without registering it:
//! loads it, gets the class factory, hands the provider a file as a stream
//! and saves the thumbnail it returns as a PNG.
//!
//! ```text
//! cargo build --release -p painter-shell
//! cargo run -p painter-shell --example try_dll -- target/release/painter_shell.dll file.painter out.png 256
//! ```

#[cfg(windows)]
fn main() {
    use windows::core::{Interface, GUID, HSTRING};
    use windows::Win32::Graphics::Gdi::{GetObjectW, BITMAP};
    use windows::Win32::System::Com::{CoInitializeEx, IClassFactory, COINIT_APARTMENTTHREADED};
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    use windows::Win32::UI::Shell::PropertiesSystem::IInitializeWithStream;
    use windows::Win32::UI::Shell::{IThumbnailProvider, SHCreateMemStream, WTS_ALPHATYPE};

    let args: Vec<String> = std::env::args().collect();
    let (dll, file, out) = (&args[1], &args[2], &args[3]);
    let size: u32 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(256);
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let module = LoadLibraryW(&HSTRING::from(dll.as_str())).expect("load the DLL");
        let get = GetProcAddress(module, windows::core::s!("DllGetClassObject")).expect("DllGetClassObject exported");
        type GetClassObject =
            unsafe extern "system" fn(*const GUID, *const GUID, *mut *mut std::ffi::c_void) -> windows::core::HRESULT;
        let get: GetClassObject = std::mem::transmute(get);
        let mut factory = std::ptr::null_mut();
        get(&GUID::from_u128(painter_shell::CLSID), &IClassFactory::IID, &mut factory).ok().expect("class factory");
        let factory = IClassFactory::from_raw(factory);
        let init: IInitializeWithStream = factory.CreateInstance(None).expect("provider");
        let bytes = std::fs::read(file).expect("read the file");
        let stream = SHCreateMemStream(Some(&bytes)).expect("stream");
        init.Initialize(&stream, 0).expect("Initialize");
        let provider: IThumbnailProvider = init.cast().expect("IThumbnailProvider");
        let (mut bitmap, mut alpha) = (Default::default(), WTS_ALPHATYPE::default());
        provider.GetThumbnail(size, &mut bitmap, &mut alpha).expect("GetThumbnail");
        let mut info = BITMAP::default();
        GetObjectW(bitmap, std::mem::size_of::<BITMAP>() as i32, Some(&mut info as *mut _ as *mut _));
        let (w, h) = (info.bmWidth as u32, info.bmHeight.unsigned_abs());
        let bits = std::slice::from_raw_parts(info.bmBits as *const u8, (w * h * 4) as usize);
        let rgba: Vec<u8> = bits.as_chunks::<4>().0.iter().flat_map(|p| [p[2], p[1], p[0], p[3]]).collect();
        image::RgbaImage::from_raw(w, h, rgba).unwrap().save(out).unwrap();
        println!("{w}×{h} thumbnail, alpha type {}", alpha.0);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Windows only");
}
