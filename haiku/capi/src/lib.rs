//! retifs Map for Haiku: the shared core to C (and so to the BeAPI front
//! end), JSON in and out, a map's tiles as draw lists (bytes). The same calls
//! as the GTK front end makes; the tiles are drawn by the app itself from
//! the map library's style (`vector`), where GTK hands them to libshumate.
//!
//! The last load (`rm_load`) and the places fetched for a view are kept
//! here, so the other calls name places by number. Every returned string is
//! the caller's to free with `rm_free`, every byte buffer with
//! `rm_free_bytes`. Calls block: the front end makes them off its window
//! thread.

use std::collections::{HashMap, VecDeque};
use std::ffi::{CStr, CString, c_char, c_double, c_int};
use std::sync::{Mutex, OnceLock};

#[cfg(test)]
mod tests {
    #[test]
    fn tiles_cover_their_area() {
        let (w, n, e, s) = super::tile_area(8, 145, 75);
        // Tallinn (24.75, 59.44) is in tile 8/145/75.
        assert!(w < 24.75 && 24.75 < e && s < 59.44 && 59.44 < n, "{w} {n} {e} {s}");
    }
}

use retifs_map_core::vector::{self, Style};
use retifs_map_core::{Kind, Map, Place, Tileset};
use serde_json::{Value, json};

/// Raw tiles kept for zooms past a map's deepest (each drawn from its
/// ancestor many times).
const TILE_CACHE: usize = 64;

#[derive(Default)]
struct State {
    libraries: Vec<String>,
    tilesets: Vec<Tileset>,
    styles: HashMap<String, Style>,
    places: Vec<Place>,
    tiles: HashMap<(String, u32, u32, u32), Option<Vec<u8>>>,
    tile_order: VecDeque<(String, u32, u32, u32)>,
}

fn map() -> &'static Map {
    static MAP: OnceLock<Map> = OnceLock::new();
    MAP.get_or_init(Map::new)
}

thread_local! {
    /// A connection per tile-drawing thread: a Map's calls run one at a
    /// time, and one slow tile mustn't hold up the others.
    static TILE_MAP: Map = Map::new();
}

fn state() -> std::sync::MutexGuard<'static, State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

fn out(v: Value) -> *mut c_char {
    CString::new(v.to_string().replace('\0', " ")).expect("no NUL left").into_raw()
}

fn done(r: Result<Value, String>) -> *mut c_char {
    out(match r {
        Ok(v) => v,
        Err(e) => json!({ "error": e }),
    })
}

/// # Safety
/// `s` is null or a NUL-terminated string.
unsafe fn arg(s: *const c_char) -> String {
    if s.is_null() { String::new() } else { unsafe { CStr::from_ptr(s) }.to_string_lossy().into_owned() }
}

fn kind(k: Kind) -> &'static str {
    match k {
        Kind::Photo => "photo",
        Kind::Person => "person",
        Kind::Event => "event",
        Kind::Other => "place",
    }
}

fn place_json(i: usize, p: &Place) -> Value {
    json!({
        "i": i, "ns": p.ns, "id": p.id, "kind": kind(p.kind), "title": p.title, "label": p.label,
        "lat": p.lat, "lon": p.lon, "when": p.when, "detail": p.detail, "minzoom": p.minzoom,
        "thumb": p.thumb.is_some(),
    })
}

fn bytes(v: Vec<u8>, len: *mut usize) -> *mut u8 {
    let mut b = v.into_boxed_slice();
    unsafe { *len = b.len() };
    let p = b.as_mut_ptr();
    std::mem::forget(b);
    p
}

