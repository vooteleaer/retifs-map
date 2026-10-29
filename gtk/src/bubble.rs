//! The info bubble of a place clicked on the map: over the map (a GTK
//! overlay), kept pointing at its place as the map moves and zooms. What it
//! shows depends on the record: a photo's preview (a click opens it larger), a
//! contact's details, an event's time and place, a place's category and hours.

use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, glib};
use retifs_map_core::{Kind, Place};

use crate::{bg, kind_name, App};

/// The bubble's width.
const WIDTH: i32 = 300;

/// Shows the bubble for `p` (`here`: how many records are at that spot).
pub fn show(me: &Rc<App>, p: &Place, here: usize) {
    close(me);
    me.bubble_opened.set(std::time::Instant::now());
    let card = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).width_request(WIDTH).css_classes(["card", "map-bubble"]).build();
    // Title row: the name, and a close button.
    let head = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let title = if here > 1 { format!("{} (and {} more here)", p.title, here - 1) } else { p.title.clone() };
    head.append(&gtk::Label::builder().label(&title).xalign(0.0).wrap(true).max_width_chars(CHARS - 4).hexpand(true).css_classes(["heading"]).build());
    let close = gtk::Button::builder().icon_name("window-close-symbolic").valign(gtk::Align::Start).css_classes(["flat", "circular"]).build();
    let me2 = me.clone();
    close.connect_clicked(move |_| self::close(&me2));
    head.append(&close);
    card.append(&head);
    let what = if p.detail.is_empty() { kind_name(p.kind).to_string() } else { p.detail.clone() };
    card.append(&dim(&format!("{what} · {}", p.ns)));
    // The body: filled in once the details (and a photo's preview) are here.
    let body = gtk::Box::new(gtk::Orientation::Vertical, 4);
    card.append(&body);
    if p.kind == Kind::Photo {
        photo(me, p, &body);
    }
    let (me2, p2, body2) = (me.clone(), p.clone(), body.clone());
    let (ns, id) = (p.ns.clone(), p.id.clone());
    bg(&me.core, move |m| m.details(&ns, &id), move |attrs| {
        fill(&me2, &p2, &body2, &attrs);
        let me3 = me2.clone();
        glib::idle_add_local_once(move || place(&me3));
    });

    // The card with a pointer below it, over the map, above its place.
    let arrow = gtk::Label::builder().label("▼").css_classes(["map-bubble-arrow"]).build();
    let upper = gtk::Box::builder().orientation(gtk::Orientation::Vertical).halign(gtk::Align::Start).valign(gtk::Align::Start).build();
    upper.append(&card);
    upper.append(&arrow);
    me.map_overlay.add_overlay(&upper);
    *me.bubble.borrow_mut() = Some((p.lat, p.lon, upper.upcast()));
    place(me);
    // Its size settles once the details (and a photo) are in.
    let me2 = me.clone();
    glib::timeout_add_local_once(std::time::Duration::from_millis(50), move || place(&me2));
}

/// Takes the bubble away.
pub fn close(me: &Rc<App>) {
    if let Some((_, _, w)) = me.bubble.borrow_mut().take() {
        me.map_overlay.remove_overlay(&w);
    }
}

/// Puts the bubble over its place: its pointer's tip on the place, kept on
/// the map's area.
pub fn place(me: &Rc<App>) {
    let Some((lat, lon, w)) = me.bubble.borrow().clone() else { return };
    let Some(vp) = me.smap.viewport() else { return };
    let (x, y) = vp.location_to_widget_coords(&me.smap, lat, lon);
    let (_, width, _, _) = w.measure(gtk::Orientation::Horizontal, -1);
    let (_, height, _, _) = w.measure(gtk::Orientation::Vertical, width);
    let max_x = (me.smap.width() - width).max(0) as f64;
    w.set_margin_start((x - width as f64 / 2.0).clamp(0.0, max_x) as i32);
    w.set_margin_top((y - height as f64 + 4.0).max(0.0) as i32);
    // Off the map's edge: hidden until it's back.
    w.set_visible(x >= 0.0 && y >= 0.0 && x <= me.smap.width() as f64 && y <= me.smap.height() as f64);
}

/// An image scaled down to fit `w`×`h` (pixels; drawn at half that, sharp on
/// HiDPI), keeping its shape.
fn fitted(bytes: &[u8], w: i32, h: i32) -> Option<gdk::Texture> {
    let pb = gtk::gdk_pixbuf::Pixbuf::from_read(std::io::Cursor::new(bytes.to_vec())).ok()?;
    let scale = (w as f64 / pb.width() as f64).min(h as f64 / pb.height() as f64).min(1.0);
    let small = pb.scale_simple(((pb.width() as f64 * scale) as i32).max(1), ((pb.height() as f64 * scale) as i32).max(1), gtk::gdk_pixbuf::InterpType::Bilinear)?;
    Some(gdk::Texture::for_pixbuf(&small))
}

/// Labels wrap at this many characters, so the bubble keeps its width.
const CHARS: i32 = 36;

fn dim(text: &str) -> gtk::Label {
    gtk::Label::builder().label(text).xalign(0.0).wrap(true).max_width_chars(CHARS).css_classes(["dim-label", "caption"]).build()
}

fn line(body: &gtk::Box, label: &str, value: &str) {
    if value.trim().is_empty() {
        return;
    }
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.append(&gtk::Label::builder().label(label).xalign(0.0).width_chars(9).valign(gtk::Align::Start).css_classes(["dim-label", "caption"]).build());
    row.append(&gtk::Label::builder().label(value).xalign(0.0).wrap(true).max_width_chars(CHARS - 10).hexpand(true).selectable(true).build());
    body.append(&row);
}

