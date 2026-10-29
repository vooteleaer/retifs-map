//! retifs Map for Linux: GTK 4 / libadwaita with libshumate's map over the
//! shared core. Every record with a place (ADR-0027): photos where they were
//! taken, contacts where they live, events where they happen, anything else
//! that says where it is. Contacts and events with an address but no
//! coordinates are placed with a click or an address search (off until the
//! user turns it on); new places are saved as records of their own. It
//! updates as the libraries change.

mod announces;
mod bubble;
mod icons;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use retifs_map_core::{Found, Kind, Map, Place, Tileset, Unplaced};
use shumate::prelude::*;

fn main() -> glib::ExitCode {
    let app = adw::Application::builder().application_id("org.retifs.Map").build();
    app.connect_activate(build);
    app.run()
}

fn bg<T: Send + 'static>(map: &Arc<Map>, work: impl FnOnce(&Map) -> T + Send + 'static, then: impl FnOnce(T) + 'static) {
    let map = map.clone();
    glib::spawn_future_local(async move {
        if let Ok(r) = gio::spawn_blocking(move || work(&map)).await {
            then(r);
        }
    });
}

/// The layers: what each kind is called and drawn with.
const KINDS: [(Kind, &str, &str); 4] = [
    (Kind::Photo, "Photos", "image-x-generic-symbolic"),
    (Kind::Person, "Contacts", "avatar-default-symbolic"),
    (Kind::Event, "Events", "x-office-calendar-symbolic"),
    (Kind::Other, "Saved places", "mark-location-symbolic"),
];

fn icon(kind: Kind) -> &'static str {
    KINDS.iter().find(|(k, _, _)| *k == kind).map(|(_, _, i)| *i).unwrap_or("mark-location-symbolic")
}

fn kind_name(kind: Kind) -> &'static str {
    KINDS.iter().find(|(k, _, _)| *k == kind).map(|(_, n, _)| *n).unwrap_or("")
}

/// At most this many records are listed as waiting for a place; the rest are counted.
const UNPLACED_SHOWN: usize = 100;
/// Markers closer than this (pixels) at the current zoom become one.
const CELL: f64 = 56.0;
/// Libraries with more places than this aren't what the first view fits.
const FIT_MAX: usize = 1000;

struct App {
    core: Arc<Map>,
    smap: shumate::SimpleMap,
    layer: shumate::MarkerLayer,
    unplaced_group: adw::PreferencesGroup,
    unplaced_row: adw::ExpanderRow,
    unplaced_rows: RefCell<Vec<gtk::Widget>>,
    banner: adw::Banner,
    card: adw::ActionRow,
    card_box: gtk::Box,
    places: RefCell<Vec<Place>>,
    unplaced: RefCell<Vec<Unplaced>>,
    placing: RefCell<Option<Unplaced>>,
    thumbs: RefCell<HashMap<String, gdk::Texture>>,
    tried: RefCell<HashSet<String>>,
    libraries: RefCell<Vec<String>>,
    counts: RefCell<HashMap<String, u64>>,
    status: gtk::Label,
    fitted: Cell<bool>,
    zoom: Cell<i32>,
    window: adw::ApplicationWindow,
    search: gtk::SearchEntry,
    /// "Search addresses online" in the main menu: off until the user agrees.
    results: gtk::ListBox,
    results_pop: gtk::Popover,
    found: RefCell<Vec<Found>>,
    /// Address searches started: an answer to an older one is dropped.
    searches: Cell<u64>,
    /// A spot picked but not saved: a search result or a click while adding.
    pin_layer: shumate::MarkerLayer,
    adding: Cell<bool>,
    card_button: gtk::Button,
    card_action: RefCell<Option<Rc<dyn Fn()>>>,
    /// The maps in the libraries (ADR-0028), and which one is the base: a
    /// tileset's record ID, or "" for none (a blank map).
    tilesets: RefCell<Vec<Tileset>>,
    base: RefCell<Option<String>>,
    /// The layers: libraries. Map libraries to choose the base map from, and
    /// a switch for each library with places; the ones switched off.
    layers_group: adw::PreferencesGroup,
    layer_rows: RefCell<Vec<gtk::Widget>>,
    hidden: RefCell<HashSet<String>>,
    /// Libraries joined from elsewhere (not made here): these can be left.
    joined: RefCell<Vec<String>>,
    /// Libraries held lazily: their places are asked for by the cells on
    /// screen (ADR-0031), kept per (library, cell) until they change.
    lazy: RefCell<Vec<String>>,
    /// Address libraries: searched from the search field, not drawn.
    addressed: RefCell<Vec<String>>,
    cell_places: RefCell<HashMap<(String, String), Vec<Place>>>,
    cells_asked: RefCell<HashSet<(String, String)>>,
    /// The user switched the map off themselves: don't pick one for them.
    no_map_chosen: Cell<bool>,
    /// When the base map was last drawn (for redrawing as its library changes).
    base_drawn: Cell<std::time::Instant>,
    draw_pending: Cell<bool>,
    /// The info bubble's own layer (above the markers, not redrawn with them).
    /// The info bubble: over the map (not a marker: a tall one wasn't drawn),
    /// kept above its place as the map moves.
    map_overlay: gtk::Overlay,
    bubble: RefCell<Option<(f64, f64, gtk::Widget)>>,
    bubble_opened: Cell<std::time::Instant>,
    demo_opened: Cell<bool>,
}

