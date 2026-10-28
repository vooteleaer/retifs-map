//! Builds a map library (ADR-0028): a directory of tiles (`<z>/<x>/<y>.pbf`,
//! as tilemaker writes them) into retifs records, one per tile, plus the
//! tileset record that describes them. Publish the library afterwards as a
//! channel (`retifs channel publish-ns`) so others can follow it.
//!
//!     retifs-map-import tiles DIR --ns NS --name NAME [--style FILE]
//!         [--source TEXT] [--source-date DATE] [--overlay] [--replace]
//!     retifs-map-import places FILE.jsonl --ns NS [--replace]
//!
//! `places` takes one record per line as a JSON object (osm-places.py
//! writes them): strings, numbers, booleans and lists become attributes.

use std::path::{Path, PathBuf};
use std::time::Instant;

use libretifs::{AttrValue, Client, Request, Response, TxStep};

pub const TILESET_TYPE: &str = "application/x-vnd.retifs.tileset";
pub const VECTOR_TILE: &str = "application/vnd.mapbox-vector-tile";

/// Records per transaction, and the most tile data in one.
const BATCH: usize = 500;
const BATCH_BYTES: usize = 16 << 20;

fn usage() -> ! {
    eprintln!("retifs-map-import tiles DIR --ns NS --name NAME [--style FILE] [--source TEXT] [--source-date DATE] [--overlay] [--replace]");
    eprintln!("retifs-map-import places FILE.jsonl --ns NS [--replace]");
    std::process::exit(2)
}

fn fail(e: impl std::fmt::Display) -> ! {
    eprintln!("retifs-map-import: {e}");
    std::process::exit(1)
}

struct Opts {
    places: bool,
    dir: PathBuf,
    ns: String,
    name: String,
    style: Option<PathBuf>,
    source: String,
    source_date: String,
    overlay: bool,
    replace: bool,
}

fn opts() -> Opts {
    let mut args = std::env::args().skip(1);
    let places = match args.next().as_deref() {
        Some("tiles") => false,
        Some("places") => true,
        _ => usage(),
    };
    let dir = PathBuf::from(args.next().unwrap_or_else(|| usage()));
    let mut o = Opts { places, dir, ns: String::new(), name: String::new(), style: None, source: "OpenStreetMap".into(), source_date: String::new(), overlay: false, replace: false };
    while let Some(a) = args.next() {
        let mut val = || args.next().unwrap_or_else(|| usage());
        match a.as_str() {
            "--ns" => o.ns = val(),
            "--name" => o.name = val(),
            "--style" => o.style = Some(PathBuf::from(val())),
            "--source" => o.source = val(),
            "--source-date" => o.source_date = val(),
            "--overlay" => o.overlay = true,
            "--replace" => o.replace = true,
            _ => usage(),
        }
    }
    if o.ns.is_empty() || (o.name.is_empty() && !o.places) {
        usage();
    }
    o
}

/// Every tile under `dir`: (z, x, y, extension, path).
fn tiles(dir: &Path) -> Vec<(i64, i64, i64, String, PathBuf)> {
    let num = |p: &Path| p.file_stem().and_then(|s| s.to_str()).and_then(|s| s.parse::<i64>().ok());
    let mut out = Vec::new();
    let Ok(zs) = std::fs::read_dir(dir) else { return out };
    for z in zs.flatten().map(|e| e.path()).filter(|p| p.is_dir()) {
        let Some(zn) = num(&z) else { continue };
        for x in std::fs::read_dir(&z).into_iter().flatten().flatten().map(|e| e.path()) {
            let Some(xn) = num(&x) else { continue };
            for y in std::fs::read_dir(&x).into_iter().flatten().flatten().map(|e| e.path()) {
                let (Some(yn), Some(ext)) = (num(&y), y.extension().and_then(|e| e.to_str()).map(str::to_string)) else { continue };
                out.push((zn, xn, yn, ext, y));
            }
        }
    }
    out.sort();
    out
}

