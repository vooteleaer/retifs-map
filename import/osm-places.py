#!/usr/bin/env python3
"""OpenStreetMap points of interest as place records (ADR-0027, ADR-0028).

Reads `osmium export -f geojsonseq` output on stdin and writes one record per
line for `retifs rec import` (KEY=VALUE, tab-separated). Areas become a point
at the middle of their outline.

    osmium tags-filter in.osm.pbf nwr/amenity nwr/shop ... -o poi.osm.pbf
    osmium export poi.osm.pbf -f geojsonseq -a type,id | osm-places.py > places.tsv
    retifs rec import NS < places.tsv
"""
import json
import sys

PLACE_TYPE = "application/x-vnd.retifs.place"
# The tag that says what a place is, in order of preference.
CLASSES = ["amenity", "shop", "tourism", "leisure", "historic", "healthcare", "office", "craft", "natural", "man_made", "sport"]
# Natural features worth a place; the rest of `natural` (wood, water) is map.
NATURAL = {"peak", "spring", "beach", "cave_entrance", "bay", "cape", "rock", "stone", "waterfall"}
SKIP = {"parking", "parking_space", "bench", "waste_basket", "bicycle_parking", "toilets", "vending_machine", "shelter_type", "grit_bin", "recycling", "post_box", "telephone", "fountain", "hunting_stand", "fire_hydrant"}
KEEP = {"opening_hours": "opening_hours", "website": "website", "contact:website": "website", "phone": "phone", "contact:phone": "phone", "wheelchair": "wheelchair", "cuisine": "cuisine", "description": "note", "wikipedia": "wikipedia"}


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


def clean(v):
    return " ".join(str(v).split()).replace("=", "-")


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
        "geo": f"{lat:.6f},{lon:.6f}",
        "category": clean(tags[cls]),
        "osm:class": cls,
        "source": f"osm:{tags.get('@type', 'node')}/{tags.get('@id', '')}",
    }
    street = " ".join(x for x in [tags.get("addr:street"), tags.get("addr:housenumber")] if x)
    addr = ", ".join(x for x in [street, tags.get("addr:city") or tags.get("addr:place")] if x)
    if addr:
        rec["address"] = clean(addr)
    for k, to in KEEP.items():
        if k in tags and to not in rec:
            rec[to] = clean(tags[k])
    # Quoted, so commas stay text (only geo is a list).
    print("\t".join(f"{k}={v}" if k == "geo" else f'{k}="{v}"' for k, v in rec.items()))
    n += 1
print(f"{n} places", file=sys.stderr)
