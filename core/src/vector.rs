//! A map library's vector tiles drawn by the app itself (Haiku: with the
//! Haiku API), from the same MapLibre style the Linux app gives libshumate:
//! the tile decoded (Mapbox Vector Tile), the style's layers applied (their
//! filters, zoom ranges and paint), and what to draw handed over as a list:
//! filled areas, lines and labels, in tile units (0..1 across the tile).
//!
//! Only what the maps' styles use is understood: `fill`, `line`, `symbol`
//! and `background` layers; the expressions `get`, `has`, `literal`,
//! `coalesce`, `==`, `!=`, `<`, `<=`, `>`, `>=`, `in`, `!`, `all`, `any`,
//! `zoom`, and `interpolate` (linear) over the zoom.

use std::collections::HashMap;

use serde_json::Value;

// ------------------------------------------------------------------ tiles

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeomType {
    Point,
    Line,
    Polygon,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct Feature {
    pub kind: GeomType,
    pub props: HashMap<String, Value>,
    /// Points, lines or polygon rings, in the layer's extent.
    pub parts: Vec<Vec<(i32, i32)>>,
}

#[derive(Debug, Clone)]
pub struct Layer {
    pub name: String,
    pub extent: u32,
    pub features: Vec<Feature>,
}

#[derive(Debug, Clone, Default)]
pub struct Tile {
    pub layers: Vec<Layer>,
}

/// Reads protobuf fields: (field number, wire type, value or bytes).
struct Pb<'a> {
    b: &'a [u8],
    i: usize,
}

enum Field<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    Fixed32(u32),
    Fixed64(u64),
}

impl<'a> Pb<'a> {
    fn new(b: &'a [u8]) -> Self {
        Pb { b, i: 0 }
    }

    fn varint(&mut self) -> Result<u64, String> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = *self.b.get(self.i).ok_or("a truncated varint")?;
            self.i += 1;
            v |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err("a varint too long".into())
    }

    fn next(&mut self) -> Result<Option<(u32, Field<'a>)>, String> {
        if self.i >= self.b.len() {
            return Ok(None);
        }
        let key = self.varint()?;
        let (num, wire) = ((key >> 3) as u32, key & 7);
        let f = match wire {
            0 => Field::Varint(self.varint()?),
            1 => {
                let s = self.b.get(self.i..self.i + 8).ok_or("a truncated fixed64")?;
                self.i += 8;
                Field::Fixed64(u64::from_le_bytes(s.try_into().unwrap()))
            }
            2 => {
                let n = self.varint()? as usize;
                let s = self.b.get(self.i..self.i + n).ok_or("a truncated field")?;
                self.i += n;
                Field::Bytes(s)
            }
            5 => {
                let s = self.b.get(self.i..self.i + 4).ok_or("a truncated fixed32")?;
                self.i += 4;
                Field::Fixed32(u32::from_le_bytes(s.try_into().unwrap()))
            }
            w => return Err(format!("wire type {w}")),
        };
        Ok(Some((num, f)))
    }
}

fn packed(b: &[u8]) -> Result<Vec<u32>, String> {
    let mut p = Pb::new(b);
    let mut out = Vec::new();
    while p.i < b.len() {
        out.push(p.varint()? as u32);
    }
    Ok(out)
}

fn zigzag(v: u32) -> i32 {
    ((v >> 1) as i32) ^ -((v & 1) as i32)
}

fn value(b: &[u8]) -> Result<Value, String> {
    let mut p = Pb::new(b);
    let mut v = Value::Null;
    while let Some((n, f)) = p.next()? {
        v = match (n, f) {
            (1, Field::Bytes(s)) => Value::String(String::from_utf8_lossy(s).into_owned()),
            (2, Field::Fixed32(x)) => serde_json::json!(f32::from_bits(x) as f64),
            (3, Field::Fixed64(x)) => serde_json::json!(f64::from_bits(x)),
            (4, Field::Varint(x)) => serde_json::json!(x as i64),
            (5, Field::Varint(x)) => serde_json::json!(x),
            (6, Field::Varint(x)) => serde_json::json!(((x >> 1) as i64) ^ -((x & 1) as i64)),
            (7, Field::Varint(x)) => Value::Bool(x != 0),
            _ => v,
        };
    }
    Ok(v)
}

