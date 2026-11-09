//! Everything that can block, off the UI thread: reading the libraries,
//! Announces and joining, places by area, a map's style, a photo's
//! thumbnail, and searching. Each answer is queued as an `Event` and the
//! window is told once to look; it takes all that's queued.
//!
//! Two job threads as in the Haiku app: a slow one (reading libraries,
//! polling) and a quick one (styles, places by area, thumbnails), so a slow
//! library doesn't hold up the map. Searching has a thread of its own that
//! only ever runs the latest search typed.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Condvar, Mutex, OnceLock};

use libretifs::HeardLibrary;
use retifs_map_core::vector::Style;
use retifs_map_core::{Found, Map, Place, Tileset};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::*;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// Something's queued for the window.
pub const WM_EVENTS: u32 = WM_APP + 1;
/// The main window, for the threads to post to.
pub static MAIN: AtomicIsize = AtomicIsize::new(0);

/// One library read.
pub struct Library {
    pub ns: String,
    pub tilesets: Vec<Tileset>,
    pub places: Vec<Place>,
    /// Held lazily, its places asked for by area (a map's cafés).
    pub by_cell: bool,
    /// A gazetteer: only searched, nothing of it to show.
    pub address: bool,
}

/// Decoded pixels (premultiplied BGRA, top-down), for a Direct2D bitmap.
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

pub enum Event {
    /// The libraries here, in order, or why they couldn't be read.
    Libraries(Result<Vec<String>, String>),
    Library(Library),
    LoadDone,
    Heard(Result<Vec<HeardLibrary>, String>),
    Joined(Result<String, String>),
    /// A library changed: everything is read again.
    Changed,
    Cells { ns: String, found: Vec<(String, Vec<Place>)> },
    Style { id: String, style: Result<Style, String> },
    Thumb { key: String, image: Option<Image> },
    Found { seq: u64, found: Result<Vec<Found>, String> },
    Tile(crate::tiles::Done),
}

pub enum Job {
    Load,
    /// Announces, and whether these libraries changed (every few seconds).
    Poll(Vec<String>),
    Join(HeardLibrary),
    Cells(String, Vec<String>),
    Style(Tileset),
    Thumb(Place),
}

static EVENTS: Mutex<VecDeque<Event>> = Mutex::new(VecDeque::new());

pub fn send(e: Event) {
    let first = {
        let mut q = EVENTS.lock().unwrap();
        q.push_back(e);
        q.len() == 1
    };
    // One message for a burst: the window takes all of them at once.
    if first {
        let h = MAIN.load(Ordering::SeqCst);
        if h != 0 {
            unsafe {
                let _ = PostMessageW(HWND(h as _), WM_EVENTS, WPARAM(0), LPARAM(0));
            }
        }
    }
}

pub fn take() -> Vec<Event> {
    EVENTS.lock().unwrap().drain(..).collect()
}

struct Workers {
    slow: Mutex<Sender<Job>>,
    quick: Mutex<Sender<Job>>,
}

static WORKERS: OnceLock<Workers> = OnceLock::new();

/// Starts the job and search threads (once).
pub fn start() {
    let (slow, slow_rx) = channel();
    let (quick, quick_rx) = channel();
    if WORKERS.set(Workers { slow: Mutex::new(slow), quick: Mutex::new(quick) }).is_err() {
        return;
    }
    std::thread::spawn(move || jobs(slow_rx));
    std::thread::spawn(move || jobs(quick_rx));
    std::thread::spawn(searcher);
}

pub fn ask(job: Job) {
    let Some(w) = WORKERS.get() else { return };
    let to = match job {
        Job::Load | Job::Poll(_) | Job::Join(_) => &w.slow,
        _ => &w.quick,
    };
    let _ = to.lock().unwrap().send(job);
}