/// Everything the window shows, read again: `{"libraries", "lazy",
/// "address", "tilesets": [{"ns", "id", "name", "bounds", "minzoom",
/// "maxzoom", "overlay", "attribution"}], "places": [{"i", "ns", "id",
/// "kind", "title", "label", "lat", "lon", "when", "detail", "minzoom",
/// "thumb"}], "unplaced": [{"ns", "id", "kind", "title", "key", "address"}],
/// "error"}`. Libraries held lazily (a map's public places) aren't read
/// whole: `rm_places_in` asks them for a view's places.
#[unsafe(no_mangle)]
pub extern "C" fn rm_load() -> *mut c_char {
    let m = map();
    let libraries = match m.libraries() {
        Ok(l) => l,
        Err(e) => return out(json!({ "error": e })),
    };
    let lazy = m.lazy(&libraries);
    let address = m.address_libraries(&libraries);
    let whole: Vec<String> = libraries.iter().filter(|l| !lazy.contains(l) && !address.contains(l)).cloned().collect();
    let tilesets = m.tilesets(&libraries);
    let (places, unplaced) = m.places(&whole).unwrap_or_default();
    let v = json!({
        "libraries": libraries,
        "lazy": lazy,
        "address": address,
        "tilesets": tilesets.iter().map(|t| json!({
            "ns": t.ns, "id": t.id, "name": t.name, "bounds": t.bounds, "minzoom": t.minzoom,
            "maxzoom": t.maxzoom, "overlay": t.overlay, "attribution": t.attribution,
        })).collect::<Vec<_>>(),
        "places": places.iter().enumerate().map(|(i, p)| place_json(i, p)).collect::<Vec<_>>(),
        "unplaced": unplaced.iter().map(|u| json!({
            "ns": u.ns, "id": u.id, "kind": kind(u.kind), "title": u.title, "key": u.key, "address": u.address,
        })).collect::<Vec<_>>(),
    });
    let mut s = state();
    s.libraries = libraries;
    s.tilesets = tilesets;
    s.places = places;
    out(v)
}

/// The places of a lazily held library around a view (zoom-12 and zoom-14
/// cells, as the GTK app asks), added to those kept: `[{place…}]`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rm_places_in(ns: *const c_char, west: c_double, south: c_double, east: c_double, north: c_double, zoom: c_int) -> *mut c_char {
    let ns = unsafe { arg(ns) };
    let mut cells = Map::cells_at(12, west, south, east, north);
    if zoom >= 14 {
        cells.extend(Map::cells_at(14, west, south, east, north));
    }
    let found = map().places_in(&ns, &cells).unwrap_or_default();
    let mut s = state();
    let mut added = Vec::new();
    for p in found.into_iter().flat_map(|(_, ps)| ps) {
        if !s.places.iter().any(|q| q.ns == p.ns && q.id == p.id && q.label == p.label) {
            let i = s.places.len();
            added.push(place_json(i, &p));
            s.places.push(p);
        }
    }
    out(json!(added))
}

/// Reads a map's style: `{"background"}` (0xAARRGGBB) or `{"error"}`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rm_style(tileset: *const c_char) -> *mut c_char {
    let id = unsafe { arg(tileset) };
    let t = state().tilesets.iter().find(|t| t.id == id).cloned();
    let Some(t) = t else { return out(json!({ "error": "no such map" })) };
    done(map().style_json(&t).and_then(|j| Style::parse(&j)).map(|style| {
        let bg = style.background;
        state().styles.insert(id, style);
        json!({ "background": bg })
    }))
}

/// A tile's bytes: Ok(None) where the map has none, an error when it
/// couldn't be read now (not kept: asked again later).
fn raw_tile(t: &Tileset, style: &Style, z: u32, x: u32, y: u32) -> Result<Option<Vec<u8>>, String> {
    let key = (t.id.clone(), z, x, y);
    if let Some(hit) = state().tiles.get(&key) {
        return Ok(hit.clone());
    }
    let data = TILE_MAP.with(|m| m.file(&t.ns, &style.tile_path(z, x, y)))?;
    let mut s = state();
    s.tiles.insert(key.clone(), data.clone());
    s.tile_order.push_back(key);
    while s.tile_order.len() > TILE_CACHE {
        if let Some(old) = s.tile_order.pop_front() {
            s.tiles.remove(&old);
        }
    }
    Ok(data)
}

