//! Looks an address up (address search must be on): `cargo run --example lookup -- "Kalda, Prangli"`.
fn main() {
    let text = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    match retifs_map_core::Map::new().lookup(&text) {
        Ok(found) => found.iter().for_each(|f| println!("{:.5}, {:.5}  {}", f.lat, f.lon, f.name)),
        Err(e) => eprintln!("{e}"),
    }
}
