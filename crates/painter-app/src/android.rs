//! Android glue: what the app asks of the system through JNI (the Java side
//! of the activity), and the activity handle from `painter-android`.
//!
//! - the soft keyboard, shown while a text field has the focus;
//! - access to the device's shared storage (Download, Documents, Pictures,
//!   DCIM): "All files access" on Android 11+, the storage permissions
//!   before; [`shared_root`] is where it starts;
//! - a Wi-Fi multicast lock while looking for network scanners (Android
//!   drops mDNS answers without one);
//! - the clipboard: pictures copied in other apps (read through their
//!   content URI), and text copied in Painter.
//!
//! JNI is called through its function table directly ([`Env`]): the few
//! calls needed, by their fixed positions in the JNI specification, each
//! followed by a check for a Java exception.

use std::ffi::{c_char, c_void, CStr, CString};
use std::sync::OnceLock;

use winit::platform::android::activity::AndroidApp;

static APP: OnceLock<AndroidApp> = OnceLock::new();

/// The activity, from `android_main`.
pub(crate) fn set_app(app: AndroidApp) {
    let _ = APP.set(app);
}

// ---- a minimal JNI ---------------------------------------------------------------------

type Obj = *mut c_void;
type Id = *mut c_void;

/// A JNI argument (`jvalue`); written here, read by the VM.
#[repr(C)]
#[allow(dead_code)]
#[derive(Clone, Copy)]
union Value {
    z: u8,
    i: i32,
    l: Obj,
    /// Keeps the union 8 bytes wide, like `jvalue`.
    _j: i64,
}

fn obj(o: Obj) -> Value {
    Value { l: o }
}

fn int(i: i32) -> Value {
    Value { _j: 0 }.with_int(i)
}

fn boolean(b: bool) -> Value {
    Value { _j: 0 }.with_bool(b)
}

impl Value {
    fn with_int(mut self, i: i32) -> Value {
        self.i = i;
        self
    }

    fn with_bool(mut self, b: bool) -> Value {
        self.z = b as u8;
        self
    }
}

/// Positions in the JNI function table (`JNINativeInterface`).
mod f {
    pub const FIND_CLASS: usize = 6;
    pub const EXCEPTION_DESCRIBE: usize = 16;
    pub const EXCEPTION_CLEAR: usize = 17;
    pub const PUSH_LOCAL_FRAME: usize = 19;
    pub const POP_LOCAL_FRAME: usize = 20;
    pub const NEW_GLOBAL_REF: usize = 21;
    pub const NEW_OBJECT_A: usize = 30;
    pub const GET_OBJECT_CLASS: usize = 31;
    pub const GET_METHOD_ID: usize = 33;
    pub const CALL_OBJECT_METHOD_A: usize = 36;
    pub const CALL_INT_METHOD_A: usize = 51;
    pub const CALL_VOID_METHOD_A: usize = 63;
    pub const GET_STATIC_METHOD_ID: usize = 113;
    pub const CALL_STATIC_OBJECT_METHOD_A: usize = 116;
    pub const CALL_STATIC_BOOLEAN_METHOD_A: usize = 119;
    pub const GET_STATIC_FIELD_ID: usize = 144;
    pub const GET_STATIC_INT_FIELD: usize = 150;
    pub const NEW_STRING_UTF: usize = 167;
    pub const GET_STRING_UTF_CHARS: usize = 169;
    pub const RELEASE_STRING_UTF_CHARS: usize = 170;
    pub const NEW_OBJECT_ARRAY: usize = 172;
    pub const SET_OBJECT_ARRAY_ELEMENT: usize = 174;
    pub const NEW_BYTE_ARRAY: usize = 176;
    pub const GET_BYTE_ARRAY_REGION: usize = 200;
    pub const EXCEPTION_CHECK: usize = 228;
    /// In the invocation interface (`JNIInvokeInterface`).
    pub const ATTACH_CURRENT_THREAD: usize = 4;
}

