package org.retifs.map

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Paint
import android.graphics.Rect
import android.graphics.RectF
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.gestures.detectTransformGestures
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableDoubleStateOf
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.drawscope.drawIntoCanvas
import androidx.compose.ui.graphics.nativeCanvas
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalDensity
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlin.math.floor
import kotlin.math.hypot
import kotlin.math.ln
import kotlin.math.pow

/** Where the map looks: its centre in the 0..1 world, and its zoom. */
class MapPosition(x: Double, y: Double, zoom: Double) {
    var x by mutableDoubleStateOf(x)
    var y by mutableDoubleStateOf(y)
    var zoom by mutableDoubleStateOf(zoom)

    fun lat() = Mercator.lat(y)
    fun lon() = Mercator.lon(x)

    fun moveTo(lat: Double, lon: Double, zoom: Double = this.zoom) {
        x = Mercator.x(lon)
        y = Mercator.y(lat)
        this.zoom = zoom.coerceIn(MIN_ZOOM, MAX_ZOOM)
    }

    /** West, south, east, north of a view `w` × `h` pixels. */
    fun bounds(w: Int, h: Int, density: Float): DoubleArray {
        val world = Tiles.SIZE * density * 2.0.pow(zoom)
        val (dx, dy) = w / 2.0 / world to h / 2.0 / world
        return doubleArrayOf(Mercator.lon(x - dx), Mercator.lat(y + dy), Mercator.lon(x + dx), Mercator.lat(y - dy))
    }

    companion object {
        const val MIN_ZOOM = 1.0
        const val MAX_ZOOM = 20.0
    }
}

/** A record at a place, as the core lists it (`i`: its number there). */
data class Place(
    val i: Int, val ns: String, val id: String, val kind: String, val title: String, val label: String,
    val lat: Double, val lon: Double, val whenAt: String, val detail: String, val minzoom: Int?, val thumb: Boolean,
) {
    val x = Mercator.x(lon)
    val y = Mercator.y(lat)
}

/** Category icons (white on transparent) and photo thumbnails, made once. */
object Marks {
    private val icons = HashMap<String, Bitmap?>()
    private val colors = HashMap<String, Int>()
    val thumbs = android.util.LruCache<Int, Bitmap>(200)

    fun icon(category: String, px: Int): Bitmap? = synchronized(icons) {
        icons.getOrPut("$category@$px") {
            Core.icon(category, px)?.let { rgba ->
                val pixels = IntArray(px * px) { i -> ((rgba[i * 4 + 3].toInt() and 0xff) shl 24) or 0xffffff }
                Bitmap.createBitmap(pixels, px, px, Bitmap.Config.ARGB_8888)
            }
        }
    }

    fun color(category: String): Int = synchronized(colors) { colors.getOrPut(category) { Core.iconColor(category) or 0xff000000.toInt() } }

    fun kindColor(kind: String): Int = when (kind) {
        "photo" -> 0xff3584e4.toInt()
        "person" -> 0xff9141ac.toInt()
        "event" -> 0xffe66100.toInt()
        else -> 0xff2e8b57.toInt()
    }
}

/**
 * The map: tiles of the maps shown (a base map, overlays over it), their
 * labels over them, and the places. Drag to move, pinch or double tap to
 * zoom, tap a place for it, long press anywhere to save a place there.
 */
