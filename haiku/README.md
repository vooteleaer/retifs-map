# retifs Map for Haiku

The map with the Haiku API, over the shared core (`../core`) as a C library
(`capi/`, `retifs_map.h`).

- The map is drawn here: each tile's draw list (the map library's own
  MapLibre style applied by the core, `core/src/vector.rs`) filled and
  stroked with BShapes into bitmaps by three workers; labels over the whole
  view, so they don't collide across tiles. Until a tile comes, the coarser
  one is shown enlarged. Tiles come from map libraries in retifs only.
- The sidebar is the libraries: every library with something for the map
  (a map's tiles, places, photos, events, contacts) has a checkbox; a map is
  a library like the others. With several maps turned on, the one turned on
  last is drawn.
- Announces: the public libraries heard that the map can use, with Join.
- A marker clicked opens its bubble (a photo's preview, which opens larger;
  a contact's details; an event's time and place; a place's hours), or, for
  several places apart, zooms in on them. Photos show their thumbnails;
  markers that would overlap are pushed apart.
- The search field (Enter) finds addresses and places; picking one goes
  there. A lazy library's places (a map's cafés) are asked for by area from
  zoom 12.

## Building, on Haiku

    haiku/build.sh && haiku/package.sh

with retifs next to retifs-map. The result is `haiku/build/Map` and the
`retifs_map` package.

## Later

- Several maps over each other, each with its own opacity (a slider per map
  library): each map's tiles drawn into bitmaps of their own, composed with
  B_OP_ALPHA at a constant alpha.
- Adding and placing a place; a map's public places (cafés) with their
  category icons (Maki).