/// A Java exception or a missing class / method: the call didn't happen.
#[derive(Debug)]
struct Failed;

type R<T> = Result<T, Failed>;

/// This thread's JNI environment (`JNIEnv*`).
struct Env(*mut c_void);

impl Env {
    /// The function at `index` of the table, as type `F` (an `extern "system" fn`).
    ///
    /// # Safety
    /// `F` must be the signature the JNI specification gives that entry.
    unsafe fn func<F: Copy>(&self, index: usize) -> F {
        let table = *(self.0 as *const *const *const c_void);
        std::mem::transmute_copy(&*table.add(index))
    }

    /// Clears a pending Java exception; `Err` when there was one.
    fn check(&self) -> R<()> {
        // SAFETY: entries 228, 16 and 17 with their specified signatures.
        unsafe {
            let check: extern "system" fn(*mut c_void) -> u8 = self.func(f::EXCEPTION_CHECK);
            if check(self.0) != 0 {
                let describe: extern "system" fn(*mut c_void) = self.func(f::EXCEPTION_DESCRIBE);
                let clear: extern "system" fn(*mut c_void) = self.func(f::EXCEPTION_CLEAR);
                describe(self.0);
                clear(self.0);
                return Err(Failed);
            }
        }
        Ok(())
    }

    fn nonnull(&self, o: Obj) -> R<Obj> {
        self.check()?;
        if o.is_null() {
            Err(Failed)
        } else {
            Ok(o)
        }
    }

    fn class(&self, name: &str) -> R<Obj> {
        let name = CString::new(name).map_err(|_| Failed)?;
        // SAFETY: FindClass(env, name).
        let c = unsafe {
            let find: extern "system" fn(*mut c_void, *const c_char) -> Obj = self.func(f::FIND_CLASS);
            find(self.0, name.as_ptr())
        };
        self.nonnull(c)
    }

    fn method(&self, class: Obj, name: &str, sig: &str, statik: bool) -> R<Id> {
        let (name, sig) = (CString::new(name).map_err(|_| Failed)?, CString::new(sig).map_err(|_| Failed)?);
        // SAFETY: Get(Static)MethodID(env, class, name, sig).
        let id = unsafe {
            let get: extern "system" fn(*mut c_void, Obj, *const c_char, *const c_char) -> Id =
                self.func(if statik { f::GET_STATIC_METHOD_ID } else { f::GET_METHOD_ID });
            get(self.0, class, name.as_ptr(), sig.as_ptr())
        };
        self.nonnull(id)
    }

    fn object_method(&self, target: Obj, name: &str, sig: &str) -> R<Id> {
        // SAFETY: GetObjectClass(env, obj).
        let class = unsafe {
            let get: extern "system" fn(*mut c_void, Obj) -> Obj = self.func(f::GET_OBJECT_CLASS);
            get(self.0, target)
        };
        let class = self.nonnull(class)?;
        self.method(class, name, sig, false)
    }

    /// Calls an instance method returning an object (may be null).
    fn call_object(&self, target: Obj, name: &str, sig: &str, args: &[Value]) -> R<Obj> {
        let id = self.object_method(target, name, sig)?;
        // SAFETY: CallObjectMethodA with arguments matching `sig`.
        let o = unsafe {
            let call: extern "system" fn(*mut c_void, Obj, Id, *const Value) -> Obj = self.func(f::CALL_OBJECT_METHOD_A);
            call(self.0, target, id, args.as_ptr())
        };
        self.check()?;
        Ok(o)
    }

    fn call_int(&self, target: Obj, name: &str, sig: &str, args: &[Value]) -> R<i32> {
        let id = self.object_method(target, name, sig)?;
        // SAFETY: CallIntMethodA with arguments matching `sig`.
        let v = unsafe {
            let call: extern "system" fn(*mut c_void, Obj, Id, *const Value) -> i32 = self.func(f::CALL_INT_METHOD_A);
            call(self.0, target, id, args.as_ptr())
        };
        self.check()?;
        Ok(v)
    }