@Composable
fun MapCanvas(
    pos: MapPosition,
    maps: List<String>,
    background: Int,
    places: List<Place>,
    me: Pair<Double, Double>?,
    pin: Pair<Double, Double>?,
    tap: (Place?) -> Unit,
    longPress: (Double, Double) -> Unit,
    settled: (DoubleArray) -> Unit,
) {
    val density = LocalDensity.current.density
    val scope = rememberCoroutineScope()
    var drawn by remember { mutableIntStateOf(0) }
    val loading = remember { HashSet<String>() }
    var size by remember { mutableStateOf(0 to 0) }
    val tilePx = (Tiles.SIZE * density).toInt()

    BoxWithConstraints(Modifier.fillMaxSize()) {
        val w = constraints.maxWidth
        val h = constraints.maxHeight
        size = w to h
        val z = floor(pos.zoom).toInt().coerceIn(0, MapPosition.MAX_ZOOM.toInt())
        val n = 1 shl z
        val world = tilePx * 2.0.pow(pos.zoom)
        val side = world / n
        val left = pos.x * world - w / 2.0
        val top = pos.y * world - h / 2.0
        val x0 = floor(left / side).toInt()
        val x1 = floor((left + w) / side).toInt()
        val y0 = floor(top / side).toInt().coerceAtLeast(0)
        val y1 = floor((top + h) / side).toInt().coerceAtMost(n - 1)

        // Tiles in view not drawn yet: drawn now, nearest the centre first.
        LaunchedEffect(z, x0, x1, y0, y1, maps, background, drawn) {
            if (maps.isEmpty()) return@LaunchedEffect
            val want = ArrayList<Triple<Int, Int, Int>>()
            for (ty in y0..y1) for (tx in x0..x1) want += Triple(Math.floorMod(tx, n), ty, tx)
            want.sortBy { (_, ty, tx) -> hypot(tx - (x0 + x1) / 2.0, ty - (y0 + y1) / 2.0) }
            for ((tx, ty, _) in want) {
                val key = Tiles.key(maps, z, tx, ty)
                if (Tiles.cached(key) != null || Tiles.missing(key) || !loading.add(key)) continue
                scope.launch {
                    var again = false
                    Tiles.load(maps, background, z, tx, ty, tilePx, density) { again = true }
                    loading.remove(key)
                    if (again) delay(5_000)
                    drawn++
                }
            }
        }
        // The view stopped moving: say where it is (places of lazy libraries are asked for it).
        LaunchedEffect(pos.x, pos.y, pos.zoom, w, h) {
            delay(600)
            settled(pos.bounds(w, h, density))
        }

        val tilePaint = remember { Paint(Paint.FILTER_BITMAP_FLAG) }
        val text = remember { Paint(Paint.ANTI_ALIAS_FLAG).apply { textAlign = Paint.Align.CENTER; typeface = android.graphics.Typeface.DEFAULT } }
        val halo = remember { Paint(Paint.ANTI_ALIAS_FLAG).apply { textAlign = Paint.Align.CENTER; style = Paint.Style.STROKE; strokeJoin = Paint.Join.ROUND } }
        val dot = remember { Paint(Paint.ANTI_ALIAS_FLAG) }
        val ring = remember { Paint(Paint.ANTI_ALIAS_FLAG).apply { style = Paint.Style.STROKE; color = 0xffffffff.toInt() } }

        Canvas(
            Modifier.fillMaxSize()
                .pointerInput(Unit) {
                    detectTransformGestures { centroid, pan, zoomChange, _ ->
                        val (vw, vh) = size
                        val before = tilePx * 2.0.pow(pos.zoom)
                        val wx = pos.x + (centroid.x - vw / 2.0) / before
                        val wy = pos.y + (centroid.y - vh / 2.0) / before
                        pos.zoom = (pos.zoom + ln(zoomChange.toDouble()) / ln(2.0)).coerceIn(MapPosition.MIN_ZOOM, MapPosition.MAX_ZOOM)
                        val after = tilePx * 2.0.pow(pos.zoom)
                        pos.x = wx - (centroid.x - vw / 2.0) / after - pan.x / after
                        pos.y = (wy - (centroid.y - vh / 2.0) / after - pan.y / after).coerceIn(0.0, 1.0)
                        pos.x = pos.x - floor(pos.x)
                    }
                }
                .pointerInput(places) {
                    detectTapGestures(
                        onTap = { at -> tap(nearest(pos, places, at, size, tilePx, density)) },
                        onDoubleTap = { at ->
                            val (vw, vh) = size
                            val before = tilePx * 2.0.pow(pos.zoom)
                            val wx = pos.x + (at.x - vw / 2.0) / before
                            val wy = pos.y + (at.y - vh / 2.0) / before
                            pos.zoom = (pos.zoom + 1).coerceAtMost(MapPosition.MAX_ZOOM)
                            val after = tilePx * 2.0.pow(pos.zoom)
                            pos.x = wx - (at.x - vw / 2.0) / after
                            pos.y = wy - (at.y - vh / 2.0) / after
                        },
                        onLongPress = { at ->
                            val (vw, vh) = size
                            val world0 = tilePx * 2.0.pow(pos.zoom)
                            longPress(Mercator.lat(pos.y + (at.y - vh / 2.0) / world0), Mercator.lon(pos.x + (at.x - vw / 2.0) / world0))
                        },
                    )
                },
        ) {
            @Suppress("UNUSED_EXPRESSION") drawn
            drawIntoCanvas { cv ->
                val c = cv.nativeCanvas
                c.drawColor(background)
                val scale = side / tilePx
                val shown = ArrayList<Pair<Drawn, Pair<Float, Float>>>()
                for (ty in y0..y1) for (tx in x0..x1) {
                    val wrapped = Math.floorMod(tx, n)
                    val dx = (tx * side - left).toFloat()
                    val dy = (ty * side - top).toFloat()
                    val dst = RectF(dx, dy, dx + side.toFloat(), dy + side.toFloat())
                    val d = if (maps.isEmpty()) null else Tiles.cached(Tiles.key(maps, z, wrapped, ty))
                    if (d != null) {
                        c.drawBitmap(d.bitmap, null, dst, tilePaint)
                        shown += d to (dx to dy)
                        continue
                    }
                    // Not drawn yet: an ancestor's part, enlarged, meanwhile.
                    for (up in 1..4) {
                        if (z - up < 0) break
                        val a = Tiles.cached(Tiles.key(maps, z - up, wrapped shr up, ty shr up)) ?: continue
                        val part = tilePx shr up
                        val sx = (wrapped - ((wrapped shr up) shl up)) * part
                        val sy = (ty - ((ty shr up) shl up)) * part
                        c.drawBitmap(a.bitmap, Rect(sx, sy, sx + part, sy + part), dst, tilePaint)
                        break
                    }
                }
                // Labels over every tile, none over another.
                val taken = ArrayList<RectF>()
                for ((d, at) in shown) {
                    for (l in d.labels) {
                        val lx = at.first + l.x * side.toFloat()
                        val ly = at.second + l.y * side.toFloat()
                        val sz = l.size * scale.toFloat().coerceIn(0.75f, 1.5f)
                        text.textSize = sz
                        val half = text.measureText(l.text) / 2 + 2
                        val box = RectF(lx - half, ly - sz, lx + half, ly + sz / 3)
                        if (taken.any { RectF.intersects(it, box) }) continue
                        taken += box
                        c.save()
                        if (l.angle != 0f) c.rotate(l.angle, lx, ly)
                        if (l.haloWidth > 0 && (l.halo ushr 24) != 0) {
                            halo.textSize = sz
                            halo.color = l.halo
                            halo.strokeWidth = maxOf(1f, l.haloWidth) * 2
                            c.drawText(l.text, lx, ly + sz / 3, halo)
                        }
                        text.color = l.color
                        c.drawText(l.text, lx, ly + sz / 3, text)
                        c.restore()
                    }
                }
                // Places: the user's own always, a map's from its zoom on.
                val r = 7 * density
                var count = 0
                for (p in places) {
                    if (p.minzoom != null && pos.zoom < p.minzoom) continue
                    val px = ((p.x - pos.x) * (tilePx * 2.0.pow(pos.zoom)) + w / 2.0).let { var v = it; val wp = tilePx * 2.0.pow(pos.zoom); if (v < -50) v += wp; if (v > w + 50) v -= wp; v }.toFloat()
                    val py = ((p.y - pos.y) * (tilePx * 2.0.pow(pos.zoom)) + h / 2.0).toFloat()
                    if (px < -30 || px > w + 30 || py < -30 || py > h + 30) continue
                    if (++count > 500) break
                    val thumb = if (p.kind == "photo" && p.thumb) Marks.thumbs.get(p.i) else null
                    if (thumb != null) {
                        val s = 18 * density
                        c.drawBitmap(thumb, null, RectF(px - s, py - s, px + s, py + s), tilePaint)
                        ring.strokeWidth = 2 * density
                        c.drawRect(px - s, py - s, px + s, py + s, ring)
                        continue
                    }
                    val category = p.detail.takeIf { p.kind == "place" && it.isNotEmpty() }
                    dot.color = category?.let { Marks.color(it) } ?: Marks.kindColor(p.kind)
                    val big = if (category != null) r * 1.6f else r
                    c.drawCircle(px, py, big, dot)
                    ring.strokeWidth = 1.5f * density
                    c.drawCircle(px, py, big, ring)
                    if (category != null) Marks.icon(category, (big * 1.3f).toInt())?.let { ic -> c.drawBitmap(ic, px - ic.width / 2f, py - ic.height / 2f, null) }
                }
                // A place handed over (geo: link), and where you are.
                pin?.let { (lat, lon) ->
                    val world0 = tilePx * 2.0.pow(pos.zoom)
                    val px = ((Mercator.x(lon) - pos.x) * world0 + w / 2.0).toFloat()
                    val py = ((Mercator.y(lat) - pos.y) * world0 + h / 2.0).toFloat()
                    dot.color = 0xffe01b24.toInt()
                    c.drawCircle(px, py - 14 * density, 9 * density, dot)
                    c.drawRect(px - 1.5f * density, py - 14 * density, px + 1.5f * density, py, dot)
                }
                me?.let { (lat, lon) ->
                    val world0 = tilePx * 2.0.pow(pos.zoom)
                    val px = ((Mercator.x(lon) - pos.x) * world0 + w / 2.0).toFloat()
                    val py = ((Mercator.y(lat) - pos.y) * world0 + h / 2.0).toFloat()
                    dot.color = 0x403584e4
                    c.drawCircle(px, py, 18 * density, dot)
                    dot.color = 0xff3584e4.toInt()
                    c.drawCircle(px, py, 7 * density, dot)
                    ring.strokeWidth = 2 * density
                    c.drawCircle(px, py, 7 * density, ring)
                }
            }
        }
        // Photos' thumbnails on the map, read as their places come into view.
        LaunchedEffect(places, z, x0, y0) {
            val bounds = pos.bounds(w, h, density)
            for (p in places) {
                if (p.kind != "photo" || !p.thumb || Marks.thumbs.get(p.i) != null) continue
                if (p.lon < bounds[0] || p.lon > bounds[2] || p.lat < bounds[1] || p.lat > bounds[3]) continue
                val b = core { Core.thumb(p.i) }?.let { BitmapFactory.decodeByteArray(it, 0, it.size) } ?: continue
                Marks.thumbs.put(p.i, b)
                drawn++
            }
        }
    }
}

/** The place drawn nearest a tap, within a finger's width. */
private fun nearest(pos: MapPosition, places: List<Place>, at: Offset, size: Pair<Int, Int>, tilePx: Int, density: Float): Place? {
    val world = tilePx * 2.0.pow(pos.zoom)
    val (w, h) = size
    return places
        .filter { it.minzoom == null || pos.zoom >= it.minzoom }
        .map { p -> p to hypot((p.x - pos.x) * world + w / 2.0 - at.x, (p.y - pos.y) * world + h / 2.0 - at.y) }
        .filter { it.second < 24 * density }
        .minByOrNull { it.second }?.first
}