fn build(gapp: &adw::Application) {
    let core = Arc::new(Map::new());
    // The map's places: small dots, names on a light label.
    let css = gtk::CssProvider::new();
    css.load_from_string(&(
        ".map-place { min-width: 20px; min-height: 20px; padding: 2px; border: 1.5px solid white; box-shadow: 0 1px 2px alpha(black, 0.4); }          .map-name { background: alpha(white, 0.85); color: #222; border-radius: 4px; padding: 0 4px; }          .map-bubble { padding: 10px 12px; box-shadow: 0 2px 8px alpha(black, 0.3); }          .map-bubble-photo { border-radius: 6px; }          .map-bubble-arrow { color: @card_bg_color; margin-top: -6px; font-size: 18px; text-shadow: 0 2px 2px alpha(black, 0.2); } "
            .to_string()
            + &icons::css()),
    );
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    }

    // Sidebar: the layers, and what could still be placed.
    let layers_group = adw::PreferencesGroup::builder().title("Libraries").description("Maps to draw on, and places to show.").build();
    let add_layer = gtk::Button::builder().icon_name("list-add-symbolic").tooltip_text("Add a library (its link)").valign(gtk::Align::Center).css_classes(["flat"]).build();
    layers_group.set_header_suffix(Some(&add_layer));
    let unplaced_group = adw::PreferencesGroup::builder()
        .title("Not on the map yet")
        .description("Contacts and events with an address but no place. Pick one, then click the map or search its address.")
        .visible(false)
        .build();
    let unplaced_row = adw::ExpanderRow::builder().build();
    unplaced_row.add_prefix(&gtk::Image::from_icon_name("find-location-symbolic"));
    unplaced_group.add(&unplaced_row);
    // Wrapped and narrow: a long message mustn't widen the sidebar.
    let status = gtk::Label::builder().xalign(0.0).margin_start(12).margin_end(12).margin_bottom(8).wrap(true).wrap_mode(gtk::pango::WrapMode::WordChar).max_width_chars(36).css_classes(["dim-label", "caption"]).build();
    let side_inner = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(24).margin_start(12).margin_end(12).margin_top(12).margin_bottom(12).build();
    side_inner.append(&layers_group);
    side_inner.append(&unplaced_group);
    let side_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    side_box.append(&gtk::ScrolledWindow::builder().child(&side_inner).hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).build());
    side_box.append(&status);
    let side_view = adw::ToolbarView::new();
    side_view.add_top_bar(&adw::HeaderBar::new());
    side_view.set_content(Some(&side_box));
    let side_page = adw::NavigationPage::builder().title("Map").child(&side_view).build();

    // The map: tiles only from map libraries in retifs (ADR-0028), never from the internet.
    let smap = shumate::SimpleMap::new();
    smap.set_vexpand(true);
    smap.set_hexpand(true);
    let viewport = smap.viewport().expect("a viewport");
    let layer = shumate::MarkerLayer::new(&viewport);
    smap.add_overlay_layer(&layer);
    let pin_layer = shumate::MarkerLayer::new(&viewport);
    smap.add_overlay_layer(&pin_layer);
    let search = gtk::SearchEntry::builder().placeholder_text("Search an address").width_chars(36).build();
    let results = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).build();
    results.add_css_class("boxed-list");
    let results_pop = gtk::Popover::builder()
        .child(&gtk::ScrolledWindow::builder().child(&results).propagate_natural_height(true).max_content_height(420).min_content_width(420).build())
        .autohide(true)
        .has_arrow(false)
        .build();
    results_pop.set_parent(&search);
    let add = gtk::Button::builder().icon_name("list-add-symbolic").tooltip_text("Add a place").build();
    let header = adw::HeaderBar::builder().title_widget(&search).build();
    header.pack_start(&add);
    // Public libraries heard on the network, as LXMF apps list announces (ADR-0029).
    let announces = gtk::Button::builder().label("Announces").tooltip_text("Public libraries heard on the network").build();
    header.pack_end(&announces);

    let banner = adw::Banner::builder().button_label("Cancel").revealed(false).build();
    let card = adw::ActionRow::builder().build();
    card.add_css_class("card");
    let card_button = gtk::Button::builder().valign(gtk::Align::Center).visible(false).css_classes(["suggested-action"]).build();
    let card_close = gtk::Button::builder().icon_name("window-close-symbolic").valign(gtk::Align::Center).css_classes(["flat", "circular"]).build();
    card.add_suffix(&card_button);
    card.add_suffix(&card_close);
    let card_box = gtk::Box::builder().halign(gtk::Align::Center).valign(gtk::Align::End).margin_bottom(18).width_request(360).visible(false).build();
    card_box.append(&card);
    let overlay = gtk::Overlay::builder().child(&smap).build();
    overlay.add_overlay(&card_box);
    let content = adw::ToolbarView::new();
    content.add_top_bar(&header);
    content.add_top_bar(&banner);
    content.set_content(Some(&overlay));
    let content_page = adw::NavigationPage::builder().title("Map").child(&content).build();

    let split = adw::NavigationSplitView::builder().sidebar(&side_page).content(&content_page).min_sidebar_width(260.0).max_sidebar_width(360.0).build();
    let window = adw::ApplicationWindow::builder().application(gapp).title("Map").default_width(1100).default_height(720).content(&split).build();

    let me = Rc::new(App {
        core,
        smap: smap.clone(),
        layer,
        unplaced_group,
        unplaced_row,
        unplaced_rows: RefCell::new(Vec::new()),
        banner: banner.clone(),
        card,
        card_box,
        places: RefCell::new(Vec::new()),
        unplaced: RefCell::new(Vec::new()),
        placing: RefCell::new(None),
        thumbs: RefCell::new(HashMap::new()),
        tried: RefCell::new(HashSet::new()),
        libraries: RefCell::new(Vec::new()),
        counts: RefCell::new(HashMap::new()),
        status,
        fitted: Cell::new(false),
        zoom: Cell::new(-1),
        window: window.clone(),
        search: search.clone(),
        results,
        results_pop,
        found: RefCell::new(Vec::new()),
        searches: Cell::new(0),
        pin_layer,
        adding: Cell::new(false),
        card_button: card_button.clone(),
        card_action: RefCell::new(None),
        tilesets: RefCell::new(Vec::new()),
        base: RefCell::new(None),
        layers_group: layers_group.clone(),
        layer_rows: RefCell::new(Vec::new()),
        hidden: RefCell::new(HashSet::new()),
        joined: RefCell::new(Vec::new()),
        lazy: RefCell::new(Vec::new()),
        addressed: RefCell::new(Vec::new()),
        cell_places: RefCell::new(HashMap::new()),
        cells_asked: RefCell::new(HashSet::new()),
        base_drawn: Cell::new(std::time::Instant::now()),
        no_map_chosen: Cell::new(false),
        draw_pending: Cell::new(false),
        map_overlay: overlay.clone(),
        bubble: RefCell::new(None),
        bubble_opened: Cell::new(std::time::Instant::now()),
        demo_opened: Cell::new(false),
    });
    {
        let me2 = me.clone();
        add_layer.connect_clicked(move |_| add_layer_dialog(&me2));
    }
    {
        let me2 = me.clone();
        announces.connect_clicked(move |_| announces::open(&me2));
    }
    {
        let me2 = me.clone();
        card_button.connect_clicked(move |_| {
            let action = me2.card_action.borrow().clone();
            if let Some(f) = action {
                f();
            }
        });
    }
    {
        let me2 = me.clone();
        card_close.connect_clicked(move |_| {
            me2.card_box.set_visible(false);
            me2.pin_layer.remove_all();
        });
    }
    {
        let me2 = me.clone();
        search.connect_activate(move |e| find(&me2, e.text().to_string(), true));
        // As it's typed, too.
        search.set_search_delay(350);
        let me2 = me.clone();
        search.connect_search_changed(move |e| {
            let text = e.text().to_string();
            if retifs_map_core::fold(&text).concat().chars().count() >= 3 {
                find(&me2, text, false);
            }
        });
    }
    {
        let me2 = me.clone();
        add.connect_clicked(move |_| {
            *me2.placing.borrow_mut() = None;
            me2.adding.set(true);
            me2.banner.set_title("Click the map where the new place is, or search its address");
            me2.banner.set_revealed(true);
        });
    }
    // Zooming regroups the markers, once per zoom step rather than per frame.
    {
        let me2 = me.clone();
        viewport.connect_zoom_level_notify(move |vp| {
            if vp.zoom_level().round() as i32 != me2.zoom.get() {
                draw(&me2);
            }
        });
        let me2 = me.clone();
        viewport.connect_latitude_notify(move |_| {
            bubble::place(&me2);
            draw_soon(&me2)
        });
        let me2 = me.clone();
        viewport.connect_longitude_notify(move |_| {
            bubble::place(&me2);
            draw_soon(&me2)
        });
        let me2 = me.clone();
        viewport.connect_zoom_level_notify(move |_| bubble::place(&me2));
    }
    {
        let me2 = me.clone();
        banner.connect_button_clicked(move |b| {
            b.set_revealed(false);
            *me2.placing.borrow_mut() = None;
            me2.adding.set(false);
        });
    }
    // A click on the map while placing: there.
    {
        let me2 = me.clone();
        let click = gtk::GestureClick::new();
        click.connect_released(move |g, _, x, y| {
            if me2.placing.borrow().is_none() && !me2.adding.get() {
                // Elsewhere on the map: the bubble goes (not the click that opened it).
                if me2.bubble_opened.get().elapsed() > std::time::Duration::from_millis(400) {
                    bubble::close(&me2);
                }
                return;
            }
            let Some(w) = g.widget() else { return };
            let Some(vp) = me2.smap.viewport() else { return };
            let (lat, lon) = vp.widget_coords_to_location(&w, x, y);
            if me2.adding.replace(false) {
                me2.banner.set_revealed(false);
                pin(&me2, lat, lon);
                save_dialog(&me2, "", "", lat, lon);
            } else {
                place_here(&me2, lat, lon);
            }
        });
        smap.add_controller(click);
    }

    // A blank map until the libraries' maps are known.
    layers_ui(&me);
    *me.base.borrow_mut() = None;
    reload(&me);
    {
        let me2 = me.clone();
        glib::timeout_add_seconds_local(3, move || {
            let libs = me2.libraries.borrow().clone();
            let me3 = me2.clone();
            bg(&me2.core, move |m| m.change_counts(&libs), move |counts| {
                if counts.is_empty() || counts == *me3.counts.borrow() {
                    return;
                }
                let changed: HashSet<String> = counts.iter().filter(|(ns, n)| me3.counts.borrow().get(*ns) != Some(*n)).map(|(ns, _)| ns.clone()).collect();
                refresh_base(&me3, &changed);
                // A map library changing (tiles) has no places to reload.
                let maps: HashSet<String> = me3.tilesets.borrow().iter().map(|t| t.ns.clone()).collect();
                if changed.is_subset(&maps) {
                    *me3.counts.borrow_mut() = counts;
                } else {
                    reload(&me3);
                }
            });
            glib::ControlFlow::Continue
        });
    }
    snapshot_on_request(&window);
    demo_search(&me);
    window.present();
}

