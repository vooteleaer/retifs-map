# retifs Map

A map on [retifs](https://github.com/vooteleaer/retifs): every record that says where it is (`geo`, or a labelled `geo:<label>` such as a contact's home, ADR-0027), whatever it is: photos, contacts, events, saved places. The map owns none of them; it shows them, and lets you place a contact or an event whose address has no coordinates yet.

> A demo app of [retifs](https://github.com/vooteleaer/retifs), part of its test release. To try it, start with [getting started](https://github.com/vooteleaer/retifs/blob/main/docs/getting-started.md).

- **No internet services.** The map itself is a retifs library too (ADR-0028): vector tiles as records, followed like any library and kept or fetched as needed, so it works offline and over Reticulum alone. Addresses and places to search come from libraries as well (built from OpenStreetMap with `import/`).
- **Libraries, not layers:** the sidebar lists your libraries (with the public ones heard announced above them); each is shown or not, with how many places it has.
- **Search** in the bar at the top: addresses and places in the libraries shown, as you type.
- Markers grouped by zoom (tens of thousands of photos stay usable); a photo's marker opens it, a contact's or an event's shows what it is.

`core/` talks to `retifsd` through libretifs. Front ends:
- `gtk/`: Linux (GTK 4, libadwaita, libshumate). Build on Linux (or WSL): `cargo build -p retifs-map-gtk`.
- `android/`: the phone app (Kotlin, Compose, the core through JNI). It asks the retifs app (`org.retifs.desktop`) for each connection to `retifsd`, like the other retifs apps. Build: `powershell -ExecutionPolicy Bypass -File android\build-apk.ps1 [-Abis arm64-v8a]` (NDK 27, JDK 17, retifs next to this repository).
- `haiku/`: a BeAPI app over the core's C API.

## Making map libraries

`import/` builds them:

```sh
# tiles (from tilemaker) into a library, one record per tile, then publish it
retifs-map-import tiles DIR --ns eesti-kaart --name "Eesti" --style styles/osm-light.json
# places and addresses from an OpenStreetMap extract
osmium export poi.osm.pbf -f geojsonseq -a type,id | import/osm-places.py > places.jsonl
retifs-map-import places places.jsonl --ns eesti-kohad --index cell,type,find
```

Announce the library (`retifs ns announce NS`) and every retifs node that hears it can follow it.

## License

[MIT](LICENSE)
