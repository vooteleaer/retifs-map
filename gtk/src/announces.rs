//! Announces, as in LXMF apps: the public libraries heard on the network
//! (ADR-0029), those the map can use, each with Join. It updates while open.

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use libretifs::HeardLibrary;

use crate::{bg, reload, App};

/// "map · 38822 places · 120 photos": what of it the map shows.
fn holds(l: &HeardLibrary) -> String {
    let mut out = Vec::new();
    for (t, n) in &l.has {
        let word = match t.as_str() {
            retifs_map_core::TILESET_TYPE => Some("map".to_string()),
            retifs_map_core::VECTOR_TILE => Some(format!("{n} map tiles")),
            retifs_map_core::PLACE_TYPE => Some(format!("{n} places")),
            t if t.starts_with("image/") => Some(format!("{n} photos")),
            t if libretifs::types::is(t, libretifs::types::PERSON) => Some(format!("{n} contacts")),
            t if libretifs::types::is(t, libretifs::types::EVENT) => Some(format!("{n} events")),
            _ => None,
        };
        out.extend(word);
    }
    out.join(" · ")
}

fn ago(ms: u64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    match now.saturating_sub(ms) / 1000 {
        s if s < 60 => "just now".into(),
        s if s < 3600 => format!("{} min ago", s / 60),
        s => format!("{} h ago", s / 3600),
    }
}

pub fn open(me: &Rc<App>) {
    let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build();
    let empty = adw::StatusPage::builder()
        .icon_name("network-wireless-symbolic")
        .title("Nothing heard yet")
        .description("Public libraries announced on the network show up here: maps, places and more. Announces come every few minutes.")
        .build();
    let more = gtk::Label::builder().xalign(0.0).wrap(true).css_classes(["dim-label", "caption"]).build();
    let body = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).margin_start(12).margin_end(12).margin_top(12).margin_bottom(12).build();
    body.append(&gtk::Label::builder().label("Public libraries heard on the network. Joining fetches only what you look at.").xalign(0.0).wrap(true).css_classes(["dim-label"]).build());
    body.append(&list);
    body.append(&more);
    let stack = gtk::Stack::new();
    stack.add_named(&empty, Some("empty"));
    stack.add_named(&gtk::ScrolledWindow::builder().child(&body).vexpand(true).build(), Some("list"));
    let view = adw::ToolbarView::new();
    view.add_top_bar(&adw::HeaderBar::new());
    view.set_content(Some(&stack));
    let dialog = adw::Dialog::builder().title("Announces").content_width(520).content_height(560).child(&view).build();

    let fill: Rc<dyn Fn()> = {
        let (me, list, stack, more) = (me.clone(), list.clone(), stack.clone(), more.clone());
        Rc::new(move || {
            let (me2, list, stack, more) = (me.clone(), list.clone(), stack.clone(), more.clone());
            bg(&me.core, |m| m.heard(), move |r| {
                let heard = r.unwrap_or_default();
                let (usable, other): (Vec<HeardLibrary>, Vec<HeardLibrary>) = heard.into_iter().partition(|l| !l.readable || retifs_map_core::map_uses(l));
                list.remove_all();
                for l in &usable {
                    let sub = if l.readable {
                        format!("{}\n{} · from <{}…>, {} hop(s), {}", l.about, holds(l), &l.node[..8.min(l.node.len())], l.hops, ago(l.heard_ms))
                    } else {
                        format!("Needs a newer retifs (announce v{}, format {})", l.v, l.fmt)
                    };
                    let title = if l.name.is_empty() { "A public library".to_string() } else { l.name.clone() };
                    let row = adw::ActionRow::builder().title(glib::markup_escape_text(&title)).subtitle(glib::markup_escape_text(&sub)).subtitle_lines(3).build();
                    row.add_prefix(&gtk::Image::from_icon_name(if l.has.iter().any(|(t, _)| t == retifs_map_core::TILESET_TYPE || t == retifs_map_core::VECTOR_TILE) { "map-symbolic" } else { "mark-location-symbolic" }));
                    match (&l.here, l.readable) {
                        (Some(ns), _) => row.add_suffix(&gtk::Label::builder().label("Already joined").tooltip_text(format!("Here as {ns}")).css_classes(["dim-label"]).build()),
                        (None, false) => {}
                        (None, true) => {
                            let join = gtk::Button::builder().label("Join").valign(gtk::Align::Center).css_classes(["suggested-action"]).build();
                            let (me3, l2) = (me2.clone(), l.clone());
                            join.connect_clicked(move |b| {
                                b.set_sensitive(false);
                                b.set_label("Joining…");
                                let (me4, b2, l3) = (me3.clone(), b.clone(), l2.clone());
                                bg(&me3.core, move |m| m.join_heard(&l3), move |r| match r {
                                    Ok(ns) => {
                                        b2.set_label("Already joined");
                                        b2.set_tooltip_text(Some(&format!("Here as {ns}")));
                                        reload(&me4);
                                    }
                                    Err(e) => {
                                        b2.set_label("Join");
                                        b2.set_sensitive(true);
                                        me4.status.set_text(&format!("Not joined: {e}"));
                                    }
                                });
                            });
                            row.add_suffix(&join);
                        }
                    }
                    list.append(&row);
                }
                more.set_text(&match other.len() {
                    0 => String::new(),
                    n => format!("{n} more heard that the map doesn't show (other kinds of data; the desktop app lists them)."),
                });
                more.set_visible(!other.is_empty());
                stack.set_visible_child_name(if usable.is_empty() && other.is_empty() { "empty" } else { "list" });
            });
        })
    };
    fill();
    // While open, again every 5 s: announces keep coming.
    let alive = Rc::new(std::cell::Cell::new(true));
    {
        let (fill, alive) = (fill.clone(), alive.clone());
        glib::timeout_add_seconds_local(5, move || {
            if !alive.get() {
                return glib::ControlFlow::Break;
            }
            fill();
            glib::ControlFlow::Continue
        });
    }
    dialog.connect_closed(move |_| alive.set(false));
    dialog.present(Some(&me.window));
}