fn reload(me: &Rc<App>) {
    let me2 = me.clone();
    bg(&me.core, |m| {
        let libs = m.libraries().unwrap_or_default();
        // Lazily held libraries aren't asked for all their places at once.
        let lazy = m.lazy(&libs);
        let whole: Vec<String> = libs.iter().filter(|n| !lazy.contains(n)).cloned().collect();
        let r = m.places(&whole);
        let counts = m.change_counts(&libs);
        let maps = m.tilesets(&libs);
        let joined = m.joined(&libs);
        let addressed = m.address_libraries(&libs);
        (libs, r, counts, maps, joined, lazy, addressed)
    }, move |(libs, r, counts, maps, joined, lazy, addressed)| {
        *me2.joined.borrow_mut() = joined;
        *me2.addressed.borrow_mut() = addressed;
        // Changed: their cells are asked for again (live).
        *me2.lazy.borrow_mut() = lazy;
        me2.cell_places.borrow_mut().clear();
        me2.cells_asked.borrow_mut().clear();
        *me2.libraries.borrow_mut() = libs;
        *me2.counts.borrow_mut() = counts;
        *me2.tilesets.borrow_mut() = maps;
        match r {
            Ok((placed, unplaced)) => {
                *me2.places.borrow_mut() = placed;
                *me2.unplaced.borrow_mut() = unplaced;
                me2.status.set_text("");
            }
            Err(e) => me2.status.set_text(&format!("retifsd isn't answering: {e}")),
        }
        sidebar(&me2);
        draw(&me2);
    });
}

/// The layers' counts and the records waiting for a place.
fn sidebar(me: &Rc<App>) {
    layers_ui(me);
    for w in me.unplaced_rows.borrow_mut().drain(..) {
        me.unplaced_row.remove(&w);
    }
    let unplaced = me.unplaced.borrow();
    me.unplaced_group.set_visible(!unplaced.is_empty());
    me.unplaced_row.set_title(&format!("{} waiting", unplaced.len()));
    if unplaced.len() > UNPLACED_SHOWN {
        me.unplaced_row.set_subtitle(&format!("the first {UNPLACED_SHOWN} shown"));
    }
    for u in unplaced.iter().take(UNPLACED_SHOWN) {
        let row = adw::ActionRow::builder().title(glib::markup_escape_text(&u.title)).subtitle(glib::markup_escape_text(&u.address)).activatable(true).build();
        row.add_prefix(&gtk::Image::from_icon_name(icon(u.kind)));
        let (me2, u2) = (me.clone(), u.clone());
        row.connect_activated(move |_| {
            me2.adding.set(false);
            me2.banner.set_title(&format!("Click the map where {} is, or pick a search result", u2.title));
            me2.banner.set_revealed(true);
            *me2.placing.borrow_mut() = Some(u2.clone());
            me2.search.set_text(&u2.address);
            find(&me2, u2.address.clone(), true);
        });
        me.unplaced_row.add_row(&row);
        me.unplaced_rows.borrow_mut().push(row.upcast());
    }
}

/// Places of the layers that are on: those held whole, and those of lazily
/// held libraries fetched for the cells seen so far.
fn visible(me: &Rc<App>) -> Vec<Place> {
    let hidden = me.hidden.borrow();
    let mut out: Vec<Place> = me.places.borrow().iter().filter(|p| !hidden.contains(&p.ns)).cloned().collect();
    for ((ns, _), places) in me.cell_places.borrow().iter() {
        if !hidden.contains(ns) {
            out.extend(places.iter().cloned());
        }
    }
    out
}

/// Cells this close in: a lazily held library's places are asked for from
/// this zoom on (a cell is a zoom-12 tile).
const CELLS_FROM: f64 = 12.0;

