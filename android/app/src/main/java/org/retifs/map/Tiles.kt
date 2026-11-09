package org.retifs.map

import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.DashPathEffect
import android.graphics.Paint
import android.graphics.Path
import android.util.LruCache
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.withContext
import java.nio.ByteBuffer
import java.nio.ByteOrder
import kotlin.math.PI
import kotlin.math.atan
import kotlin.math.ln
import kotlin.math.tan
import kotlin.math.cos

/** Web Mercator: the world as a 0..1 square. */
object Mercator {
    fun x(lon: Double) = (lon + 180.0) / 360.0
    fun y(lat: Double): Double {
        val phi = Math.toRadians(lat.coerceIn(-85.05, 85.05))
        return (1.0 - ln(tan(phi) + 1.0 / cos(phi)) / PI) / 2.0
    }
    fun lon(x: Double) = x * 360.0 - 180.0
    fun lat(y: Double) = Math.toDegrees(atan(kotlin.math.sinh(PI * (1.0 - 2.0 * y))))
}

/** A label of a tile, drawn over the map (so tiles never cut it). */
class Label(val color: Int, val halo: Int, val haloWidth: Float, val size: Float, val x: Float, val y: Float, val angle: Float, val text: String)

/** A tile drawn: its shapes as a bitmap, its labels apart. */
class Drawn(val bitmap: Bitmap, val labels: List<Label>)

/**
 * A map's tiles, drawn here from their draw lists (the core reads the map
 * library's vector tiles and its style, as for the Haiku app): fills and
 * lines into a bitmap per tile, labels kept to draw over everything.
 */
object Tiles {
    /** A tile's side on screen at its own zoom, in dp (as MapLibre's raster: 256). */
    const val SIZE = 256

    private val cache = object : LruCache<String, Drawn>((Runtime.getRuntime().maxMemory() / 1024 / 4).toInt()) {
        override fun sizeOf(key: String, value: Drawn): Int = value.bitmap.byteCount / 1024
    }

    /** Tiles that aren't there (outside the map): not asked again. */
    private val none = HashSet<String>()

    @OptIn(ExperimentalCoroutinesApi::class)
    private val drawing = Dispatchers.IO.limitedParallelism(4)

    fun key(maps: List<String>, z: Int, x: Int, y: Int) = "${maps.joinToString("+")}/$z/$x/$y"

    fun cached(key: String): Drawn? = cache.get(key)

    fun missing(key: String) = synchronized(none) { key in none }

    /**
     * Tile z/x/y of `maps` (base first, overlays over it) drawn `px` pixels
     * square on `background`; null if none of them has one, or it couldn't
     * be read now (`retry` then says so).
     */
    suspend fun load(maps: List<String>, background: Int, z: Int, x: Int, y: Int, px: Int, density: Float, retry: () -> Unit = {}): Drawn? {
        val key = key(maps, z, x, y)
        cache.get(key)?.let { return it }
        val lists = withContext(drawing) { maps.map { Core.tile(it, z, x, y) } }
        if (lists.any { it != null && it.isEmpty() }) {
            retry()
            return null
        }
        if (lists.all { it == null }) {
            synchronized(none) { none += key }
            return null
        }
        val drawn = withContext(Dispatchers.Default) {
            val bitmap = Bitmap.createBitmap(px, px, Bitmap.Config.RGB_565)
            val canvas = Canvas(bitmap)
            canvas.drawColor(background)
            val labels = ArrayList<Label>()
            for (list in lists) if (list != null) draw(list, canvas, px.toFloat(), density, labels)
            Drawn(bitmap, labels)
        }
        cache.put(key, drawn)
        return drawn
    }

    fun clear() {
        cache.evictAll()
        synchronized(none) { none.clear() }
    }

    /** A draw list (`vector::encode`) into `canvas`, `side` pixels a tile; labels into `labels`. */
    private fun draw(bytes: ByteArray, canvas: Canvas, side: Float, density: Float, labels: MutableList<Label>) {
        val b = ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN)
        val fill = Paint(Paint.ANTI_ALIAS_FLAG).apply { style = Paint.Style.FILL }
        val stroke = Paint(Paint.ANTI_ALIAS_FLAG).apply { style = Paint.Style.STROKE; strokeCap = Paint.Cap.ROUND; strokeJoin = Paint.Join.ROUND }
        fun parts(closed: Boolean): Path {
            val path = Path()
            repeat(b.int) {
                val n = b.int
                for (i in 0 until n) {
                    val x = b.float * side
                    val y = b.float * side
                    if (i == 0) path.moveTo(x, y) else path.lineTo(x, y)
                }
                if (closed && n > 0) path.close()
            }
            return path
        }
        while (b.hasRemaining()) {
            when (b.get().toInt()) {
                0 -> {
                    val color = b.int
                    val outline = b.int
                    val path = parts(true)
                    path.fillType = Path.FillType.EVEN_ODD
                    fill.color = color
                    canvas.drawPath(path, fill)
                    if (outline != 0) {
                        stroke.color = outline
                        stroke.strokeWidth = density * 0.5f
                        stroke.pathEffect = null
                        canvas.drawPath(path, stroke)
                    }
                }
                1 -> {
                    val color = b.int
                    val width = b.float
                    val dashes = FloatArray(b.get().toInt() and 0xff) { b.float * density }
                    val path = parts(false)
                    stroke.color = color
                    stroke.strokeWidth = maxOf(0.5f, width) * density
                    stroke.pathEffect = if (dashes.size >= 2) DashPathEffect(dashes, 0f) else null
                    canvas.drawPath(path, stroke)
                }
                2 -> {
                    val color = b.int
                    val halo = b.int
                    val haloWidth = b.float
                    val size = b.float
                    val x = b.float
                    val y = b.float
                    val angle = b.float
                    val text = ByteArray(b.short.toInt() and 0xffff).also { b.get(it) }.toString(Charsets.UTF_8)
                    labels += Label(color, halo, haloWidth * density, size * density, x, y, angle, text)
                }
                else -> return // a newer format: what came before is drawn
            }
        }
    }
}
