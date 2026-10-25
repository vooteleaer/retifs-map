//! Places on retifs: every record that says where it is (`geo`, or a
//! labelled `geo:<label>` such as a contact's home, ADR-0027), whatever it
//! is: photos, contacts, events, and anything else. The map owns none of
//! them; it shows them, and lets the user place a contact or an event whose
//! address or location has no coordinates yet.

use std::collections::HashMap;
use std::path::PathBuf;

use libretifs::{types, AttrValue, Client, RecordView, Request, Response};

pub const ME: &str = "me";
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
    /// The first content block's read-cap (a photo), and its thumbnail's.
    pub thumb: Option<String>,
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
    let kind = Kind::of(&text(r, "type").unwrap_or_default());
    let title = title_of(r);
    let when = text(r, "taken").or_else(|| text(r, "start")).unwrap_or_default();
    let thumb_index = r.attrs.iter().find_map(|(k, v)| match (k.as_str(), v) {
        ("block:thumb", AttrValue::Int(i)) if *i > 0 => Some(*i as usize),
        _ => None,
    });
    let thumb = (kind == Kind::Photo).then(|| thumb_index.and_then(|i| r.blocks.get(i)).or(r.blocks.first()).cloned()).flatten();
    let mut placed = Vec::new();
    for (k, v) in &r.attrs {
        let label = if k == "geo" { "" } else if let Some(l) = k.strip_prefix("geo:") { l } else { continue };
        if let Some((lat, lon)) = latlon(v) {
            placed.push(Place { ns: ns.into(), id: r.id.clone(), kind, title: title.clone(), label: label.into(), lat, lon, when: when.clone(), thumb: thumb.clone() });
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

/// An address search's answer: a suggestion the user may pick.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    /// The whole name Nominatim gives ("Kalda, Prangli, Viimsi vald, Harju maakond, Eesti").
    pub name: String,
    pub lat: f64,
    pub lon: f64,
}

fn found(json: &str) -> Result<Vec<Found>, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    Ok(v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|r| {
                    let num = |k: &str| r.get(k).and_then(|x| x.as_str()).and_then(|s| s.parse::<f64>().ok());
                    Some(Found { name: r.get("display_name")?.as_str()?.to_string(), lat: num("lat")?, lon: num("lon")? })
                })
                .collect()
        })
        .unwrap_or_default())
}

/// Whether address search is on: a file in the user's config, set once per device.
fn lookup_flag() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"));
    base.join("retifs-map").join("address-search-on")
}

pub fn lookup_allowed() -> bool {
    lookup_flag().exists()
}

pub fn allow_lookup(on: bool) -> std::io::Result<()> {
    let f = lookup_flag();
    if on {
        std::fs::create_dir_all(f.parent().expect("a config dir"))?;
        std::fs::write(f, b"Address search in OpenStreetMap's Nominatim was turned on here (ADR-0027).\n")
    } else {
        std::fs::remove_file(f).or_else(|e| if e.kind() == std::io::ErrorKind::NotFound { Ok(()) } else { Err(e) })
    }
}

/// The connection to `retifsd`.
pub struct Map {
    rt: tokio::runtime::Runtime,
    store: PathBuf,
    /// When Nominatim was last asked: at most once a second (its usage policy).
    last_lookup: std::sync::Mutex<Option<std::time::Instant>>,
}

impl Default for Map {
    fn default() -> Self {
        Self::new()
    }
}

impl Map {
    pub fn new() -> Self {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("a tokio runtime");
        Map { rt, store: libretifs::default_store_dir(), last_lookup: std::sync::Mutex::new(None) }
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

    /// Looks an address up in OpenStreetMap's Nominatim, if the user turned it on.
    pub fn lookup(&self, text: &str) -> Result<Vec<Found>, String> {
        if !lookup_allowed() {
            return Err("address search is off".into());
        }
        {
            let mut last = self.last_lookup.lock().expect("lookup lock");
            if let Some(t) = *last {
                let wait = std::time::Duration::from_secs(1).saturating_sub(t.elapsed());
                std::thread::sleep(wait);
            }
            *last = Some(std::time::Instant::now());
        }
        let body = ureq::get("https://nominatim.openstreetmap.org/search")
            .set("User-Agent", concat!("retifs-map/", env!("CARGO_PKG_VERSION"), " (a retifs demo app)"))
            .query("q", text)
            .query("format", "jsonv2")
            .query("limit", "8")
            .call()
            .map_err(|e| e.to_string())?
            .into_string()
            .map_err(|e| e.to_string())?;
        found(&body)
    }

    fn client(&self) -> Result<Client, String> {
        self.rt.block_on(Client::connect(&self.store)).map_err(|e| e.to_string())
    }

    fn call(&self, c: &mut Client, req: Request) -> Result<Response, String> {
        match self.rt.block_on(c.call(req)).map_err(|e| e.to_string())? {
            Response::Error { message } => Err(message),
            r => Ok(r),
        }
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
        names.retain(|n| n != ME && !n.starts_with("ch-"));
        names.insert(0, ME.into());
        Ok(names)
    }

    /// Everything placed in `libraries`, and contacts and events that could be.
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

    /// A small image of a photo (its thumbnail block), at most 2 MB.
    pub fn thumb(&self, p: &Place) -> Option<Vec<u8>> {
        let cap = p.thumb.clone()?;
        let size: u64 = cap.rsplit(':').next()?.parse().ok()?;
        if size > 2 << 20 {
            return None;
        }
        let mut c = self.client().ok()?;
        match self.call(&mut c, Request::ReadRange { cap, ns: Some(p.ns.clone()), offset: 0, len: size, wait_ms: 30_000 }) {
            Ok(Response::Range { data }) => Some(data),
            _ => None,
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
    fn nominatim_answers() {
        let json = r#"[{"place_id":1,"lat":"59.6236","lon":"25.0061","display_name":"Prangli, Viimsi vald, Harju maakond, Eesti"},{"lat":"x","lon":"1","display_name":"bad"}]"#;
        assert_eq!(found(json).unwrap(), vec![Found { name: "Prangli, Viimsi vald, Harju maakond, Eesti".into(), lat: 59.6236, lon: 25.0061 }]);
        assert!(found("[]").unwrap().is_empty());
    }
}