    fn call_void(&self, target: Obj, name: &str, sig: &str, args: &[Value]) -> R<()> {
        let id = self.object_method(target, name, sig)?;
        // SAFETY: CallVoidMethodA with arguments matching `sig`.
        unsafe {
            let call: extern "system" fn(*mut c_void, Obj, Id, *const Value) = self.func(f::CALL_VOID_METHOD_A);
            call(self.0, target, id, args.as_ptr());
        }
        self.check()
    }

    fn call_static_object(&self, class: &str, name: &str, sig: &str, args: &[Value]) -> R<Obj> {
        let c = self.class(class)?;
        let id = self.method(c, name, sig, true)?;
        // SAFETY: CallStaticObjectMethodA with arguments matching `sig`.
        let o = unsafe {
            let call: extern "system" fn(*mut c_void, Obj, Id, *const Value) -> Obj = self.func(f::CALL_STATIC_OBJECT_METHOD_A);
            call(self.0, c, id, args.as_ptr())
        };
        self.check()?;
        Ok(o)
    }

    fn call_static_bool(&self, class: &str, name: &str, sig: &str, args: &[Value]) -> R<bool> {
        let c = self.class(class)?;
        let id = self.method(c, name, sig, true)?;
        // SAFETY: CallStaticBooleanMethodA with arguments matching `sig`.
        let v = unsafe {
            let call: extern "system" fn(*mut c_void, Obj, Id, *const Value) -> u8 = self.func(f::CALL_STATIC_BOOLEAN_METHOD_A);
            call(self.0, c, id, args.as_ptr())
        };
        self.check()?;
        Ok(v != 0)
    }

    fn static_int(&self, class: &str, name: &str) -> R<i32> {
        let c = self.class(class)?;
        let n = CString::new(name).map_err(|_| Failed)?;
        // SAFETY: GetStaticFieldID(env, class, name, "I").
        let id = unsafe {
            let get: extern "system" fn(*mut c_void, Obj, *const c_char, *const c_char) -> Id = self.func(f::GET_STATIC_FIELD_ID);
            get(self.0, c, n.as_ptr(), c"I".as_ptr())
        };
        let id = self.nonnull(id)?;
        // SAFETY: GetStaticIntField(env, class, id).
        let v = unsafe {
            let get: extern "system" fn(*mut c_void, Obj, Id) -> i32 = self.func(f::GET_STATIC_INT_FIELD);
            get(self.0, c, id)
        };
        self.check()?;
        Ok(v)
    }

    fn new_object(&self, class: &str, sig: &str, args: &[Value]) -> R<Obj> {
        let c = self.class(class)?;
        let id = self.method(c, "<init>", sig, false)?;
        // SAFETY: NewObjectA with arguments matching the constructor's `sig`.
        let o = unsafe {
            let new: extern "system" fn(*mut c_void, Obj, Id, *const Value) -> Obj = self.func(f::NEW_OBJECT_A);
            new(self.0, c, id, args.as_ptr())
        };
        self.nonnull(o)
    }

    fn string(&self, s: &str) -> R<Obj> {
        // Modified UTF-8 is plain UTF-8 for text without NULs or astral characters.
        let s = CString::new(s).map_err(|_| Failed)?;
        // SAFETY: NewStringUTF(env, chars).
        let o = unsafe {
            let new: extern "system" fn(*mut c_void, *const c_char) -> Obj = self.func(f::NEW_STRING_UTF);
            new(self.0, s.as_ptr())
        };
        self.nonnull(o)
    }