/// Asks lazily held libraries for the places in the cells on screen that
/// haven't been asked for yet.
fn ask_cells(me: &Rc<App>, z: f64) {
    if z < CELLS_FROM || me.lazy.borrow().is_empty() {
        return;
    }
    let Some(vp) = me.smap.viewport() else { return };
    let (w, h) = (me.smap.width() as f64, me.smap.height() as f64);
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let (north, west) = vp.widget_coords_to_location(&me.smap, 0.0, 0.0);
    let (south, east) = vp.widget_coords_to_location(&me.smap, w, h);
    // Landmarks in zoom-12 cells from zoom 12; the rest in zoom-14 cells from 14.
    let mut cells = Map::cells(west, south, east, north);
    if z >= 14.0 {
        cells.extend(Map::cells_at(14, west, south, east, north));
    }
    for ns in me.lazy.borrow().iter().filter(|n| !me.hidden.borrow().contains(*n)) {
        let missing: Vec<String> = cells.iter().filter(|c| !me.cells_asked.borrow().contains(&(ns.clone(), (*c).clone()))).take(16).cloned().collect();
        if missing.is_empty() {
            continue;
        }
        for c in &missing {
            me.cells_asked.borrow_mut().insert((ns.clone(), c.clone()));
        }
        let (me2, ns2) = (me.clone(), ns.clone());
        bg(&me.core, move |m| m.places_in(&ns2, &missing).map(|r| (ns2, r)), move |r| {
            if let Ok((ns, found)) = r {
                for (cell, places) in found {
                    me2.cell_places.borrow_mut().insert((ns.clone(), cell), places);
                }
                draw(&me2);
                layers_ui(&me2);
            }
        });
    }
}

/// Where a place is on the whole world's map at zoom `z`, in pixels (Web Mercator).
fn pixel(lat: f64, lon: f64, z: f64) -> (f64, f64) {
    let world = 256.0 * 2f64.powf(z);
    let phi = lat.clamp(-85.05, 85.05).to_radians();
    ((lon + 180.0) / 360.0 * world, (1.0 - (phi.tan() + 1.0 / phi.cos()).ln() / std::f64::consts::PI) / 2.0 * world)
}

/// Places of one kind close together at this zoom; the first is the newest.
struct Group {
    kind: Kind,
    members: Vec<Place>,
}

fn groups(places: Vec<Place>, z: f64) -> Vec<Group> {
    let mut at: HashMap<(Kind, i64, i64), usize> = HashMap::new();
    let mut out: Vec<Group> = Vec::new();
    for p in places {
        let (x, y) = pixel(p.lat, p.lon, z);
        let key = (p.kind, (x / CELL).floor() as i64, (y / CELL).floor() as i64);
        match at.get(&key) {
            Some(&i) => out[i].members.push(p),
            None => {
                at.insert(key, out.len());
                out.push(Group { kind: p.kind, members: vec![p] });
            }
        }
    }
    out
}

fn draw(me: &Rc<App>) {
    let places = visible(me);
    if !me.fitted.get() {
        // RETIFS_MAP_VIEW=lat,lon,zoom: where to open instead (demos).
        let view: Option<Vec<f64>> = std::env::var("RETIFS_MAP_VIEW").ok().map(|v| v.split(',').filter_map(|n| n.trim().parse().ok()).collect());
        // Around the user's own things: the map's places (a country's cafés)
        // and libraries of thousands would show the whole country.
        let mut per_lib: HashMap<&str, usize> = HashMap::new();
        for p in &places {
            *per_lib.entry(p.ns.as_str()).or_default() += 1;
        }
        let own: Vec<Place> = places.iter().filter(|p| p.minzoom.is_none() && per_lib[p.ns.as_str()] <= FIT_MAX).cloned().collect();
        // Nothing of one's own (a storage-less node: places come by cell only
        // once zoomed in): the shown map's area.
        let shown = me.base.borrow().clone().unwrap_or_default();
        let area = me.tilesets.borrow().iter().find(|t| t.id == shown).and_then(|t| t.bounds);
        match (view.as_deref(), me.smap.map(), me.smap.viewport()) {
            (Some([lat, lon, z]), Some(map), Some(vp)) => {
                vp.set_zoom_level(*z);
                map.center_on(*lat, *lon);
                me.fitted.set(true);
            }
            _ if !own.is_empty() => {
                fit(me, &own);
                me.fitted.set(true);
            }
            _ => {
                if let Some([w, s, e, n]) = area {
                    let corners = [(s, w), (n, e)].map(|(lat, lon)| Place { ns: String::new(), id: String::new(), kind: Kind::Other, title: String::new(), label: String::new(), lat, lon, when: String::new(), detail: String::new(), minzoom: None, thumb: None, preview: None });
                    fit(me, &corners);
                    me.fitted.set(true);
                } else if !places.is_empty() {
                    fit(me, &places);
                    me.fitted.set(true);
                }
            }
        }
    }
    let z = me.smap.viewport().map(|v| v.zoom_level()).unwrap_or(10.0).round();
    me.zoom.set(z as i32);
    ask_cells(me, z);
    // Only what's on screen, and a screen around it, gets a marker.
    let on_screen: Box<dyn Fn(f64, f64) -> bool> = match me.smap.viewport() {
        Some(vp) => {
            let (cx, cy) = pixel(vp.latitude(), vp.longitude(), z);
            let (w, h) = (me.smap.width().max(800) as f64, me.smap.height().max(600) as f64);
            Box::new(move |lat, lon| {
                let (x, y) = pixel(lat, lon, z);
                (x - cx).abs() < w * 1.5 && (y - cy).abs() < h * 1.5
            })
        }
        None => Box::new(|_, _| true),
    };
    let (map_places, own): (Vec<Place>, Vec<Place>) = places.into_iter().partition(|p| p.minzoom.is_some());
    me.layer.remove_all();
    // The map's places, as a map shows them: those that matter at this zoom,
    // the more important first, and none on top of another.
    for p in declutter(map_places.into_iter().filter(|p| p.minzoom.unwrap_or(0) as f64 <= z && on_screen(p.lat, p.lon)).collect(), z) {
        me.layer.add_marker(&place_marker(me, &p, z));
    }
    // The user's own things: grouped over everything (so groups don't change
    // as the map moves), drawn above the map's places.
    let mut groups = groups(own, z);
    groups.retain(|g| on_screen(g.members[0].lat, g.members[0].lon));
    for g in &groups {
        me.layer.add_marker(&marker(me, g));
    }
    load_thumbs(me, &groups);
    // RETIFS_MAP_OPEN=<name>: that place's bubble, once it's on the map (demos).
    if !me.demo_opened.get() {
        if let Some(want) = std::env::var("RETIFS_MAP_OPEN").ok().filter(|w| !w.is_empty()) {
            if let Some(p) = visible(me).into_iter().find(|p| p.title.contains(&want)) {
                me.demo_opened.set(true);
                let me2 = me.clone();
                glib::timeout_add_local_once(std::time::Duration::from_millis(1500), move || focus(&me2, &p, 1));
            }
        }
    }
}

/// Places closer than this (pixels) to one already shown are left out.
const SPACING: f64 = 30.0;
/// From this zoom the map's places have their names beside them.
const NAMES_FROM: f64 = 16.0;

