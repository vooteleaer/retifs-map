//! The window: on the left Announces (public libraries heard, each
//! joinable) and the libraries, each a checkbox (shown on the map or not)
//! with how many places it has; on the right the search field and the map.
//! The UI thread only draws: everything that can block runs in `work` and
//! comes back as `WM_EVENTS`.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use libretifs::HeardLibrary;
use retifs_map_core::vector::Style;
use retifs_map_core::{fold, Found, Kind, Map, Place};
use windows::core::{w, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::HiDpi::{GetDpiForWindow, SystemParametersInfoForDpi};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::canvas;
use crate::work::{self, Event, Job, Library, Search, WM_EVENTS};

const CLASS: PCWSTR = w!("retifs-map");
const ID_HEARD: i32 = 100;
const ID_JOIN: i32 = 101;
const ID_LIBS: i32 = 102;
const ID_SEARCH: i32 = 103;
const ID_RESULTS: i32 = 104;
const T_SEARCH: usize = 1;
const T_POLL: usize = 2;
/// Typing pauses this long before it's searched.
const SEARCH_MS: u32 = 300;
/// One's own places are fitted in at the start only from libraries with at
/// most this many (a map's places would show the whole country), as on Linux.
const FIT_MAX: usize = 1000;
/// Places held by area are asked for from this zoom (a cell is a zoom-12 tile).
const CELLS_FROM: f64 = 12.0;

struct Ui {
    hwnd: HWND,
    font: HFONT,
    bold: HFONT,
    heard_title: HWND,
    heard: HWND,
    join: HWND,
    libs_title: HWND,
    libs: HWND,
    status: HWND,
    search: HWND,
    results: HWND,
    canvas: HWND,
    /// The libraries here, as retifsd lists them, and what was read of each.
    order: Vec<String>,
    read: HashMap<String, Library>,
    loading: bool,
    loaded_once: bool,
    load_failed: Option<String>,
    fitted: bool,
    hidden: HashSet<String>,
    /// Libraries turned on, latest last: the map of the latest is drawn.
    map_order: Vec<String>,
    styles: HashMap<String, Arc<Style>>,
    styles_asked: HashSet<String>,
    cells_asked: HashSet<(String, String)>,
    cell_places: HashMap<(String, String), Vec<Place>>,
    /// Heard libraries the map can use (or can't read yet), and how many others.
    heard_list: Vec<HeardLibrary>,
    heard_other: usize,
    joining: bool,
    found: Vec<Found>,
    searches: u64,
    /// The rows as last filled (not filled again when nothing changed).
    lib_rows: Vec<(String, String, bool)>,
    heard_rows: Vec<(String, String)>,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
    /// The lists are being filled: their notifications aren't the user's.
    static FILLING: Cell<bool> = const { Cell::new(false) };
}

/// The window's state, if it isn't busy (a message sent while it's changing).
fn ui<R>(f: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    UI.with(|u| u.try_borrow_mut().ok().and_then(|mut u| u.as_mut().map(f)))
}

fn wide(s: &str) -> HSTRING {
    HSTRING::from(s)
}

unsafe fn scale(hwnd: HWND, v: i32) -> i32 {
    v * GetDpiForWindow(hwnd).max(96) as i32 / 96
}

unsafe fn set_text(h: HWND, text: &str) {
    let _ = SetWindowTextW(h, &wide(text));
}

unsafe fn text_of(h: HWND) -> String {
    let len = GetWindowTextLengthW(h);
    let mut buf = vec![0u16; len as usize + 1];
    let n = GetWindowTextW(h, &mut buf);
    String::from_utf16_lossy(&buf[..n as usize])
}

/// "95,335": a count, its thousands set apart.
fn grouped(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// "map · 38822 places · 120 photos": what of a heard library the map shows.
fn holds(l: &HeardLibrary) -> String {
    let mut out = Vec::new();
    for (t, n) in &l.has {
        let word = match t.as_str() {
            retifs_map_core::TILESET_TYPE => Some("map".to_string()),
            retifs_map_core::VECTOR_TILE => Some(format!("{} map tiles", grouped(*n as usize))),
            retifs_map_core::PLACE_TYPE => Some(format!("{} places", grouped(*n as usize))),
            retifs_map_core::ADDRESS_TYPE => Some(format!("{} addresses", grouped(*n as usize))),
            t if t.starts_with("image/") => Some(format!("{n} photos")),
            t if libretifs::types::is(t, libretifs::types::PERSON) => Some(format!("{n} contacts")),
            t if libretifs::types::is(t, libretifs::types::EVENT) => Some(format!("{n} events")),
            _ => None,
        };
        out.extend(word);
    }
    out.join(" \u{b7} ")
}

fn ago(ms: u64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    match now.saturating_sub(ms) / 1000 {
        s if s < 60 => "just now".into(),
        s if s < 3600 => format!("{} min ago", s / 60),
        s => format!("{} h ago", s / 3600),
    }
}

/// Where the app writes what went wrong (a windowed program has no console).
fn log_line(text: &str) {
    use std::io::Write;
    let dir = std::path::PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default()).join("retifs").join("logs");
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("retifs-map.log")) {
        let _ = writeln!(f, "{text}");
    }
}