    fn rust_string(&self, s: Obj) -> R<String> {
        // SAFETY: GetStringUTFChars / ReleaseStringUTFChars on a java.lang.String.
        unsafe {
            let get: extern "system" fn(*mut c_void, Obj, *mut u8) -> *const c_char = self.func(f::GET_STRING_UTF_CHARS);
            let release: extern "system" fn(*mut c_void, Obj, *const c_char) = self.func(f::RELEASE_STRING_UTF_CHARS);
            let p = get(self.0, s, std::ptr::null_mut());
            if p.is_null() {
                self.check()?;
                return Err(Failed);
            }
            let out = CStr::from_ptr(p).to_string_lossy().into_owned();
            release(self.0, s, p);
            Ok(out)
        }
    }

    fn global(&self, o: Obj) -> Obj {
        // SAFETY: NewGlobalRef(env, obj).
        unsafe {
            let new: extern "system" fn(*mut c_void, Obj) -> Obj = self.func(f::NEW_GLOBAL_REF);
            new(self.0, o)
        }
    }
}

/// Runs `f` with this thread's JNI environment (attached on first use) and
/// the activity; local references made inside are released afterwards.
fn with_env<T>(f: impl FnOnce(&Env, Obj) -> R<T>) -> Option<T> {
    let ctx = ndk_context::android_context();
    let (vm, activity) = (ctx.vm(), ctx.context());
    if vm.is_null() || activity.is_null() {
        return None;
    }
    let mut env: *mut c_void = std::ptr::null_mut();
    // SAFETY: `vm` is the process's JavaVM (set up by android-activity); entry 4 of its
    // invocation table is AttachCurrentThread, which gives this thread's JNIEnv.
    unsafe {
        let table = *(vm as *const *const *const c_void);
        let attach: extern "system" fn(*mut c_void, *mut *mut c_void, *mut c_void) -> i32 =
            std::mem::transmute_copy(&*table.add(f::ATTACH_CURRENT_THREAD));
        if attach(vm, &mut env, std::ptr::null_mut()) != 0 || env.is_null() {
            return None;
        }
    }
    let env = Env(env);
    // SAFETY: PushLocalFrame / PopLocalFrame around the calls free their local references.
    unsafe {
        let push: extern "system" fn(*mut c_void, i32) -> i32 = env.func(f::PUSH_LOCAL_FRAME);
        if push(env.0, 64) != 0 {
            let _ = env.check();
            return None;
        }
    }
    let result = f(&env, activity);
    unsafe {
        let pop: extern "system" fn(*mut c_void, Obj) -> Obj = env.func(f::POP_LOCAL_FRAME);
        pop(env.0, std::ptr::null_mut());
    }
    result.ok()
}

fn sdk_int(env: &Env) -> R<i32> {
    env.static_int("android/os/Build$VERSION", "SDK_INT")
}

fn system_service(env: &Env, activity: Obj, name: &str) -> R<Obj> {
    let name = env.string(name)?;
    let s = env.call_object(activity, "getSystemService", "(Ljava/lang/String;)Ljava/lang/Object;", &[obj(name)])?;
    env.nonnull(s)
}

// ---- keyboard ------------------------------------------------------------------------

/// Shows or hides the soft keyboard (when a text field gains or loses the focus).
pub(crate) fn show_keyboard(show: bool) {
    if let Some(app) = APP.get() {
        if show {
            app.show_soft_input(true);
        } else {
            app.hide_soft_input(true);
        }
    }
}

// ---- shared storage ------------------------------------------------------------------

/// Whether the app may read and write the device's shared storage.
pub(crate) fn has_storage_access() -> bool {
    with_env(|env, activity| {
        if sdk_int(env)? >= 30 {
            env.call_static_bool("android/os/Environment", "isExternalStorageManager", "()Z", &[])
        } else {
            let permission = env.string("android.permission.WRITE_EXTERNAL_STORAGE")?;
            Ok(env.call_int(activity, "checkSelfPermission", "(Ljava/lang/String;)I", &[obj(permission)])? == 0)
        }
    })
    .unwrap_or(false)
}

