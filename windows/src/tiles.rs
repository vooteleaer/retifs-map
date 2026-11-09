//! A map library's tiles, drawn by the app itself as the Haiku app draws
//! them: fetched and decoded on a few threads, the style applied
//! (`vector::draw_list`), and made ready for Direct2D there too (path
//! geometries and text layouts), so the window only puts a tile on a bitmap
//! once and then draws that.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use retifs_map_core::vector::{self, Draw, Style};
use retifs_map_core::{Map, Tileset};
use windows::core::{w, HSTRING};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;

/// A tile's size on screen at its own zoom (DIPs), as the styles expect.
pub const SIZE: f32 = 256.0;
/// Threads fetching and drawing tiles: one slow tile mustn't hold up the rest.
const THREADS: usize = 4;
/// Raw tiles kept for zooms past a map's deepest (each drawn from its
/// ancestor many times).
const RAW_KEEP: usize = 64;

/// Direct2D's and DirectWrite's factories, shared by every thread (D2D's
/// is made multithreaded for that; DirectWrite's shared one is).
pub struct Factories {
    pub d2d: ID2D1Factory,
    pub dwrite: IDWriteFactory,
}

unsafe impl Send for Factories {}
unsafe impl Sync for Factories {}

pub fn factories() -> &'static Factories {
    static F: OnceLock<Factories> = OnceLock::new();
    F.get_or_init(|| unsafe {
        Factories {
            d2d: D2D1CreateFactory(D2D1_FACTORY_TYPE_MULTI_THREADED, None).expect("Direct2D"),
            dwrite: DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).expect("DirectWrite"),
        }
    })
}

pub enum Paint {
    Fill { color: u32, outline: u32 },
    Line { color: u32, width: f32, stroke: ID2D1StrokeStyle },
}

/// Things of one look, in one geometry (in a tile's DIPs, 0..SIZE).
pub struct Shape {
    pub geometry: ID2D1PathGeometry,
    pub paint: Paint,
}

pub struct Label {
    pub layout: IDWriteTextLayout,
    pub width: f32,
    pub height: f32,
    /// Where (tile units, 0..1) and at what angle (degrees).
    pub x: f32,
    pub y: f32,
    pub angle: f32,
    pub color: u32,
    pub halo: u32,
    pub halo_width: f32,
}

/// What a tile draws.
pub struct Art {
    pub shapes: Vec<Shape>,
    pub labels: Vec<Label>,
}

// Made on a tile thread, then only used by the window: the geometries come
// from the multithreaded factory, the layouts from DirectWrite's shared one.
unsafe impl Send for Art {}
unsafe impl Sync for Art {}

/// A tile done: its art, None where the map has none, an error when it
/// couldn't be read now (asked again later).
pub struct Done {
    pub map: u64,
    pub key: (u32, u32, u32),
    pub art: Result<Option<Art>, String>,
}

struct Request {
    map: u64,
    key: (u32, u32, u32),
    tileset: Arc<Tileset>,
    style: Arc<Style>,
}

static QUEUE: (Mutex<Vec<Request>>, Condvar) = (Mutex::new(Vec::new()), Condvar::new());

pub fn start() {
    for _ in 0..THREADS {
        std::thread::spawn(worker);
    }
}

/// Asks for tile `key` (z, x, y) of map number `map`.
pub fn ask(map: u64, key: (u32, u32, u32), tileset: &Arc<Tileset>, style: &Arc<Style>) {
    let (q, c) = &QUEUE;
    q.lock().unwrap().push(Request { map, key, tileset: tileset.clone(), style: style.clone() });
    c.notify_one();
}

/// Drops what's queued but no longer looked at (moved or zoomed away);
/// the keys dropped, no longer pending.
pub fn keep_only(map: u64, wanted: &HashSet<(u32, u32, u32)>) -> Vec<(u32, u32, u32)> {
    let mut q = QUEUE.0.lock().unwrap();
    let mut dropped = Vec::new();
    q.retain(|r| {
        let keep = r.map == map && wanted.contains(&r.key);
        if !keep && r.map == map {
            dropped.push(r.key);
        }
        keep
    });
    dropped
}

