//! retifs Map for Linux: GTK 4 / libadwaita with libshumate's map over the
//! shared core. Every record with a place (ADR-0027): photos where they were
//! taken, contacts where they live, events where they happen, anything else
//! that says where it is. Contacts and events with an address but no
//! coordinates are placed with a click. It updates as the libraries change.

use std::cell::RefCell;
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

const KINDS: [(Kind, &str, &str); 4] = [
    (Kind::Photo, "Photos", "image-x-generic-symbolic"),
    (Kind::Person, "People", "avatar-default-symbolic"),
    (Kind::Event, "Events", "x-office-calendar-symbolic"),
    (Kind::Other, "Other", "mark-location-symbolic"),
];

fn icon(kind: Kind) -> &'static str {
    KINDS.iter().find(|(k, _, _)| *k == kind).map(|(_, _, i)| *i).unwrap_or("mark-location-symbolic")
}

struct App {
    core: Arc<Map>,
    smap: shumate::SimpleMap,
    layer: shumate::MarkerLayer,
    list: gtk::ListBox,
    unplaced_list: gtk::ListBox,
    unplaced_head: gtk::Label,
    filters: RefCell<Vec<(Kind, gtk::CheckButton)>>,
    banner: adw::Banner,
    card: adw::ActionRow,
    card_box: gtk::Box,
    places: RefCell<Vec<Place>>,
    unplaced: RefCell<Vec<Unplaced>>,
    placing: RefCell<Option<Unplaced>>,
    thumbs: RefCell<HashMap<String, gdk::Texture>>,
    libraries: RefCell<Vec<String>>,
    counts: RefCell<HashMap<String, u64>>,
    status: gtk::Label,
    fitted: RefCell<bool>,
}

fn build(gapp: &adw::Application) {
    let core = Arc::new(Map::new());

    // Sidebar: what to show, the places, and what could be placed.
    let filter_box = gtk::FlowBox::builder().selection_mode(gtk::SelectionMode::None).max_children_per_line(2).margin_start(12).margin_end(12).build();
    let mut filters = Vec::new();
    for (kind, name, _) in KINDS {
        let c = gtk::CheckButton::builder().label(name).active(true).build();
        filter_box.append(&c);
        filters.push((kind, c));
    }
    let list = gtk::ListBox::new();
    list.add_css_class("navigation-sidebar");
    let unplaced_head = gtk::Label::builder().label("Not on the map yet").xalign(0.0).margin_start(18).margin_top(12).css_classes(["dim-label", "caption-heading"]).build();
    let unplaced_list = gtk::ListBox::new();
    unplaced_list.add_css_class("navigation-sidebar");
    let status = gtk::Label::builder().xalign(0.0).margin_start(12).margin_bottom(8).css_classes(["dim-label", "caption"]).build();
    let side_inner = gtk::Box::new(gtk::Orientation::Vertical, 6);
    side_inner.append(&filter_box);
    side_inner.append(&list);
    side_inner.append(&unplaced_head);
    side_inner.append(&unplaced_list);
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
        list: list.clone(),
        unplaced_list: unplaced_list.clone(),
        unplaced_head,
        filters: RefCell::new(filters),
        banner: banner.clone(),
        card,
        card_box,
        places: RefCell::new(Vec::new()),
        unplaced: RefCell::new(Vec::new()),
        placing: RefCell::new(None),
        thumbs: RefCell::new(HashMap::new()),
        libraries: RefCell::new(Vec::new()),
        counts: RefCell::new(HashMap::new()),
        status,
        fitted: RefCell::new(false),
    });
    for (_, c) in me.filters.borrow().iter() {
        let me2 = me.clone();
        c.connect_toggled(move |_| show(&me2));
    }
    {
        let me2 = me.clone();
        list.connect_row_activated(move |_, row| {
            let p = visible(&me2).get(row.index() as usize).cloned();
            if let Some(p) = p {
                focus(&me2, &p);
            }
        });
    }
    {
        let me2 = me.clone();
        unplaced_list.connect_row_activated(move |_, row| {
            let u = me2.unplaced.borrow().get(row.index() as usize).cloned();
            if let Some(u) = u {
                me2.banner.set_title(&format!("Click the map where {} is ({})", u.title, u.address));
                me2.banner.set_revealed(true);
                *me2.placing.borrow_mut() = Some(u);
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
            }
            Err(e) => me2.status.set_text(&format!("retifsd isn't answering: {e}")),
        }
        show(&me2);
    });
}