pub fn run() {
    std::panic::set_hook(Box::new(|info| log_line(&format!("panic: {info}"))));
    unsafe {
        let _ = InitCommonControlsEx(&INITCOMMONCONTROLSEX { dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32, dwICC: ICC_LISTVIEW_CLASSES | ICC_STANDARD_CLASSES });
        let instance = GetModuleHandleW(None).unwrap_or_default();
        let class = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH((COLOR_WINDOW.0 + 1) as _),
            hIcon: LoadIconW(None, IDI_APPLICATION).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&class);
        canvas::register();
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS,
            w!("Map"),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            1180,
            780,
            None,
            None,
            instance,
            None,
        ) else {
            return;
        };
        work::MAIN.store(hwnd.0 as isize, Ordering::SeqCst);
        // Sized for its screen.
        let _ = SetWindowPos(hwnd, None, 0, 0, scale(hwnd, 1180), scale(hwnd, 780), SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
        build(hwnd);
        crate::tiles::start();
        work::start();
        work::ask(Job::Load);
        work::ask(Job::Poll);
        SetTimer(hwnd, T_POLL, 5000, None);
        let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if before_dispatch(&msg) {
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Keys in the search field move in its results; a click elsewhere closes them.
unsafe fn before_dispatch(msg: &MSG) -> bool {
    if msg.message == WM_LBUTTONDOWN {
        ui(|u| {
            if msg.hwnd != u.results && msg.hwnd != u.search {
                hide_results(u);
            }
        });
        return false;
    }
    if msg.message != WM_KEYDOWN {
        return false;
    }
    ui(|u| {
        if msg.hwnd != u.search {
            return false;
        }
        let open = IsWindowVisible(u.results).as_bool();
        let count = u.found.len() as isize;
        let current = SendMessageW(u.results, LB_GETCURSEL, WPARAM(0), LPARAM(0)).0;
        match VIRTUAL_KEY(msg.wParam.0 as u16) {
            VK_DOWN | VK_UP if open && count > 0 => {
                let step = if VIRTUAL_KEY(msg.wParam.0 as u16) == VK_DOWN { 1 } else { -1 };
                let next = (current + step).clamp(0, count - 1);
                SendMessageW(u.results, LB_SETCURSEL, WPARAM(next as usize), LPARAM(0));
                true
            }
            VK_RETURN => {
                if open && count > 0 {
                    pick(u, current.max(0) as usize);
                } else {
                    let _ = KillTimer(u.hwnd, T_SEARCH);
                    search_now(u, true);
                }
                true
            }
            VK_ESCAPE if open => {
                hide_results(u);
                true
            }
            _ => false,
        }
    })
    .unwrap_or(false)
}

unsafe fn child(parent: HWND, class: PCWSTR, text: &str, style: WINDOW_STYLE, ex: WINDOW_EX_STYLE, id: i32) -> HWND {
    CreateWindowExW(ex, class, &wide(text), WS_CHILD | WS_VISIBLE | style, 0, 0, 10, 10, parent, HMENU(id as _), GetModuleHandleW(None).unwrap_or_default(), None).unwrap_or_default()
}

/// The system's message font at this DPI, and its bold.
unsafe fn fonts(dpi: u32) -> (HFONT, HFONT) {
    let mut m = NONCLIENTMETRICSW { cbSize: std::mem::size_of::<NONCLIENTMETRICSW>() as u32, ..Default::default() };
    let _ = SystemParametersInfoForDpi(SPI_GETNONCLIENTMETRICS.0, m.cbSize, Some(&mut m as *mut _ as *mut _), 0, dpi);
    let font = CreateFontIndirectW(&m.lfMessageFont);
    let mut b = m.lfMessageFont;
    b.lfWeight = 600;
    (font, CreateFontIndirectW(&b))
}

unsafe fn list(parent: HWND, id: i32, checkboxes: bool) -> HWND {
    let h = child(parent, WC_LISTVIEWW, "", WS_TABSTOP | WINDOW_STYLE((LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS | LVS_NOCOLUMNHEADER) as u32), WS_EX_CLIENTEDGE, id);
    let mut ex = LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER;
    if checkboxes {
        ex |= LVS_EX_CHECKBOXES;
    }
    SendMessageW(h, LVM_SETEXTENDEDLISTVIEWSTYLE, WPARAM(0), LPARAM(ex as isize));
    for i in 0..2 {
        let col = LVCOLUMNW { mask: LVCF_WIDTH | LVCF_SUBITEM, cx: 100, iSubItem: i, ..Default::default() };
        SendMessageW(h, LVM_INSERTCOLUMNW, WPARAM(i as usize), LPARAM(&col as *const _ as isize));
    }
    h
}

unsafe fn build(hwnd: HWND) {
    let (font, bold) = fonts(GetDpiForWindow(hwnd));
    let heard_title = child(hwnd, WC_STATICW, "Announces", WINDOW_STYLE(0), WINDOW_EX_STYLE(0), 0);
    let heard = list(hwnd, ID_HEARD, false);
    let join = child(hwnd, WC_BUTTONW, "Join", WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32), WINDOW_EX_STYLE(0), ID_JOIN);
    let _ = EnableWindow(join, false);
    let libs_title = child(hwnd, WC_STATICW, "Libraries", WINDOW_STYLE(0), WINDOW_EX_STYLE(0), 0);
    let libs = list(hwnd, ID_LIBS, true);
    let status = child(hwnd, WC_STATICW, "Asking retifs\u{2026}", WINDOW_STYLE(0x80) /* SS_NOPREFIX */, WINDOW_EX_STYLE(0), 0);
    let search = child(hwnd, WC_EDITW, "", WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32), WS_EX_CLIENTEDGE, ID_SEARCH);
    let cue = wide("Search places and addresses");
    SendMessageW(search, EM_SETCUEBANNER, WPARAM(1), LPARAM(cue.as_ptr() as isize));
    let results = child(hwnd, WC_LISTBOXW, "", WS_BORDER | WS_VSCROLL | WINDOW_STYLE((LBS_NOTIFY | LBS_NOINTEGRALHEIGHT) as u32), WINDOW_EX_STYLE(0), ID_RESULTS);
    let _ = ShowWindow(results, SW_HIDE);
    let canvas = canvas::create(hwnd);
    UI.with(|u| {
        *u.borrow_mut() = Some(Ui {
            hwnd,
            font,
            bold,
            heard_title,
            heard,
            join,
            libs_title,
            libs,
            status,
            search,
            results,
            canvas,
            order: Vec::new(),
            read: HashMap::new(),
            loading: false,
            loaded_once: false,
            load_failed: None,
            fitted: false,
            hidden: HashSet::new(),
            map_order: Vec::new(),
            styles: HashMap::new(),
            styles_asked: HashSet::new(),
            cells_asked: HashSet::new(),
            cell_places: HashMap::new(),
            heard_list: Vec::new(),
            heard_other: 0,
            joining: false,
            found: Vec::new(),
            searches: 0,
            lib_rows: Vec::new(),
            heard_rows: Vec::new(),
        })
    });
    set_fonts();
    layout(hwnd);
    ui(|u| fill_heard(u));
}