/// What to draw of map `tileset`'s tile z/x/y (`vector::encode`), its
/// length in `*len`. NULL with `*len` 0 where the map has no tile there, 1
/// when it couldn't be read now (ask again later). Past the map's deepest
/// zoom, a part of its tile there, enlarged.
///
/// # Safety
/// `tileset` is a NUL-terminated string; `len` points at a size_t.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rm_tile(tileset: *const c_char, z: c_int, x: c_int, y: c_int, len: *mut usize) -> *mut u8 {
    let id = unsafe { arg(tileset) };
    unsafe { *len = 0 };
    let (t, style) = {
        let s = state();
        let Some(t) = s.tilesets.iter().find(|t| t.id == id).cloned() else { return std::ptr::null_mut() };
        let Some(style) = s.styles.get(&id).cloned() else { return std::ptr::null_mut() };
        (t, style)
    };
    let (z, x, y) = (z.max(0) as u32, x.max(0) as u32, y.max(0) as u32);
    // Outside the map's area there are no tiles: not asked for (a missing
    // tile is looked for on other nodes too, and that takes a while).
    if let Some([west, south, east, north]) = t.bounds {
        let (w, n, e, s) = tile_area(z, x, y);
        if e < west || w > east || s > north || n < south {
            return std::ptr::null_mut();
        }
    }
    let deepest = t.maxzoom.clamp(0, 22) as u32;
    let d = z.saturating_sub(deepest);
    let (tz, tx, ty) = (z - d, x >> d, y >> d);
    let data = match raw_tile(&t, &style, tz, tx, ty) {
        Ok(Some(data)) => data,
        Ok(None) => return std::ptr::null_mut(),
        Err(_) => {
            unsafe { *len = 1 };
            return std::ptr::null_mut();
        }
    };
    let Ok(tile) = vector::decode(&data) else { return std::ptr::null_mut() };
    let scale = (1u32 << d) as f32;
    let offset = ((x - (tx << d)) as f32, (y - (ty << d)) as f32);
    bytes(vector::encode(&vector::draw_list(&style, &tile, z as f64, scale, offset)), len)
}

/// A tile's west, north, east and south edges (degrees).
fn tile_area(z: u32, x: u32, y: u32) -> (f64, f64, f64, f64) {
    let n = 2f64.powi(z as i32);
    let lon = |x: f64| x / n * 360.0 - 180.0;
    let lat = |y: f64| (std::f64::consts::PI * (1.0 - 2.0 * y / n)).sinh().atan().to_degrees();
    (lon(x as f64), lat(y as f64), lon(x as f64 + 1.0), lat(y as f64 + 1.0))
}

/// A photo's thumbnail (place number `i`), its length in `*len`; NULL if none.
///
/// # Safety
/// `len` points at a size_t.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rm_thumb(i: c_int, len: *mut usize) -> *mut u8 {
    let p = state().places.get(i.max(0) as usize).cloned();
    match p.and_then(|p| map().thumb(&p)) {
        Some(b) => bytes(b, len),
        None => std::ptr::null_mut(),
    }
}

/// A record's attributes for its bubble: `[[key, value]]`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rm_details(ns: *const c_char, id: *const c_char) -> *mut c_char {
    let (ns, id) = unsafe { (arg(ns), arg(id)) };
    out(json!(map().details(&ns, &id)))
}

