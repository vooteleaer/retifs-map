//! retifs Map for Linux: GTK 4 / libadwaita with libshumate's map over the
//! shared core. Every record with a place (ADR-0027): photos where they were
//! taken, contacts where they live, events where they happen, anything else
//! that says where it is. Contacts and events with an address but no
//! coordinates are placed with a click. It updates as the libraries change.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use retifs_map_core::{Kind, Map, Place, Unplaced};
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
    layers: RefCell<Vec<(Kind, adw::SwitchRow)>>,
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
}

fn build(gapp: &adw::Application) {
    let core = Arc::new(Map::new());

    // Sidebar: the layers, and what could still be placed.
    let layers_group = adw::PreferencesGroup::builder().title("Layers").build();
    let mut layers = Vec::new();
    for (kind, name, icon_name) in KINDS {
        let row = adw::SwitchRow::builder().title(name).active(true).build();
        row.add_prefix(&gtk::Image::from_icon_name(icon_name));
        layers_group.add(&row);
        layers.push((kind, row));
    }
    let unplaced_group = adw::PreferencesGroup::builder()
        .title("Not on the map yet")
        .description("Contacts and events with an address but no place. Pick one, then click the map.")
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

    // The map, OpenStreetMap's tiles.
    let smap = shumate::SimpleMap::new();
    let registry = shumate::MapSourceRegistry::with_defaults();
    if let Some(src) = registry.by_id("osm-mapnik") {
        smap.set_map_source(Some(&src));
    }
    smap.set_vexpand(true);
    smap.set_hexpand(true);
    let viewport = smap.viewport().expect("a viewport");
    let layer = shumate::MarkerLayer::new(&viewport);
    smap.add_overlay_layer(&layer);
    let banner = adw::Banner::builder().button_label("Cancel").revealed(false).build();
    let card = adw::ActionRow::builder().build();
    card.add_css_class("card");
    let card_box = gtk::Box::builder().halign(gtk::Align::Center).valign(gtk::Align::End).margin_bottom(18).width_request(360).visible(false).build();
    card_box.append(&card);
    let overlay = gtk::Overlay::builder().child(&smap).build();
    overlay.add_overlay(&card_box);
    let content = adw::ToolbarView::new();
    content.add_top_bar(&adw::HeaderBar::new());
    content.add_top_bar(&banner);
    content.set_content(Some(&overlay));
    let content_page = adw::NavigationPage::builder().title("Map").child(&content).build();

    let split = adw::NavigationSplitView::builder().sidebar(&side_page).content(&content_page).build();
    let window = adw::ApplicationWindow::builder().application(gapp).title("Map").default_width(1100).default_height(720).content(&split).build();

    let me = Rc::new(App {
        core,
        smap: smap.clone(),
        layer,
        layers: RefCell::new(layers),
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
    });
    for (_, row) in me.layers.borrow().iter() {
        let me2 = me.clone();
        row.connect_active_notify(move |_| draw(&me2));
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
        });
    }
    // A click on the map while placing: there.
    {
        let me2 = me.clone();
        let click = gtk::GestureClick::new();
        click.connect_released(move |g, _, x, y| {
            let Some(u) = me2.placing.borrow_mut().take() else { return };
            let Some(w) = g.widget() else { return };
            let Some(vp) = me2.smap.viewport() else { return };
            let (lat, lon) = vp.widget_coords_to_location(&w, x, y);
            me2.banner.set_revealed(false);
            let me3 = me2.clone();
            bg(&me2.core, move |m| m.place(&u.ns, &u.id, &u.key, lat, lon), move |r| {
                if let Err(e) = r {
                    me3.status.set_text(&format!("Not placed: {e}"));
                }
                reload(&me3);
            });
        });
        smap.add_controller(click);
    }

    reload(&me);
    {
        let me2 = me.clone();
        glib::timeout_add_seconds_local(3, move || {
            let libs = me2.libraries.borrow().clone();
            let me3 = me2.clone();
            bg(&me2.core, move |m| m.change_counts(&libs), move |counts| {
                if !counts.is_empty() && counts != *me3.counts.borrow() {
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
        (libs, r, counts)
    }, move |(libs, r, counts)| {
        *me2.libraries.borrow_mut() = libs;
        *me2.counts.borrow_mut() = counts;
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
    let places = me.places.borrow();
    for (kind, row) in me.layers.borrow().iter() {
        let n = places.iter().filter(|p| p.kind == *kind).count();
        row.set_subtitle(&format!("{n} on the map"));
    }
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
            me2.banner.set_title(&format!("Click the map where {} is ({})", u2.title, u2.address));
            me2.banner.set_revealed(true);
            *me2.placing.borrow_mut() = Some(u2.clone());
        });
        me.unplaced_row.add_row(&row);
        me.unplaced_rows.borrow_mut().push(row.upcast());
    }
}

/// Places of the layers that are on.
fn visible(me: &Rc<App>) -> Vec<Place> {
    let on: HashSet<Kind> = me.layers.borrow().iter().filter(|(_, r)| r.is_active()).map(|(k, _)| *k).collect();
    me.places.borrow().iter().filter(|p| on.contains(&p.kind)).cloned().collect()
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