/// Every control's font (again after a DPI change).
unsafe fn set_fonts() {
    let Some((controls, titles, font, bold)) = ui(|u| (vec![u.heard, u.join, u.libs, u.status, u.search, u.results], vec![u.heard_title, u.libs_title], u.font, u.bold)) else { return };
    for h in controls {
        SendMessageW(h, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
    }
    for h in titles {
        SendMessageW(h, WM_SETFONT, WPARAM(bold.0 as usize), LPARAM(1));
    }
}

/// The sidebar's width and the search strip's height.
unsafe fn frame(hwnd: HWND) -> (i32, i32) {
    (scale(hwnd, 300), scale(hwnd, 50))
}

unsafe fn layout(hwnd: HWND) {
    // Moving windows sends messages back here: no borrow held meanwhile.
    let Some((heard_title, heard, join, libs_title, libs, status, search, results, canvas, font)) = ui(|u| (u.heard_title, u.heard, u.join, u.libs_title, u.libs, u.status, u.search, u.results, u.canvas, u.font)) else { return };
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let s = |v| scale(hwnd, v);
    let (w, h) = (rc.right, rc.bottom);
    let (side, strip) = frame(hwnd);
    let m = s(12);
    let inner = side - 2 * m;
    let place = |c: HWND, x: i32, y: i32, cw: i32, ch: i32| {
        let _ = MoveWindow(c, x, y, cw.max(0), ch.max(0), true);
    };
    let mut y = m;
    place(heard_title, m, y, inner, s(20));
    y += s(24);
    place(heard, m, y, inner, s(150));
    y += s(150) + s(6);
    place(join, side - m - s(100), y, s(100), s(28));
    y += s(28) + s(14);
    place(libs_title, m, y, inner, s(20));
    y += s(24);
    let status_h = s(56);
    place(libs, m, y, inner, h - y - status_h - s(8));
    place(status, m, h - status_h, inner, status_h - s(4));
    // The search field as high as its font needs (an edit's text sits at its top).
    let dc = GetDC(hwnd);
    let old = SelectObject(dc, font);
    let mut tm = TEXTMETRICW::default();
    let _ = GetTextMetricsW(dc, &mut tm);
    SelectObject(dc, old);
    ReleaseDC(hwnd, dc);
    let eh = tm.tmHeight + s(10);
    let sw = (w - side - 2 * m).min(s(480));
    place(search, side + m, (strip - eh) / 2, sw, eh);
    place(canvas, side, strip, w - side, h - strip);
    if IsWindowVisible(results).as_bool() {
        let mut r = RECT::default();
        let _ = GetWindowRect(results, &mut r);
        place(results, side + m, (strip - eh) / 2 + eh + 1, sw, r.bottom - r.top);
    }
    // The first column takes what the count's column and a scroll bar leave.
    for (list, second) in [(heard, s(76)), (libs, s(112))] {
        SendMessageW(list, LVM_SETCOLUMNWIDTH, WPARAM(1), LPARAM(second as isize));
        SendMessageW(list, LVM_SETCOLUMNWIDTH, WPARAM(0), LPARAM((inner - second - s(24)) as isize));
    }
    let _ = InvalidateRect(hwnd, None, true);
}

unsafe fn status(u: &Ui, text: &str) {
    set_text(u.status, text);
}

/// The places shown: those of the libraries ticked, read whole or by area.
fn visible(u: &Ui) -> Vec<Place> {
    let mut out: Vec<Place> = Vec::new();
    for ns in u.order.iter().filter(|ns| !u.hidden.contains(*ns)) {
        if let Some(l) = u.read.get(ns) {
            out.extend(l.places.iter().cloned());
        }
    }
    for ((ns, _), places) in &u.cell_places {
        if !u.hidden.contains(ns) {
            out.extend(places.iter().cloned());
        }
    }
    out
}

/// Everything shown again: the places, the libraries' list, the map.
unsafe fn refresh(u: &mut Ui) {
    let places = visible(u);
    canvas::with(|c| c.set_places(places));
    fill_libraries(u);
    choose_map(u);
}

/// A row per library with something the map shows: a map, places, or
/// places held by area (a map's cafés). A gazetteer is only searched.
unsafe fn fill_libraries(u: &mut Ui) {
    let mut cells: HashMap<&str, usize> = HashMap::new();
    for ((ns, _), places) in &u.cell_places {
        *cells.entry(ns.as_str()).or_default() += places.len();
    }
    let rows: Vec<(String, String, bool)> = u
        .order
        .iter()
        .filter_map(|ns| u.read.get(ns))
        .filter(|l| !l.address && (!l.tilesets.is_empty() || !l.places.is_empty() || l.by_cell))
        .map(|l| {
            let mut what = Vec::new();
            if !l.tilesets.is_empty() {
                what.push("map".to_string());
            }
            let n = l.places.len() + cells.get(l.ns.as_str()).copied().unwrap_or(0);
            match (n, l.by_cell) {
                (0, true) => what.push("places by area".into()),
                (1, _) => what.push("1 place".into()),
                (0, false) => {}
                (n, _) => what.push(format!("{} places", grouped(n))),
            }
            (l.ns.clone(), what.join(" \u{b7} "), !u.hidden.contains(&l.ns))
        })
        .collect();
    if rows == u.lib_rows && !rows.is_empty() {
        return;
    }
    FILLING.set(true);
    SendMessageW(u.libs, LVM_DELETEALLITEMS, WPARAM(0), LPARAM(0));
    for (i, (ns, what, shown)) in rows.iter().enumerate() {
        set_row(u.libs, i, ns, what, Some(*shown));
    }
    if rows.is_empty() {
        let text = if u.loaded_once { "None with places yet" } else { "Reading\u{2026}" };
        set_row(u.libs, 0, text, "", None);
    }
    FILLING.set(false);
    u.lib_rows = rows;
}

/// A list's row `i`: its text, the second column's, and its checkbox
/// (None: none shown).
unsafe fn set_row(list: HWND, i: usize, first: &str, second: &str, check: Option<bool>) {
    for (col, text) in [first, second].iter().enumerate() {
        let t = wide(text);
        let item = LVITEMW { mask: LVIF_TEXT, iItem: i as i32, iSubItem: col as i32, pszText: PWSTR(t.as_ptr() as *mut u16), ..Default::default() };
        let msg = if col == 0 { LVM_INSERTITEMW } else { LVM_SETITEMTEXTW };
        SendMessageW(list, msg, WPARAM(i), LPARAM(&item as *const _ as isize));
    }
    let image = match check {
        Some(true) => 2,
        Some(false) => 1,
        None => 0,
    };
    let state = LVITEMW { stateMask: LVIS_STATEIMAGEMASK, state: LIST_VIEW_ITEM_STATE_FLAGS(image << 12), ..Default::default() };
    SendMessageW(list, LVM_SETITEMSTATE, WPARAM(i), LPARAM(&state as *const _ as isize));
}

/// Draws the map of the library turned on last that has one, else the
/// first listed (none: a blank map); says why there's none.
unsafe fn choose_map(u: &mut Ui) {
    let tilesets: Vec<retifs_map_core::Tileset> = u.order.iter().filter_map(|ns| u.read.get(ns)).flat_map(|l| l.tilesets.iter().cloned()).collect();
    let prefer = u.map_order.iter().rev().chain(u.order.iter());
    let pick = prefer.filter(|ns| !u.hidden.contains(*ns)).find_map(|ns| tilesets.iter().find(|t| t.ns == *ns && !t.overlay)).cloned();
    let current = canvas::with(|c| c.shown_map()).flatten();
    match &pick {
        Some(t) if current.as_deref() != Some(t.id.as_str()) => match u.styles.get(&t.id) {
            Some(style) => {
                let style = style.clone();
                canvas::with(|c| c.set_map(Some((t.clone(), style))));
                fit_first(u);
            }
            None => {
                if u.styles_asked.insert(t.id.clone()) {
                    work::ask(Job::Style(t.clone()));
                }
            }
        },
        Some(_) => {}
        None if current.is_some() => {
            canvas::with(|c| c.set_map(None));
        }
        None => {}
    }
    let message = if let Some(e) = &u.load_failed {
        format!("Can't reach retifs: is retifsd running?\n\n{e}\n\nThe map asks again every few seconds.")
    } else if u.loaded_once && tilesets.is_empty() {
        "No map library here yet.\n\nJoin one under Announces, on the left: the map comes from a retifs library, like everything else here.".to_string()
    } else if u.loaded_once && pick.is_none() {
        "The map libraries are hidden: tick one on the left to see the map.".to_string()
    } else {
        String::new()
    };
    canvas::with(|c| c.set_message(&message));
}

/// The first time there's something to show: one's own places, else the
/// map's area (RETIFS_MAP_VIEW=lat,lon,zoom: where to open instead).
unsafe fn fit_first(u: &mut Ui) {
    if u.fitted || !u.loaded_once || canvas::with(|c| c.touched).unwrap_or(true) {
        return;
    }
    let view: Option<Vec<f64>> = std::env::var("RETIFS_MAP_VIEW").ok().map(|v| v.split(',').filter_map(|n| n.trim().parse().ok()).collect());
    if let Some([lat, lon, z]) = view.as_deref() {
        canvas::with(|c| c.center_on(*lat, *lon, *z));
        u.fitted = true;
        return;
    }
    let places = visible(u);
    let mut per_lib: HashMap<&str, usize> = HashMap::new();
    for p in &places {
        *per_lib.entry(p.ns.as_str()).or_default() += 1;
    }
    let own: Vec<&Place> = places.iter().filter(|p| p.minzoom.is_none() && per_lib[p.ns.as_str()] <= FIT_MAX).collect();
    if !own.is_empty() {
        let (mut w, mut s, mut e, mut n) = (180.0f64, 90.0f64, -180.0f64, -90.0f64);
        for p in &own {
            (w, s, e, n) = (w.min(p.lon), s.min(p.lat), e.max(p.lon), n.max(p.lat));
        }
        let pad = ((e - w).max(n - s) * 0.05).max(0.002);
        canvas::with(|c| c.fit(w - pad, s - pad, e + pad, n + pad, 16.0));
        u.fitted = true;
        return;
    }
    let shown = canvas::with(|c| c.shown_map()).flatten();
    let bounds = u.read.values().flat_map(|l| l.tilesets.iter()).find(|t| Some(&t.id) == shown.as_ref()).and_then(|t| t.bounds);
    if let Some([w, s, e, n]) = bounds {
        canvas::with(|c| c.fit(w, s, e, n, 16.0));
        u.fitted = true;
    }
}

/// Lazily held libraries are asked for the places in the cells on screen
/// not asked for yet, once zoomed in enough to show them.
unsafe fn ask_cells(u: &mut Ui) {
    let Some((z, (w, s, e, n))) = canvas::with(|c| (c.zoom(), c.area())) else { return };
    if z < CELLS_FROM {
        return;
    }
    // Landmarks in zoom-12 cells from zoom 12; the rest in zoom-14 cells from 14.
    let mut cells = Map::cells(w, s, e, n);
    if z >= 14.0 {
        cells.extend(Map::cells_at(14, w, s, e, n));
    }
    for ns in u.order.iter().filter(|ns| !u.hidden.contains(*ns) && u.read.get(*ns).is_some_and(|l| l.by_cell)) {
        let missing: Vec<String> = cells.iter().filter(|c| !u.cells_asked.contains(&(ns.clone(), (*c).clone()))).take(40).cloned().collect();
        if missing.is_empty() {
            continue;
        }
        for c in &missing {
            u.cells_asked.insert((ns.clone(), c.clone()));
        }
        work::ask(Job::Cells(ns.clone(), missing));
    }
}

/// Announces: the public libraries heard that the map can use, each with
/// what it holds and whether it's here already.
unsafe fn fill_heard(u: &mut Ui) {
    let mut rows: Vec<(String, String)> = u
        .heard_list
        .iter()
        .map(|l| {
            let name = if l.name.is_empty() { "A public library".to_string() } else { l.name.clone() };
            let what = holds(l);
            let first = if what.is_empty() { name } else { format!("{name} \u{2014} {what}") };
            let state = match (&l.here, l.readable) {
                (Some(_), _) => "joined",
                (None, false) => "newer retifs",
                (None, true) => "",
            };
            (first, state.to_string())
        })
        .collect();
    if rows.is_empty() {
        rows.push(("Nothing heard yet".into(), String::new()));
    }
    if rows != u.heard_rows {
        let selected = SendMessageW(u.heard, LVM_GETNEXTITEM, WPARAM(usize::MAX), LPARAM(LVNI_SELECTED as isize)).0;
        FILLING.set(true);
        SendMessageW(u.heard, LVM_DELETEALLITEMS, WPARAM(0), LPARAM(0));
        for (i, (first, second)) in rows.iter().enumerate() {
            set_row(u.heard, i, first, second, None);
        }
        if selected >= 0 && (selected as usize) < rows.len() && rows.len() == u.heard_rows.len() {
            let item = LVITEMW { stateMask: LVIS_SELECTED, state: LVIS_SELECTED, ..Default::default() };
            SendMessageW(u.heard, LVM_SETITEMSTATE, WPARAM(selected as usize), LPARAM(&item as *const _ as isize));
        }
        FILLING.set(false);
        u.heard_rows = rows;
    }
    update_join(u);
}

fn selected_heard(u: &Ui) -> Option<&HeardLibrary> {
    let i = unsafe { SendMessageW(u.heard, LVM_GETNEXTITEM, WPARAM(usize::MAX), LPARAM(LVNI_SELECTED as isize)).0 };
    (i >= 0).then(|| u.heard_list.get(i as usize)).flatten()
}

/// Join is for a heard library picked that isn't here yet.
unsafe fn update_join(u: &Ui) {
    let can = !u.joining && selected_heard(u).is_some_and(|l| l.readable && l.here.is_none());
    let _ = EnableWindow(u.join, can);
    set_text(u.join, if u.joining { "Joining\u{2026}" } else { "Join" });
}

/// A heard library picked: what it is, in the status line.
unsafe fn describe_heard(u: &Ui) {
    let Some(l) = selected_heard(u) else { return };
    let text = if !l.readable {
        format!("Needs a newer retifs (announce v{}, format {})", l.v, l.fmt)
    } else {
        let node = &l.node[..8.min(l.node.len())];
        let here = l.here.as_ref().map(|ns| format!("\nHere as {ns}")).unwrap_or_default();
        format!("{}\nFrom <{node}\u{2026}>, {} hop(s), {}{here}", l.about, l.hops, ago(l.heard_ms))
    };
    status(u, text.trim_start());
}

unsafe fn join(u: &mut Ui) {
    let Some(l) = selected_heard(u).filter(|l| l.readable && l.here.is_none()).cloned() else { return };
    u.joining = true;
    status(u, &format!("Joining {}\u{2026} only what you look at is fetched.", l.name));
    work::ask(Job::Join(l));
    update_join(u);
}

/// Searches what's typed (3 letters or more as it's typed; anything on Enter).
unsafe fn search_now(u: &mut Ui, asked: bool) {
    let text = text_of(u.search);
    let letters = fold(&text).concat().chars().count();
    if letters == 0 || (letters < 3 && !asked) {
        u.searches += 1;
        hide_results(u);
        return;
    }
    u.searches += 1;
    let libraries = u.order.iter().filter(|ns| !u.hidden.contains(*ns)).cloned().collect();
    work::search(Search { seq: u.searches, text, here: visible(u), libraries });
}

unsafe fn show_results(u: &mut Ui, found: Vec<Found>) {
    SendMessageW(u.results, LB_RESETCONTENT, WPARAM(0), LPARAM(0));
    let names: Vec<String> = if found.is_empty() { vec!["Nothing found in the libraries shown".into()] } else { found.iter().map(|f| f.name.clone()).collect() };
    for n in &names {
        let t = wide(n);
        SendMessageW(u.results, LB_ADDSTRING, WPARAM(0), LPARAM(t.as_ptr() as isize));
    }
    if !found.is_empty() {
        SendMessageW(u.results, LB_SETCURSEL, WPARAM(0), LPARAM(0));
    }
    u.found = found;
    let item = SendMessageW(u.results, LB_GETITEMHEIGHT, WPARAM(0), LPARAM(0)).0 as i32;
    let rows = names.len().min(10) as i32;
    let mut r = RECT::default();
    let _ = GetWindowRect(u.search, &mut r);
    let mut at = POINT { x: r.left, y: r.bottom };
    let _ = ScreenToClient(u.hwnd, &mut at);
    let _ = SetWindowPos(u.results, HWND_TOP, at.x, at.y + 1, r.right - r.left, rows * item + 4, SWP_SHOWWINDOW | SWP_NOACTIVATE);
}

unsafe fn hide_results(u: &mut Ui) {
    if IsWindowVisible(u.results).as_bool() {
        let _ = ShowWindow(u.results, SW_HIDE);
    }
}

/// A search result picked: the map goes there; a place's bubble opens, an
/// address gets one of its own.
unsafe fn pick(u: &mut Ui, i: usize) {
    let Some(f) = u.found.get(i).cloned() else { return };
    hide_results(u);
    let place = f.place.clone().unwrap_or_else(|| Place {
        ns: String::new(),
        id: String::new(),
        kind: Kind::Other,
        title: f.name.clone(),
        label: String::new(),
        lat: f.lat,
        lon: f.lon,
        when: String::new(),
        detail: "address".into(),
        minzoom: None,
        thumb: None,
        preview: None,
    });
    canvas::with(|c| {
        c.center_on(f.lat, f.lon, f.zoom);
        c.open_bubble(place, 1);
    });
    let _ = SetFocus(u.canvas);
}

/// What the threads found.
unsafe fn events() {
    for e in work::take() {
        match e {
            Event::Tile(d) => {
                canvas::with(|c| c.tile_done(d));
            }
            Event::Thumb { key, image } => {
                if let Some(i) = image {
                    canvas::with(|c| c.add_thumb(key, i));
                }
            }
            e => {
                ui(|u| event(u, e));
            }
        }
    }
}

unsafe fn event(u: &mut Ui, e: Event) {
    match e {
        Event::Libraries(Ok(list)) => {
            u.order = list;
            u.loading = true;
            u.load_failed = None;
            status(u, "Reading libraries\u{2026}");
        }
        Event::Libraries(Err(e)) => {
            u.loading = false;
            u.load_failed = Some(e.clone());
            status(u, "retifs isn't answering");
            choose_map(u);
        }
        Event::Library(l) => {
            let i = u.order.iter().position(|ns| *ns == l.ns).unwrap_or(0);
            status(u, &format!("Reading libraries\u{2026} {} of {}: {}", i + 1, u.order.len(), l.ns));
            u.read.insert(l.ns.clone(), l);
            refresh(u);
            ask_cells(u);
        }
        Event::LoadDone => {
            u.loading = false;
            u.loaded_once = true;
            let order = u.order.clone();
            u.read.retain(|ns, _| order.contains(ns));
            u.cell_places.retain(|(ns, _), _| order.contains(ns));
            // A style that couldn't be read is asked for again.
            let styles = u.styles.clone();
            u.styles_asked.retain(|id| styles.contains_key(id));
            refresh(u);
            fit_first(u);
            ask_cells(u);
            summary(u);
        }
        Event::Heard(Ok(list)) => {
            let (usable, other): (Vec<HeardLibrary>, Vec<HeardLibrary>) = list.into_iter().partition(|l| !l.readable || retifs_map_core::map_uses(l));
            u.heard_list = usable;
            u.heard_other = other.len();
            fill_heard(u);
        }
        Event::Heard(Err(_)) => {}
        Event::Joined(r) => {
            u.joining = false;
            match r {
                Ok(ns) => status(u, &format!("Joined {ns}: what you look at comes as you look at it.")),
                Err(e) => status(u, &format!("Not joined: {e}")),
            }
            update_join(u);
            work::ask(Job::Load);
            work::ask(Job::Poll);
        }
        Event::Changed => work::ask(Job::Load),
        Event::Cells { ns, found } => {
            for (cell, places) in found {
                u.cell_places.insert((ns.clone(), cell), places);
            }
            refresh(u);
        }
        Event::Style { id, style } => match style {
            Ok(s) => {
                u.styles.insert(id, Arc::new(s));
                choose_map(u);
            }
            Err(e) => status(u, &format!("The map's style couldn't be read: {e}")),
        },
        Event::Found { seq, found } => {
            if seq != u.searches {
                return; // typed on since
            }
            match found {
                Ok(f) => show_results(u, f),
                Err(e) => status(u, &format!("Search failed: {e}")),
            }
        }
        Event::Tile(_) | Event::Thumb { .. } => {}
    }
}

/// How many places are shown, in the status line.
unsafe fn summary(u: &Ui) {
    let n = visible(u).len();
    let shown = u.lib_rows.iter().filter(|r| r.2).count();
    let places = if n == 1 { "1 place".to_string() } else { format!("{} places", grouped(n)) };
    let maps = u.read.values().any(|l| !l.tilesets.is_empty());
    status(u, &format!("{places} on the map from {shown} of {} libraries{}", u.lib_rows.len(), if maps { "" } else { "; no map library" }));
}

/// A library's checkbox clicked: shown or hidden on the map.
unsafe fn toggled(u: &mut Ui, i: usize, shown: bool) {
    let Some(row) = u.lib_rows.get_mut(i) else { return };
    if row.2 == shown {
        return;
    }
    row.2 = shown;
    let ns = row.0.clone();
    if shown {
        u.hidden.remove(&ns);
        u.map_order.retain(|n| *n != ns);
        u.map_order.push(ns);
    } else {
        u.hidden.insert(ns);
    }
    refresh(u);
    ask_cells(u);
    summary(u);
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        // Labels on the window's own background; the status line dimmer.
        WM_CTLCOLORSTATIC => {
            let dc = HDC(wparam.0 as _);
            SetBkColor(dc, COLORREF(GetSysColor(COLOR_WINDOW)));
            if ui(|u| u.status.0 == lparam.0 as _).unwrap_or(false) {
                SetTextColor(dc, COLORREF(GetSysColor(COLOR_GRAYTEXT)));
            }
            LRESULT(GetSysColorBrush(COLOR_WINDOW).0 as isize)
        }
        WM_PAINT => {
            // The lines between the sidebar, the search strip and the map.
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let (side, strip) = frame(hwnd);
            let line = CreateSolidBrush(COLORREF(0x00d8_d8d8));
            FillRect(dc, &RECT { left: side - 1, top: 0, right: side, bottom: rc.bottom }, line);
            FillRect(dc, &RECT { left: side, top: strip - 1, right: rc.right, bottom: strip }, line);
            let _ = DeleteObject(line);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_SIZE => {
            layout(hwnd);
            LRESULT(0)
        }
        WM_DPICHANGED => {
            let r = &*(lparam.0 as *const RECT);
            let dpi = (wparam.0 & 0xffff) as u32;
            let (font, bold) = fonts(dpi);
            let old = ui(|u| (std::mem::replace(&mut u.font, font), std::mem::replace(&mut u.bold, bold)));
            set_fonts();
            if let Some((a, b)) = old {
                let _ = DeleteObject(a);
                let _ = DeleteObject(b);
            }
            let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
            layout(hwnd);
            canvas::with(|c| c.dpi_changed());
            LRESULT(0)
        }
        WM_GETMINMAXINFO => {
            let m = &mut *(lparam.0 as *mut MINMAXINFO);
            m.ptMinTrackSize = POINT { x: scale(hwnd, 760), y: scale(hwnd, 520) };
            LRESULT(0)
        }
        WM_EVENTS => {
            events();
            LRESULT(0)
        }
        canvas::WM_VIEW => {
            ui(|u| ask_cells(u));
            LRESULT(0)
        }
        WM_TIMER => {
            match wparam.0 {
                T_SEARCH => {
                    let _ = KillTimer(hwnd, T_SEARCH);
                    ui(|u| search_now(u, false));
                }
                // Announces keep coming; retifs may have started.
                T_POLL => {
                    ui(|u| {
                        if u.load_failed.is_some() {
                            work::ask(Job::Load);
                        } else if !u.loading {
                            work::ask(Job::Poll);
                        }
                    });
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_NOTIFY => {
            if FILLING.get() {
                return LRESULT(0);
            }
            let hdr = &*(lparam.0 as *const NMHDR);
            let id = hdr.idFrom as i32;
            if hdr.code == LVN_ITEMCHANGED {
                let nm = &*(lparam.0 as *const NMLISTVIEW);
                let (new, old) = (nm.uNewState, nm.uOldState);
                if id == ID_LIBS && (new ^ old) & LVIS_STATEIMAGEMASK.0 != 0 && new & LVIS_STATEIMAGEMASK.0 != 0 && nm.iItem >= 0 {
                    let shown = (new & LVIS_STATEIMAGEMASK.0) >> 12 == 2;
                    ui(|u| toggled(u, nm.iItem as usize, shown));
                }
                if id == ID_HEARD && (new ^ old) & LVIS_SELECTED.0 != 0 {
                    ui(|u| {
                        update_join(u);
                        describe_heard(u);
                    });
                }
            } else if id == ID_HEARD && hdr.code == NM_DBLCLK {
                ui(|u| join(u));
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let (id, code) = ((wparam.0 & 0xffff) as i32, ((wparam.0 >> 16) & 0xffff) as u32);
            match (id, code) {
                (ID_JOIN, _) => {
                    ui(|u| join(u));
                }
                (ID_SEARCH, EN_CHANGE) => {
                    SetTimer(hwnd, T_SEARCH, SEARCH_MS, None);
                }
                (ID_RESULTS, LBN_SELCHANGE) => {
                    ui(|u| {
                        let i = SendMessageW(u.results, LB_GETCURSEL, WPARAM(0), LPARAM(0)).0;
                        if i >= 0 {
                            pick(u, i as usize);
                        }
                    });
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
