package org.retifs.map

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/**
 * The map's core (Rust: `haiku/capi`, the same calls as the Haiku app,
 * through `android/jni`). Every call blocks: call it off the main thread.
 * Answers are JSON; tiles are draw lists ([Tiles.parse]).
 */
object Core {
    init {
        System.loadLibrary("retifs_map_android")
    }

    /** `retifs`: the class whose static `connect()` gives connections. */
    external fun init(retifs: Class<*>)
    /** `{"libraries", "lazy"}` or `{"error"}`; forgets what was read before. */
    external fun libraries(): String
    /** One library: `{"ns", "tilesets", "places", "unplaced", "by_cell", "records", "peers", "writable", "address"}`. */
    external fun library(ns: String, lazy: Boolean): String
    /** A lazily held library's places around a view: `[{place…}]`. */
    external fun placesIn(ns: String, west: Double, south: Double, east: Double, north: Double, zoom: Int): String
    /** `{"background"}` (0xAARRGGBB) or `{"error"}`. */
    external fun style(tileset: String): String
    /** A tile's draw list; null where the map has none, empty when it couldn't be read now. */
    external fun tile(tileset: String, z: Int, x: Int, y: Int): ByteArray?
    external fun thumb(place: Int): ByteArray?
    external fun preview(place: Int): ByteArray?
    /** A category's icon, `size` px square, white on transparent (RGBA bytes), or null. */
    external fun icon(category: String, size: Int): ByteArray?
    /** A category's circle colour (0xRRGGBB). */
    external fun iconColor(category: String): Int
    /** `[[key, value]]` */
    external fun details(ns: String, id: String): String
    /** `[{"name", "lat", "lon", "zoom", "place"}]` or `{"error"}` */
    external fun find(text: String): String
    external fun savePlace(ns: String, name: String, lat: Double, lon: Double, address: String): String
    external fun place(ns: String, id: String, key: String, lat: Double, lon: Double): String
    /** `{"usable": [{"name", "about", "address", "here", "hops", "has", …}], "other"}` */
    external fun heard(): String
    external fun changeCounts(): String
}

suspend fun <T> core(work: () -> T): T = withContext(Dispatchers.IO) { work() }
