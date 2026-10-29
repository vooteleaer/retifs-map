#!/usr/bin/env python3
"""OpenStreetMap points of interest as place records (ADR-0027, ADR-0028).

Reads `osmium export -f geojsonseq` output on stdin and writes one record per
line as JSON, for `retifs-map-import places`. Areas become a point at the
middle of their outline. Each place gets a `minzoom`: from which zoom a map
shows it, by how much it matters (as OpenStreetMap's own map does).

    osmium tags-filter in.osm.pbf nwr/amenity nwr/shop ... -o poi.osm.pbf
    osmium export poi.osm.pbf -f geojsonseq -a type,id | osm-places.py > places.jsonl
    retifs-map-import places places.jsonl --ns NS --index cell,type,find
"""
import json
import math
import sys
import unicodedata

PLACE_TYPE = "application/x-vnd.retifs.place"
# The tag that says what a place is, in order of preference.
CLASSES = ["amenity", "shop", "tourism", "leisure", "historic", "healthcare", "office", "craft", "natural", "man_made", "sport"]
# Natural features worth a place; the rest of `natural` (wood, water) is map.
NATURAL = {"peak", "spring", "beach", "cave_entrance", "bay", "cape", "rock", "stone", "waterfall"}
SKIP = {"parking", "parking_space", "bench", "waste_basket", "bicycle_parking", "toilets", "vending_machine", "shelter_type", "grit_bin", "recycling", "post_box", "telephone", "fountain", "hunting_stand", "fire_hydrant"}
KEEP = {"opening_hours": "opening_hours", "website": "website", "contact:website": "website", "phone": "phone", "contact:phone": "phone", "wheelchair": "wheelchair", "cuisine": "cuisine", "description": "note", "wikipedia": "wikipedia"}


# From which zoom a place shows: the landmarks early, the everyday later, the
# small things only close up. Anything not listed: 16 (a shop: 15).
ZOOM = {
    12: {"hospital", "university", "castle", "manor", "zoo", "theme_park", "stadium", "aerodrome", "airport", "ferry_terminal"},
    13: {"museum", "theatre", "cinema", "townhall", "college", "place_of_worship", "monastery", "hotel", "camp_site", "viewpoint",
         "attraction", "peak", "beach", "library", "police", "fire_station", "ruins", "monument", "fort", "lighthouse", "spa",
         "nature_reserve", "golf_course", "marina", "arts_centre", "courthouse", "bus_station", "mall", "department_store"},
    14: {"supermarket", "fuel", "school", "guest_house", "hostel", "chalet", "sports_centre", "swimming_pool", "park",
         "community_centre", "post_office", "pharmacy", "clinic", "memorial", "archaeological_site", "spring", "waterfall",
         "cave_entrance", "gallery", "information_office", "charging_station"},
    15: {"restaurant", "cafe", "bar", "pub", "fast_food", "bank", "doctors", "dentist", "kindergarten", "convenience",
         "bakery", "car_repair", "hardware", "clothes", "veterinary", "marketplace", "swimming_area", "sauna", "car_wash"},
    17: {"information", "picnic_site", "playground", "bbq", "firepit", "drinking_water", "atm", "boundary_stone", "wayside_cross",
         "wayside_shrine", "stone", "rock", "pitch", "fitness_station", "bird_hide", "hunting_stand"},
}


def minzoom(cls, value, tags):
    z = next((z for z, names in ZOOM.items() if value in names), 15 if cls == "shop" else 16)
    # Better known: earlier (it has an encyclopedia article).
    if "wikipedia" in tags or "wikidata" in tags:
        z -= 2
    return max(z, 10)


# An area wider than this (degrees) isn't a place on the map but a region
# (a bay, a national park): its middle can be anywhere.
MAX_SPAN = 0.1


def middle(geom):
    t = geom["type"]
    c = geom["coordinates"]
    if t == "Point":
        return c
    ring = c[0] if t == "Polygon" else c[0][0] if t == "MultiPolygon" else c if t == "LineString" else None
    if not ring:
        return None
    xs, ys = [p[0] for p in ring], [p[1] for p in ring]
    if max(xs) - min(xs) > MAX_SPAN or max(ys) - min(ys) > MAX_SPAN:
        return None
    return [sum(xs) / len(ring), sum(ys) / len(ring)]


# The cell a place is in, "z/x/y": a map asks for the cells on screen (exact
# terms, so a lazily held library answers them through its holder's blind
# index, ADR-0019) instead of every place there is. Two levels, as tiles are:
# places shown from zoom 13 or earlier (landmarks) in zoom-12 cells, a few
# dozen to a cell; the rest in zoom-14 cells, asked for only from zoom 14.
COARSE, FINE = 12, 14


def cell(lat, lon, z):
    n = 2 ** z
    x = int((lon + 180.0) / 360.0 * n)
    phi = math.radians(max(min(lat, 85.05), -85.05))
    y = int((1.0 - math.log(math.tan(phi) + 1.0 / math.cos(phi)) / math.pi) / 2.0 * n)
    return f"{z}/{x}/{y}"


def clean(v):
    return " ".join(str(v).split())


def fold(text):
    """Lower case, no diacritics, words of letters and digits in any script
    (as osm-addresses.py and the map's search fold them)."""
    t = unicodedata.normalize("NFKD", text)
    t = "".join(c for c in t if not unicodedata.combining(c)).lower()
    return "".join(c if c.isalnum() else " " for c in t).split()


def find_terms(name, whole):
    """Search terms (blind-index exact match, ADR-0019): the beginnings of
    the name's words, from two letters; `whole`'s words as they are (what it
    is, where: "cafe", "tartu")."""
    out = set(fold(whole))
    for w in fold(name):
        out.add(w)
        out.update(w[:n] for n in range(2, min(len(w), 12) + 1))
    return sorted(out)


n = 0
for line in sys.stdin:
    line = line.strip().lstrip("\x1e")
    if not line:
        continue
    f = json.loads(line)
    tags = f.get("properties", {})
    name = tags.get("name")
    if not name:
        continue
    cls = next((c for c in CLASSES if c in tags and (c != "natural" or tags[c] in NATURAL)), None)
    if not cls or tags[cls] in SKIP:
        continue
    pt = middle(f["geometry"])
    if not pt:
        continue
    lon, lat = pt
    rec = {
        "type": PLACE_TYPE,
        "name": clean(name),
        "geo": [round(lat, 6), round(lon, 6)],
        "category": clean(tags[cls]),
        "minzoom": minzoom(cls, tags[cls], tags),
        "osm:class": cls,
        "source": f"osm:{tags.get('@type', 'node')}/{tags.get('@id', '')}",
    }
    rec["cell"] = cell(lat, lon, COARSE if rec["minzoom"] <= 13 else FINE)
    street = " ".join(x for x in [tags.get("addr:street"), tags.get("addr:housenumber")] if x)
    addr = ", ".join(x for x in [street, tags.get("addr:city") or tags.get("addr:place")] if x)
    if addr:
        rec["address"] = clean(addr)
    for k, to in KEEP.items():
        if k in tags and to not in rec:
            rec[to] = clean(tags[k])
    rec["find"] = find_terms(rec["name"], " ".join([rec["category"], rec.get("address", ""), tags.get("addr:city", "")]))
    print(json.dumps(rec, ensure_ascii=False))
    n += 1
print(f"{n} places", file=sys.stderr)