fn jobs(rx: Receiver<Job>) {
    // WIC (thumbnails) wants COM on this thread.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let m = Map::new();
    let mut counts: Option<HashMap<String, u64>> = None;
    while let Ok(job) = rx.recv() {
        match job {
            Job::Load => {
                load(&m);
                counts = None;
            }
            Job::Poll(libraries) => {
                send(Event::Heard(m.heard()));
                // Changed since last asked: read again (the first time only
                // notes them; libraries come and go with a load of their own).
                let now = m.change_counts(&libraries);
                if counts.as_ref().is_some_and(|c| c.len() == now.len() && c.keys().all(|k| now.contains_key(k)) && *c != now) {
                    send(Event::Changed);
                }
                counts = Some(now);
            }
            Job::Join(l) => send(Event::Joined(m.join_heard(&l))),
            Job::Cells(ns, cells) => {
                let found = m.places_in(&ns, &cells).unwrap_or_default();
                send(Event::Cells { ns, found });
            }
            Job::Style(t) => {
                let style = m.style_json(&t).and_then(|j| Style::parse(&j));
                send(Event::Style { id: t.id.clone(), style });
            }
            Job::Thumb(p) => {
                let image = m.thumb(&p).and_then(|b| unsafe { decode(&b, 256).ok() });
                send(Event::Thumb { key: thumb_key(&p), image });
            }
        }
    }
}

pub fn thumb_key(p: &Place) -> String {
    format!("{}/{}", p.ns, p.id)
}

/// Reads every library, one by one, so the window shows each as it comes
/// (as the Haiku app does). Held lazily: its places by area, not all of them.
fn load(m: &Map) {
    let libraries = match m.libraries() {
        Ok(l) => l,
        Err(e) => {
            send(Event::Libraries(Err(e)));
            return;
        }
    };
    send(Event::Libraries(Ok(libraries.clone())));
    let lazy = m.lazy(&libraries);
    for ns in &libraries {
        let one = [ns.clone()];
        let tilesets = m.tilesets(&one);
        let address = !m.address_libraries(&one).is_empty();
        let held_lazily = lazy.contains(ns);
        let by_cell = !address && held_lazily && !m.place_libraries(&one).is_empty();
        let places = if address || held_lazily { Vec::new() } else { m.places(&one).map(|p| p.0).unwrap_or_default() };
        send(Event::Library(Library { ns: ns.clone(), tilesets, places, by_cell, address }));
    }
    send(Event::LoadDone);
}

/// A search to run: only the latest is, the ones typed over are dropped.
pub struct Search {
    pub seq: u64,
    pub text: String,
    /// The places shown (one's own are found here first).
    pub here: Vec<Place>,
    /// The libraries shown, and the gazetteers.
    pub libraries: Vec<String>,
}

static SEARCH: (Mutex<Option<Search>>, Condvar) = (Mutex::new(None), Condvar::new());

pub fn search(s: Search) {
    let (m, c) = &SEARCH;
    *m.lock().unwrap() = Some(s);
    c.notify_one();
}

fn searcher() {
    let m = Map::new();
    loop {
        let s = {
            let (lock, c) = &SEARCH;
            let mut next = lock.lock().unwrap();
            while next.is_none() {
                next = c.wait(next).unwrap();
            }
            next.take().unwrap()
        };
        let found = m.find_in(&s.text, &s.here, &s.libraries);
        send(Event::Found { seq: s.seq, found });
    }
}

/// An image file (a photo's thumbnail) as pixels, at most `max` across.
unsafe fn decode(data: &[u8], max: u32) -> windows::core::Result<Image> {
    let f: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
    let stream = f.CreateStream()?;
    stream.InitializeFromMemory(data)?;
    let decoder = f.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)?;
    let frame = decoder.GetFrame(0)?;
    let (mut w, mut h) = (0u32, 0u32);
    frame.GetSize(&mut w, &mut h)?;
    let k = (max as f64 / w.max(h).max(1) as f64).min(1.0);
    let (tw, th) = (((w as f64 * k).round() as u32).max(1), ((h as f64 * k).round() as u32).max(1));
    let scaler = f.CreateBitmapScaler()?;
    scaler.Initialize(&frame, tw, th, WICBitmapInterpolationModeFant)?;
    let converter = f.CreateFormatConverter()?;
    converter.Initialize(&scaler, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)?;
    let mut pixels = vec![0u8; (tw * th * 4) as usize];
    converter.CopyPixels(std::ptr::null(), tw * 4, &mut pixels)?;
    Ok(Image { width: tw, height: th, pixels })
}