/// A geometry's command stream as parts (MoveTo starts one).
fn geometry(cmds: &[u32]) -> Vec<Vec<(i32, i32)>> {
    let mut parts: Vec<Vec<(i32, i32)>> = Vec::new();
    let (mut x, mut y) = (0i32, 0i32);
    let mut i = 0;
    while i < cmds.len() {
        let (id, count) = (cmds[i] & 7, (cmds[i] >> 3) as usize);
        i += 1;
        match id {
            1 | 2 => {
                for _ in 0..count {
                    let (Some(dx), Some(dy)) = (cmds.get(i), cmds.get(i + 1)) else { return parts };
                    x = x.wrapping_add(zigzag(*dx));
                    y = y.wrapping_add(zigzag(*dy));
                    i += 2;
                    if id == 1 {
                        parts.push(vec![(x, y)]);
                    } else if let Some(p) = parts.last_mut() {
                        p.push((x, y));
                    }
                }
            }
            7 => {
                if let Some(p) = parts.last_mut() {
                    if let Some(first) = p.first().copied() {
                        p.push(first);
                    }
                }
            }
            _ => return parts,
        }
    }
    parts
}

fn layer(b: &[u8]) -> Result<Layer, String> {
    let mut p = Pb::new(b);
    let (mut name, mut extent) = (String::new(), 4096u32);
    let (mut keys, mut values, mut raw) = (Vec::new(), Vec::new(), Vec::new());
    while let Some((n, f)) = p.next()? {
        match (n, f) {
            (1, Field::Bytes(s)) => name = String::from_utf8_lossy(s).into_owned(),
            (2, Field::Bytes(s)) => raw.push(s),
            (3, Field::Bytes(s)) => keys.push(String::from_utf8_lossy(s).into_owned()),
            (4, Field::Bytes(s)) => values.push(value(s)?),
            (5, Field::Varint(x)) => extent = x as u32,
            _ => {}
        }
    }
    let mut features = Vec::with_capacity(raw.len());
    for f in raw {
        let mut p = Pb::new(f);
        let (mut kind, mut tags, mut geom) = (GeomType::Unknown, Vec::new(), Vec::new());
        while let Some((n, field)) = p.next()? {
            match (n, field) {
                (2, Field::Bytes(s)) => tags = packed(s)?,
                (3, Field::Varint(t)) => {
                    kind = match t {
                        1 => GeomType::Point,
                        2 => GeomType::Line,
                        3 => GeomType::Polygon,
                        _ => GeomType::Unknown,
                    }
                }
                (4, Field::Bytes(s)) => geom = packed(s)?,
                _ => {}
            }
        }
        let mut props = HashMap::new();
        for kv in tags.chunks_exact(2) {
            if let (Some(k), Some(v)) = (keys.get(kv[0] as usize), values.get(kv[1] as usize)) {
                props.insert(k.clone(), v.clone());
            }
        }
        features.push(Feature { kind, props, parts: geometry(&geom) });
    }
    Ok(Layer { name, extent: extent.max(1), features })
}

/// A vector tile, gzipped or not.
pub fn decode(data: &[u8]) -> Result<Tile, String> {
    let raw;
    let data = if data.starts_with(&[0x1f, 0x8b]) {
        use std::io::Read;
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(data).read_to_end(&mut out).map_err(|e| format!("gzip: {e}"))?;
        raw = out;
        &raw[..]
    } else {
        data
    };
    let mut p = Pb::new(data);
    let mut tile = Tile::default();
    while let Some((n, f)) = p.next()? {
        if let (3, Field::Bytes(s)) = (n, f) {
            tile.layers.push(layer(s)?);
        }
    }
    Ok(tile)
}

// ------------------------------------------------------------------ style

/// What an expression is evaluated against: a feature at a zoom.
struct Ctx<'a> {
    props: &'a HashMap<String, Value>,
    zoom: f64,
}

fn num(v: &Value) -> Option<f64> {
    v.as_f64()
}

