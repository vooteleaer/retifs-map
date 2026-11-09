//! The map: the shown map library's tiles, drawn with Direct2D; the places
//! shown on it as markers, grouped by zoom so tens of thousands stay
//! usable; a place's bubble; and dragging, the wheel and the keys to move
//! around. Tiles come from `tiles`; the places, the map and its style are
//! handed over by the window.
//!
//! Everything here is in DIPs (1/96 inch): the render target knows the DPI.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use retifs_map_core::vector::Style;
use retifs_map_core::{icons, Kind, Place, Tileset};
use windows::core::{w, HSTRING, PCWSTR};
use windows::Foundation::Numerics::Matrix3x2;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, ScreenToClient, PAINTSTRUCT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::tiles::{self, factories, Art, Paint, SIZE};
use crate::work::{self, Image, Job};

pub const CLASS: PCWSTR = w!("retifs-map-canvas");
/// The view moved or zoomed: to the window, which asks for places there.
pub const WM_VIEW: u32 = WM_APP + 2;
const MIN_ZOOM: f64 = 2.0;
const MAX_ZOOM: f64 = 18.0;
/// Markers closer than this at the zoom shown become one (as on Linux and Haiku).
const CELL: f64 = 56.0;
/// Markers pushed apart to at least this, so none hides another.
const APART: f32 = 34.0;
const RADIUS: f32 = 13.0;
const KEEP_TILES: usize = 220;
/// New tile bitmaps made per frame at most; the rest the next, so a pan stays smooth.
const RENDER_PER_FRAME: usize = 6;
/// A tile that couldn't be read is asked for again after this long.
const RETRY_S: u64 = 10;
const ZOOM_MS: f64 = 180.0;
const T_ANIM: usize = 1;
const T_RETRY: usize = 2;

type Key = (u32, u32, u32);

/// Web Mercator, the whole world 0..1 across and down.
pub fn merc(lat: f64, lon: f64) -> (f64, f64) {
    let phi = lat.clamp(-85.05, 85.05).to_radians();
    ((lon + 180.0) / 360.0, (1.0 - (phi.tan() + 1.0 / phi.cos()).ln() / std::f64::consts::PI) / 2.0)
}

fn unmerc(x: f64, y: f64) -> (f64, f64) {
    ((std::f64::consts::PI * (1.0 - 2.0 * y)).sinh().atan().to_degrees(), x * 360.0 - 180.0)
}

fn rgba(c: u32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: ((c >> 16) & 0xff) as f32 / 255.0, g: ((c >> 8) & 0xff) as f32 / 255.0, b: (c & 0xff) as f32 / 255.0, a: (c >> 24) as f32 / 255.0 }
}

fn rect(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: l, top: t, right: r, bottom: b }
}

fn inside(r: &D2D_RECT_F, x: f32, y: f32) -> bool {
    x >= r.left && x <= r.right && y >= r.top && y <= r.bottom
}

fn ellipse(x: f32, y: f32, r: f32) -> D2D1_ELLIPSE {
    D2D1_ELLIPSE { point: D2D_POINT_2F { x, y }, radiusX: r, radiusY: r }
}

fn round_rect(r: D2D_RECT_F, radius: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius }
}

const IDENTITY: Matrix3x2 = Matrix3x2 { M11: 1.0, M12: 0.0, M21: 0.0, M22: 1.0, M31: 0.0, M32: 0.0 };

/// A turn by `deg` around (x, y).
fn rotation(deg: f32, x: f32, y: f32) -> Matrix3x2 {
    let (s, c) = deg.to_radians().sin_cos();
    Matrix3x2 { M11: c, M12: s, M21: -s, M22: c, M31: x - c * x + s * y, M32: y - s * x - c * y }
}

/// A kind's colour and sign, as in the Haiku app.
fn kind_look(k: Kind) -> (u32, &'static str) {
    match k {
        Kind::Photo => (0xff35_84e4, "\u{25a3}"),
        Kind::Person => (0xff26_a269, "\u{263a}"),
        Kind::Event => (0xffe6_6100, "\u{25a6}"),
        Kind::Other => (0xff91_41ac, "\u{25cf}"),
    }
}

/// What a place is, in a word or two.
fn kind_word(p: &Place) -> String {
    match p.kind {
        Kind::Photo => "Photo".into(),
        Kind::Person => "Contact".into(),
        Kind::Event => "Event".into(),
        Kind::Other if !p.detail.is_empty() => {
            let mut d = p.detail.clone();
            if let Some(f) = d.get_mut(0..1) {
                f.make_ascii_uppercase();
            }
            d
        }
        Kind::Other => "Place".into(),
    }
}

/// A map's place with a category (a café): its icon on its group's colour.
fn has_icon(p: &Place) -> bool {
    p.kind == Kind::Other && !p.detail.is_empty()
}

struct Entry {
    /// None: the map has no tile here (drawn as its background).
    art: Option<Arc<Art>>,
    bitmap: Option<ID2D1Bitmap>,
    used: u64,
}

struct Shown {
    id: u64,
    tileset: Arc<Tileset>,
    style: Arc<Style>,
}

struct Marker {
    place: Place,
    x: f64,
    y: f64,
}

struct Hit {
    x: f32,
    y: f32,
    r: f32,
    members: Vec<usize>,
}

struct Bubble {
    place: Place,
    /// How many places at that spot.
    here: usize,
    card: D2D_RECT_F,
    close: D2D_RECT_F,
}

struct Anim {
    from: f64,
    to: f64,
    x: f32,
    y: f32,
    start: Instant,
}

struct Drag {
    x: f32,
    y: f32,
    moved: bool,
}

struct Formats {
    small: IDWriteTextFormat,
    body: IDWriteTextFormat,
    title: IDWriteTextFormat,
    /// Centred in a box: a marker's sign, a count, the zoom buttons.
    sign: IDWriteTextFormat,
    count: IDWriteTextFormat,
    button: IDWriteTextFormat,
    message: IDWriteTextFormat,
}

unsafe fn format(size: f32, bold: bool, centred: bool) -> IDWriteTextFormat {
    let weight = if bold { DWRITE_FONT_WEIGHT_SEMI_BOLD } else { DWRITE_FONT_WEIGHT_NORMAL };
    let f = factories().dwrite.CreateTextFormat(w!("Segoe UI"), None, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, w!("")).expect("a font");
    if centred {
        let _ = f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
        let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
    }
    f
}

