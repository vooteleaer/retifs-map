//! retifs Map for Android: the map's C API (`haiku/capi`, the same calls as
//! the Haiku app) to Kotlin through JNI (`org.retifs.map.Core`): JSON
//! strings, tiles as draw lists (`vector::encode`), images as bytes.
//!
//! retifsd's socket is in the retifs app's files, out of this app's reach:
//! every connection comes from the retifs app's access service, asked
//! through `org.retifs.map.Retifs.connect()` (a descriptor, or -1).

use std::ffi::{c_char, CStr, CString};
use std::os::fd::FromRawFd;
use std::sync::OnceLock;

use jni::objects::{GlobalRef, JByteArray, JClass, JString};
use jni::sys::{jboolean, jbyteArray, jdouble, jint, jstring, JNI_TRUE};
use jni::{JNIEnv, JavaVM};
use retifs_map as capi;

static VM: OnceLock<(JavaVM, GlobalRef)> = OnceLock::new();

/// A connection from the access service, through Kotlin.
fn connect() -> Result<std::os::unix::net::UnixStream, String> {
    let (vm, class) = VM.get().ok_or("not set up")?;
    let mut env = vm.attach_current_thread().map_err(|e| e.to_string())?;
    let class: &JClass = class.as_obj().into();
    let fd = env.call_static_method(class, "connect", "()I", &[]).and_then(|v| v.i()).map_err(|e| e.to_string())?;
    if fd < 0 {
        return Err("retifs hasn't let this app in, or isn't running".into());
    }
    // SAFETY: the descriptor was detached from its ParcelFileDescriptor for us.
    Ok(unsafe { std::os::unix::net::UnixStream::from_raw_fd(fd) })
}

fn arg(env: &mut JNIEnv, s: &JString) -> CString {
    let s: String = env.get_string(s).map(Into::into).unwrap_or_default();
    CString::new(s.replace('\0', " ")).expect("no NUL left")
}

/// A string from the C API, freed, to Java.
fn out(env: &mut JNIEnv, s: *mut c_char) -> jstring {
    if s.is_null() {
        return std::ptr::null_mut();
    }
    let text = unsafe { CStr::from_ptr(s) }.to_string_lossy().into_owned();
    unsafe { capi::rm_free(s) };
    env.new_string(text).map(|s| s.into_raw()).unwrap_or(std::ptr::null_mut())
}

/// Bytes from the C API, freed, to Java (null for none).
fn bytes(env: &mut JNIEnv, p: *mut u8, len: usize) -> jbyteArray {
    if p.is_null() {
        return std::ptr::null_mut();
    }
    let data = unsafe { std::slice::from_raw_parts(p, len) }.to_vec();
    unsafe { capi::rm_free_bytes(p, len) };
    env.byte_array_from_slice(&data).map(JByteArray::into_raw).unwrap_or(std::ptr::null_mut())
}

/// Called once: the class whose static `connect()` gives connections.
#[no_mangle]
pub extern "system" fn Java_org_retifs_map_Core_init<'l>(env: JNIEnv<'l>, _: JClass<'l>, retifs: JClass<'l>) {
    if VM.get().is_some() {
        return;
    }
    if let (Ok(vm), Ok(class)) = (env.get_java_vm(), env.new_global_ref(retifs)) {
        let _ = VM.set((vm, class));
        retifs_map_core::set_connector(Box::new(connect));
    }
}

/// `{"libraries", "lazy"}` or `{"error"}`; forgets what was read before.
#[no_mangle]
pub extern "system" fn Java_org_retifs_map_Core_libraries<'l>(mut env: JNIEnv<'l>, _: JClass<'l>) -> jstring {
    let r = capi::rm_libraries();
    out(&mut env, r)
}

/// One library read (`rm_library`): its maps, places and what it is.
#[no_mangle]
pub extern "system" fn Java_org_retifs_map_Core_library<'l>(mut env: JNIEnv<'l>, _: JClass<'l>, ns: JString<'l>, lazy: jboolean) -> jstring {
    let ns = arg(&mut env, &ns);
    let r = unsafe { capi::rm_library(ns.as_ptr(), (lazy == JNI_TRUE) as i32) };
    out(&mut env, r)
}

/// A lazily held library's places around a view: `[{place…}]`.
#[no_mangle]
pub extern "system" fn Java_org_retifs_map_Core_placesIn<'l>(mut env: JNIEnv<'l>, _: JClass<'l>, ns: JString<'l>, west: jdouble, south: jdouble, east: jdouble, north: jdouble, zoom: jint) -> jstring {
    let ns = arg(&mut env, &ns);
    let r = unsafe { capi::rm_places_in(ns.as_ptr(), west, south, east, north, zoom) };
    out(&mut env, r)
}

/// A map's style read: `{"background"}` or `{"error"}`.
#[no_mangle]
pub extern "system" fn Java_org_retifs_map_Core_style<'l>(mut env: JNIEnv<'l>, _: JClass<'l>, tileset: JString<'l>) -> jstring {
    let t = arg(&mut env, &tileset);
    let r = unsafe { capi::rm_style(t.as_ptr()) };
    out(&mut env, r)
}