/// The places to show, most important (lowest minzoom) first, leaving out any
/// that would crowd one already in: its dot, and its name when names show.
fn declutter(mut places: Vec<Place>, z: f64) -> Vec<Place> {
    places.sort_by(|a, b| a.minzoom.cmp(&b.minzoom).then(a.title.cmp(&b.title)));
    const CELLS: f64 = 64.0;
    // Each one's box: the dot (and a little room), and the label to its right.
    let boxed = |p: &Place| {
        let (x, y) = pixel(p.lat, p.lon, z);
        let w = if z >= NAMES_FROM { p.title.chars().count() as f64 * 6.5 + 12.0 } else { 0.0 };
        (x - SPACING / 2.0, y - SPACING / 2.0, x + SPACING / 2.0 + w, y + SPACING / 2.0)
    };
    let mut taken: HashMap<(i64, i64), Vec<(f64, f64, f64, f64)>> = HashMap::new();
    let mut out = Vec::new();
    for p in places {
        let b = boxed(&p);
        let cells = |b: (f64, f64, f64, f64)| {
            let (x0, y0, x1, y1) = ((b.0 / CELLS).floor() as i64, (b.1 / CELLS).floor() as i64, (b.2 / CELLS).floor() as i64, (b.3 / CELLS).floor() as i64);
            (x0..=x1).flat_map(move |x| (y0..=y1).map(move |y| (x, y)))
        };
        let crowded = cells(b).any(|c| taken.get(&c).is_some_and(|v| v.iter().any(|o| b.0 < o.2 && o.0 < b.2 && b.1 < o.3 && o.1 < b.3)));
        if !crowded {
            for c in cells(b) {
                taken.entry(c).or_default().push(b);
            }
            out.push(p);
        }
    }
    out
}

/// A map's place: a small dot with what it is, its name when close in.
fn place_marker(me: &Rc<App>, p: &Place, z: f64) -> shumate::Marker {
    let m = shumate::Marker::new();
    m.set_location(p.lat, p.lon);
    let (icon, group) = icons::style_of(&p.detail);
    let dot = gtk::Button::builder().css_classes(["circular", "map-place", &format!("pc-{group}")]).tooltip_text(format!("{} ({})", p.title, p.detail)).build();
    if let Some(t) = icons::texture(icon, 14) {
        dot.set_child(Some(&gtk::Image::builder().paintable(&t).pixel_size(14).build()));
    }
    let (me2, p2) = (me.clone(), p.clone());
    dot.connect_clicked(move |_| focus(&me2, &p2, 1));
    if z >= NAMES_FROM {
        // A marker is centred on its place: an empty space as wide as the
        // name on the dot's other side keeps the dot there.
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let name = gtk::Label::builder().label(&p.title).css_classes(["caption", "map-name"]).build();
        let space = gtk::Box::builder().can_target(false).build();
        let same = gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal);
        same.add_widget(&space);
        same.add_widget(&name);
        row.append(&space);
        row.append(&dot);
        row.append(&name);
        m.set_child(Some(&row));
    } else {
        m.set_child(Some(&dot));
    }
    m
}


/// Draws again once the map has stopped moving for a moment.
fn draw_soon(me: &Rc<App>) {
    if me.draw_pending.replace(true) {
        return;
    }
    let me2 = me.clone();
    glib::timeout_add_local_once(std::time::Duration::from_millis(200), move || {
        me2.draw_pending.set(false);
        draw(&me2);
    });
}

/// A marker for a group: the newest photo's picture or the kind's icon, and how many.
fn marker(me: &Rc<App>, g: &Group) -> shumate::Marker {
    let n = g.members.len();
    let first = &g.members[0];
    let lat = g.members.iter().map(|p| p.lat).sum::<f64>() / n as f64;
    let lon = g.members.iter().map(|p| p.lon).sum::<f64>() / n as f64;
    let m = shumate::Marker::new();
    m.set_location(lat, lon);
    let tip = if n == 1 { first.title.clone() } else { format!("{n} × {}", kind_name(g.kind).to_lowercase()) };
    let button = gtk::Button::builder().css_classes(["circular", "osd"]).tooltip_text(&tip).build();
    match me.thumbs.borrow().get(&first.id) {
        Some(t) => {
            let pic = gtk::Image::builder().paintable(t).pixel_size(MARK).overflow(gtk::Overflow::Hidden).css_classes(["circular"]).build();
            button.set_child(Some(&pic));
            button.add_css_class("flat");
        }
        None => button.set_icon_name(icon(g.kind)),
    }
    let child: gtk::Widget = if n > 1 {
        let text = if n > 9999 { format!("{}k", n / 1000) } else { n.to_string() };
        let badge = gtk::Label::builder().label(text).halign(gtk::Align::End).valign(gtk::Align::Start).css_classes(["osd", "caption-heading"]).can_target(false).build();
        let o = gtk::Overlay::builder().child(&button).build();
        o.add_overlay(&badge);
        o.upcast()
    } else {
        button.clone().upcast()
    };
    let (me2, members) = (me.clone(), g.members.clone());
    button.connect_clicked(move |_| open(&me2, &members));
    m.set_child(Some(&child));
    m
}

/// A click on a marker: one place shows its card; a group zooms in until it
/// comes apart, or, if they're all at one spot, shows the newest and the count.
fn open(me: &Rc<App>, members: &[Place]) {
    let spread = members.iter().any(|p| (p.lat - members[0].lat).abs() > 1e-5 || (p.lon - members[0].lon).abs() > 1e-5);
    if members.len() > 1 && spread {
        me.card_box.set_visible(false);
        fit(me, members);
        return;
    }
    focus(me, &members[0], members.len());
}

/// Shows a place's info bubble and glides the map to it.
fn focus(me: &Rc<App>, p: &Place, here: usize) {
    me.card_box.set_visible(false);
    *me.card_action.borrow_mut() = None;
    me.pin_layer.remove_all();
    bubble::show(me, p, here);
    glide(me, p.lat, p.lon, None);
}

/// Moves the map there smoothly (and to `zoom`, if given); without a map
/// shown (the blank style) libshumate can't animate, so it just goes.
fn glide(me: &Rc<App>, lat: f64, lon: f64, zoom: Option<f64>) {
    let Some(map) = me.smap.map() else { return };
    let real = me.base.borrow().as_deref().is_some_and(|b| !b.is_empty());
    match (real, zoom) {
        (true, Some(z)) => map.go_to_full(lat, lon, z),
        (true, None) => map.go_to(lat, lon),
        (false, z) => {
            if let (Some(z), Some(vp)) = (z, me.smap.viewport()) {
                vp.set_zoom_level(z);
            }
            map.center_on(lat, lon);
        }
    }
}

/// Centres and zooms to show every place.
fn fit(me: &Rc<App>, places: &[Place]) {
    let (mut lat0, mut lat1, mut lon0, mut lon1) = (90.0f64, -90.0f64, 180.0f64, -180.0f64);
    for p in places {
        lat0 = lat0.min(p.lat);
        lat1 = lat1.max(p.lat);
        lon0 = lon0.min(p.lon);
        lon1 = lon1.max(p.lon);
    }
    let span = (lat1 - lat0).max(lon1 - lon0).max(0.002);
    let zoom = ((360.0 / span).log2() - 0.5).clamp(2.0, 18.0);
    if let Some(map) = me.smap.map() {
        // Gliding there, except the first time (nothing to glide from).
        if me.fitted.get() {
            glide(me, (lat0 + lat1) / 2.0, (lon0 + lon1) / 2.0, Some(zoom));
        } else if let Some(vp) = me.smap.viewport() {
            vp.set_zoom_level(zoom);
            map.center_on((lat0 + lat1) / 2.0, (lon0 + lon1) / 2.0);
        }
    }
}