fn worker() {
    // A connection per thread: a Map's calls run one at a time.
    let m = Map::new();
    let mut strokes: HashMap<Vec<u32>, ID2D1StrokeStyle> = HashMap::new();
    let mut formats: HashMap<u32, IDWriteTextFormat> = HashMap::new();
    loop {
        let r = {
            let (lock, c) = &QUEUE;
            let mut q = lock.lock().unwrap();
            while q.is_empty() {
                q = c.wait(q).unwrap();
            }
            // The newest first: what was just looked at.
            q.pop().unwrap()
        };
        let art = tile(&m, &r).map(|list| list.map(|list| unsafe { build(&list, &mut strokes, &mut formats) }));
        crate::work::send(crate::work::Event::Tile(Done { map: r.map, key: r.key, art }));
    }
}

/// A tile's west, north, east and south edges (degrees).
fn tile_area(z: u32, x: u32, y: u32) -> (f64, f64, f64, f64) {
    let n = 2f64.powi(z as i32);
    let lon = |x: f64| x / n * 360.0 - 180.0;
    let lat = |y: f64| (std::f64::consts::PI * (1.0 - 2.0 * y / n)).sinh().atan().to_degrees();
    (lon(x as f64), lat(y as f64), lon(x as f64 + 1.0), lat(y as f64 + 1.0))
}

/// What to draw of a tile: past the map's deepest zoom, a part of its tile
/// there, enlarged (as `rm_tile` does for the Haiku app).
fn tile(m: &Map, r: &Request) -> Result<Option<Vec<Draw>>, String> {
    let (z, x, y) = r.key;
    let t = &r.tileset;
    // Outside the map's area there are no tiles: not asked for (a missing
    // tile is looked for on other nodes too, and that takes a while).
    if let Some([west, south, east, north]) = t.bounds {
        let (w, n, e, s) = tile_area(z, x, y);
        if e < west || w > east || s > north || n < south {
            return Ok(None);
        }
    }
    let deepest = t.maxzoom.clamp(0, 22) as u32;
    let d = z.saturating_sub(deepest);
    let (tz, tx, ty) = (z - d, x >> d, y >> d);
    let Some(data) = raw(m, t, &r.style, tz, tx, ty)? else { return Ok(None) };
    let Ok(decoded) = vector::decode(&data) else { return Ok(None) };
    let scale = (1u32 << d) as f32;
    let offset = ((x - (tx << d)) as f32, (y - (ty << d)) as f32);
    Ok(Some(vector::draw_list(&r.style, &decoded, z as f64, scale, offset)))
}

type RawKey = (String, u32, u32, u32);

/// A tile's bytes, kept a while (an ancestor is asked for by its children).
fn raw(m: &Map, t: &Tileset, style: &Style, z: u32, x: u32, y: u32) -> Result<Option<Vec<u8>>, String> {
    static RAW: Mutex<(Option<HashMap<RawKey, Option<Vec<u8>>>>, VecDeque<RawKey>)> = Mutex::new((None, VecDeque::new()));
    let key = (t.id.clone(), z, x, y);
    if let Some(hit) = RAW.lock().unwrap().0.as_ref().and_then(|c| c.get(&key)) {
        return Ok(hit.clone());
    }
    let data = m.file(&t.ns, &style.tile_path(z, x, y))?;
    let mut raw = RAW.lock().unwrap();
    raw.0.get_or_insert_with(HashMap::new).insert(key.clone(), data.clone());
    raw.1.push_back(key);
    while raw.1.len() > RAW_KEEP {
        if let Some(old) = raw.1.pop_front() {
            if let Some(c) = raw.0.as_mut() {
                c.remove(&old);
            }
        }
    }
    Ok(data)
}

fn point(p: (f32, f32)) -> D2D_POINT_2F {
    D2D_POINT_2F { x: p.0 * SIZE, y: p.1 * SIZE }
}

/// A geometry of these parts: closed and filled (areas) or open (lines).
unsafe fn geometry(parts: &[&Vec<(f32, f32)>], filled: bool) -> Option<ID2D1PathGeometry> {
    let g = factories().d2d.CreatePathGeometry().ok()?;
    let sink = g.Open().ok()?;
    // Winding: outer rings and holes turn opposite ways in a vector tile, and
    // areas of one look merged into one geometry mustn't cancel out.
    sink.SetFillMode(D2D1_FILL_MODE_WINDING);
    for p in parts {
        if p.len() < if filled { 3 } else { 2 } {
            continue;
        }
        let pts: Vec<D2D_POINT_2F> = p.iter().map(|q| point(*q)).collect();
        sink.BeginFigure(pts[0], if filled { D2D1_FIGURE_BEGIN_FILLED } else { D2D1_FIGURE_BEGIN_HOLLOW });
        sink.AddLines(&pts[1..]);
        sink.EndFigure(if filled { D2D1_FIGURE_END_CLOSED } else { D2D1_FIGURE_END_OPEN });
    }
    sink.Close().ok()?;
    Some(g)
}