fn eq(a: &Value, b: &Value) -> bool {
    match (num(a), num(b)) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

fn eval(e: &Value, c: &Ctx) -> Value {
    let Some(arr) = e.as_array() else { return e.clone() };
    let Some(op) = arr.first().and_then(Value::as_str) else { return e.clone() };
    let arg = |i: usize| arr.get(i).map(|a| eval(a, c)).unwrap_or(Value::Null);
    match op {
        "get" => arr.get(1).and_then(Value::as_str).and_then(|k| c.props.get(k).cloned()).unwrap_or(Value::Null),
        "has" => Value::Bool(arr.get(1).and_then(Value::as_str).is_some_and(|k| c.props.contains_key(k))),
        "literal" => arr.get(1).cloned().unwrap_or(Value::Null),
        "zoom" => serde_json::json!(c.zoom),
        "coalesce" => arr[1..].iter().map(|a| eval(a, c)).find(|v| !v.is_null()).unwrap_or(Value::Null),
        "==" => Value::Bool(eq(&arg(1), &arg(2))),
        "!=" => Value::Bool(!eq(&arg(1), &arg(2))),
        "<" | "<=" | ">" | ">=" => {
            let (Some(a), Some(b)) = (num(&arg(1)), num(&arg(2))) else { return Value::Bool(false) };
            Value::Bool(match op {
                "<" => a < b,
                "<=" => a <= b,
                ">" => a > b,
                _ => a >= b,
            })
        }
        "in" => {
            let (needle, hay) = (arg(1), arg(2));
            Value::Bool(match &hay {
                Value::Array(a) => a.iter().any(|x| eq(x, &needle)),
                Value::String(s) => needle.as_str().is_some_and(|n| s.contains(n)),
                _ => false,
            })
        }
        "!" => Value::Bool(!truthy(&arg(1))),
        "all" => Value::Bool(arr[1..].iter().all(|a| truthy(&eval(a, c)))),
        "any" => Value::Bool(arr[1..].iter().any(|a| truthy(&eval(a, c)))),
        "interpolate" => {
            // ["interpolate", ["linear"], input, stop, value, …]
            let Some(x) = num(&arg(2)) else { return Value::Null };
            let stops: Vec<(f64, f64)> = arr[3..].chunks_exact(2).filter_map(|s| Some((num(&s[0])?, num(&eval(&s[1], c))?))).collect();
            serde_json::json!(interpolate(&stops, x))
        }
        _ => e.clone(),
    }
}

fn interpolate(stops: &[(f64, f64)], x: f64) -> f64 {
    let Some(first) = stops.first() else { return 0.0 };
    if x <= first.0 {
        return first.1;
    }
    for w in stops.windows(2) {
        let ((x0, y0), (x1, y1)) = (w[0], w[1]);
        if x <= x1 {
            return y0 + (y1 - y0) * (x - x0) / (x1 - x0).max(f64::EPSILON);
        }
    }
    stops.last().unwrap().1
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Null => false,
        _ => true,
    }
}