/// Asks for shared storage access: Android 11+ opens the "All files access"
/// page for Painter; earlier versions show the permission dialog.
pub(crate) fn request_storage_access() {
    with_env(|env, activity| {
        if sdk_int(env)? >= 30 {
            let package = env.call_object(activity, "getPackageName", "()Ljava/lang/String;", &[])?;
            let package = env.rust_string(env.nonnull(package)?)?;
            let text = env.string(&format!("package:{package}"))?;
            let uri = env.call_static_object("android/net/Uri", "parse", "(Ljava/lang/String;)Landroid/net/Uri;", &[obj(text)])?;
            let action = env.string("android.settings.MANAGE_APP_ALL_FILES_ACCESS_PERMISSION")?;
            let intent = env.new_object("android/content/Intent", "(Ljava/lang/String;Landroid/net/Uri;)V", &[obj(action), obj(uri)])?;
            env.call_void(activity, "startActivity", "(Landroid/content/Intent;)V", &[obj(intent)])
        } else {
            let string_class = env.class("java/lang/String")?;
            // SAFETY: NewObjectArray(env, 2, String, null).
            let list = unsafe {
                let new: extern "system" fn(*mut c_void, i32, Obj, Obj) -> Obj = env.func(f::NEW_OBJECT_ARRAY);
                new(env.0, 2, string_class, std::ptr::null_mut())
            };
            let list = env.nonnull(list)?;
            for (i, name) in ["android.permission.READ_EXTERNAL_STORAGE", "android.permission.WRITE_EXTERNAL_STORAGE"].into_iter().enumerate() {
                let s = env.string(name)?;
                // SAFETY: SetObjectArrayElement(env, array, index, string).
                unsafe {
                    let set: extern "system" fn(*mut c_void, Obj, i32, Obj) = env.func(f::SET_OBJECT_ARRAY_ELEMENT);
                    set(env.0, list, i as i32, s);
                }
                env.check()?;
            }
            env.call_void(activity, "requestPermissions", "([Ljava/lang/String;I)V", &[obj(list), int(1)])
        }
    });
}

/// The root of the shared storage (usually /storage/emulated/0).
pub(crate) fn shared_root() -> Option<std::path::PathBuf> {
    static ROOT: OnceLock<Option<std::path::PathBuf>> = OnceLock::new();
    ROOT.get_or_init(|| {
        with_env(|env, _| {
            let dir = env.call_static_object("android/os/Environment", "getExternalStorageDirectory", "()Ljava/io/File;", &[])?;
            let path = env.call_object(env.nonnull(dir)?, "getAbsolutePath", "()Ljava/lang/String;", &[])?;
            Ok(std::path::PathBuf::from(env.rust_string(env.nonnull(path)?)?))
        })
        .or_else(|| Some(std::path::PathBuf::from("/storage/emulated/0")))
    })
    .clone()
}

// ---- network -------------------------------------------------------------------------

/// Holds a Wi-Fi multicast lock from the first call on, so mDNS answers from
/// network scanners reach the app.
pub(crate) fn multicast_lock() {
    static LOCK: OnceLock<Option<usize>> = OnceLock::new();
    LOCK.get_or_init(|| {
        with_env(|env, activity| {
            let wifi = system_service(env, activity, "wifi")?;
            let tag = env.string("Painter scanners")?;
            let lock =
                env.call_object(wifi, "createMulticastLock", "(Ljava/lang/String;)Landroid/net/wifi/WifiManager$MulticastLock;", &[obj(tag)])?;
            let lock = env.nonnull(lock)?;
            env.call_void(lock, "setReferenceCounted", "(Z)V", &[boolean(false)])?;
            env.call_void(lock, "acquire", "()V", &[])?;
            // Kept (a global reference) for the life of the app.
            Ok(env.global(lock) as usize)
        })
    });
}

// ---- clipboard -----------------------------------------------------------------------

