//! retifs Map for Linux: GTK 4 / libadwaita with libshumate's map over the
//! shared core. Every record with a place (ADR-0027): photos where they were
//! taken, contacts where they live, events where they happen, anything else
//! that says where it is. Contacts and events with an address but no
//! coordinates are placed with a click or an address search (off until the
//! user turns it on); new places are saved as records of their own. It
//! updates as the libraries change.

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
    search_on: gio::SimpleAction,
    results: gtk::ListBox,
    results_pop: gtk::Popover,
    found: RefCell<Vec<Found>>,
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
    /// When the base map was last drawn (for redrawing as its library changes).
    base_drawn: Cell<std::time::Instant>,
}

fn build(gapp: &adw::Application) {
    let core = Arc::new(Map::new());

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
    let status = gtk::Label::builder().xalign(0.0).margin_start(12).margin_bottom(8).css_classes(["dim-label", "caption"]).build();
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
    let search_on = gio::SimpleAction::new_stateful("online-search", None, &retifs_map_core::lookup_allowed().to_variant());
    let menu = gio::Menu::new();
    menu.append(Some("Search addresses online (OpenStreetMap)"), Some("win.online-search"));
    header.pack_end(&gtk::MenuButton::builder().icon_name("open-menu-symbolic").menu_model(&menu).tooltip_text("Menu").build());

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

    let split = adw::NavigationSplitView::builder().sidebar(&side_page).content(&content_page).build();
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
        search_on: search_on.clone(),
        results,
        results_pop,
        found: RefCell::new(Vec::new()),
        pin_layer,
        adding: Cell::new(false),
        card_button: card_button.clone(),
        card_action: RefCell::new(None),
        tilesets: RefCell::new(Vec::new()),
        base: RefCell::new(None),
        layers_group: layers_group.clone(),
        layer_rows: RefCell::new(Vec::new()),
        hidden: RefCell::new(HashSet::new()),
        base_drawn: Cell::new(std::time::Instant::now()),
    });
    {
        let me2 = me.clone();
        add_layer.connect_clicked(move |_| add_layer_dialog(&me2));
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
        search_on.connect_activate(move |a, _| {
            let on = !a.state().and_then(|v| v.get::<bool>()).unwrap_or(false);
            set_online_search(&me2, on);
        });
        window.add_action(&search_on);
    }
    {
        let me2 = me.clone();
        search.connect_activate(move |e| ask_search(&me2, e.text().to_string()));
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
    window.present();
}

fn reload(me: &Rc<App>) {
    let me2 = me.clone();
    bg(&me.core, |m| {
        let libs = m.libraries().unwrap_or_default();
        let r = m.places(&libs);
        let counts = m.change_counts(&libs);
        let maps = m.tilesets(&libs);
        (libs, r, counts, maps)
    }, move |(libs, r, counts, maps)| {
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
            if retifs_map_core::lookup_allowed() {
                search(&me2, u2.address.clone());
            }
        });
        me.unplaced_row.add_row(&row);
        me.unplaced_rows.borrow_mut().push(row.upcast());
    }
}