/// The pictures for the photo markers on the map (one per group), a batch at a time.
fn load_thumbs(me: &Rc<App>, groups: &[Group]) {
    let missing: Vec<Place> = groups
        .iter()
        .filter(|g| g.kind == Kind::Photo)
        .map(|g| &g.members[0])
        .filter(|p| p.thumb.is_some() && !me.tried.borrow().contains(&p.id))
        .take(24)
        .cloned()
        .collect();
    if missing.is_empty() {
        return;
    }
    // Asked once each, whether or not it comes.
    me.tried.borrow_mut().extend(missing.iter().map(|p| p.id.clone()));
    let me2 = me.clone();
    bg(&me.core, move |m| missing.iter().map(|p| (p.id.clone(), m.thumb(p))).collect::<Vec<_>>(), move |got| {
        let mut any = false;
        for (id, bytes) in got {
            if let Some(t) = bytes.and_then(|b| square(&b)) {
                me2.thumbs.borrow_mut().insert(id, t);
                any = true;
            }
        }
        if any {
            draw(&me2);
        }
    });
}

/// Writes the waiting record's place (a click or a search result) and redraws.
fn place_here(me: &Rc<App>, lat: f64, lon: f64) {
    let Some(u) = me.placing.borrow_mut().take() else { return };
    me.banner.set_revealed(false);
    me.pin_layer.remove_all();
    me.card_box.set_visible(false);
    let me2 = me.clone();
    bg(&me.core, move |m| m.place(&u.ns, &u.id, &u.key, lat, lon), move |r| {
        if let Err(e) = r {
            me2.status.set_text(&format!("Not placed: {e}"));
        }
        reload(&me2);
    });
}

/// A pin where a place is about to be saved.
fn pin(me: &Rc<App>, lat: f64, lon: f64) {
    me.pin_layer.remove_all();
    let m = shumate::Marker::new();
    m.set_location(lat, lon);
    m.set_child(Some(&gtk::Image::builder().icon_name("mark-location-symbolic").pixel_size(32).css_classes(["accent"]).build()));
    me.pin_layer.add_marker(&m);
}

/// Searches the address libraries joined here (ADR-0027): retifs only, no
/// internet service. `asked`: Enter was pressed, so "nothing found" is said.
fn find(me: &Rc<App>, text: String, asked: bool) {
    let n = me.searches.get() + 1;
    me.searches.set(n);
    if asked {
        me.status.set_text("Searching…");
    }
    let me2 = me.clone();
    let here = visible(me);
    bg(&me.core, move |m| m.find(&text, &here), move |r| {
        if me2.searches.get() != n {
            return; // typed on since
        }
        me2.status.set_text("");
        match r {
            Ok(found) if asked || !found.is_empty() => show_found(&me2, found),
            Ok(_) => me2.results_pop.popdown(),
            Err(e) if asked => me2.status.set_text(&format!("Search failed: {e}")),
            Err(_) => {}
        }
    });
}

/// Search results in the popover under the search field.
fn show_found(me: &Rc<App>, found: Vec<Found>) {
    let me2 = me.clone();
    {
        me2.results.remove_all();
        if found.is_empty() {
            let row = adw::ActionRow::builder().title("Nothing found").subtitle("in the address libraries joined here").build();
            me2.results.append(&row);
        }
        for (i, f) in found.iter().enumerate() {
            let (head, rest) = f.name.split_once(", ").unwrap_or((&f.name, ""));
            let row = adw::ActionRow::builder().title(glib::markup_escape_text(head)).subtitle(glib::markup_escape_text(rest)).activatable(true).build();
            let me3 = me2.clone();
            row.connect_activated(move |_| {
                let f = me3.found.borrow().get(i).cloned();
                if let Some(f) = f {
                    picked(&me3, &f);
                }
            });
            me2.results.append(&row);
        }
        *me2.found.borrow_mut() = found;
        me2.results_pop.popup();
    }
}

/// A search result picked: the waiting record goes there, or it may be saved.
fn picked(me: &Rc<App>, f: &Found) {
    me.results_pop.popdown();
    // A place: its bubble, as when it's clicked on the map.
    if let Some(p) = &f.place {
        glide(me, p.lat, p.lon, Some(f.zoom.max(me.smap.viewport().map(|v| v.zoom_level()).unwrap_or(0.0))));
        me.card_box.set_visible(false);
        me.pin_layer.remove_all();
        bubble::show(me, p, 1);
        return;
    }
    glide(me, f.lat, f.lon, Some(f.zoom));
    bubble::close(me);
    pin(me, f.lat, f.lon);
    let head = f.name.split(", ").next().unwrap_or(&f.name).to_string();
    let waiting = me.placing.borrow().clone();
    me.card.set_title(&glib::markup_escape_text(&head));
    me.card.set_subtitle(&glib::markup_escape_text(&f.name));
    let (lat, lon, name) = (f.lat, f.lon, f.name.clone());
    let me2 = me.clone();
    match waiting {
        Some(u) => {
            me.card_button.set_label(&format!("Place {} here", u.title));
            *me.card_action.borrow_mut() = Some(Rc::new(move || place_here(&me2, lat, lon)));
        }
        None => {
            me.card_button.set_label("Save place");
            me.adding.set(false);
            me.banner.set_revealed(false);
            *me.card_action.borrow_mut() = Some(Rc::new(move || save_dialog(&me2, &head, &name, lat, lon)));
        }
    }
    me.card_button.set_visible(true);
    me.card_box.set_visible(true);
}

/// Names a new place and picks its library, then saves it as a record.
fn save_dialog(me: &Rc<App>, name: &str, address: &str, lat: f64, lon: f64) {
    let libs = me.libraries.borrow().clone();
    let name_row = adw::EntryRow::builder().title("Name").text(name).build();
    let lib_model = gtk::StringList::new(&libs.iter().map(String::as_str).collect::<Vec<_>>());
    let lib_row = adw::ComboRow::builder().title("Library").model(&lib_model).build();
    let group = adw::PreferencesGroup::new();
    group.add(&name_row);
    group.add(&lib_row);
    let d = adw::AlertDialog::builder().heading("Save place").body(format!("{lat:.5}, {lon:.5}")).extra_child(&group).build();
    d.add_responses(&[("cancel", "Cancel"), ("save", "Save")]);
    d.set_response_appearance("save", adw::ResponseAppearance::Suggested);
    d.set_default_response(Some("save"));
    d.set_response_enabled("save", !name.trim().is_empty());
    {
        let d2 = d.clone();
        name_row.connect_changed(move |e| d2.set_response_enabled("save", !e.text().trim().is_empty()));
    }
    let (me2, address) = (me.clone(), address.to_string());
    d.connect_response(None, move |_, r| {
        if r != "save" {
            return;
        }
        let name = name_row.text().to_string();
        let ns = libs.get(lib_row.selected() as usize).cloned().unwrap_or_else(|| retifs_map_core::ME.into());
        let (me3, address) = (me2.clone(), address.clone());
        bg(&me2.core, move |m| m.save_place(&ns, &name, lat, lon, &address), move |r| {
            match r {
                Ok(_) => {
                    me3.pin_layer.remove_all();
                    me3.card_box.set_visible(false);
                }
                Err(e) => me3.status.set_text(&format!("Not saved: {e}")),
            }
            reload(&me3);
        });
    });
    d.present(Some(&me.window));
}

