//! Icons for the map's places: Maki (Mapbox's map icons, CC0, icons/maki),
//! white on a circle coloured by what kind of place it is, as OpenStreetMap's
//! own map does. Built into the app; each front end draws them (GTK with
//! its SVG loader, the Haiku app through the C library).

/// The colour groups: each group's circle colour.
pub const GROUPS: [(&str, &str); 11] = [
    ("food", "#c77400"),
    ("shop", "#ac39ac"),
    ("stay", "#0092da"),
    ("culture", "#734a08"),
    ("learn", "#8a6d3b"),
    ("health", "#bf0000"),
    ("nature", "#2e8b2e"),
    ("sport", "#39a0a0"),
    ("transport", "#0a5fbf"),
    ("faith", "#555555"),
    ("other", "#6b6b6b"),
];

/// A place's category (OSM's value, `_` as spaces) → its icon and group.
pub fn style_of(category: &str) -> (&'static str, &'static str) {
    match category {
        "restaurant" => ("restaurant", "food"),
        "cafe" => ("cafe", "food"),
        "fast food" => ("fast-food", "food"),
        "bar" | "pub" | "biergarten" => ("bar", "food"),
        "bakery" | "pastry" | "confectionery" => ("bakery", "food"),
        "ice cream" => ("ice-cream", "food"),
        "alcohol" | "beverages" | "wine" => ("alcohol-shop", "shop"),
        "supermarket" | "greengrocer" | "farm" | "butcher" => ("grocery", "shop"),
        "convenience" | "kiosk" => ("convenience", "shop"),
        "clothes" | "boutique" | "fashion" => ("clothing-store", "shop"),
        "shoes" => ("shoe", "shop"),
        "hardware" | "doityourself" | "trade" => ("hardware", "shop"),
        "florist" => ("florist", "shop"),
        "garden centre" => ("garden-centre", "shop"),
        "furniture" => ("furniture", "shop"),
        "jewelry" => ("jewelry-store", "shop"),
        "gift" | "souvenir" => ("gift", "shop"),
        "hairdresser" | "beauty" => ("hairdresser", "shop"),
        "optician" => ("optician", "shop"),
        "car repair" | "car" | "car parts" | "tyres" => ("car-repair", "transport"),
        "mall" | "department store" | "marketplace" => ("shop", "shop"),
        "hotel" | "motel" | "guest house" | "hostel" | "apartment" => ("lodging", "stay"),
        "chalet" | "alpine hut" | "wilderness hut" => ("home", "stay"),
        "camp site" | "caravan site" => ("campsite", "stay"),
        "shelter" => ("shelter", "stay"),
        "information" | "information office" => ("information", "stay"),
        "viewpoint" => ("viewpoint", "stay"),
        "attraction" | "theme park" => ("attraction", "stay"),
        "picnic site" | "bbq" | "firepit" => ("picnic-site", "stay"),
        "museum" => ("museum", "culture"),
        "gallery" | "arts centre" | "artwork" => ("art-gallery", "culture"),
        "theatre" => ("theatre", "culture"),
        "cinema" => ("cinema", "culture"),
        "castle" | "manor" | "fort" => ("castle", "culture"),
        "monument" | "memorial" | "statue" => ("monument", "culture"),
        "ruins" | "archaeological site" | "boundary stone" | "wayside cross" | "wayside shrine" => ("historic", "culture"),
        "townhall" | "courthouse" | "community centre" => ("town-hall", "culture"),
        "library" => ("library", "learn"),
        "school" | "kindergarten" => ("school", "learn"),
        "college" | "university" => ("college", "learn"),
        "hospital" => ("hospital", "health"),
        "clinic" | "doctors" => ("doctor", "health"),
        "dentist" => ("dentist", "health"),
        "pharmacy" | "chemist" => ("pharmacy", "health"),
        "veterinary" => ("veterinary", "health"),
        "police" => ("police", "health"),
        "fire station" => ("fire-station", "health"),
        "park" | "garden" => ("park", "nature"),
        "nature reserve" => ("natural", "nature"),
        "playground" => ("playground", "nature"),
        "beach" | "swimming area" => ("beach", "nature"),
        "peak" | "rock" | "stone" | "cave entrance" | "cape" => ("mountain", "nature"),
        "spring" | "drinking water" => ("drinking-water", "nature"),
        "waterfall" => ("waterfall", "nature"),
        "zoo" => ("zoo", "nature"),
        "bird hide" | "hunting stand" => ("observation-tower", "nature"),
        "sports centre" | "fitness centre" | "fitness station" => ("fitness-centre", "sport"),
        "swimming pool" | "water park" => ("swimming", "sport"),
        "stadium" | "pitch" | "track" => ("stadium", "sport"),
        "golf course" => ("golf", "sport"),
        "sauna" | "spa" => ("hot-spring", "sport"),
        "marina" | "slipway" => ("harbor", "transport"),
        "fuel" => ("fuel", "transport"),
        "charging station" => ("charging-station", "transport"),
        "car wash" => ("car", "transport"),
        "bus station" => ("bus", "transport"),
        "ferry terminal" => ("ferry", "transport"),
        "aerodrome" | "airport" => ("airport", "transport"),
        "bicycle rental" | "bicycle" => ("bicycle", "transport"),
        "lighthouse" => ("lighthouse", "transport"),
        "place of worship" | "monastery" => ("place-of-worship", "faith"),
        "grave yard" | "cemetery" => ("cemetery", "faith"),
        "bank" | "atm" => ("bank", "other"),
        "post office" => ("post", "other"),
        "toilets" => ("toilet", "other"),
        _ => ("marker", "other"),
    }
}