/// Round caps and joins; a dash in widths (Direct2D's unit), flat-ended.
unsafe fn stroke(dash: &[f32], width: f32, strokes: &mut HashMap<Vec<u32>, ID2D1StrokeStyle>) -> Option<ID2D1StrokeStyle> {
    let dashes: Vec<f32> = if dash.len() >= 2 { dash.iter().map(|d| (d / width.max(0.1)).max(0.1)).collect() } else { Vec::new() };
    let key: Vec<u32> = dashes.iter().map(|d| d.to_bits()).collect();
    if let Some(s) = strokes.get(&key) {
        return Some(s.clone());
    }
    let cap = if dashes.is_empty() { D2D1_CAP_STYLE_ROUND } else { D2D1_CAP_STYLE_FLAT };
    let props = D2D1_STROKE_STYLE_PROPERTIES {
        startCap: cap,
        endCap: cap,
        dashCap: D2D1_CAP_STYLE_FLAT,
        lineJoin: D2D1_LINE_JOIN_ROUND,
        miterLimit: 10.0,
        dashStyle: if dashes.is_empty() { D2D1_DASH_STYLE_SOLID } else { D2D1_DASH_STYLE_CUSTOM },
        dashOffset: 0.0,
    };
    let s = factories().d2d.CreateStrokeStyle(&props, if dashes.is_empty() { None } else { Some(&dashes) }).ok()?;
    strokes.insert(key, s.clone());
    Some(s)
}

/// The draw list as shapes (things of one look in a row merged: a city's
/// buildings are one geometry, not thousands) and labels.
unsafe fn build(list: &[Draw], strokes: &mut HashMap<Vec<u32>, ID2D1StrokeStyle>, formats: &mut HashMap<u32, IDWriteTextFormat>) -> Art {
    let mut art = Art { shapes: Vec::new(), labels: Vec::new() };
    let mut i = 0;
    while i < list.len() {
        match &list[i] {
            Draw::Fill { color, outline, .. } => {
                let mut parts = Vec::new();
                while let Some(Draw::Fill { color: c, outline: o, parts: p }) = list.get(i) {
                    if c != color || o != outline {
                        break;
                    }
                    parts.extend(p.iter());
                    i += 1;
                }
                if let Some(geometry) = geometry(&parts, true) {
                    art.shapes.push(Shape { geometry, paint: Paint::Fill { color: *color, outline: *outline } });
                }
            }
            Draw::Line { color, width, dash, .. } => {
                let mut parts = Vec::new();
                while let Some(Draw::Line { color: c, width: wd, dash: d, parts: p }) = list.get(i) {
                    if c != color || wd != width || d != dash {
                        break;
                    }
                    parts.extend(p.iter());
                    i += 1;
                }
                if let (Some(geometry), Some(stroke)) = (geometry(&parts, false), stroke(dash, *width, strokes)) {
                    art.shapes.push(Shape { geometry, paint: Paint::Line { color: *color, width: width.max(0.5), stroke } });
                }
            }
            Draw::Label { color, halo, halo_width, size, x, y, angle, text } => {
                i += 1;
                let key = (size * 2.0).round() as u32;
                let format = match formats.get(&key) {
                    Some(f) => f.clone(),
                    None => {
                        let Ok(f) = factories().dwrite.CreateTextFormat(w!("Segoe UI"), None, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, key as f32 / 2.0, w!("")) else { continue };
                        let _ = f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
                        formats.insert(key, f.clone());
                        f
                    }
                };
                let wide: Vec<u16> = HSTRING::from(text.as_str()).as_wide().to_vec();
                let Ok(layout) = factories().dwrite.CreateTextLayout(&wide, &format, 4096.0, 4096.0) else { continue };
                let mut m = DWRITE_TEXT_METRICS::default();
                if layout.GetMetrics(&mut m).is_err() {
                    continue;
                }
                art.labels.push(Label { layout, width: m.width, height: m.height, x: *x, y: *y, angle: *angle, color: *color, halo: *halo, halo_width: *halo_width });
            }
        }
    }
    art
}