pub struct Canvas {
    hwnd: HWND,
    rt: Option<ID2D1HwndRenderTarget>,
    brush: Option<ID2D1SolidColorBrush>,
    text: Formats,
    dpi: f32,
    /// The zoom (fractional) and the middle of the view (Mercator, 0..1).
    zoom: f64,
    mx: f64,
    my: f64,
    anim: Option<Anim>,
    shown: Option<Shown>,
    serial: u64,
    background: u32,
    credit: String,
    tiles: HashMap<Key, Entry>,
    pending: HashSet<Key>,
    failed: HashMap<Key, Instant>,
    uses: u64,
    markers: Vec<Marker>,
    hits: Vec<Hit>,
    hover: bool,
    bubble: Option<Bubble>,
    icons: HashMap<String, Option<ID2D1Bitmap>>,
    thumbs: HashMap<String, Image>,
    thumb_bitmaps: HashMap<String, ID2D1Bitmap>,
    thumbs_asked: HashSet<String>,
    drag: Option<Drag>,
    /// The user moved the map: it isn't fitted to the data any more.
    pub touched: bool,
    /// Said over the map: why there's nothing (retifsd not running, no map).
    message: String,
}

thread_local! {
    static CANVAS: RefCell<Option<Canvas>> = const { RefCell::new(None) };
}

/// The map's state, if it isn't busy (a message sent while it's drawing).
pub fn with<R>(f: impl FnOnce(&mut Canvas) -> R) -> Option<R> {
    CANVAS.with(|c| c.try_borrow_mut().ok().and_then(|mut c| c.as_mut().map(f)))
}

pub unsafe fn register() {
    let class = WNDCLASSW {
        style: CS_DBLCLKS,
        lpfnWndProc: Some(wndproc),
        hInstance: GetModuleHandleW(None).unwrap_or_default().into(),
        lpszClassName: CLASS,
        hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
        ..Default::default()
    };
    RegisterClassW(&class);
}

pub unsafe fn create(parent: HWND) -> HWND {
    let hwnd = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        CLASS,
        w!(""),
        WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS | WS_TABSTOP,
        0,
        0,
        10,
        10,
        parent,
        None,
        GetModuleHandleW(None).unwrap_or_default(),
        None,
    )
    .unwrap_or_default();
    // Estonia until the libraries are read (the maps here are of it).
    let (mx, my) = merc(58.65, 25.0);
    let c = Canvas {
        hwnd,
        rt: None,
        brush: None,
        text: Formats {
            small: format(11.0, false, false),
            body: format(12.5, false, false),
            title: format(14.5, true, false),
            sign: format(13.0, true, true),
            count: format(10.0, true, true),
            button: format(18.0, true, true),
            message: {
                // Centred across only: it's laid out to its own height.
                let f = format(13.0, false, false);
                let _ = f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
                f
            },
        },
        dpi: GetDpiForWindow(hwnd).max(96) as f32,
        zoom: 7.0,
        mx,
        my,
        anim: None,
        shown: None,
        serial: 0,
        background: 0xfff2_efe9,
        credit: String::new(),
        tiles: HashMap::new(),
        pending: HashSet::new(),
        failed: HashMap::new(),
        uses: 0,
        markers: Vec::new(),
        hits: Vec::new(),
        hover: false,
        bubble: None,
        icons: HashMap::new(),
        thumbs: HashMap::new(),
        thumb_bitmaps: HashMap::new(),
        thumbs_asked: HashSet::new(),
        drag: None,
        touched: false,
        message: String::new(),
    };
    CANVAS.with(|cv| *cv.borrow_mut() = Some(c));
    SetTimer(hwnd, T_RETRY, 5000, None);
    hwnd
}

