//! Places on retifs: every record that says where it is (`geo`, or a
//! labelled `geo:<label>` such as a contact's home, ADR-0027), whatever it
//! is: photos, contacts, events, and anything else. The map owns none of
//! them; it shows them, and lets the user place a contact or an event whose
//! address or location has no coordinates yet.

pub mod icons;
pub mod vector;

use std::collections::HashMap;
use std::path::PathBuf;

use libretifs::{types, AttrValue, Client, RecordView, Request, Response};
use unicode_normalization::{char::is_combining_mark, UnicodeNormalization};

pub const ME: &str = "me";
/// Words of an address search sent as terms (the rest are only typed).
const MAX_FIND_WORDS: usize = 8;
/// Answers an address search gives.
const FOUND_MAX: usize = 10;
/// A place saved for its own sake (ADR-0027).
pub const PLACE_TYPE: &str = "application/x-vnd.retifs.place";

/// What a placed record is, for its marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Photo,
    Person,
    Event,
    Other,
}

impl Kind {
    fn of(t: &str) -> Kind {
        if t.starts_with("image/") || t == "image" || t == "photo" {
            Kind::Photo
        } else if types::is(t, types::PERSON) {
            Kind::Person
        } else if types::is(t, types::EVENT) {
            Kind::Event
        } else {
            Kind::Other
        }
    }
}

/// A record at a place (one per labelled place: a contact's home and work are two).
#[derive(Debug, Clone, PartialEq)]
pub struct Place {
    pub ns: String,
    pub id: String,
    pub kind: Kind,
    pub title: String,
    /// The label of a labelled place (`home`), "" for plain `geo`.
    pub label: String,
    pub lat: f64,
    pub lon: f64,
    /// When: a photo's `taken`, an event's `start`.
    pub when: String,
    /// What it is, in a word or two: a place's category ("cafe"), else "".
    pub detail: String,
    /// A map's place (a public library's café): from which zoom it shows
    /// (ADR-0028). None for the user's own things, which are always shown.
    pub minzoom: Option<i64>,
    /// The first content block's read-cap (a photo), and its thumbnail's.
    pub thumb: Option<String>,
    /// A photo's larger preview block (1600 px, the extractor's), if it has one.
    pub preview: Option<String>,
}

/// A record with an address or a location but no coordinates for it.
#[derive(Debug, Clone, PartialEq)]
pub struct Unplaced {
    pub ns: String,
    pub id: String,
    pub kind: Kind,
    pub title: String,
    /// The key to write (`geo`, `geo:home`) and the text it's for.
    pub key: String,
    pub address: String,
}

fn text(r: &RecordView, k: &str) -> Option<String> {
    r.attrs.iter().find(|(key, _)| key == k).and_then(|(_, v)| if let AttrValue::Str(s) = v { Some(s.clone()) } else { None })
}

fn latlon(v: &AttrValue) -> Option<(f64, f64)> {
    match v {
        AttrValue::List(l) => match l.as_slice() {
            [AttrValue::Float(a), AttrValue::Float(b)] => Some((*a, *b)),
            [AttrValue::Int(a), AttrValue::Int(b)] => Some((*a as f64, *b as f64)),
            _ => None,
        },
        // "59.43, 24.75" written by hand.
        AttrValue::Str(s) => {
            let (a, b) = s.split_once(',')?;
            Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
        }
        _ => None,
    }
}

fn title_of(r: &RecordView) -> String {
    let full = [text(r, "given"), text(r, "family")].into_iter().flatten().collect::<Vec<_>>().join(" ");
    [text(r, "title"), text(r, "name").filter(|_| text(r, "path").is_none()), Some(full), text(r, "path").map(|p| p.rsplit('/').next().unwrap_or(&p).to_string()), text(r, "name")]
        .into_iter()
        .flatten()
        .find(|s| !s.trim().is_empty())
        .unwrap_or_else(|| r.id[..8].to_string())
}

/// The places of one record, and what of it has no place yet.
fn places_of(ns: &str, r: &RecordView) -> (Vec<Place>, Vec<Unplaced>) {
    // An address library's streets and towns are searched, not drawn.
    if text(r, "type").as_deref() == Some(ADDRESS_TYPE) {
        return (Vec::new(), Vec::new());
    }
    let kind = Kind::of(&text(r, "type").unwrap_or_default());
    let title = title_of(r);
    let when = text(r, "taken").or_else(|| text(r, "start")).unwrap_or_default();
    let thumb_index = r.attrs.iter().find_map(|(k, v)| match (k.as_str(), v) {
        ("block:thumb", AttrValue::Int(i)) if *i > 0 => Some(*i as usize),
        _ => None,
    });
    let thumb = (kind == Kind::Photo).then(|| thumb_index.and_then(|i| r.blocks.get(i)).or(r.blocks.first()).cloned()).flatten();
    let preview = (kind == Kind::Photo)
        .then(|| {
            r.attrs.iter().find_map(|(k, v)| match (k.as_str(), v) {
                ("block:preview", AttrValue::Int(i)) if *i > 0 => r.blocks.get(*i as usize).cloned(),
                _ => None,
            })
        })
        .flatten();
    let detail = text(r, "category").map(|c| c.replace('_', " ")).unwrap_or_default();
    let minzoom = r.attrs.iter().find(|(k, _)| k == "minzoom").and_then(|(_, v)| if let AttrValue::Int(i) = v { Some(*i) } else { None });
    let mut placed = Vec::new();
    for (k, v) in &r.attrs {
        let label = if k == "geo" { "" } else if let Some(l) = k.strip_prefix("geo:") { l } else { continue };
        if let Some((lat, lon)) = latlon(v) {
            placed.push(Place { ns: ns.into(), id: r.id.clone(), kind, title: title.clone(), label: label.into(), lat, lon, when: when.clone(), detail: detail.clone(), minzoom, thumb: thumb.clone(), preview: preview.clone() });
        }
    }
    // Addresses (contacts) and locations (events) without coordinates.
    let mut unplaced = Vec::new();
    for (k, v) in &r.attrs {
        let AttrValue::Str(addr) = v else { continue };
        let key = match (kind, k.as_str()) {
            (Kind::Person, "address") => "geo".to_string(),
            (Kind::Person, a) if a.starts_with("address:") => format!("geo:{}", &a[8..]),
            (Kind::Event, "location") => "geo".to_string(),
            _ => continue,
        };
        if !r.attrs.iter().any(|(k2, _)| *k2 == key) {
            unplaced.push(Unplaced { ns: ns.into(), id: r.id.clone(), kind, title: title.clone(), key, address: addr.clone() });
        }
    }
    (placed, unplaced)
}