/// The clipboard's first item, if any.
fn clip_item(env: &Env, activity: Obj) -> R<Option<Obj>> {
    let cm = system_service(env, activity, "clipboard")?;
    let clip = env.call_object(cm, "getPrimaryClip", "()Landroid/content/ClipData;", &[])?;
    if clip.is_null() || env.call_int(clip, "getItemCount", "()I", &[])? < 1 {
        return Ok(None);
    }
    let item = env.call_object(clip, "getItemAt", "(I)Landroid/content/ClipData$Item;", &[int(0)])?;
    Ok((!item.is_null()).then_some(item))
}

/// The bytes of the file behind the clipboard's first item, when it's a URI
/// (pictures copied in other apps come that way).
pub(crate) fn clipboard_file() -> Option<Vec<u8>> {
    with_env(|env, activity| {
        let Some(item) = clip_item(env, activity)? else { return Ok(None) };
        let uri = env.call_object(item, "getUri", "()Landroid/net/Uri;", &[])?;
        if uri.is_null() {
            return Ok(None);
        }
        read_uri(env, activity, uri).map(Some)
    })
    .flatten()
}

/// The file another app asked Painter to open ("Open with", a tap in a file
/// manager): its name (or a guess) and bytes. Read once, at startup.
pub(crate) fn opened_file() -> Option<(String, Vec<u8>)> {
    with_env(|env, activity| {
        let intent = env.call_object(activity, "getIntent", "()Landroid/content/Intent;", &[])?;
        if intent.is_null() {
            return Ok(None);
        }
        let action = env.call_object(intent, "getAction", "()Ljava/lang/String;", &[])?;
        if action.is_null() || env.rust_string(action)? != "android.intent.action.VIEW" {
            return Ok(None);
        }
        let uri = env.call_object(intent, "getData", "()Landroid/net/Uri;", &[])?;
        if uri.is_null() {
            return Ok(None);
        }
        let segment = env.call_object(uri, "getLastPathSegment", "()Ljava/lang/String;", &[])?;
        let name = if segment.is_null() { String::new() } else { env.rust_string(segment)? };
        let bytes = read_uri(env, activity, uri)?;
        Ok(Some((name, bytes)))
    })
    .flatten()
}

/// Everything behind a content or file URI.
fn read_uri(env: &Env, activity: Obj, uri: Obj) -> R<Vec<u8>> {
    let resolver = env.call_object(activity, "getContentResolver", "()Landroid/content/ContentResolver;", &[])?;
    let stream = env.call_object(env.nonnull(resolver)?, "openInputStream", "(Landroid/net/Uri;)Ljava/io/InputStream;", &[obj(uri)])?;
    let stream = env.nonnull(stream)?;
    const CHUNK: usize = 64 * 1024;
    // SAFETY: NewByteArray(env, length).
    let buffer = unsafe {
        let new: extern "system" fn(*mut c_void, i32) -> Obj = env.func(f::NEW_BYTE_ARRAY);
        new(env.0, CHUNK as i32)
    };
    let buffer = env.nonnull(buffer)?;
    let mut chunk = vec![0u8; CHUNK];
    let mut bytes = Vec::new();
    loop {
        let n = env.call_int(stream, "read", "([B)I", &[obj(buffer)])?;
        if n < 0 {
            break;
        }
        let n = (n as usize).min(CHUNK);
        // SAFETY: GetByteArrayRegion(env, array, 0, n, out) into a buffer of CHUNK bytes.
        unsafe {
            let get: extern "system" fn(*mut c_void, Obj, i32, i32, *mut u8) = env.func(f::GET_BYTE_ARRAY_REGION);
            get(env.0, buffer, 0, n as i32, chunk.as_mut_ptr());
        }
        env.check()?;
        bytes.extend_from_slice(&chunk[..n]);
        if bytes.len() > 256 * 1024 * 1024 {
            break;
        }
    }
    env.call_void(stream, "close", "()V", &[])?;
    Ok(bytes)
}