/// What a library holds on the map: "8 photos · 1 contact".
fn holds(places: &[Place], ns: &str) -> String {
    let words = [(Kind::Photo, "photo", "photos"), (Kind::Person, "contact", "contacts"), (Kind::Event, "event", "events"), (Kind::Other, "place", "places")];
    words
        .iter()
        .filter_map(|(k, one, many)| {
            let n = places.iter().filter(|p| p.ns == ns && p.kind == *k).count();
            (n > 0).then(|| format!("{n} {}", if n == 1 { one } else { many }))
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// The libraries, one row each, whatever they hold: a map, places, photos,
/// contacts, any mix (a library has no kind). The switch shows or hides all
/// of it. One map is drawn at a time: the one of the library switched on last,
/// at first the first one.
fn layers_ui(me: &Rc<App>) {
    for r in me.layer_rows.borrow_mut().drain(..) {
        me.layers_group.remove(&r);
    }
    let maps: Vec<Tileset> = me.tilesets.borrow().iter().filter(|t| !t.overlay).cloned().collect();
    // No map yet (none at start, or the one shown was left): the first one
    // that's on, as soon as there is one, unless the user turned maps off.
    let none_shown = me.base.borrow().as_deref().is_none_or(|b| b.is_empty() || !maps.iter().any(|t| t.id == b));
    if none_shown && !me.no_map_chosen.get() {
        let first = maps.iter().find(|t| !me.hidden.borrow().contains(&t.ns)).map(|t| t.id.clone()).unwrap_or_default();
        if me.base.borrow().as_deref() != Some(first.as_str()) {
            set_base(me, &first);
        }
    }
    let places = me.places.borrow();
    // Those with a map or places, and every joined library, even before
    // anything of it has arrived: joining shows at once.
    let joined = me.joined.borrow().clone();
    let mut libs: Vec<String> = places.iter().map(|p| p.ns.clone()).chain(maps.iter().map(|t| t.ns.clone())).chain(joined.iter().cloned()).collect();
    libs.sort();
    libs.dedup();
    if maps.is_empty() {
        let row = adw::ActionRow::builder().title("No map yet").subtitle("Add a library with a map with +").build();
        row.add_prefix(&gtk::Image::from_icon_name("dialog-information-symbolic"));
        me.layers_group.add(&row);
        me.layer_rows.borrow_mut().push(row.upcast());
    }
    let base = me.base.borrow().clone().unwrap_or_default();
    for ns in libs {
        let map = maps.iter().find(|t| t.ns == ns).cloned();
        let mut what: Vec<String> = Vec::new();
        if let Some(t) = &map {
            what.push(if t.id == base { format!("map: {} (shown)", t.name) } else { format!("map: {}", t.name) });
        }
        let h = holds(&places, &ns);
        if !h.is_empty() {
            what.push(h);
        }
        if me.addressed.borrow().contains(&ns) {
            what.push("addresses: found by the search field".to_string());
        } else if me.lazy.borrow().contains(&ns) && map.is_none() {
            // A lazily held library's places come by area: what's been fetched.
            let n: usize = me.cell_places.borrow().iter().filter(|((l, _), _)| *l == ns).map(|(_, p)| p.len()).sum();
            what.push(if n > 0 { format!("{n} places in view so far (fetched by area)") } else { "places come by area: zoom in to 12 or closer".to_string() });
        } else if what.is_empty() && joined.contains(&ns) {
            what.push("joined: nothing here yet, it fills in from the network".to_string());
        }
        let ns2 = ns.clone();
        let row = adw::SwitchRow::builder().title(glib::markup_escape_text(&ns)).subtitle(glib::markup_escape_text(&what.join(" · "))).active(!me.hidden.borrow().contains(&ns)).build();
        let (me2, maps2) = (me.clone(), maps.clone());
        row.connect_active_notify(move |r| {
            if r.is_active() {
                me2.no_map_chosen.set(false);
                me2.hidden.borrow_mut().remove(&ns);
                // Its map, if it has one, becomes the one drawn.
                if let Some(t) = maps2.iter().find(|t| t.ns == ns) {
                    set_base(&me2, &t.id);
                }
            } else {
                me2.hidden.borrow_mut().insert(ns.clone());
                let shown = me2.base.borrow().clone().unwrap_or_default();
                if maps2.iter().any(|t| t.ns == ns && t.id == shown) {
                    // Another library's map, if one that's on has one.
                    let next = maps2.iter().find(|t| !me2.hidden.borrow().contains(&t.ns)).map(|t| t.id.clone()).unwrap_or_default();
                    me2.no_map_chosen.set(next.is_empty());
                    set_base(&me2, &next);
                }
            }
            draw(&me2);
            let me3 = me2.clone();
            glib::idle_add_local_once(move || layers_ui(&me3));
        });
        if me.joined.borrow().contains(&ns2) {
            let leave = gtk::Button::builder().icon_name("user-trash-symbolic").tooltip_text("Leave this library").valign(gtk::Align::Center).css_classes(["flat"]).build();
            let (me2, ns3) = (me.clone(), ns2.clone());
            leave.connect_clicked(move |_| leave_dialog(&me2, &ns3));
            row.add_suffix(&leave);
        }
        me.layers_group.add(&row);
        me.layer_rows.borrow_mut().push(row.upcast());
    }
}

/// Leaves a joined library, after asking: it's gone from this device, others
/// keep theirs, and it can be joined again from Announces.
fn leave_dialog(me: &Rc<App>, ns: &str) {
    let d = adw::AlertDialog::builder()
        .heading(format!("Leave {ns}?"))
        .body("It's removed from this device: its places and map go, and the space it took is freed. Others keep their copies, and you can join it again from Announces.")
        .build();
    d.add_responses(&[("cancel", "Cancel"), ("leave", "Leave")]);
    d.set_response_appearance("leave", adw::ResponseAppearance::Destructive);
    let (me2, ns) = (me.clone(), ns.to_string());
    d.connect_response(None, move |_, r| {
        if r != "leave" {
            return;
        }
        let (me3, ns2) = (me2.clone(), ns.clone());
        bg(&me2.core, move |m| m.leave(&ns2), move |r| {
            if let Err(e) = r {
                me3.status.set_text(&format!("Not left: {e}"));
            }
            *me3.base.borrow_mut() = None;
            reload(&me3);
        });
    });
    d.present(Some(&me.window));
}

/// A new map under the places: the map layer comes on top of the others, so
/// the markers are put back above it.
fn set_source(me: &Rc<App>, source: &impl IsA<shumate::MapSource>) {
    me.smap.set_map_source(Some(source));
    for l in [&me.layer, &me.pin_layer] {
        me.smap.remove_overlay_layer(l);
        me.smap.add_overlay_layer(l);
    }
    me.base_drawn.set(std::time::Instant::now());
}

/// Tiles that weren't there when the map was drawn (a map library still
/// syncing, or updated) are fetched again: the map is redrawn when its
/// library changes, at most every 30 s.
fn refresh_base(me: &Rc<App>, changed: &HashSet<String>) {
    let Some(id) = me.base.borrow().clone() else { return };
    let ns = me.tilesets.borrow().iter().find(|t| t.id == id).map(|t| t.ns.clone());
    if let Some(ns) = ns {
        if changed.contains(&ns) && me.base_drawn.get().elapsed() > std::time::Duration::from_secs(30) {
            set_base(me, &id);
        }
    }
}

/// Nothing but the land colour: places still show, there's just no map under them.
const BLANK_STYLE: &str = r##"{"version":8,"sources":{},"layers":[{"id":"background","type":"background","paint":{"background-color":"#f2efe9"}}]}"##;

/// Draws the map from a tileset in retifs, or a blank one ("").
fn set_base(me: &Rc<App>, id: &str) {
    *me.base.borrow_mut() = Some(id.to_string());
    if !shumate::VectorRenderer::is_supported() {
        me.status.set_text("This libshumate can't draw vector maps");
        return;
    }
    let t = me.tilesets.borrow().iter().find(|t| t.id == id).cloned();
    let Some(t) = t else {
        if let Ok(blank) = shumate::VectorRenderer::new("blank", BLANK_STYLE) {
            blank.set_max_zoom_level(18);
            set_source(me, &blank);
        }
        return;
    };
    let me2 = me.clone();
    bg(&me.core, move |m| m.style(&t).map(|s| (t, s)), move |r| match r {
        Ok((t, style)) => match shumate::VectorRenderer::new(&format!("retifs-{}", t.id), &style) {
            Ok(renderer) => {
                renderer.set_name(&t.name);
                renderer.set_license(&t.attribution);
                renderer.set_min_zoom_level(t.minzoom.max(0) as u32);
                renderer.set_max_zoom_level(18);
                set_source(&me2, &renderer);
            }
            Err(e) => me2.status.set_text(&format!("The map's style: {e}")),
        },
        Err(e) => me2.status.set_text(&format!("The map: {e}")),
    });
}

/// Adds a layer: joins a library from its `retifs:` link.
fn add_layer_dialog(me: &Rc<App>) {
    let link_row = adw::EntryRow::builder().title("Link (retifs:…)").build();
    let name_row = adw::EntryRow::builder().title("Name here (optional)").build();
    let group = adw::PreferencesGroup::new();
    group.add(&link_row);
    group.add(&name_row);
    let d = adw::AlertDialog::builder()
        .heading("Add a library")
        .body("A library's link: its places come onto the map, or, for a map library, its map. Only what you look at is fetched.")
        .extra_child(&group)
        .build();
    d.add_responses(&[("cancel", "Cancel"), ("add", "Add")]);
    d.set_response_appearance("add", adw::ResponseAppearance::Suggested);
    d.set_default_response(Some("add"));
    d.set_response_enabled("add", false);
    {
        let d2 = d.clone();
        link_row.connect_changed(move |e| d2.set_response_enabled("add", e.text().trim().starts_with("retifs:")));
    }
    let me2 = me.clone();
    d.connect_response(None, move |_, r| {
        if r != "add" {
            return;
        }
        let (link, name) = (link_row.text().to_string(), name_row.text().to_string());
        let me3 = me2.clone();
        me2.status.set_text("Adding the library…");
        bg(&me2.core, move |m| m.add_layer(&link, &name), move |r| {
            match r {
                Ok(ns) => me3.status.set_text(&format!("Added {ns}: it fills in as it syncs")),
                Err(e) => me3.status.set_text(&format!("Not added: {e}")),
            }
            reload(&me3);
        });
    });
    d.present(Some(&me.window));
}

/// A marker's picture: the middle square of a thumbnail, small.
const MARK: i32 = 44;

fn square(bytes: &[u8]) -> Option<gdk::Texture> {
    let pb = gtk::gdk_pixbuf::Pixbuf::from_read(std::io::Cursor::new(bytes.to_vec())).ok()?;
    let side = pb.width().min(pb.height());
    let cut = pb.new_subpixbuf((pb.width() - side) / 2, (pb.height() - side) / 2, side, side);
    let small = cut.scale_simple(MARK * 2, MARK * 2, gtk::gdk_pixbuf::InterpType::Bilinear)?;
    Some(gdk::Texture::for_pixbuf(&small))
}

/// RETIFS_MAP_SEARCH=<text>: typed into the search field (demos). The
/// results, which a window snapshot leaves out, go to `<snapshot>-search.png`;
/// with RETIFS_MAP_PICK set, the first is then picked.
fn demo_search(me: &Rc<App>) {
    let Ok(text) = std::env::var("RETIFS_MAP_SEARCH") else { return };
    let me2 = me.clone();
    glib::timeout_add_local_once(std::time::Duration::from_secs(4), move || me2.search.set_text(&text));
    let path = std::env::var("RETIFS_MAP_SNAPSHOT").ok().map(|p| p.replace(".png", "-search.png"));
    let (me2, seen) = (me.clone(), Cell::new(0));
    glib::timeout_add_seconds_local(2, move || {
        if !me2.results_pop.is_visible() {
            return glib::ControlFlow::Continue;
        }
        if let Some(path) = &path {
            let pop = &me2.results_pop;
            let snapshot = gtk::Snapshot::new();
            gtk::WidgetPaintable::new(Some(pop)).snapshot(&snapshot, pop.width() as f64, pop.height() as f64);
            if let (Some(node), Some(renderer)) = (snapshot.to_node(), pop.renderer()) {
                let _ = renderer.render_texture(&node, None).save_to_png(path);
            }
        }
        seen.set(seen.get() + 1);
        if seen.get() == 2 && std::env::var_os("RETIFS_MAP_PICK").is_some() {
            if let Some(f) = me2.found.borrow().first().cloned() {
                picked(&me2, &f);
            }
            return glib::ControlFlow::Break;
        }
        glib::ControlFlow::Continue
    });
}

/// RETIFS_MAP_SNAPSHOT=<file.png>: the window drawn to a file every 2 s.
fn snapshot_on_request(window: &adw::ApplicationWindow) {
    let Ok(path) = std::env::var("RETIFS_MAP_SNAPSHOT") else { return };
    let window = window.clone();
    glib::timeout_add_seconds_local(2, move || {
        let paintable = gtk::WidgetPaintable::new(Some(&window));
        let snapshot = gtk::Snapshot::new();
        paintable.snapshot(&snapshot, window.width() as f64, window.height() as f64);
        if let (Some(node), Some(renderer)) = (snapshot.to_node(), window.renderer()) {
            let _ = renderer.render_texture(&node, None).save_to_png(&path);
        }
        glib::ControlFlow::Continue
    });
}