/// "2026-10-25T17:00" as "25.10.2026 17:00".
fn when(t: &str) -> String {
    let (date, time) = t.split_once('T').unwrap_or((t, ""));
    let d: Vec<&str> = date.split('-').collect();
    let date = if d.len() == 3 { format!("{}.{}.{}", d[2], d[1], d[0]) } else { date.to_string() };
    format!("{date} {}", time.get(..5).unwrap_or(time)).trim().to_string()
}

/// The record's details, by what it is.
fn fill(_me: &Rc<App>, p: &Place, body: &gtk::Box, attrs: &[(String, String)]) {
    let get = |k: &str| attrs.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone()).unwrap_or_default();
    let labelled = |prefix: &str| -> Vec<(String, String)> {
        attrs.iter().filter_map(|(k, v)| {
            if k == prefix {
                Some((String::new(), v.clone()))
            } else {
                k.strip_prefix(&format!("{prefix}:")).map(|l| (l.to_string(), v.clone()))
            }
        }).collect()
    };
    match p.kind {
        Kind::Photo => {
            line(body, "Taken", &when(&get("taken")));
            line(body, "Camera", &[get("camera:make"), get("camera:model")].join(" "));
        }
        Kind::Person => {
            for (l, v) in labelled("phone") {
                line(body, if l.is_empty() { "Phone" } else { &l }, &v);
            }
            for (l, v) in labelled("email") {
                line(body, if l.is_empty() { "E-mail" } else { &l }, &v);
            }
            for (l, v) in labelled("address") {
                line(body, if l.is_empty() { "Address" } else { &l }, &v);
            }
            line(body, "Birthday", &get("birthday"));
            line(body, "Note", &get("note"));
        }
        Kind::Event => {
            let (start, end) = (get("start"), get("end"));
            let span = match (start.is_empty(), end.is_empty()) {
                (false, false) if start.get(..10) == end.get(..10) => format!("{} – {}", when(&start), end.get(11..16).unwrap_or(&end)),
                (false, false) => format!("{} – {}", when(&start), when(&end)),
                (false, true) => when(&start),
                _ => String::new(),
            };
            line(body, "When", &span);
            line(body, "Where", &get("location"));
            line(body, "Repeats", &get("rrule"));
            let people: Vec<String> = attrs.iter().filter(|(k, _)| k.starts_with("attendee:")).map(|(_, v)| v.clone()).collect();
            line(body, "With", &people.join(", "));
            line(body, "Note", &get("note"));
        }
        Kind::Other => {
            line(body, "Address", &get("address"));
            line(body, "Open", &get("opening_hours"));
            line(body, "Phone", &get("phone"));
            line(body, "Website", &get("website"));
            line(body, "Cuisine", &get("cuisine"));
            line(body, "Note", &get("note"));
        }
    }
    body.append(&dim(&format!("{:.5}, {:.5}", p.lat, p.lon)));
}

/// A photo's preview in the bubble; a click opens it larger.
fn photo(me: &Rc<App>, p: &Place, body: &gtk::Box) {
    // An Image of a fixed size: a Picture's height-for-width sizing inside a
    // map marker left the whole bubble unshown.
    let pic = gtk::Image::builder().pixel_size(WIDTH - 24).height_request(200).build();
    pic.add_css_class("map-bubble-photo");
    body.append(&pic);
    let cap = p.preview.clone().or_else(|| p.thumb.clone());
    let (pic2, ns, me4) = (pic.clone(), p.ns.clone(), me.clone());
    let cap2 = cap.clone();
    bg(&me.core, move |m| cap2.and_then(|c| m.image(&ns, &c, 8 << 20)), move |bytes| {
        // Scaled to the bubble: a marker is laid out at its child's natural
        // size, and a 1600 px picture's would push the bubble off the map.
        if let Some(t) = bytes.and_then(|b| fitted(&b, (WIDTH - 24) * 2, 400)) {
            pic2.set_paintable(Some(&t));
            let me3 = me4.clone();
            glib::idle_add_local_once(move || place(&me3));
        }
    });
    let click = gtk::GestureClick::new();
    let (me2, p2, pic3) = (me.clone(), p.clone(), pic.clone());
    click.connect_released(move |_, _, _, _| viewer(&me2, &p2, pic3.paintable()));
    pic.set_cursor_from_name(Some("pointer"));
    pic.add_controller(click);
}

/// The photo, large, in a window of its own.
fn viewer(me: &Rc<App>, p: &Place, shown: Option<gdk::Paintable>) {
    let pic = gtk::Picture::builder().content_fit(gtk::ContentFit::Contain).can_shrink(true).hexpand(true).vexpand(true).build();
    if let Some(t) = shown {
        pic.set_paintable(Some(&t));
    }
    // The preview at its own size (1600 px), in place of the bubble's small one.
    if let Some(cap) = p.preview.clone().or_else(|| p.thumb.clone()) {
        let (pic2, ns) = (pic.clone(), p.ns.clone());
        bg(&me.core, move |m| m.image(&ns, &cap, 8 << 20), move |bytes| {
            if let Some(t) = bytes.and_then(|b| gdk::Texture::from_bytes(&glib::Bytes::from_owned(b)).ok()) {
                pic2.set_paintable(Some(&t));
            }
        });
    }
    let view = adw::ToolbarView::new();
    view.add_top_bar(&adw::HeaderBar::new());
    view.set_content(Some(&pic));
    let d = adw::Dialog::builder().title(&p.title).content_width(1000).content_height(720).child(&view).build();
    d.present(Some(&me.window));
}