/// A tile's draw list; null where the map has none, empty when it couldn't
/// be read now (ask again later).
#[no_mangle]
pub extern "system" fn Java_org_retifs_map_Core_tile<'l>(mut env: JNIEnv<'l>, _: JClass<'l>, tileset: JString<'l>, z: jint, x: jint, y: jint) -> jbyteArray {
    let t = arg(&mut env, &tileset);
    let mut len = 0usize;
    let p = unsafe { capi::rm_tile(t.as_ptr(), z, x, y, &mut len) };
    if p.is_null() {
        return if len == 1 { env.new_byte_array(0).map(JByteArray::into_raw).unwrap_or(std::ptr::null_mut()) } else { std::ptr::null_mut() };
    }
    bytes(&mut env, p, len)
}

/// Place `i`'s thumbnail (a photo), or null.
#[no_mangle]
pub extern "system" fn Java_org_retifs_map_Core_thumb<'l>(mut env: JNIEnv<'l>, _: JClass<'l>, i: jint) -> jbyteArray {
    let mut len = 0usize;
    let p = unsafe { capi::rm_thumb(i, &mut len) };
    bytes(&mut env, p, len)
}

/// Place `i`'s larger preview (a photo), or null.
#[no_mangle]
pub extern "system" fn Java_org_retifs_map_Core_preview<'l>(mut env: JNIEnv<'l>, _: JClass<'l>, i: jint) -> jbyteArray {
    let mut len = 0usize;
    let p = unsafe { capi::rm_preview(i, &mut len) };
    bytes(&mut env, p, len)
}

/// A category's icon, `size` pixels square, white on transparent (RGBA), or null.
#[no_mangle]
pub extern "system" fn Java_org_retifs_map_Core_icon<'l>(mut env: JNIEnv<'l>, _: JClass<'l>, category: JString<'l>, size: jint) -> jbyteArray {
    let c = arg(&mut env, &category);
    let mut color = 0u32;
    let p = unsafe { capi::rm_icon(c.as_ptr(), size, &mut color) };
    let n = (size.clamp(4, 256) as usize).pow(2) * 4;
    bytes(&mut env, p, n)
}

/// A category's circle colour (0xRRGGBB).
#[no_mangle]
pub extern "system" fn Java_org_retifs_map_Core_iconColor<'l>(mut env: JNIEnv<'l>, _: JClass<'l>, category: JString<'l>) -> jint {
    let c = arg(&mut env, &category);
    let mut color = 0u32;
    let p = unsafe { capi::rm_icon(c.as_ptr(), 4, &mut color) };
    unsafe { capi::rm_free_bytes(p, 4 * 4 * 4) };
    color as jint
}

/// A record's attributes for its bubble: `[[key, value]]`.
#[no_mangle]
pub extern "system" fn Java_org_retifs_map_Core_details<'l>(mut env: JNIEnv<'l>, _: JClass<'l>, ns: JString<'l>, id: JString<'l>) -> jstring {
    let (ns, id) = (arg(&mut env, &ns), arg(&mut env, &id));
    let r = unsafe { capi::rm_details(ns.as_ptr(), id.as_ptr()) };
    out(&mut env, r)
}

/// An address search: `[{"name", "lat", "lon", "zoom", "place"}]` or `{"error"}`.
#[no_mangle]
pub extern "system" fn Java_org_retifs_map_Core_find<'l>(mut env: JNIEnv<'l>, _: JClass<'l>, text: JString<'l>) -> jstring {
    let t = arg(&mut env, &text);
    let r = unsafe { capi::rm_find(t.as_ptr()) };
    out(&mut env, r)
}

/// A new place in `ns`: `{"id"}` or `{"error"}`.
#[no_mangle]
pub extern "system" fn Java_org_retifs_map_Core_savePlace<'l>(mut env: JNIEnv<'l>, _: JClass<'l>, ns: JString<'l>, name: JString<'l>, lat: jdouble, lon: jdouble, address: JString<'l>) -> jstring {
    let (ns, name, address) = (arg(&mut env, &ns), arg(&mut env, &name), arg(&mut env, &address));
    let r = unsafe { capi::rm_save_place(ns.as_ptr(), name.as_ptr(), lat, lon, address.as_ptr()) };
    out(&mut env, r)
}

/// Places a record (`key`: `geo`, `geo:home`): `{}` or `{"error"}`.
#[no_mangle]
pub extern "system" fn Java_org_retifs_map_Core_place<'l>(mut env: JNIEnv<'l>, _: JClass<'l>, ns: JString<'l>, id: JString<'l>, key: JString<'l>, lat: jdouble, lon: jdouble) -> jstring {
    let (ns, id, key) = (arg(&mut env, &ns), arg(&mut env, &id), arg(&mut env, &key));
    let r = unsafe { capi::rm_place(ns.as_ptr(), id.as_ptr(), key.as_ptr(), lat, lon) };
    out(&mut env, r)
}

/// Public libraries heard that the map can use: `{"usable": […], "other"}`.
#[no_mangle]
pub extern "system" fn Java_org_retifs_map_Core_heard<'l>(mut env: JNIEnv<'l>, _: JClass<'l>) -> jstring {
    let r = capi::rm_heard();
    out(&mut env, r)
}

/// `{ns: change count}` of the libraries read.
#[no_mangle]
pub extern "system" fn Java_org_retifs_map_Core_changeCounts<'l>(mut env: JNIEnv<'l>, _: JClass<'l>) -> jstring {
    let r = capi::rm_change_counts();
    out(&mut env, r)
}