/// A map in a library (ADR-0028): its tiles are records of that library.
pub const TILESET_TYPE: &str = "application/x-vnd.retifs.tileset";
/// A map's tiles (ADR-0028).
pub const VECTOR_TILE: &str = "application/vnd.mapbox-vector-tile";

#[derive(Debug, Clone, PartialEq)]
pub struct Tileset {
    pub ns: String,
    pub id: String,
    pub name: String,
    /// west, south, east, north
    pub bounds: Option<[f64; 4]>,
    pub minzoom: i64,
    pub maxzoom: i64,
    pub overlay: bool,
    pub attribution: String,
    /// The style (MapLibre style JSON): the record's content.
    style: Option<String>,
}

fn tileset_of(ns: &str, r: &RecordView) -> Tileset {
    let int = |k: &str| r.attrs.iter().find(|(key, _)| key == k).and_then(|(_, v)| if let AttrValue::Int(i) = v { Some(*i) } else { None });
    let bounds = r.attrs.iter().find(|(k, _)| k == "bounds").and_then(|(_, v)| match v {
        AttrValue::List(l) => {
            let n: Vec<f64> = l.iter().filter_map(|x| if let AttrValue::Float(f) = x { Some(*f) } else { None }).collect();
            (n.len() == 4).then(|| [n[0], n[1], n[2], n[3]])
        }
        _ => None,
    });
    Tileset {
        ns: ns.into(),
        id: r.id.clone(),
        name: text(r, "name").unwrap_or_else(|| ns.to_string()),
        bounds,
        minzoom: int("minzoom").unwrap_or(0),
        maxzoom: int("maxzoom").unwrap_or(14),
        overlay: r.attrs.iter().any(|(k, v)| k == "overlay" && *v == AttrValue::Bool(true)),
        attribution: text(r, "attribution").unwrap_or_default(),
        style: r.blocks.first().cloned(),
    }
}

/// Where the gateway serves libraries (`http://<library>.localhost:<port>`).
pub fn gateway() -> String {
    std::env::var("RETIFS_GATEWAY").unwrap_or_else(|_| "localhost:8740".into())
}

/// A style's relative tile URLs made absolute: the map library on the gateway.
pub fn absolute_style(style: &str, ns: &str) -> Result<String, String> {
    let mut v: serde_json::Value = serde_json::from_str(style).map_err(|e| e.to_string())?;
    let base = format!("http://{ns}.{}", gateway());
    if let Some(sources) = v.get_mut("sources").and_then(|s| s.as_object_mut()) {
        for src in sources.values_mut() {
            if let Some(tiles) = src.get_mut("tiles").and_then(|t| t.as_array_mut()) {
                for t in tiles.iter_mut() {
                    if let Some(u) = t.as_str().filter(|u| u.starts_with('/')) {
                        *t = serde_json::Value::String(format!("{base}{u}"));
                    }
                }
            }
        }
    }
    Ok(v.to_string())
}

/// Whether the map shows anything of a heard library: a map, places, an
/// address library (searched), or records that may carry a place (photos,
/// contacts, events).
pub fn map_uses(l: &libretifs::HeardLibrary) -> bool {
    l.has.iter().any(|(t, _)| t == TILESET_TYPE || t == VECTOR_TILE || t == PLACE_TYPE || t == ADDRESS_TYPE || t.starts_with("image/") || types::is(t, types::PERSON) || types::is(t, types::EVENT))
}

/// An address search's answer: a suggestion the user may pick.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    /// The whole name, most specific first ("Raekoja plats 1, Tallinn").
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    /// How close to show it: a house closer than a street, a street than a town.
    pub zoom: f64,
    /// A place found (a map's café, one's own photo or contact): picked, its
    /// bubble opens. None for an address.
    pub place: Option<Place>,
}

/// An address library's records (osm-addresses.py): a street or a settlement
/// each, listing its houses; searched by the beginnings of their words.
pub const ADDRESS_TYPE: &str = "application/x-vnd.retifs.address";

/// Text as an address search compares it: lower case, no diacritics, words
/// of letters and digits, in any script (as osm-addresses.py folds it).
pub fn fold(text: &str) -> Vec<String> {
    let mut out = String::new();
    for c in text.nfkd() {
        if is_combining_mark(c) {
            continue;
        }
        for l in c.to_lowercase() {
            out.push(if l.is_alphanumeric() { l } else { ' ' });
        }
    }
    out.split_whitespace().map(str::to_string).collect()
}

/// How well the words name something: a word of its own name more than one
/// of where it is (its town, its address), a whole word more than its
/// beginning.
fn fit(words: &[String], own: &[String], around: &[String]) -> i64 {
    words
        .iter()
        .map(|w| {
            if own.contains(w) {
                20
            } else if own.iter().any(|o| o.starts_with(w.as_str())) {
                10
            } else if around.contains(w) {
                6
            } else if around.iter().any(|t| t.starts_with(w.as_str())) {
                3
            } else {
                0
            }
        })
        .sum()
}

