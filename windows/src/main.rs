//! retifs Map for Windows: the libraries on the left (Announces above
//! them), the map on the right, drawn by the app itself from a map
//! library's vector tiles with Direct2D, as the Haiku app draws them. No
//! web view and no internet service: the map, its places and the addresses
//! searched all come from retifs libraries.

#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod canvas;
#[cfg(windows)]
mod tiles;
#[cfg(windows)]
mod work;

#[cfg(windows)]
fn main() {
    app::run();
}

#[cfg(not(windows))]
fn main() {
    eprintln!("retifs-map-win is the Windows front end; on Linux use retifs-map-gtk");
}