macro_rules! maki {
    ($($name:literal),* $(,)?) => {
        &[$(($name, include_bytes!(concat!("../icons/maki/", $name, ".svg")).as_slice())),*]
    };
}

/// The icons `style_of` uses.
pub const SVG: &[(&str, &[u8])] = maki![
    "restaurant", "cafe", "fast-food", "bar", "bakery", "ice-cream", "alcohol-shop", "grocery", "convenience",
    "clothing-store", "shoe", "hardware", "florist", "garden-centre", "furniture", "jewelry-store", "gift",
    "hairdresser", "optician", "car-repair", "shop", "lodging", "home", "campsite", "shelter", "information",
    "viewpoint", "attraction", "picnic-site", "museum", "art-gallery", "theatre", "cinema", "castle", "monument",
    "historic", "town-hall", "library", "school", "college", "hospital", "doctor", "dentist", "pharmacy",
    "veterinary", "police", "fire-station", "park", "natural", "playground", "beach", "mountain", "drinking-water",
    "waterfall", "zoo", "observation-tower", "fitness-centre", "swimming", "stadium", "golf", "hot-spring", "harbor",
    "fuel", "charging-station", "car", "bus", "ferry", "airport", "bicycle", "lighthouse", "place-of-worship",
    "cemetery", "bank", "post", "toilet", "marker",
];

/// An icon's SVG, drawn white (`fill`), by its name.
pub fn white_svg(name: &str) -> Option<String> {
    let svg = SVG.iter().find(|(n, _)| *n == name)?.1;
    Some(String::from_utf8_lossy(svg).replacen("<svg ", "<svg fill=\"#ffffff\" ", 1))
}

/// A group's colour as 0xRRGGBB.
pub fn group_rgb(group: &str) -> u32 {
    let hex = GROUPS.iter().find(|(g, _)| *g == group).map_or("#6b6b6b", |(_, c)| c);
    u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap_or(0x6b6b6b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_is_there() {
        for c in ["restaurant", "cafe", "shelter", "school", "place of worship", "fuel", "nobody knows"] {
            let (icon, group) = style_of(c);
            assert!(SVG.iter().any(|(n, _)| *n == icon), "{icon}");
            assert!(GROUPS.iter().any(|(g, _)| *g == group), "{group}");
        }
    }
}
