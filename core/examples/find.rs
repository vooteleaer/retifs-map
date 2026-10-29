//! Searches the address libraries joined here: `cargo run --example find -- raekoja plats 1 tallinn`.
fn main() {
    let text = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    let t = std::time::Instant::now();
    match retifs_map_core::Map::new().find(&text) {
        Ok(found) => found.iter().for_each(|f| println!("{:.5}, {:.5}  z{}  {}", f.lat, f.lon, f.zoom, f.name)),
        Err(e) => eprintln!("{e}"),
    }
    eprintln!("{:.2} s", t.elapsed().as_secs_f64());
}