/// An address search in the address libraries (and the places loaded):
/// `[{"name", "lat", "lon", "zoom", "place"}]` ("place": its number, or
/// null for an address), or `{"error"}`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rm_find(text: *const c_char) -> *mut c_char {
    let text = unsafe { arg(text) };
    let here = state().places.clone();
    done(map().find(&text, &here).map(|found| {
        let s = state();
        json!(found
            .iter()
            .map(|f| {
                let i = f.place.as_ref().and_then(|p| s.places.iter().position(|q| q.ns == p.ns && q.id == p.id && q.label == p.label));
                json!({ "name": f.name, "lat": f.lat, "lon": f.lon, "zoom": f.zoom, "place": i })
            })
            .collect::<Vec<_>>())
    }))
}

/// Saves a new place in `ns`: `{"id"}` or `{"error"}`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rm_save_place(ns: *const c_char, name: *const c_char, lat: c_double, lon: c_double, address: *const c_char) -> *mut c_char {
    let (ns, name, address) = unsafe { (arg(ns), arg(name), arg(address)) };
    done(map().save_place(&ns, &name, lat, lon, &address).map(|id| json!({ "id": id })))
}

/// Places a record (`key`: `geo`, `geo:home`): `{}` or `{"error"}`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rm_place(ns: *const c_char, id: *const c_char, key: *const c_char, lat: c_double, lon: c_double) -> *mut c_char {
    let (ns, id, key) = unsafe { (arg(ns), arg(id), arg(key)) };
    done(map().place(&ns, &id, &key, lat, lon).map(|()| json!({})))
}

/// Public libraries heard on the network (ADR-0029), those the map can use
/// (or can't read yet): `{"usable": [{"name", "about", "address", "here",
/// "hops", "node", "heard_ms", "readable", "v", "fmt", "has": [[type,
/// count]], "bounds"}], "other": count}`, or `{"error"}`.
#[unsafe(no_mangle)]
pub extern "C" fn rm_heard() -> *mut c_char {
    done(map().heard().map(|list| {
        let (usable, other): (Vec<_>, Vec<_>) = list.into_iter().partition(|l| !l.readable || retifs_map_core::map_uses(l));
        json!({
            "usable": usable.iter().map(|l| json!({
                "name": l.name, "about": l.about, "address": l.address, "here": l.here,
                "hops": l.hops, "node": l.node, "heard_ms": l.heard_ms, "readable": l.readable,
                "v": l.v, "fmt": l.fmt, "has": l.has, "bounds": l.bounds,
            })).collect::<Vec<_>>(),
            "other": other.len(),
        })
    }))
}

/// Joins a heard library (by its address), lazily: `{"ns"}` or `{"error"}`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rm_join(address: *const c_char) -> *mut c_char {
    let address = unsafe { arg(address) };
    let m = map();
    done(m.heard().and_then(|all| {
        let l = all.into_iter().find(|l| l.address == address).ok_or("no longer heard")?;
        m.join_heard(&l).map(|ns| json!({ "ns": ns }))
    }))
}

/// Adds a library from its `retifs:` link as a layer: `{"ns"}` or `{"error"}`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rm_add_layer(link: *const c_char, name: *const c_char) -> *mut c_char {
    let (link, name) = unsafe { (arg(link), arg(name)) };
    done(map().add_layer(&link, &name).map(|ns| json!({ "ns": ns })))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rm_leave(ns: *const c_char) -> *mut c_char {
    let ns = unsafe { arg(ns) };
    done(map().leave(&ns).map(|()| json!({})))
}

/// How often each library loaded has changed: `{ns: count}` (reload when it differs).
#[unsafe(no_mangle)]
pub extern "C" fn rm_change_counts() -> *mut c_char {
    let libraries = state().libraries.clone();
    out(json!(map().change_counts(&libraries)))
}

/// # Safety
/// `s` came from this library, once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rm_free(s: *mut c_char) {
    if !s.is_null() {
        drop(unsafe { CString::from_raw(s) });
    }
}

/// # Safety
/// `b` and `len` came from this library, once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rm_free_bytes(b: *mut u8, len: usize) {
    if !b.is_null() {
        drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(b, len)) });
    }
}
