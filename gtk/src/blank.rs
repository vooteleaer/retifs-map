//! The map without a map library: the land colour alone. libshumate's
//! vector renderer asks its style's one data source for tiles, and with no
//! source in the style it asks nothing (NULL) for every tile, over and over;
//! so the blank style names a source, and this one answers every tile with
//! an empty one (no layers: only the background is drawn).

use gtk::glib;
use shumate::prelude::*;
use shumate::subclass::prelude::*;
use std::{future::Future, pin::Pin};

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Empty;

    #[glib::object_subclass]
    impl ObjectSubclass for Empty {
        const NAME: &'static str = "RetifsMapEmptySource";
        type Type = super::Empty;
        type ParentType = shumate::DataSource;
    }

    impl ObjectImpl for Empty {}

    impl DataSourceImpl for Empty {
        fn tile_data_future(&self, _x: i32, _y: i32, _zoom: i32) -> Pin<Box<dyn Future<Output = Result<glib::Bytes, glib::Error>> + 'static>> {
            Box::pin(async { Ok(glib::Bytes::from_static(b"")) })
        }

        // The request answered at once (the renderer takes a request that's
        // already complete); the default would go through
        // tile_data_future, whose task the binding makes without its
        // source, which libshumate then refuses.
        fn start_request(&self, x: i32, y: i32, zoom: i32, _cancellable: Option<gtk::gio::Cancellable>) -> Option<shumate::DataSourceRequest> {
            let req = shumate::DataSourceRequest::new(x, y, zoom);
            req.emit_data(&glib::Bytes::from_static(b""), true);
            Some(req)
        }
    }
}

glib::wrapper! {
    pub struct Empty(ObjectSubclass<imp::Empty>) @extends shumate::DataSource;
}

/// The blank style's source, by this name.
pub const SOURCE: &str = "blank";

/// Nothing but the land colour: places still show, there's just no map under them.
pub const STYLE: &str = r##"{"version":8,"sources":{"blank":{"type":"vector","tiles":["retifs-blank:{z}/{x}/{y}"]}},"layers":[{"id":"background","type":"background","paint":{"background-color":"#f2efe9"}}]}"##;

/// A renderer drawing the blank map.
pub fn renderer() -> Option<shumate::VectorRenderer> {
    let r = shumate::VectorRenderer::new("blank", STYLE).ok()?;
    r.set_data_source(SOURCE, &glib::Object::new::<Empty>());
    Some(r)
}