impl Canvas {
    fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(self.hwnd, None, false);
        }
    }

    /// Tells the window the view changed (lazy libraries' places there).
    fn changed(&self) {
        unsafe {
            if let Ok(parent) = GetParent(self.hwnd) {
                let _ = PostMessageW(parent, WM_VIEW, WPARAM(0), LPARAM(0));
            }
        }
    }

    fn size(&self) -> (f32, f32) {
        let mut rc = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut rc);
        }
        let k = 96.0 / self.dpi;
        (rc.right as f32 * k, rc.bottom as f32 * k)
    }

    /// Pixels (a mouse message's) as DIPs.
    fn dips(&self, lparam: LPARAM) -> (f32, f32) {
        let (x, y) = ((lparam.0 & 0xffff) as i16 as f32, ((lparam.0 >> 16) & 0xffff) as i16 as f32);
        (x * 96.0 / self.dpi, y * 96.0 / self.dpi)
    }

    fn world(&self) -> f64 {
        256.0 * 2f64.powf(self.zoom)
    }

    /// Where a place is on screen.
    fn screen(&self, x: f64, y: f64) -> (f32, f32) {
        let (w, h) = self.size();
        let world = self.world();
        (((x - self.mx) * world + w as f64 / 2.0) as f32, ((y - self.my) * world + h as f64 / 2.0) as f32)
    }

    pub fn zoom(&self) -> f64 {
        self.zoom
    }

    /// The view's west, south, east and north (degrees).
    pub fn area(&self) -> (f64, f64, f64, f64) {
        let (w, h) = self.size();
        let world = self.world();
        let (n, west) = unmerc(self.mx - w as f64 / 2.0 / world, (self.my - h as f64 / 2.0 / world).max(0.0));
        let (s, e) = unmerc(self.mx + w as f64 / 2.0 / world, (self.my + h as f64 / 2.0 / world).min(1.0));
        (west, s, e, n)
    }

    pub fn set_message(&mut self, text: &str) {
        if self.message != text {
            self.message = text.to_string();
            self.invalidate();
        }
    }

    /// Shows this map (its style read), or none.
    pub fn set_map(&mut self, map: Option<(Tileset, Arc<Style>)>) {
        self.serial += 1;
        self.tiles.clear();
        self.pending.clear();
        self.failed.clear();
        self.shown = map.map(|(t, style)| {
            self.background = style.background;
            self.credit = t.attribution.clone();
            Shown { id: self.serial, tileset: Arc::new(t), style }
        });
        if self.shown.is_none() {
            self.background = 0xfff2_efe9;
            self.credit.clear();
        }
        self.invalidate();
    }

    pub fn shown_map(&self) -> Option<String> {
        self.shown.as_ref().map(|s| s.tileset.id.clone())
    }

    /// The places to show (only those of the libraries shown).
    pub fn set_places(&mut self, places: Vec<Place>) {
        self.markers = places
            .into_iter()
            .map(|place| {
                let (x, y) = merc(place.lat, place.lon);
                Marker { place, x, y }
            })
            .collect();
        self.invalidate();
    }

    pub fn tile_done(&mut self, d: tiles::Done) {
        if self.shown.as_ref().map(|s| s.id) != Some(d.map) {
            return;
        }
        self.pending.remove(&d.key);
        match d.art {
            Ok(art) => {
                self.tiles.insert(d.key, Entry { art: art.map(Arc::new), bitmap: None, used: self.uses });
                self.trim();
            }
            Err(_) => {
                self.failed.insert(d.key, Instant::now());
            }
        }
        self.invalidate();
    }

    /// Keeps the tiles last drawn, dropping the oldest beyond a few screens.
    fn trim(&mut self) {
        if self.tiles.len() <= KEEP_TILES {
            return;
        }
        let mut by_use: Vec<(u64, Key)> = self.tiles.iter().map(|(k, e)| (e.used, *k)).collect();
        by_use.sort();
        for (_, k) in by_use.iter().take(self.tiles.len() - KEEP_TILES * 9 / 10) {
            self.tiles.remove(k);
        }
    }

    pub fn add_thumb(&mut self, key: String, image: Image) {
        self.thumbs.insert(key, image);
        self.invalidate();
    }

    /// Puts (lat, lon) in the middle at `zoom`.
    pub fn center_on(&mut self, lat: f64, lon: f64, zoom: f64) {
        self.anim = None;
        (self.mx, self.my) = merc(lat, lon);
        self.zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        self.touched = true;
        self.invalidate();
        self.changed();
    }

    /// Fits west, south, east, north in the view (at most `max` close).
    pub fn fit(&mut self, west: f64, south: f64, east: f64, north: f64, max: f64) {
        let (w, h) = self.size();
        let (x0, y0) = merc(north, west);
        let (x1, y1) = merc(south, east);
        let (dx, dy) = ((x1 - x0).max(1e-9), (y1 - y0).max(1e-9));
        // A little room around it.
        let z = ((w as f64 * 0.85 / (dx * 256.0)).min(h as f64 * 0.85 / (dy * 256.0))).log2();
        self.anim = None;
        self.zoom = z.floor().clamp(MIN_ZOOM, max);
        (self.mx, self.my) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
        self.invalidate();
        self.changed();
    }

    pub fn close_bubble(&mut self) {
        if self.bubble.take().is_some() {
            self.invalidate();
        }
    }

    /// Opens a place's bubble (`here`: how many at that spot); a photo's
    /// thumbnail is asked for.
    pub fn open_bubble(&mut self, place: Place, here: usize) {
        if place.kind == Kind::Photo && place.thumb.is_some() {
            let key = work::thumb_key(&place);
            if self.thumbs_asked.insert(key) {
                work::ask(Job::Thumb(place.clone()));
            }
        }
        self.bubble = Some(Bubble { place, here, card: rect(0.0, 0.0, 0.0, 0.0), close: rect(0.0, 0.0, 0.0, 0.0) });
        self.invalidate();
    }

    /// The zoom at (x, y) changed to `z`: the point under it stays there.
    fn zoom_at(&mut self, z: f64, x: f32, y: f32) {
        let (w, h) = self.size();
        let z = z.clamp(MIN_ZOOM, MAX_ZOOM);
        let (old, new) = (self.world(), 256.0 * 2f64.powf(z));
        let (dx, dy) = (x as f64 - w as f64 / 2.0, y as f64 - h as f64 / 2.0);
        let (px, py) = (self.mx + dx / old, self.my + dy / old);
        self.mx = (px - dx / new).rem_euclid(1.0);
        self.my = (py - dy / new).clamp(0.0, 1.0);
        self.zoom = z;
    }

    /// Zooms by `steps` around (x, y), gliding there.
    pub fn zoom_by(&mut self, steps: f64, x: f32, y: f32) {
        let to = (self.anim.as_ref().map_or(self.zoom, |a| a.to) + steps).clamp(MIN_ZOOM, MAX_ZOOM);
        if (to - self.zoom).abs() < 1e-6 {
            return;
        }
        self.touched = true;
        self.anim = Some(Anim { from: self.zoom, to, x, y, start: Instant::now() });
        unsafe {
            SetTimer(self.hwnd, T_ANIM, 15, None);
        }
    }

    fn zoom_middle(&mut self, steps: f64) {
        let (w, h) = self.size();
        self.zoom_by(steps, w / 2.0, h / 2.0);
    }

    fn tick(&mut self) {
        let Some(a) = &self.anim else {
            unsafe {
                let _ = KillTimer(self.hwnd, T_ANIM);
            }
            return;
        };
        let t = (a.start.elapsed().as_secs_f64() * 1000.0 / ZOOM_MS).min(1.0);
        let eased = 1.0 - (1.0 - t).powi(3);
        let (z, x, y) = (a.from + (a.to - a.from) * eased, a.x, a.y);
        self.zoom_at(z, x, y);
        if t >= 1.0 {
            self.anim = None;
            unsafe {
                let _ = KillTimer(self.hwnd, T_ANIM);
            }
            self.changed();
        }
        self.invalidate();
    }

    fn pan(&mut self, dx: f32, dy: f32) {
        let world = self.world();
        self.mx = (self.mx - dx as f64 / world).rem_euclid(1.0);
        self.my = (self.my - dy as f64 / world).clamp(0.0, 1.0);
        self.invalidate();
    }

    /// The zoom the tiles are drawn from: the nearest whole one.
    fn tile_zoom(&self) -> u32 {
        self.zoom.round().clamp(0.0, MAX_ZOOM) as u32
    }

    /// A length in DIPs on whole device pixels, so tiles meet without seams.
    fn snap(&self, v: f64) -> f32 {
        let k = self.dpi as f64 / 96.0;
        ((v * k).round() / k) as f32
    }

    pub fn dpi_changed(&mut self) {
        self.dpi = unsafe { GetDpiForWindow(self.hwnd) }.max(96) as f32;
        if let Some(rt) = &self.rt {
            unsafe { rt.SetDpi(self.dpi, self.dpi) };
        }
        // Made for the old DPI: again, sharp.
        for e in self.tiles.values_mut() {
            e.bitmap = None;
        }
        self.icons.clear();
        self.invalidate();
    }

    fn resize(&mut self) {
        let mut rc = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut rc);
            if let Some(rt) = &self.rt {
                let _ = rt.Resize(&D2D_SIZE_U { width: rc.right.max(1) as u32, height: rc.bottom.max(1) as u32 });
            }
        }
        self.invalidate();
    }

    unsafe fn target(&mut self) -> Option<(ID2D1HwndRenderTarget, ID2D1SolidColorBrush)> {
        if self.rt.is_none() {
            let mut rc = RECT::default();
            let _ = GetClientRect(self.hwnd, &mut rc);
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_UNKNOWN, alphaMode: D2D1_ALPHA_MODE_UNKNOWN },
                dpiX: self.dpi,
                dpiY: self.dpi,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let hwnd_props = D2D1_HWND_RENDER_TARGET_PROPERTIES {
                hwnd: self.hwnd,
                pixelSize: D2D_SIZE_U { width: rc.right.max(1) as u32, height: rc.bottom.max(1) as u32 },
                presentOptions: D2D1_PRESENT_OPTIONS_NONE,
            };
            let rt = factories().d2d.CreateHwndRenderTarget(&props, &hwnd_props).ok()?;
            self.brush = rt.CreateSolidColorBrush(&rgba(0xff00_0000), None).ok();
            self.rt = Some(rt);
        }
        Some((self.rt.clone()?, self.brush.clone()?))
    }

    /// The device was lost: everything made on it is made again.
    fn drop_target(&mut self) {
        self.rt = None;
        self.brush = None;
        for e in self.tiles.values_mut() {
            e.bitmap = None;
        }
        self.icons.clear();
        self.thumb_bitmaps.clear();
    }

    fn request(&mut self, key: Key) {
        let Some(s) = &self.shown else { return };
        if self.pending.contains(&key) {
            return;
        }
        if let Some(t) = self.failed.get(&key) {
            if t.elapsed().as_secs() < RETRY_S {
                return;
            }
            self.failed.remove(&key);
        }
        self.pending.insert(key);
        tiles::ask(s.id, key, &s.tileset, &s.style);
    }

    unsafe fn paint(&mut self) {
        let Some((rt, brush)) = self.target() else { return };
        let (w, h) = self.size();
        rt.BeginDraw();
        rt.SetTransform(&IDENTITY);
        rt.Clear(Some(&rgba(self.background)));
        let drawn = self.draw_tiles(&rt);
        self.draw_labels(&rt, &brush, &drawn, w, h);
        self.draw_markers(&rt, &brush, w, h);
        self.draw_bubble(&rt, &brush, w, h);
        self.draw_controls(&rt, &brush, w, h);
        if let Err(e) = rt.EndDraw(None, None) {
            if e.code() == D2DERR_RECREATE_TARGET {
                self.drop_target();
                self.invalidate();
            }
        }
    }

    /// The tiles on screen (a tile not here yet: a coarser one enlarged, or
    /// finer ones, until it comes). Those drawn, for their labels.
    unsafe fn draw_tiles(&mut self, rt: &ID2D1HwndRenderTarget) -> Vec<(D2D_RECT_F, Arc<Art>)> {
        let mut drawn = Vec::new();
        let Some(id) = self.shown.as_ref().map(|s| s.id) else { return drawn };
        self.uses += 1;
        let (w, h) = self.size();
        let tz = self.tile_zoom();
        let tile = SIZE as f64 * 2f64.powf(self.zoom - tz as f64);
        let world = self.world();
        let (left, top) = (self.mx * world - w as f64 / 2.0, self.my * world - h as f64 / 2.0);
        let n = 1i64 << tz;
        let (x0, x1) = ((left / tile).floor() as i64, ((left + w as f64) / tile).floor() as i64);
        let (y0, y1) = ((top / tile).floor() as i64, ((top + h as f64) / tile).floor() as i64);
        let mut wanted = HashSet::new();
        let mut budget = RENDER_PER_FRAME;
        let mut more = false;
        let uses = self.uses;
        let background = self.background;
        for ty in y0.max(0)..=y1.min(n - 1) {
            for tx in x0..=x1 {
                let key = (tz, tx.rem_euclid(n) as u32, ty as u32);
                let (l, t) = (tx as f64 * tile - left, ty as f64 * tile - top);
                let r = rect(self.snap(l), self.snap(t), self.snap(l + tile), self.snap(t + tile));
                let here = self.tiles.get_mut(&key).map(|e| {
                    e.used = uses;
                    if let Some(art) = &e.art {
                        if e.bitmap.is_none() && budget > 0 {
                            budget -= 1;
                            e.bitmap = render(rt, art, background);
                        }
                    }
                    (e.art.clone(), e.bitmap.clone())
                });
                match here {
                    Some((Some(art), Some(bitmap))) => {
                        rt.DrawBitmap(&bitmap, Some(&r), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
                        drawn.push((r, art));
                    }
                    // None there: the background.
                    Some((None, _)) => {}
                    Some((Some(_), None)) => {
                        more = true;
                        self.draw_stand_in(rt, key, r);
                    }
                    None => {
                        wanted.insert(key);
                        self.request(key);
                        self.draw_stand_in(rt, key, r);
                    }
                }
            }
        }
        for k in tiles::keep_only(id, &wanted) {
            self.pending.remove(&k);
        }
        if more {
            self.invalidate();
        }
        drawn
    }

    /// Until a tile comes: its nearest coarser tile here, enlarged, and any
    /// of its finer ones (zooming out).
    unsafe fn draw_stand_in(&self, rt: &ID2D1HwndRenderTarget, (z, x, y): Key, r: D2D_RECT_F) {
        for up in 1..=6u32.min(z) {
            let Some(b) = self.tiles.get(&(z - up, x >> up, y >> up)).and_then(|e| e.bitmap.as_ref()) else { continue };
            let part = SIZE / (1u32 << up) as f32;
            let (sx, sy) = ((x - ((x >> up) << up)) as f32 * part, (y - ((y >> up) << up)) as f32 * part);
            let src = rect(sx, sy, sx + part, sy + part);
            rt.DrawBitmap(b, Some(&r), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, Some(&src));
            break;
        }
        let (hw, hh) = ((r.right - r.left) / 2.0, (r.bottom - r.top) / 2.0);
        for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            if let Some(b) = self.tiles.get(&(z + 1, x * 2 + dx, y * 2 + dy)).and_then(|e| e.bitmap.as_ref()) {
                let (l, t) = (r.left + dx as f32 * hw, r.top + dy as f32 * hh);
                rt.DrawBitmap(b, Some(&rect(l, t, l + hw, t + hh)), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
            }
        }
    }

    /// The tiles' labels, none over another (the first placed wins).
    unsafe fn draw_labels(&self, rt: &ID2D1HwndRenderTarget, brush: &ID2D1SolidColorBrush, drawn: &[(D2D_RECT_F, Arc<Art>)], w: f32, h: f32) {
        let mut taken: Vec<D2D_RECT_F> = Vec::new();
        for (r, art) in drawn {
            let size = r.right - r.left;
            for l in &art.labels {
                let (ax, ay) = (r.left + l.x * size, r.top + l.y * size);
                if ax < -200.0 || ay < -50.0 || ax > w + 200.0 || ay > h + 50.0 {
                    continue;
                }
                let (s, c) = l.angle.to_radians().sin_cos();
                // Its box, upright around the anchor (turned: the box of that).
                let bw = c.abs() * l.width + s.abs() * l.height;
                let bh = s.abs() * l.width + c.abs() * l.height;
                let b = rect(ax - bw / 2.0 - 2.0, ay - bh / 2.0 - 2.0, ax + bw / 2.0 + 2.0, ay + bh / 2.0 + 2.0);
                if taken.iter().any(|t| t.left < b.right && b.left < t.right && t.top < b.bottom && b.top < t.bottom) {
                    continue;
                }
                taken.push(b);
                if l.angle.abs() > 0.5 {
                    rt.SetTransform(&rotation(l.angle, ax, ay));
                }
                let origin = D2D_POINT_2F { x: ax - l.width / 2.0, y: ay - l.height / 2.0 };
                if l.halo_width > 0.0 && l.halo >> 24 != 0 {
                    brush.SetColor(&rgba(l.halo));
                    let hr = l.halo_width.clamp(1.0, 2.0);
                    for i in 0..8 {
                        let (s, c) = (i as f32 * std::f32::consts::FRAC_PI_4).sin_cos();
                        rt.DrawTextLayout(D2D_POINT_2F { x: origin.x + c * hr, y: origin.y + s * hr }, &l.layout, brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
                    }
                }
                brush.SetColor(&rgba(l.color));
                rt.DrawTextLayout(origin, &l.layout, brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
                rt.SetTransform(&IDENTITY);
            }
        }
    }

    /// A category's icon (made once, kept), `size` DIPs.
    unsafe fn icon(&mut self, rt: &ID2D1HwndRenderTarget, category: &str) -> Option<ID2D1Bitmap> {
        if let Some(i) = self.icons.get(category) {
            return i.clone();
        }
        let px = (17.0 * self.dpi / 96.0).round() as u32;
        let made = icon_pixels(category, px).and_then(|pixels| bitmap(rt, px, px, &pixels, self.dpi));
        self.icons.insert(category.to_string(), made.clone());
        made
    }

    unsafe fn draw_markers(&mut self, rt: &ID2D1HwndRenderTarget, brush: &ID2D1SolidColorBrush, w: f32, h: f32) {
        // Grouped by cells of the whole zoom below, so groups don't change
        // while gliding between zooms or moving.
        let cells = 256.0 * 2f64.powf(self.zoom.floor()) / CELL;
        let whole = self.zoom.round() as i64;
        let mut index: HashMap<(Kind, i64, i64), usize> = HashMap::new();
        let mut groups: Vec<(Kind, Vec<usize>)> = Vec::new();
        for (i, m) in self.markers.iter().enumerate() {
            if m.place.minzoom.is_some_and(|z| whole < z) {
                continue;
            }
            let (sx, sy) = self.screen(m.x, m.y);
            if sx < -60.0 || sy < -60.0 || sx > w + 60.0 || sy > h + 60.0 {
                continue;
            }
            let key = (m.place.kind, (m.x * cells).floor() as i64, (m.y * cells).floor() as i64);
            match index.get(&key) {
                Some(&g) => groups[g].1.push(i),
                None => {
                    index.insert(key, groups.len());
                    groups.push((m.place.kind, vec![i]));
                }
            }
        }
        // Each where its places are on average, then pushed apart a few times.
        let mut at: Vec<(f32, f32)> = groups
            .iter()
            .map(|(_, members)| {
                let (mut sx, mut sy) = (0.0, 0.0);
                for &i in members {
                    let (x, y) = self.screen(self.markers[i].x, self.markers[i].y);
                    sx += x;
                    sy += y;
                }
                (sx / members.len() as f32, sy / members.len() as f32)
            })
            .collect();
        if at.len() < 600 {
            for _ in 0..8 {
                let mut moved = false;
                for i in 0..at.len() {
                    for j in i + 1..at.len() {
                        let (mut dx, mut dy) = (at[j].0 - at[i].0, at[j].1 - at[i].1);
                        let mut len = (dx * dx + dy * dy).sqrt();
                        if len >= APART {
                            continue;
                        }
                        if len < 0.5 {
                            let a = std::f32::consts::TAU * (j % 6) as f32 / 6.0;
                            (dx, dy, len) = (a.cos(), a.sin(), 1.0);
                        }
                        let push = (APART - len) / 2.0 / len;
                        at[i].0 -= dx * push;
                        at[i].1 -= dy * push;
                        at[j].0 += dx * push;
                        at[j].1 += dy * push;
                        moved = true;
                    }
                }
                if !moved {
                    break;
                }
            }
        }
        self.hits.clear();
        for ((kind, members), (x, y)) in groups.into_iter().zip(at) {
            let first = &self.markers[members[0]].place;
            let (mut color, sign) = kind_look(kind);
            let icon = if has_icon(first) {
                let detail = first.detail.clone();
                color = 0xff00_0000 | icons::group_rgb(icons::style_of(&detail).1);
                self.icon(rt, &detail)
            } else {
                None
            };
            brush.SetColor(&rgba(0x40_000000));
            rt.FillEllipse(&ellipse(x, y + 1.5, RADIUS + 1.5), brush);
            brush.SetColor(&rgba(0xffff_ffff));
            rt.FillEllipse(&ellipse(x, y, RADIUS + 1.5), brush);
            brush.SetColor(&rgba(color));
            rt.FillEllipse(&ellipse(x, y, RADIUS), brush);
            match icon {
                Some(b) => {
                    let s = 17.0 / 2.0;
                    rt.DrawBitmap(&b, Some(&rect(x - s, y - s, x + s, y + s)), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
                }
                None => {
                    brush.SetColor(&rgba(0xffff_ffff));
                    text(rt, sign, &self.text.sign, rect(x - RADIUS, y - RADIUS - 1.0, x + RADIUS, y + RADIUS - 1.0), brush);
                }
            }
            if members.len() > 1 {
                let n = members.len();
                let count = if n > 9999 { format!("{}k", n / 1000) } else { n.to_string() };
                let cw = 8.0 + 6.0 * count.len() as f32;
                let badge = rect(x + RADIUS - cw / 2.0, y - RADIUS - 6.0, x + RADIUS + cw / 2.0, y - RADIUS + 8.0);
                brush.SetColor(&rgba(0xe0_282828));
                rt.FillRoundedRectangle(&round_rect(badge, 7.0), brush);
                brush.SetColor(&rgba(0xffff_ffff));
                text(rt, &count, &self.text.count, badge, brush);
            }
            self.hits.push(Hit { x, y, r: RADIUS + 3.0, members });
        }
    }

    unsafe fn draw_bubble(&mut self, rt: &ID2D1HwndRenderTarget, brush: &ID2D1SolidColorBrush, w: f32, h: f32) {
        let Some(b) = &self.bubble else { return };
        let p = b.place.clone();
        let here = b.here;
        let (mx, my) = merc(p.lat, p.lon);
        let (px, py) = self.screen(mx, my);
        if px < -20.0 || py < -20.0 || px > w + 20.0 || py > h + 20.0 {
            return;
        }
        // A photo's thumbnail, once it's come (made a bitmap here once).
        let key = work::thumb_key(&p);
        if !self.thumb_bitmaps.contains_key(&key) {
            if let Some(img) = self.thumbs.get(&key) {
                if let Some(bm) = bitmap(rt, img.width, img.height, &img.pixels, 96.0) {
                    self.thumb_bitmaps.insert(key.clone(), bm);
                }
            }
        }
        let thumb = self.thumb_bitmaps.get(&key).cloned();
        let pad = 12.0;
        let thumb_w = if thumb.is_some() { 96.0 } else { 0.0 };
        let text_w = 250.0;
        let card_w = pad * 2.0 + text_w + if thumb.is_some() { thumb_w + pad } else { 0.0 };
        let title = if p.label.is_empty() { p.title.clone() } else { format!("{} ({})", p.title, p.label) };
        let mut lines = vec![kind_word(&p)];
        if !p.when.is_empty() {
            lines[0] = format!("{} · {}", lines[0], p.when.replace('T', " ").chars().take(16).collect::<String>());
        }
        lines.push(format!("{:.5}, {:.5}", p.lat, p.lon));
        // An address found has no library.
        if !p.ns.is_empty() {
            let mut library = format!("In {}", p.ns);
            if here > 1 {
                library = format!("{library} \u{b7} {here} here");
            }
            lines.push(library);
        }
        let (title_layout, _, title_h) = measure(&title, &self.text.title, text_w - 20.0);
        let (body_layout, _, body_h) = measure(&lines.join("\n"), &self.text.body, text_w);
        let text_h = title_h + 4.0 + body_h;
        let card_h = pad * 2.0 + text_h.max(if thumb.is_some() { thumb_w } else { 0.0 });
        let x = (px - card_w / 2.0).clamp(6.0, (w - card_w - 6.0).max(6.0));
        let above = py - card_h - 18.0 >= 4.0;
        let y = if above { py - card_h - 18.0 } else { py + 18.0 };
        let card = rect(x, y, x + card_w, y + card_h);
        // The pointer from the card to its place.
        let tip_x = px.clamp(x + 14.0, x + card_w - 14.0);
        if let Ok(g) = factories().d2d.CreatePathGeometry() {
            if let Ok(sink) = g.Open() {
                let (base, tip) = if above { (card.bottom - 1.0, py - 4.0) } else { (card.top + 1.0, py + 4.0) };
                sink.BeginFigure(D2D_POINT_2F { x: tip_x - 9.0, y: base }, D2D1_FIGURE_BEGIN_FILLED);
                sink.AddLines(&[D2D_POINT_2F { x: tip_x + 9.0, y: base }, D2D_POINT_2F { x: px, y: tip }]);
                sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                let _ = sink.Close();
                brush.SetColor(&rgba(0x30_000000));
                rt.FillRoundedRectangle(&round_rect(rect(card.left, card.top + 2.0, card.right, card.bottom + 3.0), 9.0), brush);
                brush.SetColor(&rgba(0xffff_ffff));
                rt.FillGeometry(&g, brush, None::<&ID2D1Brush>);
            }
        }
        brush.SetColor(&rgba(0xffff_ffff));
        rt.FillRoundedRectangle(&round_rect(card, 8.0), brush);
        brush.SetColor(&rgba(0x40_000000));
        rt.DrawRoundedRectangle(&round_rect(card, 8.0), brush, 1.0, None::<&ID2D1StrokeStyle>);
        let mut tx = x + pad;
        if let Some(t) = &thumb {
            let s = t.GetSize();
            let k = (thumb_w / s.width.max(1.0)).min(thumb_w / s.height.max(1.0));
            let (tw, th) = (s.width * k, s.height * k);
            let (l, tp) = (x + pad + (thumb_w - tw) / 2.0, y + pad + (thumb_w - th) / 2.0);
            rt.DrawBitmap(t, Some(&rect(l, tp, l + tw, tp + th)), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
            tx += thumb_w + pad;
        }
        if let Some(l) = title_layout {
            brush.SetColor(&rgba(0xff20_2020));
            rt.DrawTextLayout(D2D_POINT_2F { x: tx, y: y + pad }, &l, brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
        }
        if let Some(l) = body_layout {
            brush.SetColor(&rgba(0xff60_6060));
            rt.DrawTextLayout(D2D_POINT_2F { x: tx, y: y + pad + title_h + 4.0 }, &l, brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
        }
        let close = rect(card.right - 26.0, card.top + 4.0, card.right - 4.0, card.top + 26.0);
        brush.SetColor(&rgba(0xff80_8080));
        text(rt, "\u{2715}", &self.text.sign, close, brush);
        if let Some(b) = self.bubble.as_mut() {
            b.card = card;
            b.close = close;
        }
    }

    fn button(&self, plus: bool) -> D2D_RECT_F {
        let (w, _) = self.size();
        let top = if plus { 12.0 } else { 48.0 };
        rect(w - 46.0, top, w - 12.0, top + 34.0)
    }

    /// The zoom buttons, the map's credit, what's still loading, and why
    /// there's nothing (if so).
    unsafe fn draw_controls(&self, rt: &ID2D1HwndRenderTarget, brush: &ID2D1SolidColorBrush, w: f32, h: f32) {
        for plus in [true, false] {
            let r = self.button(plus);
            brush.SetColor(&rgba(0xf0ff_ffff));
            rt.FillRoundedRectangle(&round_rect(r, 6.0), brush);
            brush.SetColor(&rgba(0x50_000000));
            rt.DrawRoundedRectangle(&round_rect(r, 6.0), brush, 1.0, None::<&ID2D1StrokeStyle>);
            brush.SetColor(&rgba(0xff40_4040));
            text(rt, if plus { "+" } else { "\u{2212}" }, &self.text.button, rect(r.left, r.top - 2.0, r.right, r.bottom - 2.0), brush);
        }
        if !self.credit.is_empty() {
            let credit = if self.credit.starts_with('\u{a9}') { self.credit.clone() } else { format!("\u{a9} {}", self.credit) };
            let (layout, tw, th) = measure(&credit, &self.text.small, w * 0.8);
            if let Some(l) = layout {
                let r = rect(w - tw - 12.0, h - th - 4.0, w, h);
                brush.SetColor(&rgba(0xc8ff_ffff));
                rt.FillRectangle(&r, brush);
                brush.SetColor(&rgba(0xff40_4040));
                rt.DrawTextLayout(D2D_POINT_2F { x: r.left + 6.0, y: r.top + 2.0 }, &l, brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
            }
        }
        if !self.pending.is_empty() {
            let (layout, tw, th) = measure(&format!("Loading map\u{2026} {}", self.pending.len()), &self.text.small, 300.0);
            if let Some(l) = layout {
                let r = rect(10.0, h - th - 18.0, 22.0 + tw, h - 10.0);
                brush.SetColor(&rgba(0xdcff_ffff));
                rt.FillRoundedRectangle(&round_rect(r, 6.0), brush);
                brush.SetColor(&rgba(0xff3c_3c3c));
                rt.DrawTextLayout(D2D_POINT_2F { x: r.left + 6.0, y: r.top + 4.0 }, &l, brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
            }
        }
        if !self.message.is_empty() {
            let bw = (w - 40.0).min(440.0);
            let (layout, _, th) = measure(&self.message, &self.text.message, bw - 32.0);
            if let Some(l) = layout {
                let r = rect((w - bw) / 2.0, (h - th) / 2.0 - 18.0, (w + bw) / 2.0, (h + th) / 2.0 + 18.0);
                brush.SetColor(&rgba(0xf4ff_ffff));
                rt.FillRoundedRectangle(&round_rect(r, 10.0), brush);
                brush.SetColor(&rgba(0x40_000000));
                rt.DrawRoundedRectangle(&round_rect(r, 10.0), brush, 1.0, None::<&ID2D1StrokeStyle>);
                brush.SetColor(&rgba(0xff30_3030));
                rt.DrawTextLayout(D2D_POINT_2F { x: r.left + 16.0, y: r.top + 18.0 }, &l, brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
            }
        }
    }

    fn hit(&self, x: f32, y: f32) -> Option<&Hit> {
        self.hits.iter().rev().find(|h| (h.x - x).powi(2) + (h.y - y).powi(2) <= h.r * h.r)
    }

    /// A click (not a drag): a button, the bubble, a marker, or the map.
    fn click(&mut self, x: f32, y: f32) {
        for plus in [true, false] {
            if inside(&self.button(plus), x, y) {
                self.zoom_middle(if plus { 1.0 } else { -1.0 });
                return;
            }
        }
        if let Some(b) = &self.bubble {
            if inside(&b.close, x, y) {
                self.close_bubble();
                return;
            }
            if inside(&b.card, x, y) {
                return;
            }
        }
        let Some(members) = self.hit(x, y).map(|h| h.members.clone()) else {
            // A click on the map itself closes the bubble.
            self.close_bubble();
            return;
        };
        // Places apart: zoomed in on; all at one spot: the first's bubble.
        let first = &self.markers[members[0]].place;
        let (mut w, mut s, mut e, mut n) = (first.lon, first.lat, first.lon, first.lat);
        let mut spread = false;
        for &i in &members {
            let p = &self.markers[i].place;
            spread |= (p.lat - first.lat).abs() > 1e-5 || (p.lon - first.lon).abs() > 1e-5;
            (w, s, e, n) = (w.min(p.lon), s.min(p.lat), e.max(p.lon), n.max(p.lat));
        }
        if members.len() > 1 && spread && self.zoom < MAX_ZOOM {
            let before = self.zoom;
            let (pw, ph) = ((e - w) * 0.2, (n - s) * 0.2);
            self.fit(w - pw, s - ph, e + pw, n + ph, MAX_ZOOM);
            if self.zoom <= before {
                self.zoom = (before + 1.0).min(MAX_ZOOM);
            }
            self.touched = true;
            return;
        }
        let place = first.clone();
        self.open_bubble(place, members.len());
    }

    fn key(&mut self, vk: u16) -> bool {
        match VIRTUAL_KEY(vk) {
            VK_ADD | VK_OEM_PLUS => self.zoom_middle(1.0),
            VK_SUBTRACT | VK_OEM_MINUS => self.zoom_middle(-1.0),
            VK_LEFT => self.pan(80.0, 0.0),
            VK_RIGHT => self.pan(-80.0, 0.0),
            VK_UP => self.pan(0.0, 80.0),
            VK_DOWN => self.pan(0.0, -80.0),
            VK_ESCAPE => self.close_bubble(),
            _ => return false,
        }
        if matches!(VIRTUAL_KEY(vk), VK_LEFT | VK_RIGHT | VK_UP | VK_DOWN) {
            self.touched = true;
            self.changed();
        }
        true
    }
}

/// A text in a box (centred if its format is).
unsafe fn text(rt: &ID2D1HwndRenderTarget, s: &str, format: &IDWriteTextFormat, r: D2D_RECT_F, brush: &ID2D1SolidColorBrush) {
    let wide = HSTRING::from(s);
    rt.DrawText(wide.as_wide(), format, &r, brush, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
}

/// A text laid out at most `max` wide: the layout, its width and height.
unsafe fn measure(s: &str, format: &IDWriteTextFormat, max: f32) -> (Option<IDWriteTextLayout>, f32, f32) {
    let wide = HSTRING::from(s);
    let Ok(l) = factories().dwrite.CreateTextLayout(wide.as_wide(), format, max.max(1.0), 2000.0) else { return (None, 0.0, 0.0) };
    let mut m = DWRITE_TEXT_METRICS::default();
    let _ = l.GetMetrics(&mut m);
    (Some(l), m.width, m.height)
}

/// Pixels (premultiplied BGRA) as a bitmap of `dpi`.
unsafe fn bitmap(rt: &ID2D1HwndRenderTarget, w: u32, h: u32, pixels: &[u8], dpi: f32) -> Option<ID2D1Bitmap> {
    let props = D2D1_BITMAP_PROPERTIES { pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED }, dpiX: dpi, dpiY: dpi };
    rt.CreateBitmap(D2D_SIZE_U { width: w, height: h }, Some(pixels.as_ptr() as *const _), w * 4, &props).ok()
}

/// A place's icon (Maki, white), `px` square, premultiplied BGRA.
fn icon_pixels(category: &str, px: u32) -> Option<Vec<u8>> {
    use resvg::{tiny_skia, usvg};
    let svg = icons::white_svg(icons::style_of(category).0)?;
    let tree = usvg::Tree::from_str(&svg, &usvg::Options::default()).ok()?;
    let mut pixmap = tiny_skia::Pixmap::new(px, px)?;
    let t = tree.size();
    let k = px as f32 / t.width().max(t.height());
    resvg::render(&tree, tiny_skia::Transform::from_scale(k, k), &mut pixmap.as_mut());
    // White: premultiplied, every channel is its coverage.
    Some(pixmap.data().chunks(4).flat_map(|p| [p[3]; 4]).collect())
}

/// A tile's art on a bitmap of its own (made once, drawn every frame).
unsafe fn render(rt: &ID2D1HwndRenderTarget, art: &Art, background: u32) -> Option<ID2D1Bitmap> {
    let target = rt.CreateCompatibleRenderTarget(Some(&D2D_SIZE_F { width: SIZE, height: SIZE }), None, None, D2D1_COMPATIBLE_RENDER_TARGET_OPTIONS_NONE).ok()?;
    let brush = target.CreateSolidColorBrush(&rgba(background), None).ok()?;
    target.BeginDraw();
    target.Clear(Some(&rgba(background)));
    for s in &art.shapes {
        match &s.paint {
            Paint::Fill { color, outline } => {
                brush.SetColor(&rgba(*color));
                target.FillGeometry(&s.geometry, &brush, None::<&ID2D1Brush>);
                if *outline != 0 {
                    brush.SetColor(&rgba(*outline));
                    target.DrawGeometry(&s.geometry, &brush, 1.0, None::<&ID2D1StrokeStyle>);
                }
            }
            Paint::Line { color, width, stroke } => {
                brush.SetColor(&rgba(*color));
                target.DrawGeometry(&s.geometry, &brush, *width, stroke);
            }
        }
    }
    target.EndDraw(None, None).ok()?;
    target.GetBitmap().ok()
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            BeginPaint(hwnd, &mut ps);
            with(|c| c.paint());
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_SIZE => {
            with(|c| c.resize());
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let _ = SetFocus(hwnd);
            SetCapture(hwnd);
            with(|c| {
                let (x, y) = c.dips(lparam);
                c.drag = Some(Drag { x, y, moved: false });
            });
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            with(|c| {
                let (x, y) = c.dips(lparam);
                match c.drag.as_mut() {
                    Some(d) => {
                        let (dx, dy) = (x - d.x, y - d.y);
                        // A few pixels' wobble is still a click.
                        if d.moved || dx.abs() + dy.abs() > 3.0 {
                            d.moved = true;
                            d.x = x;
                            d.y = y;
                            c.touched = true;
                            c.pan(dx, dy);
                        }
                    }
                    None => c.hover = c.hit(x, y).is_some(),
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let _ = ReleaseCapture();
            with(|c| {
                let (x, y) = c.dips(lparam);
                match c.drag.take() {
                    Some(d) if d.moved => c.changed(),
                    Some(_) => c.click(x, y),
                    None => {}
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONDBLCLK => {
            with(|c| {
                let (x, y) = c.dips(lparam);
                if c.hit(x, y).is_none() && !c.bubble.as_ref().is_some_and(|b| inside(&b.card, x, y)) {
                    c.zoom_by(1.0, x, y);
                }
            });
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((wparam.0 >> 16) & 0xffff) as i16 as f64 / 120.0;
            let mut pt = POINT { x: (lparam.0 & 0xffff) as i16 as i32, y: ((lparam.0 >> 16) & 0xffff) as i16 as i32 };
            let _ = ScreenToClient(hwnd, &mut pt);
            with(|c| {
                let k = 96.0 / c.dpi;
                c.zoom_by(delta, pt.x as f32 * k, pt.y as f32 * k);
            });
            LRESULT(0)
        }
        WM_KEYDOWN => {
            if with(|c| c.key(wparam.0 as u16)) == Some(true) {
                LRESULT(0)
            } else {
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
        }
        WM_GETDLGCODE => LRESULT((DLGC_WANTARROWS | DLGC_WANTCHARS) as isize),
        WM_SETCURSOR if (lparam.0 & 0xffff) as u32 == HTCLIENT => {
            let hand = with(|c| c.hover && c.drag.is_none()).unwrap_or(false);
            SetCursor(LoadCursorW(None, if hand { IDC_HAND } else { IDC_ARROW }).unwrap_or_default());
            LRESULT(1)
        }
        WM_TIMER => {
            match wparam.0 {
                T_ANIM => {
                    with(|c| c.tick());
                }
                // Tiles that couldn't be read are asked again: a redraw does it.
                T_RETRY => {
                    with(|c| {
                        if !c.failed.is_empty() {
                            c.invalidate();
                        }
                    });
                }
                _ => {}
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