fn visible(me: &Rc<App>) -> Vec<Place> {
    let on: HashSet<Kind> = me.filters.borrow().iter().filter(|(_, c)| c.is_active()).map(|(k, _)| *k).collect();
    me.places.borrow().iter().filter(|p| on.contains(&p.kind)).cloned().collect()
}

fn show(me: &Rc<App>) {
    let places = visible(me);
    me.layer.remove_all();
    me.list.remove_all();
    for p in &places {
        me.layer.add_marker(&marker(me, p));
        let sub = [p.when.replace('T', " ").get(..16).unwrap_or("").to_string(), if p.label.is_empty() { String::new() } else { p.label.clone() }, p.ns.clone()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
        let row = adw::ActionRow::builder().title(glib::markup_escape_text(&p.title)).subtitle(glib::markup_escape_text(&sub)).activatable(true).build();
        row.add_prefix(&gtk::Image::from_icon_name(icon(p.kind)));
        me.list.append(&row);
    }
    me.unplaced_list.remove_all();
    for u in me.unplaced.borrow().iter() {
        let row = adw::ActionRow::builder().title(glib::markup_escape_text(&u.title)).subtitle(glib::markup_escape_text(&u.address)).activatable(true).build();
        row.add_prefix(&gtk::Image::from_icon_name(icon(u.kind)));
        row.add_suffix(&gtk::Label::builder().label("Place").css_classes(["dim-label"]).build());
        me.unplaced_list.append(&row);
    }
    me.unplaced_head.set_visible(!me.unplaced.borrow().is_empty());
    if !me.status.text().starts_with("retifsd") {
        me.status.set_text(&format!("{} on the map", places.len()));
    }
    if !*me.fitted.borrow() && !places.is_empty() {
        fit(me, &places);
        *me.fitted.borrow_mut() = true;
    }
    load_thumbs(me, places);
}

/// A marker: a photo as a small round picture, anything else as its icon.
fn marker(me: &Rc<App>, p: &Place) -> shumate::Marker {
    let m = shumate::Marker::new();
    m.set_location(p.lat, p.lon);
    let button = gtk::Button::builder().css_classes(["circular", "osd"]).tooltip_text(&p.title).build();
    match me.thumbs.borrow().get(&p.id) {
        Some(t) => {
            let pic = gtk::Image::builder().paintable(t).pixel_size(MARK).overflow(gtk::Overflow::Hidden).css_classes(["circular"]).build();
            button.set_child(Some(&pic));
            button.add_css_class("flat");
        }
        None => button.set_icon_name(icon(p.kind)),
    }
    let (me2, p2) = (me.clone(), p.clone());
    button.connect_clicked(move |_| focus(&me2, &p2));
    m.set_child(Some(&button));
    m
}

/// Shows a place's card and centres on it.
fn focus(me: &Rc<App>, p: &Place) {
    me.card.set_title(&glib::markup_escape_text(&p.title));
    let kind = KINDS.iter().find(|(k, _, _)| *k == p.kind).map(|(_, n, _)| *n).unwrap_or("");
    let label = if p.label.is_empty() { String::new() } else { format!(" ({})", p.label) };
    me.card.set_subtitle(&glib::markup_escape_text(&format!("{kind}{label} · {} · {:.5}, {:.5}{}", p.ns, p.lat, p.lon, if p.when.is_empty() { String::new() } else { format!(" · {}", p.when.replace('T', " ")) })));
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
    let span = (lat1 - lat0).max(lon1 - lon0).max(0.005);
    let zoom = (360.0 / span).log2().clamp(2.0, 16.0) - 0.5;
    if let (Some(map), Some(vp)) = (me.smap.map(), me.smap.viewport()) {
        vp.set_zoom_level(zoom);
        map.center_on((lat0 + lat1) / 2.0, (lon0 + lon1) / 2.0);
    }
}

/// The photos' thumbnails for their markers, a batch at a time.
fn load_thumbs(me: &Rc<App>, places: Vec<Place>) {
    let missing: Vec<Place> = places.into_iter().filter(|p| p.kind == Kind::Photo && p.thumb.is_some() && !me.thumbs.borrow().contains_key(&p.id)).take(24).collect();
    if missing.is_empty() {
        return;
    }
    let me2 = me.clone();
    bg(&me.core, move |m| missing.iter().map(|p| (p.id.clone(), m.thumb(p))).collect::<Vec<_>>(), move |got| {
        let mut any = false;
        for (id, bytes) in got {
            let t = bytes.and_then(|b| square(&b));
            // Even a failed one is remembered, so it isn't asked again.
            if let Some(t) = t {
                me2.thumbs.borrow_mut().insert(id, t);
                any = true;
            }
        }
        if any {
            show(&me2);
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