fn s(v: impl Into<String>) -> AttrValue {
    AttrValue::Str(v.into())
}

async fn call(c: &mut Client, req: Request) -> Response {
    match c.call(req).await.unwrap_or_else(|e| fail(e)) {
        Response::Error { message } => fail(message),
        r => r,
    }
}

async fn query(c: &mut Client, ns: &str, term: &str) -> Vec<String> {
    match call(c, Request::Query { ns: ns.into(), terms: vec![term.into()], at: None }).await {
        Response::Records { records } => records.into_iter().map(|r| r.id).collect(),
        _ => Vec::new(),
    }
}

fn attr(v: &serde_json::Value) -> Option<AttrValue> {
    Some(match v {
        serde_json::Value::String(t) => AttrValue::Str(t.clone()),
        serde_json::Value::Bool(b) => AttrValue::Bool(*b),
        serde_json::Value::Number(n) => match n.as_i64() {
            Some(i) => AttrValue::Int(i),
            None => AttrValue::Float(n.as_f64()?),
        },
        serde_json::Value::Array(a) => AttrValue::List(a.iter().map(attr).collect::<Option<Vec<_>>>()?),
        _ => return None,
    })
}

/// Places, one JSON object per line, into `ns` (after deleting the places
/// there with --replace).
async fn places(c: &mut Client, o: &Opts) {
    let text = std::fs::read_to_string(&o.dir).unwrap_or_else(|e| fail(format!("{}: {e}", o.dir.display())));
    let old = query(c, &o.ns, "type=application/x-vnd.retifs.place").await;
    if !old.is_empty() && !o.replace {
        fail(format!("{} already has {} place(s); --replace to rebuild it", o.ns, old.len()));
    }
    let started = Instant::now();
    for ids in old.chunks(BATCH) {
        call(c, Request::Transaction { ns: o.ns.clone(), steps: ids.iter().map(|id| TxStep::Delete { id: id.clone() }).collect() }).await;
    }
    if !old.is_empty() {
        eprintln!("{} old place(s) deleted in {:.0} s", old.len(), started.elapsed().as_secs_f64());
    }
    let (mut steps, mut done) = (Vec::new(), 0usize);
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let v: serde_json::Value = serde_json::from_str(line).unwrap_or_else(|e| fail(format!("{e}: {line}")));
        let attrs: Vec<(String, AttrValue)> = v.as_object().into_iter().flatten().filter_map(|(k, v)| Some((k.clone(), attr(v)?))).collect();
        steps.push(TxStep::Create { attrs, data: None });
        if steps.len() == BATCH {
            done += steps.len();
            call(c, Request::Transaction { ns: o.ns.clone(), steps: std::mem::take(&mut steps) }).await;
        }
    }
    if !steps.is_empty() {
        done += steps.len();
        call(c, Request::Transaction { ns: o.ns.clone(), steps }).await;
    }
    println!("{done} places in {} ({:.0} s)", o.ns, started.elapsed().as_secs_f64());
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let o = opts();
    let mut c = Client::connect(&libretifs::default_store_dir()).await.unwrap_or_else(|e| fail(e));
    if o.places {
        places(&mut c, &o).await;
        return;
    }
    let found = tiles(&o.dir);
    if found.is_empty() {
        fail(format!("no tiles under {}", o.dir.display()));
    }

    // What was there before: kept unless --replace.
    let mut old = query(&mut c, &o.ns, "has:tile:z").await;
    old.extend(query(&mut c, &o.ns, &format!("type={TILESET_TYPE}")).await);
    if !old.is_empty() && !o.replace {
        fail(format!("{} already has {} map record(s); --replace to rebuild it", o.ns, old.len()));
    }
    for ids in old.chunks(BATCH) {
        call(&mut c, Request::Transaction { ns: o.ns.clone(), steps: ids.iter().map(|id| TxStep::Delete { id: id.clone() }).collect() }).await;
    }

    // tilemaker's metadata.json: bounds and zooms.
    let meta: serde_json::Value = std::fs::read(o.dir.join("metadata.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let field = |k: &str| meta.get(k).map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()));
    let minzoom = found.iter().map(|t| t.0).min().unwrap_or(0);
    let maxzoom = found.iter().map(|t| t.0).max().unwrap_or(0);
    let ext = found[0].3.clone();

    let started = Instant::now();
    let (mut steps, mut bytes, mut done, mut total_bytes) = (Vec::new(), 0usize, 0usize, 0usize);
    for (z, x, y, e, path) in &found {
        let data = std::fs::read(path).unwrap_or_else(|err| fail(format!("{}: {err}", path.display())));
        let ty = if e == "pbf" || e == "mvt" { VECTOR_TILE.to_string() } else { format!("image/{e}") };
        bytes += data.len();
        total_bytes += data.len();
        steps.push(TxStep::Create {
            attrs: vec![
                ("type".into(), s(ty)),
                ("path".into(), s(format!("/tiles/{z}/{x}/{y}.{e}"))),
                ("tile:z".into(), AttrValue::Int(*z)),
                ("tile:x".into(), AttrValue::Int(*x)),
                ("tile:y".into(), AttrValue::Int(*y)),
            ],
            data: Some(data),
        });
        if steps.len() == BATCH || bytes >= BATCH_BYTES {
            done += steps.len();
            call(&mut c, Request::Transaction { ns: o.ns.clone(), steps: std::mem::take(&mut steps) }).await;
            bytes = 0;
            let secs = started.elapsed().as_secs_f64();
            eprint!("\r{done}/{} tiles, {} MB, {:.0} tiles/s   ", found.len(), total_bytes >> 20, done as f64 / secs);
        }
    }
    if !steps.is_empty() {
        done += steps.len();
        call(&mut c, Request::Transaction { ns: o.ns.clone(), steps }).await;
    }
    eprintln!("\r{done} tiles, {} MB, in {:.0} s            ", total_bytes >> 20, started.elapsed().as_secs_f64());

    // The tileset: what this map is, and its style as the content.
    let mut attrs = vec![
        ("type".to_string(), s(TILESET_TYPE)),
        ("name".to_string(), s(o.name.clone())),
        ("format".to_string(), s(ext.clone())),
        ("tiles".to_string(), s(format!("/tiles/{{z}}/{{x}}/{{y}}.{ext}"))),
        ("minzoom".to_string(), AttrValue::Int(minzoom)),
        ("maxzoom".to_string(), AttrValue::Int(maxzoom)),
        ("attribution".to_string(), s("© OpenStreetMap contributors")),
        ("license".to_string(), s("ODbL-1.0")),
        ("source".to_string(), s(o.source.clone())),
        ("overlay".to_string(), AttrValue::Bool(o.overlay)),
    ];
    if let Some(b) = field("bounds") {
        let nums: Vec<AttrValue> = b.trim_matches(|c| c == '[' || c == ']').split(',').filter_map(|n| n.trim().parse::<f64>().ok()).map(AttrValue::Float).collect();
        if nums.len() == 4 {
            attrs.push(("bounds".into(), AttrValue::List(nums)));
        }
    }
    if !o.source_date.is_empty() {
        attrs.push(("source_date".into(), s(o.source_date.clone())));
    }
    let style = o.style.as_ref().map(|p| std::fs::read(p).unwrap_or_else(|e| fail(format!("{}: {e}", p.display()))));
    if style.is_some() {
        attrs.push(("path".into(), s("/style.json")));
    }
    call(&mut c, Request::RecordCreate { ns: o.ns.clone(), attrs, data: style }).await;
    println!("{} tiles of {} (zoom {minzoom}–{maxzoom}) in {}", done, o.name, o.ns);
}