/// A color as 0xAARRGGBB: `#rgb`, `#rrggbb`, `#rrggbbaa`, `rgb()`, `rgba()`.
pub fn color(s: &str) -> Option<u32> {
    let s = s.trim();
    if let Some(h) = s.strip_prefix('#') {
        let full: String = if h.len() == 3 { h.chars().flat_map(|c| [c, c]).collect() } else { h.to_string() };
        let v = u32::from_str_radix(&full, 16).ok()?;
        return match full.len() {
            6 => Some(0xff00_0000 | v),
            8 => Some((v & 0xff) << 24 | v >> 8),
            _ => None,
        };
    }
    let inner = s.strip_prefix("rgba(").or_else(|| s.strip_prefix("rgb("))?.strip_suffix(')')?;
    let p: Vec<f64> = inner.split(',').filter_map(|x| x.trim().parse().ok()).collect();
    let a = if p.len() == 4 { (p[3] * 255.0).round() as u32 } else { 255 };
    (p.len() >= 3).then(|| a << 24 | (p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32)
}

#[derive(Debug, Clone)]
struct StyleLayer {
    kind: String,
    source_layer: String,
    filter: Option<Value>,
    minzoom: f64,
    maxzoom: f64,
    paint: serde_json::Map<String, Value>,
    layout: serde_json::Map<String, Value>,
}

/// A MapLibre style, as far as the app draws it.
#[derive(Debug, Clone)]
pub struct Style {
    layers: Vec<StyleLayer>,
    /// The tiles' path in the library, `{z}/{x}/{y}` to be filled in.
    pub tiles: String,
    pub background: u32,
}

impl Style {
    pub fn parse(json: &str) -> Result<Style, String> {
        let v: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
        let tiles = v["sources"]
            .as_object()
            .and_then(|s| s.values().find_map(|src| src["tiles"].as_array()?.first()?.as_str().map(String::from)))
            .ok_or("the style names no tiles")?;
        let mut background = 0xfff2_efe9;
        let mut layers = Vec::new();
        for l in v["layers"].as_array().ok_or("the style has no layers")? {
            let kind = l["type"].as_str().unwrap_or("").to_string();
            let paint = l["paint"].as_object().cloned().unwrap_or_default();
            if kind == "background" {
                if let Some(c) = paint.get("background-color").and_then(Value::as_str).and_then(color) {
                    background = c;
                }
                continue;
            }
            layers.push(StyleLayer {
                kind,
                source_layer: l["source-layer"].as_str().unwrap_or("").to_string(),
                filter: l.get("filter").filter(|f| !f.is_null()).cloned(),
                minzoom: l["minzoom"].as_f64().unwrap_or(0.0),
                maxzoom: l["maxzoom"].as_f64().unwrap_or(24.0),
                paint,
                layout: l["layout"].as_object().cloned().unwrap_or_default(),
            });
        }
        Ok(Style { layers, tiles, background })
    }

    /// A tile's path in the library (relative to it, as the style has it).
    pub fn tile_path(&self, z: u32, x: u32, y: u32) -> String {
        self.tiles.replace("{z}", &z.to_string()).replace("{x}", &x.to_string()).replace("{y}", &y.to_string())
    }
}

// ------------------------------------------------------------------ drawing

#[derive(Debug, Clone, PartialEq)]
pub enum Draw {
    Fill { color: u32, outline: u32, parts: Vec<Vec<(f32, f32)>> },
    Line { color: u32, width: f32, dash: Vec<f32>, parts: Vec<Vec<(f32, f32)>> },
    Label { color: u32, halo: u32, halo_width: f32, size: f32, x: f32, y: f32, angle: f32, text: String },
}

fn paint_num(l: &StyleLayer, key: &str, c: &Ctx, default: f64) -> f64 {
    l.paint.get(key).or_else(|| l.layout.get(key)).map(|v| eval(v, c)).and_then(|v| num(&v)).unwrap_or(default)
}

fn paint_color(l: &StyleLayer, key: &str, c: &Ctx) -> Option<u32> {
    l.paint.get(key).map(|v| eval(v, c)).and_then(|v| v.as_str().and_then(color))
}

/// What to draw of `tile` at display zoom `zoom`, in tile units: `offset`
/// and `scale` place a part of an ancestor tile (a zoom past the map's
/// tiles: its tile at the deepest zoom, enlarged).
pub fn draw_list(style: &Style, tile: &Tile, zoom: f64, scale: f32, offset: (f32, f32)) -> Vec<Draw> {
    let mut out = Vec::new();
    for l in &style.layers {
        if zoom < l.minzoom || zoom >= l.maxzoom {
            continue;
        }
        let Some(src) = tile.layers.iter().find(|t| t.name == l.source_layer) else { continue };
        let ext = src.extent as f32;
        let at = |p: &(i32, i32)| (p.0 as f32 / ext * scale - offset.0, p.1 as f32 / ext * scale - offset.1);
        for f in &src.features {
            let c = Ctx { props: &f.props, zoom };
            if l.filter.as_ref().is_some_and(|filter| !truthy(&eval(filter, &c))) {
                continue;
            }
            match (l.kind.as_str(), f.kind) {
                ("fill", GeomType::Polygon) => {
                    let Some(color) = paint_color(l, "fill-color", &c) else { continue };
                    let outline = paint_color(l, "fill-outline-color", &c).unwrap_or(0);
                    out.push(Draw::Fill { color, outline, parts: f.parts.iter().map(|p| p.iter().map(at).collect()).collect() });
                }
                ("line", GeomType::Line | GeomType::Polygon) => {
                    let Some(color) = paint_color(l, "line-color", &c) else { continue };
                    let width = paint_num(l, "line-width", &c, 1.0) as f32;
                    let dash = l.paint.get("line-dasharray").and_then(Value::as_array).map(|a| a.iter().filter_map(|d| d.as_f64().map(|d| d as f32 * width)).collect()).unwrap_or_default();
                    out.push(Draw::Line { color, width, dash, parts: f.parts.iter().map(|p| p.iter().map(at).collect()).collect() });
                }
                ("symbol", kind) => {
                    let text = l.layout.get("text-field").map(|v| eval(v, &c)).unwrap_or(Value::Null);
                    let text = match text {
                        Value::String(s) => s,
                        Value::Number(n) => n.to_string(),
                        _ => continue,
                    };
                    if text.trim().is_empty() {
                        continue;
                    }
                    let placed = match kind {
                        GeomType::Point => f.parts.first().and_then(|p| p.first()).map(|p| (at(p), 0.0)),
                        GeomType::Line => line_anchor(&f.parts).map(|(p, a)| (at(&p), a)),
                        _ => None,
                    };
                    let Some(((x, y), angle)) = placed else { continue };
                    // Outside the tile: another tile places it.
                    if !(0.0..1.0).contains(&x) || !(0.0..1.0).contains(&y) {
                        continue;
                    }
                    out.push(Draw::Label {
                        color: paint_color(l, "text-color", &c).unwrap_or(0xff33_3333),
                        halo: paint_color(l, "text-halo-color", &c).unwrap_or(0),
                        halo_width: paint_num(l, "text-halo-width", &c, 0.0) as f32,
                        size: paint_num(l, "text-size", &c, 12.0) as f32,
                        x,
                        y,
                        angle,
                        text,
                    });
                }
                _ => {}
            }
        }
    }
    out
}

/// Where a line's label goes: the middle of its longest segment, along it
/// (in degrees, kept readable: never upside down).
fn line_anchor(parts: &[Vec<(i32, i32)>]) -> Option<((i32, i32), f32)> {
    let mut best: Option<(f64, (i32, i32), f32)> = None;
    for p in parts {
        for w in p.windows(2) {
            let (dx, dy) = ((w[1].0 - w[0].0) as f64, (w[1].1 - w[0].1) as f64);
            let len = dx.hypot(dy);
            if best.as_ref().is_none_or(|b| len > b.0) {
                let mut angle = dy.atan2(dx).to_degrees() as f32;
                if angle > 90.0 {
                    angle -= 180.0;
                } else if angle < -90.0 {
                    angle += 180.0;
                }
                best = Some((len, ((w[0].0 + w[1].0) / 2, (w[0].1 + w[1].1) / 2), angle));
            }
        }
    }
    best.map(|(_, p, a)| (p, a))
}

/// A draw list as bytes for the C side (little-endian): per item a kind
/// byte, then — fill (0): color, outline, parts; line (1): color, width,
/// dash count and dashes, parts; label (2): color, halo, halo width, size,
/// x, y, angle, text length (u16) and UTF-8. Parts: count, then per part a
/// point count and x, y pairs (f32). Colors are 0xAARRGGBB (u32).
pub fn encode(list: &[Draw]) -> Vec<u8> {
    let mut b = Vec::new();
    let parts = |b: &mut Vec<u8>, parts: &[Vec<(f32, f32)>]| {
        b.extend((parts.len() as u32).to_le_bytes());
        for p in parts {
            b.extend((p.len() as u32).to_le_bytes());
            for (x, y) in p {
                b.extend(x.to_le_bytes());
                b.extend(y.to_le_bytes());
            }
        }
    };
    for d in list {
        match d {
            Draw::Fill { color, outline, parts: ps } => {
                b.push(0);
                b.extend(color.to_le_bytes());
                b.extend(outline.to_le_bytes());
                parts(&mut b, ps);
            }
            Draw::Line { color, width, dash, parts: ps } => {
                b.push(1);
                b.extend(color.to_le_bytes());
                b.extend(width.to_le_bytes());
                b.push(dash.len().min(255) as u8);
                for d in dash.iter().take(255) {
                    b.extend(d.to_le_bytes());
                }
                parts(&mut b, ps);
            }
            Draw::Label { color, halo, halo_width, size, x, y, angle, text } => {
                b.push(2);
                b.extend(color.to_le_bytes());
                b.extend(halo.to_le_bytes());
                for f in [halo_width, size, x, y, angle] {
                    b.extend(f.to_le_bytes());
                }
                let t = &text.as_bytes()[..text.len().min(u16::MAX as usize)];
                b.extend((t.len() as u16).to_le_bytes());
                b.extend(t);
            }
        }
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    fn varint(mut v: u64, out: &mut Vec<u8>) {
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b);
                return;
            }
            out.push(b | 0x80);
        }
    }

    fn field(num: u32, bytes: &[u8], out: &mut Vec<u8>) {
        varint(u64::from(num) << 3 | 2, out);
        varint(bytes.len() as u64, out);
        out.extend(bytes);
    }

    fn packed(v: &[u32]) -> Vec<u8> {
        let mut o = Vec::new();
        for x in v {
            varint(u64::from(*x), &mut o);
        }
        o
    }

    fn zz(v: i32) -> u32 {
        ((v << 1) ^ (v >> 31)) as u32
    }

    /// A tile with a water square and a road named "Rüütli".
    fn sample() -> Vec<u8> {
        let mut water = Vec::new();
        field(1, b"water", &mut water);
        let mut f = Vec::new();
        varint(3 << 3, &mut f);
        varint(3, &mut f); // polygon
        let g = [9, zz(0), zz(0), 26, zz(4096), zz(0), zz(0), zz(4096), zz(-4096), zz(0), 15];
        field(4, &packed(&g), &mut f);
        field(2, &f, &mut water);
        varint(5 << 3, &mut water);
        varint(4096, &mut water);

        let mut roads = Vec::new();
        field(1, b"transportation_name", &mut roads);
        let mut f = Vec::new();
        field(2, &packed(&[0, 0]), &mut f);
        varint(3 << 3, &mut f);
        varint(2, &mut f); // line
        let g = [9, zz(1000), zz(2000), 10, zz(2000), zz(0)];
        field(4, &packed(&g), &mut f);
        field(2, &f, &mut roads);
        field(3, b"name", &mut roads);
        let mut v = Vec::new();
        field(1, "Rüütli".as_bytes(), &mut v);
        field(4, &v, &mut roads);

        let mut tile = Vec::new();
        field(3, &water, &mut tile);
        field(3, &roads, &mut tile);
        tile
    }

    const STYLE: &str = r##"{"version":8,"sources":{"omt":{"type":"vector","tiles":["/tiles/{z}/{x}/{y}.pbf"]}},
      "layers":[{"id":"bg","type":"background","paint":{"background-color":"#f2efe9"}},
        {"id":"water","type":"fill","source":"omt","source-layer":"water","paint":{"fill-color":"#aad3df"}},
        {"id":"name","type":"symbol","source":"omt","source-layer":"transportation_name","minzoom":13,
         "layout":{"text-field":["coalesce",["get","name:latin"],["get","name"]],"text-size":11},
         "paint":{"text-color":"#555555","text-halo-color":"#ffffff","text-halo-width":1.5}}]}"##;

    #[test]
    fn tiles_decode_and_draw_by_the_style() {
        let tile = decode(&sample()).unwrap();
        assert_eq!(tile.layers.len(), 2);
        assert_eq!(tile.layers[0].features[0].parts[0], [(0, 0), (4096, 0), (4096, 4096), (0, 4096), (0, 0)]);
        let style = Style::parse(STYLE).unwrap();
        assert_eq!(style.background, 0xfff2_efe9);
        assert_eq!(style.tile_path(14, 9318, 4740), "/tiles/14/9318/4740.pbf");
        let low = draw_list(&style, &tile, 12.0, 1.0, (0.0, 0.0));
        assert_eq!(low.len(), 1, "no road names below zoom 13");
        let list = draw_list(&style, &tile, 14.0, 1.0, (0.0, 0.0));
        assert!(matches!(&list[0], Draw::Fill { color: 0xffaa_d3df, .. }));
        let Draw::Label { text, x, y, angle, size, .. } = &list[1] else { panic!("{list:?}") };
        assert_eq!((text.as_str(), *size), ("Rüütli", 11.0));
        assert!((x - 0.4883).abs() < 0.001 && (y - 0.4883).abs() < 0.001 && angle.abs() < 0.01, "{x} {y} {angle}");
        assert!(!encode(&list).is_empty());
    }

    #[test]
    fn expressions_as_the_style_uses_them() {
        let props: HashMap<String, Value> = [("class".to_string(), serde_json::json!("grass")), ("admin_level".to_string(), serde_json::json!(2))].into();
        let c = Ctx { props: &props, zoom: 10.0 };
        let e = |s: &str| eval(&serde_json::from_str(s).unwrap(), &c);
        assert_eq!(e(r#"["in", ["get", "class"], ["literal", ["grass", "farmland"]]]"#), Value::Bool(true));
        assert_eq!(e(r#"["==", ["get", "admin_level"], 2]"#), Value::Bool(true));
        assert_eq!(e(r#"["==", ["get", "class"], "wood"]"#), Value::Bool(false));
        assert_eq!(num(&e(r#"["interpolate", ["linear"], ["zoom"], 7, 0.8, 14, 6]"#)).unwrap(), 0.8 + (6.0 - 0.8) * 3.0 / 7.0);
        assert_eq!(color("#aad3df"), Some(0xffaa_d3df));
        assert_eq!(color("rgba(255, 0, 0, 0.5)"), Some(0x80ff_0000));
    }
}
