#!/usr/bin/env python3
"""OpenStreetMap addresses as address records: a gazetteer searched in
retifs, with no internet service (ADR-0027).

Reads `osmium export -f geojsonseq` output on stdin and writes one record per
line as JSON, for `retifs-map-import places`. Not a record per house (Estonia
alone has 1.6 million): one per street, and one per settlement, each listing
its houses with where they are. Big ones are split into parts.

    osmium tags-filter in.osm.pbf nwr/addr:housenumber nwr/addr:housename -o addr.osm.pbf
    osmium export addr.osm.pbf -f geojsonseq -a type,id | osm-addresses.py > addresses.jsonl
    retifs-map-import places addresses.jsonl --ns NS --type application/x-vnd.retifs.address

A record:
    type    application/x-vnd.retifs.address
    kind    "street" | "settlement"
    name    "Raekoja plats" | "Vaida küla"
    in      the settlement a street is in ("Tallinn")
    geo     [lat, lon]: the middle of its houses
    houses  "label|lat|lon;…": a house's number (or its name, in the
            countryside), sorted
    find    search terms (blind-index exact match, ADR-0019): the beginnings
            of each word of the name and of the settlement, folded (lower
            case, no diacritics); a house name's whole words
    count   houses
"""
import json
import re
import sys
import unicodedata
from collections import defaultdict

ADDRESS_TYPE = "application/x-vnd.retifs.address"
# Houses per record: a record stays small (a few KB), and is fetched alone.
PART = 300
# Beginnings from this many letters (and every whole word).
PREFIX_FROM = 2
# Longer beginnings than this aren't needed to tell names apart.
PREFIX_TO = 12


def fold(text):
    """Lower case, no diacritics, words of letters and digits in any script:
    what a search compares (the map folds its query the same way)."""
    t = unicodedata.normalize("NFKD", text)
    t = "".join(c for c in t if not unicodedata.combining(c)).lower()
    return "".join(c if c.isalnum() else " " for c in t).split()


def beginnings(words):
    out = set()
    for w in words:
        out.add(w)
        for n in range(PREFIX_FROM, min(len(w), PREFIX_TO) + 1):
            out.add(w[:n])
    return out


def middle(geom):
    t, c = geom["type"], geom["coordinates"]
    if t == "Point":
        return c
    ring = c[0] if t == "Polygon" else c[0][0] if t == "MultiPolygon" else c if t == "LineString" else None
    if not ring:
        return None
    return [sum(p[0] for p in ring) / len(ring), sum(p[1] for p in ring) / len(ring)]


def median(xs):
    xs = sorted(xs)
    return xs[len(xs) // 2]


def natural(label):
    """House numbers in order: 2 before 10, 10 before 10a."""
    m = re.match(r"(\d+)(.*)", label)
    return (0, int(m.group(1)), m.group(2).lower()) if m else (1, 0, label.lower())


def clean(v):
    return " ".join(str(v).split())


# (street or "", settlement) → {label: (lat, lon)}
groups = defaultdict(dict)
# The same, by label in lower case: "1A" and "1a" are one house.
lowered = defaultdict(set)


def add(key, label, at):
    """A building in parts, or an entrance as well: the first one."""
    if label.lower() not in lowered[key]:
        lowered[key].add(label.lower())
        groups[key][label] = at

for line in sys.stdin:
    line = line.strip().lstrip("\x1e")
    if not line:
        continue
    f = json.loads(line)
    t = f.get("properties", {})
    number, housename = clean(t.get("addr:housenumber", "")), clean(t.get("addr:housename", ""))
    label = number or housename
    town = clean(t.get("addr:city", ""))
    street = clean(t.get("addr:street") or t.get("addr:place") or "")
    if not label or not (town or street):
        continue
    pt = middle(f["geometry"])
    if not pt:
        continue
    add((street, town), label, (round(pt[1], 6), round(pt[0], 6)))

# A street whose houses name no settlement joins the same street nearby that
# does (some houses of a street are tagged with their town, some aren't).
# Near: its middle within about a kilometre of one of the other's houses (a
# long street's own middle can be far from its end).
NEAR = 0.012  # degrees
by_street = defaultdict(list)
for (street, town), houses in groups.items():
    if street and town:
        by_street[street].append(town)
for (street, town) in [k for k in groups if k[0] and not k[1]]:
    houses = groups[(street, town)]
    lat, lon = median(h[0] for h in houses.values()), median(h[1] for h in houses.values())
    near = [(min(abs(la - lat) + abs(lo - lon) for la, lo in groups[(street, t)].values()), t) for t in by_street.get(street, [])]
    near = [x for x in near if x[0] < NEAR]
    if near:
        for label, at in groups.pop((street, town)).items():
            add((street, min(near)[1]), label, at)

n = 0


def emit(kind, name, town, houses, at):
    global n
    labels = sorted(houses, key=natural)
    name_words = fold(name) + fold(town)
    lat, lon = at
    for part in range(0, max(len(labels), 1), PART):
        these = labels[part:part + PART]
        find = beginnings(name_words)
        # House names (countryside farms) by their whole words: "Kase".
        for label in these:
            if not label[0].isdigit():
                find.update(fold(label))
        rec = {
            "type": ADDRESS_TYPE,
            "kind": kind,
            "name": name,
            "geo": [lat, lon],
            "houses": ";".join(f"{label.replace(';', ',').replace('|', '/')}|{houses[label][0]}|{houses[label][1]}" for label in these),
            "find": sorted(find),
            "count": len(labels),
        }
        if town and kind == "street":
            rec["in"] = town
        if len(labels) > PART:
            rec["part"] = part // PART + 1
        print(json.dumps(rec, ensure_ascii=False))
        n += 1


towns = defaultdict(dict)
for (street, town), houses in groups.items():
    if town:
        # Every house counts towards where its settlement is.
        for label, at in houses.items():
            towns[town].setdefault(f"{street} {label}", at)
    if street:
        emit("street", street, town, houses, (median(h[0] for h in houses.values()), median(h[1] for h in houses.values())))
for town, all_houses in towns.items():
    # A settlement lists the houses that have no street (farms by name).
    own = groups.get(("", town), {})
    emit("settlement", town, "", own, (median(h[0] for h in all_houses.values()), median(h[1] for h in all_houses.values())))
print(f"{n} address records from {sum(len(h) for h in groups.values())} houses", file=sys.stderr)
