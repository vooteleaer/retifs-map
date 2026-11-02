//! The map's place icons (the core's Maki set) as GTK textures and CSS.

use std::cell::RefCell;
use std::collections::HashMap;

use gtk::{gdk, gio, glib};
pub use retifs_map_core::icons::{style_of, GROUPS};

thread_local! {
    static CACHE: RefCell<HashMap<&'static str, Option<gdk::Texture>>> = RefCell::new(HashMap::new());
}

/// An icon, white, drawn at twice `size` (sharp on HiDPI screens).
pub fn texture(name: &'static str, size: i32) -> Option<gdk::Texture> {
    CACHE.with(|c| {
        c.borrow_mut()
            .entry(name)
            .or_insert_with(|| {
                let white = retifs_map_core::icons::white_svg(name)?;
                let stream = gio::MemoryInputStream::from_bytes(&glib::Bytes::from_owned(white.into_bytes()));
                let pb = gtk::gdk_pixbuf::Pixbuf::from_stream_at_scale(&stream, size * 2, size * 2, true, gio::Cancellable::NONE).ok()?;
                Some(gdk::Texture::for_pixbuf(&pb))
            })
            .clone()
    })
}

/// The CSS for the circles' colours.
pub fn css() -> String {
    GROUPS.iter().map(|(g, c)| format!(".pc-{g} {{ background: {c}; }} ")).collect()
}