/// A place a search found in a library (a map's café): by its name, what it
/// is and where.
fn place_hit(ns: &str, r: &RecordView, words: &[String]) -> Option<(i64, Found)> {
    let p = places_of(ns, r).0.into_iter().next()?;
    let address = text(r, "address").unwrap_or_default();
    let around: Vec<String> = fold(&p.detail).into_iter().chain(fold(&address)).collect();
    let name = [p.title.clone(), p.detail.clone(), address].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(", ");
    // Landmarks (shown from further out, ADR-0028) before the everyday.
    let known = p.minzoom.map_or(0, |z| (18 - z).clamp(0, 8));
    // Its whole name typed ("raekoda"), not just part of a longer one.
    let own = fold(&p.title);
    let whole = if !own.is_empty() && own.iter().all(|o| words.contains(o)) { 15 } else { 0 };
    Some((240 + fit(words, &own, &around) + known + whole, Found { name, lat: p.lat, lon: p.lon, zoom: 17.0, place: Some(p) }))
}

/// One's own things already here (photos, contacts, events, saved places)
/// whose name has every word's beginning: found at once, before the rest.
pub fn find_here(places: &[Place], text: &str) -> Vec<(i64, Found)> {
    let words: Vec<String> = fold(text).into_iter().filter(|w| !w.starts_with(|c: char| c.is_ascii_digit())).collect();
    if words.is_empty() {
        return Vec::new();
    }
    places
        .iter()
        .filter(|p| p.minzoom.is_none())
        .filter_map(|p| {
            let own = fold(&p.title);
            let around = fold(&p.label);
            words.iter().all(|w| own.iter().chain(&around).any(|o| o.starts_with(w.as_str()))).then(|| {
                let name = if p.label.is_empty() { p.title.clone() } else { format!("{} ({})", p.title, p.label) };
                (300 + fit(&words, &own, &around), Found { name, lat: p.lat, lon: p.lon, zoom: 17.0, place: Some(p.clone()) })
            })
        })
        .collect()
}

/// The answers one address record gives to a search: its houses whose
/// number is `number` (or whose name has one of `words`), else itself. With
/// a score (higher first).
fn address_hits(r: &RecordView, words: &[String], number: Option<&str>) -> Vec<(i64, Found)> {
    let Some(name) = text(r, "name") else { return Vec::new() };
    let Some((lat, lon)) = r.attrs.iter().find(|(k, _)| k == "geo").and_then(|(_, v)| latlon(v)) else { return Vec::new() };
    let town = text(r, "in");
    let street = text(r, "kind").as_deref() == Some("street");
    let count = r.attrs.iter().find_map(|(k, v)| if let (true, AttrValue::Int(n)) = (k == "count", v) { Some(*n) } else { None }).unwrap_or(0);
    let full = |head: &str| [Some(head.to_string()), town.clone()].into_iter().flatten().collect::<Vec<_>>().join(", ");
    let (own, town_words) = (fold(&name), town.as_deref().map(fold).unwrap_or_default());
    let name_words: Vec<String> = own.iter().chain(&town_words).cloned().collect();
    let fit = fit(words, &own, &town_words);
    let mut hits = Vec::new();
    for h in text(r, "houses").unwrap_or_default().split(';') {
        let mut it = h.split('|');
        let (Some(label), Some(la), Some(lo)) = (it.next(), it.next(), it.next()) else { continue };
        let (Ok(la), Ok(lo)) = (la.parse::<f64>(), lo.parse::<f64>()) else { continue };
        let folded = fold(label).join("");
        let by_number = number.is_some_and(|n| folded == n);
        // A farm by its name: the index found it by a whole word; others typed
        // may be beginnings. Not the words that name the street or town.
        let by_name = !label.starts_with(|c: char| c.is_ascii_digit())
            && fold(label).iter().any(|lw| words.iter().any(|w| w.chars().count() >= 2 && !name_words.contains(w) && lw.starts_with(w.as_str())));
        if by_number || by_name {
            let head = if street { format!("{name} {label}") } else { format!("{label}, {name}") };
            // A house by its number first; a farm by its name after the
            // streets and towns the words name.
            let base = if by_number { 400 } else { 165 };
            hits.push((base + fit, Found { name: full(&head), lat: la, lon: lo, zoom: 18.0, place: None }));
        }
    }
    if hits.is_empty() {
        // The street or settlement itself: a town before a street of its name
        // when no house number was asked for, bigger ones before smaller.
        let base = match (number.is_some(), street) {
            (true, true) => 200,
            (true, false) => 100,
            (false, false) => 260,
            (false, true) => 250,
        };
        let size = (count.max(1) as f64).log10() as i64;
        hits.push((base + fit + size, Found { name: full(&name), lat, lon, zoom: if street { 16.0 } else { 13.0 }, place: None }));
    }
    hits
}


/// Opens a connection to `retifsd` some other way than its socket (on
/// Android: from the retifs app's access service), for every Map.
#[cfg(unix)]
pub type Connector = Box<dyn Fn() -> Result<std::os::unix::net::UnixStream, String> + Send + Sync>;

#[cfg(unix)]
static CONNECTOR: std::sync::OnceLock<Connector> = std::sync::OnceLock::new();

/// Every Map's connections from `connector` from now on (set once).
#[cfg(unix)]
pub fn set_connector(connector: Connector) {
    let _ = CONNECTOR.set(connector);
}

/// The connection to `retifsd`.
pub struct Map {
    rt: tokio::runtime::Runtime,
    store: PathBuf,
    /// RETIFS_VIA=<node>: every request to that own node over a Link
    /// (ADR-0019): a thin client, nothing kept here.
    via: Option<String>,
}

/// Per library, the keys its index declares searchable: what may be asked
/// of it when held lazily (a library of tiles has no `cell`). Shared by
/// every Map of the process (an app's workers each have one).
fn key_cache() -> &'static std::sync::Mutex<HashMap<String, Vec<String>>> {
    static KEYS: std::sync::OnceLock<std::sync::Mutex<HashMap<String, Vec<String>>>> = std::sync::OnceLock::new();
    KEYS.get_or_init(Default::default)
}

/// Libraries whose index wasn't found (its holders far or away), and when:
/// not asked again for a minute, each question may wait seconds.
fn key_misses() -> &'static std::sync::Mutex<HashMap<String, std::time::Instant>> {
    static MISSES: std::sync::OnceLock<std::sync::Mutex<HashMap<String, std::time::Instant>>> = std::sync::OnceLock::new();
    MISSES.get_or_init(Default::default)
}