/// Places of the layers that are on.
fn visible(me: &Rc<App>) -> Vec<Place> {
    let hidden = me.hidden.borrow();
    me.places.borrow().iter().filter(|p| !hidden.contains(&p.ns)).cloned().collect()
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
    if !me.fitted.get() && !places.is_empty() {
        me.fitted.set(true);
        fit(me, &places);
    }
    let z = me.smap.viewport().map(|v| v.zoom_level()).unwrap_or(10.0).round();
    me.zoom.set(z as i32);
    let groups = groups(places, z);
    me.layer.remove_all();
    for g in &groups {
        me.layer.add_marker(&marker(me, g));
    }
    load_thumbs(me, &groups);
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

/// Shows a place's card and centres on it.
fn focus(me: &Rc<App>, p: &Place, here: usize) {
    let title = if here > 1 { format!("{} and {} more here", p.title, here - 1) } else { p.title.clone() };
    me.card.set_title(&glib::markup_escape_text(&title));
    let label = if p.label.is_empty() { String::new() } else { format!(" ({})", p.label) };
    let when = if p.when.is_empty() { String::new() } else { format!(" · {}", p.when.replace('T', " ")) };
    me.card.set_subtitle(&glib::markup_escape_text(&format!("{}{label} · {} · {:.5}, {:.5}{when}", kind_name(p.kind), p.ns, p.lat, p.lon)));
    me.card_button.set_visible(false);
    *me.card_action.borrow_mut() = None;
    me.pin_layer.remove_all();
    me.card_box.set_visible(true);
    if let Some(map) = me.smap.map() {
        map.center_on(p.lat, p.lon);
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
    if let (Some(map), Some(vp)) = (me.smap.map(), me.smap.viewport()) {
        vp.set_zoom_level(zoom);
        map.center_on((lat0 + lat1) / 2.0, (lon0 + lon1) / 2.0);
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

fn set_online_search(me: &Rc<App>, on: bool) {
    match retifs_map_core::allow_lookup(on) {
        Ok(()) => me.search_on.set_state(&on.to_variant()),
        Err(e) => me.status.set_text(&format!("Not changed: {e}")),
    }
}

/// Enter in the search field: asks once before anything is sent out.
fn ask_search(me: &Rc<App>, text: String) {
    if text.trim().is_empty() {
        return;
    }
    if retifs_map_core::lookup_allowed() {
        search(me, text);
        return;
    }
    let d = adw::AlertDialog::builder()
        .heading("Search addresses in OpenStreetMap?")
        .body("What you search for is sent to OpenStreetMap's Nominatim service, which also sees this computer's address on the internet. Nothing else leaves this device. You can turn it off again in the menu.")
        .build();
    d.add_responses(&[("cancel", "Not now"), ("on", "Turn on")]);
    d.set_response_appearance("on", adw::ResponseAppearance::Suggested);
    d.set_default_response(Some("on"));
    let me2 = me.clone();
    d.connect_response(None, move |_, r| {
        if r == "on" {
            set_online_search(&me2, true);
            search(&me2, text.clone());
        }
    });
    d.present(Some(&me.window));
}

fn search(me: &Rc<App>, text: String) {
    me.status.set_text("Searching…");
    let me2 = me.clone();
    bg(&me.core, move |m| m.lookup(&text), move |r| {
        me2.status.set_text("");
        let found = match r {
            Ok(f) => f,
            Err(e) => {
                me2.status.set_text(&format!("Search failed: {e}"));
                return;
            }
        };
        me2.results.remove_all();
        if found.is_empty() {
            me2.results.append(&adw::ActionRow::builder().title("Nothing found").build());
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
    });
}

/// A search result picked: the waiting record goes there, or it may be saved.
fn picked(me: &Rc<App>, f: &Found) {
    me.results_pop.popdown();
    if let (Some(map), Some(vp)) = (me.smap.map(), me.smap.viewport()) {
        vp.set_zoom_level(16.0);
        map.center_on(f.lat, f.lon);
    }
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

/// The layers, all libraries: first the maps to choose the base map from
/// (ADR-0028; the first one until the user picks another), then a switch for
/// each library with places.
fn layers_ui(me: &Rc<App>) {
    for r in me.layer_rows.borrow_mut().drain(..) {
        me.layers_group.remove(&r);
    }
    let maps: Vec<Tileset> = me.tilesets.borrow().iter().filter(|t| !t.overlay).cloned().collect();
    if me.base.borrow().is_none() {
        let first = maps.first().map(|t| t.id.clone()).unwrap_or_default();
        set_base(me, &first);
    }
    // Switches like the other libraries; one map at a time, or none.
    let chosen = me.base.borrow().clone().unwrap_or_default();
    let switches: Rc<RefCell<Vec<(String, adw::SwitchRow)>>> = Rc::new(RefCell::new(Vec::new()));
    for t in &maps {
        let row = adw::SwitchRow::builder().title(glib::markup_escape_text(&t.ns)).subtitle(glib::markup_escape_text(&format!("map: {}", t.name))).active(t.id == chosen).build();
        me.layers_group.add(&row);
        me.layer_rows.borrow_mut().push(row.clone().upcast());
        switches.borrow_mut().push((t.id.clone(), row.clone()));
        let (me2, id, all) = (me.clone(), t.id.clone(), switches.clone());
        row.connect_active_notify(move |r| {
            if r.is_active() {
                if me2.base.borrow().as_deref() != Some(id.as_str()) {
                    set_base(&me2, &id);
                }
                for (other, sw) in all.borrow().iter() {
                    if *other != id && sw.is_active() {
                        sw.set_active(false);
                    }
                }
            } else if me2.base.borrow().as_deref() == Some(id.as_str()) {
                set_base(&me2, "");
            }
        });
    }
    if maps.is_empty() {
        let row = adw::ActionRow::builder().title("No map yet").subtitle("Add a map library with +").build();
        row.add_prefix(&gtk::Image::from_icon_name("dialog-information-symbolic"));
        me.layers_group.add(&row);
        me.layer_rows.borrow_mut().push(row.upcast());
    }
    let places = me.places.borrow();
    let mut libs: Vec<String> = places.iter().map(|p| p.ns.clone()).collect();
    libs.sort();
    libs.dedup();
    for ns in libs {
        let row = adw::SwitchRow::builder().title(glib::markup_escape_text(&ns)).subtitle(holds(&places, &ns)).active(!me.hidden.borrow().contains(&ns)).build();
        let me2 = me.clone();
        row.connect_active_notify(move |r| {
            if r.is_active() {
                me2.hidden.borrow_mut().remove(&ns);
            } else {
                me2.hidden.borrow_mut().insert(ns.clone());
            }
            draw(&me2);
        });
        me.layers_group.add(&row);
        me.layer_rows.borrow_mut().push(row.upcast());
    }
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