/// A file name for bytes that came without one, from what they start with.
pub(crate) fn name_for(name: &str, bytes: &[u8]) -> String {
    let has_ext = std::path::Path::new(name).extension().is_some_and(|e| e.len() <= 7);
    if has_ext {
        return std::path::Path::new(name).file_name().map_or("Opened".into(), |n| n.to_string_lossy().into_owned());
    }
    let ext = if bytes.starts_with(b"8BPS") {
        "psd"
    } else if bytes.starts_with(b"SQLite format 3") || bytes.starts_with(b"CSFCHUNK") {
        "clip"
    } else if bytes.starts_with(b"\x89PNG") {
        "png"
    } else if bytes.starts_with(&[0xFF, 0xD8]) {
        "jpg"
    } else if bytes.starts_with(b"PK") {
        // A ZIP: OpenRaster and Krita say so in their first entry.
        let head = &bytes[..bytes.len().min(256)];
        let has = |s: &[u8]| head.windows(s.len()).any(|w| w == s);
        if has(b"image/openraster") {
            "ora"
        } else if has(b"application/x-krita") {
            "kra"
        } else {
            "painter"
        }
    } else {
        "png"
    };
    format!("Opened.{ext}")
}

/// The clipboard's text, if any.
pub(crate) fn clipboard_text() -> Option<String> {
    with_env(|env, activity| {
        let Some(item) = clip_item(env, activity)? else { return Ok(None) };
        let text = env.call_object(item, "coerceToText", "(Landroid/content/Context;)Ljava/lang/CharSequence;", &[obj(activity)])?;
        if text.is_null() {
            return Ok(None);
        }
        let text = env.call_object(text, "toString", "()Ljava/lang/String;", &[])?;
        Ok(Some(env.rust_string(env.nonnull(text)?)?))
    })
    .flatten()
}

/// Puts text on the system clipboard.
pub(crate) fn set_clipboard_text(text: &str) {
    with_env(|env, activity| {
        let cm = system_service(env, activity, "clipboard")?;
        let (label, value) = (env.string("Painter")?, env.string(text)?);
        let clip = env.call_static_object(
            "android/content/ClipData",
            "newPlainText",
            "(Ljava/lang/CharSequence;Ljava/lang/CharSequence;)Landroid/content/ClipData;",
            &[obj(label), obj(value)],
        )?;
        env.call_void(cm, "setPrimaryClip", "(Landroid/content/ClipData;)V", &[obj(env.nonnull(clip)?)])
    });
}

// ---- each frame ----------------------------------------------------------------------

/// After each frame: the keyboard follows text fields, and text copied in
/// Painter goes to the system clipboard.
pub(crate) fn end_frame(ctx: &eframe::egui::Context) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static SHOWN: AtomicBool = AtomicBool::new(false);
    let wants = ctx.wants_keyboard_input();
    if SHOWN.swap(wants, Ordering::Relaxed) != wants {
        show_keyboard(wants);
    }
    let copied = ctx.output(|o| {
        o.commands.iter().rev().find_map(|c| match c {
            eframe::egui::OutputCommand::CopyText(t) if !t.is_empty() => Some(t.clone()),
            _ => None,
        })
    });
    if let Some(text) = copied {
        set_clipboard_text(&text);
    }
}

// ---- camera --------------------------------------------------------------------------

/// Opens the system camera app for a photo; it lands in the camera folder,
/// where the file picker (opened alongside) shows it first.
pub(crate) fn open_camera() -> bool {
    with_env(|env, activity| {
        let action = env.string("android.media.action.STILL_IMAGE_CAMERA")?;
        let intent = env.new_object("android/content/Intent", "(Ljava/lang/String;)V", &[obj(action)])?;
        env.call_void(activity, "startActivity", "(Landroid/content/Intent;)V", &[obj(intent)])
    })
    .is_some()
}