const KEY_MISS_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

impl Default for Map {
    fn default() -> Self {
        Self::new()
    }
}

impl Map {
    pub fn new() -> Self {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("a tokio runtime");
        let via = std::env::var("RETIFS_VIA").ok().filter(|v| !v.trim().is_empty());
        Map { rt, store: libretifs::default_store_dir(), via }
    }

    /// Saves a new place in `ns`; its record ID.
    pub fn save_place(&self, ns: &str, name: &str, lat: f64, lon: f64, address: &str) -> Result<String, String> {
        let mut c = self.client()?;
        let mut attrs = vec![
            ("type".to_string(), AttrValue::Str(PLACE_TYPE.into())),
            ("name".to_string(), AttrValue::Str(name.trim().into())),
            ("geo".to_string(), AttrValue::List(vec![AttrValue::Float((lat * 1e6).round() / 1e6), AttrValue::Float((lon * 1e6).round() / 1e6)])),
        ];
        if !address.trim().is_empty() {
            attrs.push(("address".into(), AttrValue::Str(address.trim().into())));
        }
        match self.call(&mut c, Request::RecordCreate { ns: ns.into(), attrs, data: None })? {
            Response::Records { records } => records.first().map(|r| r.id.clone()).ok_or_else(|| "nothing saved".into()),
            other => Err(format!("unexpected answer: {other:?}")),
        }
    }

    /// Searches the address libraries joined here (ADR-0027): retifs only,
    /// no internet service. Words are matched by their beginnings ("raek
    /// tall"); a word starting with a digit is a house number.
    pub fn find(&self, text: &str, here: &[Place]) -> Result<Vec<Found>, String> {
        let libraries = self.libraries()?;
        self.find_in(text, here, &libraries)
    }

    /// As `find`, in these libraries only (those a map shows).
    pub fn find_in(&self, text: &str, here: &[Place], libraries: &[String]) -> Result<Vec<Found>, String> {
        let words = fold(text);
        let (numbers, words): (Vec<String>, Vec<String>) = words.into_iter().partition(|w| w.starts_with(|c: char| c.is_ascii_digit()));
        let number = numbers.concat();
        if words.is_empty() {
            return Ok(Vec::new());
        }
        // A blind index answers exact terms (ADR-0019): each word's
        // beginning is one. Addresses and places alike.
        let terms: Vec<String> = words.iter().take(MAX_FIND_WORDS).map(|w| format!("find=\"{w}\"")).collect();
        let mut c = self.client()?;
        let mut hits: Vec<(i64, Found)> = find_here(here, text);
        for ns in libraries.iter().cloned() {
            if !self.searchable(&mut c, &ns, "find") {
                continue;
            }
            let records = match self.call(&mut c, Request::Query { ns: ns.clone(), terms: terms.clone(), at: None }) {
                Ok(Response::Records { records }) => records,
                _ => continue,
            };
            for r in &records {
                if crate::text(r, "type").as_deref() == Some(ADDRESS_TYPE) {
                    hits.extend(address_hits(r, &words, (!number.is_empty()).then_some(number.as_str())));
                } else {
                    hits.extend(place_hit(&ns, r, &words));
                }
            }
        }
        hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.name.cmp(&b.1.name)));
        let mut seen = std::collections::HashSet::new();
        Ok(hits.into_iter().map(|(_, f)| f).filter(|f| seen.insert(f.name.clone())).take(FOUND_MAX).collect())
    }

    fn client(&self) -> Result<Client, String> {
        #[cfg(unix)]
        if let Some(connect) = CONNECTOR.get() {
            let stream = connect()?;
            return self.rt.block_on(async { Client::from_std(stream) }).map(|c| c.via(self.via.clone())).map_err(|e| e.to_string());
        }
        self.rt.block_on(Client::connect(&self.store)).map(|c| c.via(self.via.clone())).map_err(|e| e.to_string())
    }

    fn call(&self, c: &mut Client, req: Request) -> Result<Response, String> {
        match self.rt.block_on(c.call(req)).map_err(|e| e.to_string())? {
            Response::Error { message } => Err(message),
            r => Ok(r),
        }
    }

    /// The libraries among these that are only searched (their index
    /// declares `find` but no `cell`): a gazetteer, not places to draw.
    /// The libraries among these whose index declares places by cell (a
    /// map's cafés): held lazily, what they have to show is known only so.
    pub fn place_libraries(&self, libraries: &[String]) -> Vec<String> {
        let Ok(mut c) = self.client() else { return Vec::new() };
        libraries
            .iter()
            .filter(|ns| {
                self.searchable(&mut c, ns, "cell");
                key_cache().lock().expect("keys lock").get(*ns).is_some_and(|k| k.iter().any(|k| k == "cell"))
            })
            .cloned()
            .collect()
    }

    pub fn address_libraries(&self, libraries: &[String]) -> Vec<String> {
        let Ok(mut c) = self.client() else { return Vec::new() };
        libraries
            .iter()
            .filter(|ns| {
                self.searchable(&mut c, ns, "find");
                key_cache().lock().expect("keys lock").get(*ns).is_some_and(|k| k.iter().any(|k| k == "find") && !k.iter().any(|k| k == "cell"))
            })
            .cloned()
            .collect()
    }

    /// Whether `key` may be searched for in `ns`: its index declares it, or
    /// it declares nothing (held whole, searched here). Asked once a library.
    fn searchable(&self, c: &mut Client, ns: &str, key: &str) -> bool {
        let known = key_cache().lock().expect("keys lock").get(ns).cloned();
        let missed = key_misses().lock().expect("misses lock").get(ns).is_some_and(|t| t.elapsed() < KEY_MISS_WAIT);
        if known.is_none() && missed {
            return true;
        }
        let keys = known.unwrap_or_else(|| {
            let keys: Vec<String> = self
                .query(c, ns, "type=retifs.index".into())
                .iter()
                .flat_map(|r| r.attrs.iter().filter(|(k, _)| k == "keys").flat_map(|(_, v)| match v {
                    AttrValue::List(l) => l.iter().filter_map(|x| if let AttrValue::Str(s) = x { Some(s.clone()) } else { None }).collect(),
                    AttrValue::Str(s) => s.split(',').map(|x| x.trim().to_string()).collect(),
                    _ => Vec::new(),
                }))
                .collect();
            // None found may be a failed question: asked again next time.
            if keys.is_empty() {
                key_misses().lock().expect("misses lock").insert(ns.to_string(), std::time::Instant::now());
            } else {
                key_cache().lock().expect("keys lock").insert(ns.to_string(), keys.clone());
            }
            keys
        });
        keys.is_empty() || keys.iter().any(|k| k == key)
    }

    fn query(&self, c: &mut Client, ns: &str, term: String) -> Vec<RecordView> {
        match self.call(c, Request::Query { ns: ns.into(), terms: vec![term], at: None }) {
            Ok(Response::Records { records }) => records,
            _ => Vec::new(),
        }
    }

    pub fn libraries(&self) -> Result<Vec<String>, String> {
        let mut c = self.client()?;
        let mut names = match self.call(&mut c, Request::Namespaces)? {
            Response::Names { names } => names,
            _ => Vec::new(),
        };
        // Followed public libraries (`ch-…`) too: maps, and places others publish.
        names.retain(|n| n != ME);
        names.insert(0, ME.into());
        Ok(names)
    }

    /// Everything placed in `libraries`, and contacts and events that could be.
    /// Which of `libraries` are held lazily (only what's needed is here):
    /// their places are asked for by area (`places_in`), not all at once.
    pub fn lazy(&self, libraries: &[String]) -> Vec<String> {
        let Ok(mut c) = self.client() else { return Vec::new() };
        libraries
            .iter()
            .filter(|ns| matches!(self.call(&mut c, Request::NsInfo { ns: (*ns).clone() }), Ok(Response::Namespace(v)) if v.lazy))
            .cloned()
            .collect()
    }

    /// The places of a lazily held library in these cells (`12/x/y`, the
    /// importer's `cell`): exact terms, which its holder's blind index answers.
    pub fn places_in(&self, ns: &str, cells: &[String]) -> Result<Vec<(String, Vec<Place>)>, String> {
        let mut c = self.client()?;
        // Places aren't kept by cell there (a map's tiles, addresses): none.
        if !self.searchable(&mut c, ns, "cell") {
            return Ok(cells.iter().map(|cell| (cell.clone(), Vec::new())).collect());
        }
        Ok(cells
            .iter()
            .map(|cell| {
                let found = self.query(&mut c, ns, format!("cell={cell}"));
                (cell.clone(), found.iter().flat_map(|r| places_of(ns, r).0).collect())
            })
            .collect())
    }

    /// The zoom-12 cells around a view: `cell`'s values to ask for.
    pub fn cells(west: f64, south: f64, east: f64, north: f64) -> Vec<String> {
        Self::cells_at(12, west, south, east, north)
    }

    /// The cells of zoom `z` around a view (places are in zoom-12 cells if
    /// they show from zoom 13 or earlier, else in zoom-14 cells).
    pub fn cells_at(z: u32, west: f64, south: f64, east: f64, north: f64) -> Vec<String> {
        let tile = |lat: f64, lon: f64| {
            let n = 2f64.powi(z as i32);
            let x = ((lon + 180.0) / 360.0 * n).floor() as i64;
            let phi = lat.clamp(-85.05, 85.05).to_radians();
            let y = ((1.0 - (phi.tan() + 1.0 / phi.cos()).ln() / std::f64::consts::PI) / 2.0 * n).floor() as i64;
            (x, y)
        };
        let (x0, y0) = tile(north, west);
        let (x1, y1) = tile(south, east);
        (x0..=x1).flat_map(|x| (y0..=y1).map(move |y| format!("{z}/{x}/{y}"))).collect()
    }

    pub fn places(&self, libraries: &[String]) -> Result<(Vec<Place>, Vec<Unplaced>), String> {
        let mut c = self.client()?;
        let mut seen: HashMap<String, RecordView> = HashMap::new();
        let mut from: HashMap<String, String> = HashMap::new();
        for ns in libraries {
            // Records with a plain place, and the kinds that may have labelled ones.
            let mut found = self.query(&mut c, ns, "has:geo".into());
            for t in types::terms(types::PERSON).into_iter().chain(types::terms(types::EVENT)) {
                found.extend(self.query(&mut c, ns, t));
            }
            for r in found {
                from.insert(r.id.clone(), ns.clone());
                seen.insert(r.id.clone(), r);
            }
        }
        let (mut placed, mut unplaced) = (Vec::new(), Vec::new());
        for (id, r) in &seen {
            let (p, u) = places_of(&from[id], r);
            placed.extend(p);
            unplaced.extend(u);
        }
        placed.sort_by(|a, b| b.when.cmp(&a.when).then(a.title.cmp(&b.title)));
        unplaced.sort_by(|a, b| a.title.cmp(&b.title));
        Ok((placed, unplaced))
    }

    /// Places a record: `key` (`geo`, `geo:home`) = [lat, lon].
    pub fn place(&self, ns: &str, id: &str, key: &str, lat: f64, lon: f64) -> Result<(), String> {
        let mut c = self.client()?;
        let geo = AttrValue::List(vec![AttrValue::Float((lat * 1e6).round() / 1e6), AttrValue::Float((lon * 1e6).round() / 1e6)]);
        self.call(&mut c, Request::RecordSet { ns: ns.into(), id: id.into(), attrs: vec![(key.into(), geo)], unset: vec![], data: None }).map(|_| ())
    }

    /// Joins a library from its `retifs:` link, as a layer: its places on the
    /// map, or, if it's a map library, another map. Lazily: a map library is
    /// big, and only what is looked at is fetched. Its name here.
    pub fn add_layer(&self, link: &str, name: &str) -> Result<String, String> {
        let parsed = libretifs::link::Link::parse(link.trim()).map_err(|e| e.to_string())?;
        let name = if name.trim().is_empty() { libretifs::link::suggest_name(parsed.param("name").unwrap_or("layer")) } else { libretifs::link::suggest_name(name.trim()) };
        let mut c = self.client()?;
        let req = if parsed.kind == "invite" {
            Request::InviteAccept { ns: name.clone(), link: link.trim().into() }
        } else {
            let address = parsed.ns_address().ok_or("not a library link")?;
            Request::NsJoin { ns: name.clone(), address, from: parsed.params_named("from").map(String::from).collect(), lazy: true }
        };
        self.call(&mut c, req).map(|_| name)
    }

    /// Public libraries heard announced on the network (ADR-0029), newest first.
    pub fn heard(&self) -> Result<Vec<libretifs::HeardLibrary>, String> {
        let mut c = self.client()?;
        match self.call(&mut c, Request::HeardLibraries)? {
            Response::Heard { libraries } => Ok(libraries),
            _ => Err("unexpected answer".into()),
        }
    }

    /// Joins a heard library, lazily (only what's looked at is fetched),
    /// asking the node that announced it first. Its name here.
    pub fn join_heard(&self, l: &libretifs::HeardLibrary) -> Result<String, String> {
        let base = libretifs::link::suggest_name(if l.name.trim().is_empty() { "library" } else { &l.name });
        let mut c = self.client()?;
        // A name not taken here (another library may already have it).
        let taken = match self.call(&mut c, Request::Namespaces)? {
            Response::Names { names } => names,
            _ => Vec::new(),
        };
        let name = std::iter::once(base.clone()).chain((2..).map(|i| format!("{base}-{i}"))).find(|n| !taken.contains(n)).expect("a free name");
        // A map: its index held whole (its tiles' records, small: 12 MB for
        // Estonia's 95,000 tiles), so a tile is found here at once and one
        // outside the map is known missing without asking other nodes; its
        // tiles (big) fetched as they're looked at. Anything else lazily:
        // only the records asked for (a gazetteer's 380,000 addresses).
        let map = l.has.iter().any(|(t, _)| t == TILESET_TYPE || t == VECTOR_TILE);
        self.call(&mut c, Request::NsJoin { ns: name.clone(), address: l.address.clone(), from: vec![l.node.clone()], lazy: !map })?;
        if map {
            self.call(&mut c, Request::NsKeep { ns: name.clone(), mode: "rules".into() })?;
        }
        Ok(name)
    }

    /// Which of `libraries` were joined (made elsewhere), not made here: those
    /// can be left without deleting anything for anyone else.
    pub fn joined(&self, libraries: &[String]) -> Vec<String> {
        let Ok(mut c) = self.client() else { return Vec::new() };
        libraries
            .iter()
            .filter(|ns| match self.call(&mut c, Request::NsInfo { ns: (*ns).clone() }) {
                // The address names its creator: someone else's writer key.
                Ok(Response::Namespace(v)) => v.address.split(':').nth(2).is_some_and(|creator| creator != v.own_writer),
                _ => false,
            })
            .cloned()
            .collect()
    }

    /// Leaves a joined library on this device (others keep their copies).
    /// What's known of a library here: its records, peers, how it's held.
    pub fn info(&self, ns: &str) -> Result<libretifs::NsView, String> {
        let mut c = self.client()?;
        match self.call(&mut c, Request::NsInfo { ns: ns.into() })? {
            Response::Namespace(v) => Ok(v),
            _ => Err("unexpected answer".into()),
        }
    }

    /// Keeps all of a library on this device (`all`: records and content,
    /// readable offline), or only what's used (content fetched as needed).
    pub fn keep(&self, ns: &str, all: bool) -> Result<(), String> {
        let mut c = self.client()?;
        let mode = if all { "all" } else { "rules" };
        self.call(&mut c, Request::NsKeep { ns: ns.into(), mode: mode.into() }).map(|_| ())
    }

    pub fn leave(&self, ns: &str) -> Result<(), String> {
        let mut c = self.client()?;
        self.call(&mut c, Request::NsLeave { ns: ns.into(), mine: false }).map(|_| ())
    }

    /// The maps in `libraries`.
    pub fn tilesets(&self, libraries: &[String]) -> Vec<Tileset> {
        let Ok(mut c) = self.client() else { return Vec::new() };
        let mut out = Vec::new();
        for ns in libraries {
            out.extend(self.query(&mut c, ns, format!("type={TILESET_TYPE}")).iter().map(|r| tileset_of(ns, r)));
        }
        out.sort_by(|a, b| a.overlay.cmp(&b.overlay).then(a.name.cmp(&b.name)));
        out
    }

    /// A map's style, its tiles' URLs pointing at the gateway.
    pub fn style(&self, t: &Tileset) -> Result<String, String> {
        absolute_style(&self.style_json(t)?, &t.ns)
    }

    /// A map's style as the library has it (tile paths relative to it):
    /// what an app drawing the tiles itself reads (`vector`).
    pub fn style_json(&self, t: &Tileset) -> Result<String, String> {
        let cap = t.style.clone().ok_or("this map has no style")?;
        let size: u64 = cap.rsplit(':').next().and_then(|s| s.parse().ok()).ok_or("a bad read-cap")?;
        let mut c = self.client()?;
        match self.call(&mut c, Request::ReadRange { cap, ns: Some(t.ns.clone()), offset: 0, len: size, wait_ms: 30_000 })? {
            Response::Range { data } => Ok(String::from_utf8_lossy(&data).into_owned()),
            _ => Err("no style".into()),
        }
    }

    /// A file of a library by its path (a map's tile), fetched if it isn't
    /// here: Ok(None) if there's none (a tile outside the map), an error if
    /// it couldn't be read now (worth asking again later).
    pub fn file(&self, ns: &str, path: &str) -> Result<Option<Vec<u8>>, String> {
        let mut c = self.client()?;
        let records = match self.call(&mut c, Request::Query { ns: ns.into(), terms: vec![format!("path={path}")], at: None })? {
            Response::Records { records } => records,
            _ => return Err("unexpected answer".into()),
        };
        let Some(r) = records.into_iter().next() else { return Ok(None) };
        let Some(cap) = r.blocks.first().cloned() else { return Ok(Some(Vec::new())) };
        let size: u64 = cap.rsplit(':').next().and_then(|s| s.parse().ok()).ok_or("a bad read-cap")?;
        match self.call(&mut c, Request::ReadRange { cap, ns: Some(ns.to_string()), offset: 0, len: size, wait_ms: 30_000 })? {
            Response::Range { data } => Ok(Some(data)),
            _ => Err("unexpected answer".into()),
        }
    }

    /// A small image of a photo (its thumbnail block), at most 2 MB.
    pub fn thumb(&self, p: &Place) -> Option<Vec<u8>> {
        self.image(&p.ns, p.thumb.as_ref()?, 2 << 20)
    }

    /// A block's bytes (an image), if it's no bigger than `max`.
    pub fn image(&self, ns: &str, cap: &str, max: u64) -> Option<Vec<u8>> {
        let size: u64 = cap.rsplit(':').next()?.parse().ok()?;
        if size > max {
            return None;
        }
        let mut c = self.client().ok()?;
        match self.call(&mut c, Request::ReadRange { cap: cap.to_string(), ns: Some(ns.to_string()), offset: 0, len: size, wait_ms: 30_000 }) {
            Ok(Response::Range { data }) => Some(data),
            _ => None,
        }
    }

    /// A record's attributes as text, for an info bubble.
    pub fn details(&self, ns: &str, id: &str) -> Vec<(String, String)> {
        let Ok(mut c) = self.client() else { return Vec::new() };
        let text = |v: &AttrValue| -> String {
            match v {
                AttrValue::Str(s) => s.clone(),
                AttrValue::Int(i) => i.to_string(),
                AttrValue::Float(f) => format!("{f}"),
                AttrValue::Bool(b) => b.to_string(),
                AttrValue::List(l) => l.iter().map(|x| match x {
                    AttrValue::Str(s) => s.clone(),
                    AttrValue::Float(f) => format!("{f}"),
                    AttrValue::Int(i) => i.to_string(),
                    other => format!("{other:?}"),
                }).collect::<Vec<_>>().join(", "),
                other => format!("{other:?}"),
            }
        };
        match self.call(&mut c, Request::RecordGet { ns: ns.into(), id: id.into() }) {
            Ok(Response::Records { records }) => records.first().map(|r| r.attrs.iter().map(|(k, v)| (k.clone(), text(v))).collect()).unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    pub fn change_counts(&self, libraries: &[String]) -> HashMap<String, u64> {
        let Ok(mut c) = self.client() else { return HashMap::new() };
        libraries
            .iter()
            .filter_map(|ns| match self.rt.block_on(c.call(Request::Watch { ns: ns.clone(), since: 0, timeout_ms: 0 })) {
                Ok(Response::Changed { count }) => Some((ns.clone(), count)),
                _ => None,
            })
            .collect()
    }
}

#[cfg(test)]
mod find_tests {
    use super::*;

    fn rec(attrs: &[(&str, AttrValue)]) -> RecordView {
        RecordView { id: "0".repeat(32), attrs: attrs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(), blocks: Vec::new() }
    }

    fn street() -> RecordView {
        let s = |v: &str| AttrValue::Str(v.into());
        rec(&[
            ("type", s(ADDRESS_TYPE)),
            ("kind", s("street")),
            ("name", s("Raekoja plats")),
            ("in", s("Tallinn")),
            ("geo", AttrValue::List(vec![AttrValue::Float(59.4374), AttrValue::Float(24.7457)])),
            ("houses", s("1|59.437043|24.745316;10A|59.4376|24.7449")),
            ("count", AttrValue::Int(2)),
        ])
    }

    #[test]
    fn folds_as_the_importer_does() {
        assert_eq!(fold("Pärnu mnt. 10a, Tõrva-Õue ŽŠ"), vec!["parnu", "mnt", "10a", "torva", "oue", "zs"]);
        assert_eq!(fold("Нарва, ул. Пушкина 3"), vec!["нарва", "ул", "пушкина", "3"]);
    }

    #[test]
    fn a_house_number_finds_the_house_else_the_street() {
        let w = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let hits = address_hits(&street(), &w(&["raekoja"]), Some("10a"));
        assert_eq!(hits.len(), 1);
        assert_eq!((hits[0].1.name.as_str(), hits[0].1.lat, hits[0].1.zoom), ("Raekoja plats 10A, Tallinn", 59.4376, 18.0));
        let hits = address_hits(&street(), &w(&["raek"]), Some("99"));
        assert_eq!((hits[0].1.name.as_str(), hits[0].1.zoom), ("Raekoja plats, Tallinn", 16.0));
        assert!(hits[0].0 < 400, "a street is below a house found");
    }

    #[test]
    fn a_farm_is_found_by_its_name() {
        let s = |v: &str| AttrValue::Str(v.into());
        let village = rec(&[
            ("type", s(ADDRESS_TYPE)),
            ("kind", s("settlement")),
            ("name", s("Vaida küla")),
            ("geo", AttrValue::List(vec![AttrValue::Float(59.29), AttrValue::Float(24.97)])),
            ("houses", s("Kase|59.291|24.971;Mäe-Kõrtsi|59.292|24.972")),
        ]);
        let hits = address_hits(&village, &["korts".to_string(), "vaida".to_string()], None);
        assert_eq!(hits.iter().map(|h| h.1.name.as_str()).collect::<Vec<_>>(), vec!["Mäe-Kõrtsi, Vaida küla"]);
        let hits = address_hits(&village, &["vaid".to_string()], None);
        assert_eq!((hits[0].1.name.as_str(), hits[0].1.zoom), ("Vaida küla", 13.0));
    }

    #[test]
    fn places_by_name_what_and_where() {
        let s = |v: &str| AttrValue::Str(v.into());
        let cafe = rec(&[
            ("type", s(PLACE_TYPE)),
            ("name", s("Werner")),
            ("category", s("cafe")),
            ("address", s("Ülikooli 11, Tartu")),
            ("geo", AttrValue::List(vec![AttrValue::Float(58.38), AttrValue::Float(26.72)])),
        ]);
        let (score, f) = place_hit("eesti-kohad", &cafe, &["cafe".to_string(), "tartu".to_string()]).unwrap();
        assert_eq!((f.name.as_str(), f.zoom, score), ("Werner, cafe, Ülikooli 11, Tartu", 17.0, 252));
        let mut townhall = cafe.clone();
        townhall.attrs.push(("minzoom".into(), AttrValue::Int(12)));
        assert!(place_hit("eesti-kohad", &townhall, &["cafe".to_string(), "tartu".to_string()]).unwrap().0 > score, "a landmark first");
        assert_eq!(f.place.unwrap().title, "Werner");
        let one = |w: &str| place_hit("eesti-kohad", &cafe, &[w.to_string()]).unwrap().0;
        assert!(one("werner") > one("wern") && one("wern") > one("cafe"), "its name counts more than what it is");
    }

    #[test]
    fn own_things_are_found_here_first() {
        let p = |title: &str, minzoom| Place { ns: "me".into(), id: "1".into(), kind: Kind::Person, title: title.into(), label: "home".into(), lat: 59.4, lon: 24.7, when: String::new(), detail: String::new(), minzoom, thumb: None, preview: None };
        let here = vec![p("Mari Maasikas", None), p("Maarja kohvik", Some(15))];
        let hits = find_here(&here, "mari maa");
        assert_eq!(hits.iter().map(|h| h.1.name.as_str()).collect::<Vec<_>>(), vec!["Mari Maasikas (home)"]);
        assert!(hits[0].0 >= 300, "before what the network finds");
        assert!(find_here(&here, "jaan").is_empty());
    }

    #[test]
    fn address_records_are_not_drawn() {
        assert!(places_of("aadressid", &street()).0.is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(attrs: &[(&str, AttrValue)], blocks: usize) -> RecordView {
        RecordView { id: "0123456789abcdef0123456789abcdef".into(), attrs: attrs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(), blocks: (0..blocks).map(|i| format!("cap{i}")).collect() }
    }
    fn s(v: &str) -> AttrValue {
        AttrValue::Str(v.into())
    }
    fn geo(a: f64, b: f64) -> AttrValue {
        AttrValue::List(vec![AttrValue::Float(a), AttrValue::Float(b)])
    }

    #[test]
    fn photos_people_and_events() {
        let (p, u) = places_of("photos", &rec(&[("type", s("image/jpeg")), ("path", s("/a/sea.jpg")), ("geo", geo(59.4, 24.7)), ("block:thumb", AttrValue::Int(1))], 2));
        assert_eq!((p[0].kind, p[0].title.as_str(), p[0].thumb.as_deref()), (Kind::Photo, "sea.jpg", Some("cap1")));
        assert!(u.is_empty());
        let (p, u) = places_of(ME, &rec(&[("type", s("application/x-person")), ("name", s("Ema")), ("geo:home", geo(59.6, 24.9)), ("address:work", s("Tallinn"))], 0));
        assert_eq!((p.len(), p[0].label.as_str(), p[0].kind), (1, "home", Kind::Person));
        assert_eq!((u[0].key.as_str(), u[0].address.as_str()), ("geo:work", "Tallinn"));
        let (p, u) = places_of("pere", &rec(&[("type", s("event")), ("title", s("Õhtusöök")), ("location", s("Prangli"))], 0));
        assert!(p.is_empty());
        assert_eq!((u[0].kind, u[0].key.as_str()), (Kind::Event, "geo"));
        let (p, _) = places_of("x", &rec(&[("geo", s("59.43, 24.75"))], 0));
        assert_eq!((p[0].lat, p[0].lon, p[0].kind), (59.43, 24.75, Kind::Other));
        let (p, _) = places_of(ME, &rec(&[("type", s(PLACE_TYPE)), ("name", s("Suvila")), ("geo", geo(58.9, 23.5))], 0));
        assert_eq!((p[0].title.as_str(), p[0].kind), ("Suvila", Kind::Other));
    }

    #[test]
    fn cells_around_a_view() {
        assert_eq!(Map::cells(24.744, 59.436, 24.746, 59.438), vec!["12/2329/1202".to_string()]);
        assert_eq!(Map::cells(24.70, 59.40, 24.80, 59.45).len(), 2 * 3);
        assert_eq!(Map::cells_at(14, 24.744, 59.436, 24.746, 59.438), vec!["14/9318/4808".to_string()]);
    }

    #[test]
    fn maps() {
        let r = rec(&[("type", s(TILESET_TYPE)), ("name", s("Eesti")), ("minzoom", AttrValue::Int(0)), ("maxzoom", AttrValue::Int(14)), ("bounds", AttrValue::List(vec![AttrValue::Float(20.8), AttrValue::Float(57.5), AttrValue::Float(28.2), AttrValue::Float(60.0)]))], 1);
        let t = tileset_of("ch-ee", &r);
        assert_eq!((t.name.as_str(), t.bounds, t.overlay, t.style.as_deref()), ("Eesti", Some([20.8, 57.5, 28.2, 60.0]), false, Some("cap0")));
        let style = absolute_style(r#"{"sources":{"o":{"type":"vector","tiles":["/tiles/{z}/{x}/{y}.pbf","https://x/{z}"]}}}"#, "kaart").unwrap();
        assert!(style.contains(r#""http://kaart.localhost:8740/tiles/{z}/{x}/{y}.pbf""#) && style.contains("https://x/{z}"), "{style}");
    }

}
