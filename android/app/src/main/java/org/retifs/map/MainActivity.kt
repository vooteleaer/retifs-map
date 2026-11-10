package org.retifs.map

import android.Manifest
import android.annotation.SuppressLint
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.BitmapFactory
import android.location.Location
import android.location.LocationListener
import android.location.LocationManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.Image
import androidx.compose.foundation.clickable
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.LocationOn
import androidx.compose.material.icons.filled.Menu
import androidx.compose.material.icons.filled.Search
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Checkbox
import androidx.compose.material3.DrawerValue
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FloatingActionButton
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.ModalDrawerSheet
import androidx.compose.material3.ModalNavigationDrawer
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.material3.rememberDrawerState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import org.json.JSONArray
import org.json.JSONObject

class MainActivity : ComponentActivity() {
    /** A place handed over (`geo:` link): shown, pinned. */
    private var handed by mutableStateOf<Triple<Double, Double, Double>?>(null)
    /** On screen: only then are changes waited for. */
    private var shown by mutableStateOf(false)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        handed = geo(intent)
        setContent {
            val dark = isSystemInDarkTheme()
            val colors = when {
                Build.VERSION.SDK_INT >= 31 && dark -> dynamicDarkColorScheme(this)
                Build.VERSION.SDK_INT >= 31 -> dynamicLightColorScheme(this)
                dark -> darkColorScheme()
                else -> lightColorScheme()
            }
            MaterialTheme(colorScheme = colors) {
                Surface(Modifier.fillMaxSize()) { App(handed, shown) }
            }
        }
    }

    override fun onStart() {
        super.onStart()
        shown = true
    }

    override fun onStop() {
        super.onStop()
        shown = false
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        geo(intent)?.let { handed = it }
    }

    /** `geo:lat,lon?z=16`, or `geo:0,0?q=lat,lon(label)`: (lat, lon, zoom). */
    private fun geo(intent: Intent?): Triple<Double, Double, Double>? {
        val uri = intent?.data?.takeIf { it.scheme == "geo" } ?: return null
        val ssp = uri.schemeSpecificPart ?: return null
        val (place, query) = ssp.substringBefore('?') to ssp.substringAfter('?', "")
        val params = query.split('&').associate { it.substringBefore('=') to Uri.decode(it.substringAfter('=', "")) }
        val zoom = params["z"]?.toDoubleOrNull() ?: 16.0
        fun pair(s: String): Pair<Double, Double>? {
            val parts = s.substringBefore('(').split(',')
            if (parts.size < 2) return null
            return (parts[0].trim().toDoubleOrNull() ?: return null) to (parts[1].trim().toDoubleOrNull() ?: return null)
        }
        val at = pair(place)?.takeIf { it != 0.0 to 0.0 } ?: params["q"]?.let { pair(it) } ?: return null
        return Triple(at.first, at.second, zoom)
    }
}

/** A library as the map has it. */
data class Lib(
    val name: String, val maps: List<JSONObject>, val places: Int, val byCell: Boolean, val address: Boolean,
    val writable: Boolean, val records: Int, val peers: Int, val lazy: Boolean,
)

/**
 * What the map shows: the libraries it may use, read one by one (each
 * shows as soon as it's read), their maps and places; which are shown.
 */
class Atlas(val context: Context) {
    val libs = mutableStateListOf<Lib>()
    val hidden = mutableStateListOf<String>().apply { addAll(prefs().getStringSet("hidden", emptySet())!!) }
    var places by mutableStateOf<List<Place>>(emptyList())
    var reading by mutableStateOf<String?>(null)
    var progress by mutableStateOf(0f)
    var error by mutableStateOf("")
    /** Map ID → background (0xAARRGGBB), once its style is read. */
    val styles = HashMap<String, Int>()

    fun prefs() = context.getSharedPreferences("map", Context.MODE_PRIVATE)

    fun toggle(name: String) {
        if (name in hidden) hidden.remove(name) else hidden.add(name)
        prefs().edit().putStringSet("hidden", hidden.toSet()).apply()
    }

    /** The maps drawn: a base map, then overlays (of libraries shown). */
    fun maps(): List<String> {
        val all = libs.filter { it.name !in hidden }.flatMap { it.maps }.filter { styles.containsKey(it.getString("id")) }
        val base = all.firstOrNull { !it.optBoolean("overlay") }
        return listOfNotNull(base) .plus(all.filter { it.optBoolean("overlay") }).map { it.getString("id") }
    }

    fun shownPlaces(): List<Place> = places.filter { it.ns !in hidden }

    suspend fun load() {
        val answer = JSONObject(core { Core.libraries() })
        if (answer.has("error")) {
            error = answer.getString("error")
            return
        }
        error = ""
        Tiles.clear()
        Marks.thumbs.evictAll()
        val names = answer.getJSONArray("libraries").let { a -> (0 until a.length()).map { a.getString(it) } }
        val lazy = answer.optJSONArray("lazy")?.let { a -> (0 until a.length()).map { a.getString(it) } } ?: emptyList()
        libs.clear()
        val all = ArrayList<Place>()
        names.forEachIndexed { i, ns ->
            reading = "Reading libraries… $ns (${i + 1} of ${names.size})"
            progress = i.toFloat() / names.size
            val l = JSONObject(core { Core.library(ns, ns in lazy) })
            val maps = l.optJSONArray("tilesets")?.let { a -> (0 until a.length()).map { a.getJSONObject(it) } } ?: emptyList()
            for (m in maps) {
                val st = JSONObject(core { Core.style(m.getString("id")) })
                if (!st.has("error")) styles[m.getString("id")] = st.optLong("background", 0xfff2efe9).toInt()
            }
            val ps = places(l.optJSONArray("places"))
            all.addAll(ps)
            places = all.toList()
            libs.add(Lib(ns, maps, ps.size, l.optBoolean("by_cell"), l.optBoolean("address"), l.optBoolean("writable"), l.optInt("records"), l.optInt("peers"), l.optBoolean("lazy")))
        }
        reading = null
    }

    /** Places of lazily held libraries around a view (from zoom 12). */
    suspend fun around(bounds: DoubleArray, zoom: Int) {
        if (zoom < 12) return
        for (l in libs.filter { it.byCell && it.name !in hidden }) {
            val more = places(runCatching { JSONArray(core { Core.placesIn(l.name, bounds[0], bounds[1], bounds[2], bounds[3], zoom) }) }.getOrNull())
            if (more.isNotEmpty()) places = places + more
        }
    }

    fun places(a: JSONArray?): List<Place> = a?.let { (0 until it.length()).map { i -> it.getJSONObject(i) } }?.map { p ->
        Place(
            p.getInt("i"), p.getString("ns"), p.getString("id"), p.optString("kind"), p.optString("title"), p.optString("label"),
            p.getDouble("lat"), p.getDouble("lon"), p.optString("when"), p.optString("detail"),
            if (p.isNull("minzoom")) null else p.optInt("minzoom"), p.optBoolean("thumb"),
        )
    } ?: emptyList()
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun App(handed: Triple<Double, Double, Double>?, shown: Boolean) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val atlas = remember { Atlas(context) }
    val saved = atlas.prefs()
    val pos = remember { MapPosition(saved.getFloat("x", 0.5f).toDouble(), saved.getFloat("y", 0.3f).toDouble(), saved.getFloat("zoom", 2f).toDouble()) }
    val drawer = rememberDrawerState(DrawerValue.Closed)
    var reloads by remember { mutableStateOf(0) }
    var selected by remember { mutableStateOf<Place?>(null) }
    var saving by remember { mutableStateOf<Pair<Double, Double>?>(null) }
    var announces by remember { mutableStateOf(false) }
    val search = remember { Searching() }
    var me by remember { mutableStateOf<Pair<Double, Double>?>(null) }
    var follow by remember { mutableStateOf(false) }
    var pin by remember { mutableStateOf<Pair<Double, Double>?>(null) }
    val letIn = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) { reloads++ }

    LaunchedEffect(reloads) {
        val first = atlas.libs.isEmpty() && !saved.contains("x")
        atlas.load()
        // First time: the first map's area.
        if (first) atlas.libs.flatMap { it.maps }.firstOrNull { !it.isNull("bounds") }?.getJSONArray("bounds")?.let { b ->
            pos.moveTo((b.getDouble(1) + b.getDouble(3)) / 2, (b.getDouble(0) + b.getDouble(2)) / 2, 7.0)
        }
    }
    LaunchedEffect(handed) {
        handed?.let { (lat, lon, z) ->
            pos.moveTo(lat, lon, z)
            pin = lat to lon
        }
    }
    // Read again the moment a library read changes (retifsd says so), while on screen.
    LaunchedEffect(shown) {
        var known = "{}"
        while (shown) {
            val answer = JSONObject(core { Core.waitChange(known, 60_000) })
            known = answer.optJSONObject("known")?.toString() ?: "{}"
            if (!answer.optBoolean("changed") || Retifs.problem != null) continue
            // Reading already: once that's done (it may have begun before the change).
            while (atlas.reading != null) delay(500)
            atlas.load()
        }
    }
    Location(follow, onFix = { fix ->
        me = fix
        if (follow) {
            pos.moveTo(fix.first, fix.second, maxOf(pos.zoom, 15.0))
            follow = false
        }
    })

    ModalNavigationDrawer(drawerState = drawer, drawerContent = {
        ModalDrawerSheet { Drawer(atlas, announces = { announces = true }, letIn = { letIn.launch(Retifs.letIn()) }) }
    }) {
        Box(Modifier.fillMaxSize()) {
            if (Retifs.problem != null) {
                Problem(Retifs.problem!!, letIn = { letIn.launch(Retifs.letIn()) }, retry = { reloads++ })
                return@Box
            }
            val maps = atlas.maps()
            MapCanvas(
                pos = pos,
                maps = maps,
                background = maps.firstOrNull()?.let { atlas.styles[it] } ?: 0xfff2efe9.toInt(),
                places = atlas.shownPlaces(),
                me = me,
                pin = pin,
                tap = { selected = it },
                longPress = { lat, lon -> saving = lat to lon },
                settled = { b ->
                    saved.edit().putFloat("x", pos.x.toFloat()).putFloat("y", pos.y.toFloat()).putFloat("zoom", pos.zoom.toFloat()).apply()
                    scope.launch { atlas.around(b, pos.zoom.toInt()) }
                },
            )
            Column(Modifier.statusBarsPadding().padding(8.dp)) {
                Surface(shape = RoundedCornerShape(28.dp), tonalElevation = 3.dp, shadowElevation = 3.dp) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        IconButton(onClick = { scope.launch { drawer.open() } }) { Icon(Icons.Filled.Menu, "Libraries") }
                        SearchField(search, Modifier.weight(1f)) { lat, lon, zoom ->
                            pos.moveTo(lat, lon, zoom)
                            pin = lat to lon
                        }
                    }
                }
                SearchResults(atlas, search)
                atlas.reading?.let {
                    LinearProgressIndicator(progress = { atlas.progress }, modifier = Modifier.fillMaxWidth().padding(top = 4.dp))
                    Text(it, style = MaterialTheme.typography.bodySmall)
                }
                if (atlas.error.isNotEmpty()) Text(atlas.error, color = MaterialTheme.colorScheme.error)
                if (maps.isEmpty() && atlas.reading == null) {
                    Surface(shape = RoundedCornerShape(12.dp), tonalElevation = 2.dp, modifier = Modifier.padding(top = 8.dp)) {
                        Text("No map shown. Join a map library (Announces, ☰), or show one.", Modifier.padding(12.dp))
                    }
                }
            }
            FloatingActionButton(onClick = { follow = true }, modifier = Modifier.align(Alignment.BottomEnd).padding(16.dp)) { Icon(Icons.Filled.LocationOn, "Where I am") }
            // Whose map: as the map library says.
            maps.firstOrNull()?.let { id -> atlas.libs.flatMap { it.maps }.firstOrNull { it.getString("id") == id }?.optString("attribution") }?.takeIf { it.isNotEmpty() }?.let {
                Text(it, style = MaterialTheme.typography.labelSmall, modifier = Modifier.align(Alignment.BottomStart).padding(8.dp))
            }
        }
    }

    selected?.let { p -> PlaceSheet(p, close = { selected = null }) }
    saving?.let { (lat, lon) ->
        SavePlace(atlas, lat, lon, close = { saving = null }, saved = { ns ->
            saving = null
            reloads++
            pin = null
            atlas.error = "Saved in $ns"
        })
    }
    if (announces) Announces(close = { announces = false })
}

/** Where you are, from the phone's own GPS (no location service asked), while `want` or shown. */
@SuppressLint("MissingPermission")
@Composable
fun Location(want: Boolean, onFix: (Pair<Double, Double>) -> Unit) {
    val context = LocalContext.current
    var allowed by remember { mutableStateOf(context.checkSelfPermission(Manifest.permission.ACCESS_FINE_LOCATION) == PackageManager.PERMISSION_GRANTED) }
    val ask = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { allowed = it }
    LaunchedEffect(want) { if (want && !allowed) ask.launch(Manifest.permission.ACCESS_FINE_LOCATION) }
    DisposableEffect(allowed) {
        val lm = context.getSystemService(LocationManager::class.java)
        val listener = LocationListener { l: Location -> onFix(l.latitude to l.longitude) }
        if (allowed && lm.isProviderEnabled(LocationManager.GPS_PROVIDER)) {
            lm.getLastKnownLocation(LocationManager.GPS_PROVIDER)?.let { onFix(it.latitude to it.longitude) }
            lm.requestLocationUpdates(LocationManager.GPS_PROVIDER, 5_000L, 5f, listener)
        }
        onDispose { lm.removeUpdates(listener) }
    }
}

/** A place tapped: what it is, its photo, what its record says. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun PlaceSheet(p: Place, close: () -> Unit) {
    val details by produceState<List<Pair<String, String>>>(emptyList(), p) {
        value = runCatching { JSONArray(core { Core.details(p.ns, p.id) }) }.getOrNull()?.let { a ->
            (0 until a.length()).map { a.getJSONArray(it).let { d -> d.getString(0) to d.getString(1) } }
        } ?: emptyList()
    }
    val image by produceState<android.graphics.Bitmap?>(null, p) {
        if (p.kind == "photo") value = core { Core.preview(p.i) }?.let { BitmapFactory.decodeByteArray(it, 0, it.size) }
    }
    ModalBottomSheet(onDismissRequest = close) {
        Column(Modifier.padding(horizontal = 20.dp).padding(bottom = 32.dp).heightIn(max = 560.dp).verticalScroll(rememberScrollState())) {
            Text(p.title + if (p.label.isNotEmpty()) " (${p.label})" else "", style = MaterialTheme.typography.titleMedium)
            val sub = listOf(p.detail, p.whenAt.replace('T', ' '), p.ns).filter { it.isNotEmpty() }.joinToString(" · ")
            Text(sub, style = MaterialTheme.typography.bodySmall)
            image?.let { Image(it.asImageBitmap(), p.title, contentScale = ContentScale.Fit, modifier = Modifier.fillMaxWidth().padding(top = 12.dp).clip(RoundedCornerShape(12.dp))) }
            details.filter { (k, _) -> !k.startsWith("block:") && k != "extracted" }.forEach { (k, v) ->
                Row(Modifier.padding(top = 6.dp)) {
                    Text(k, Modifier.weight(0.35f), style = MaterialTheme.typography.bodySmall)
                    Text(v, Modifier.weight(0.65f))
                }
            }
            Text("%.5f, %.5f".format(p.lat, p.lon), style = MaterialTheme.typography.bodySmall, modifier = Modifier.padding(top = 8.dp))
        }
    }
}

/** A long press: a new place there, in a library this app may add to. */
@Composable
fun SavePlace(atlas: Atlas, lat: Double, lon: Double, close: () -> Unit, saved: (String) -> Unit) {
    val scope = rememberCoroutineScope()
    val writable = atlas.libs.filter { it.writable && it.maps.isEmpty() && !it.address }
    var name by remember { mutableStateOf("") }
    var ns by remember { mutableStateOf(writable.firstOrNull()?.name) }
    var error by remember { mutableStateOf("") }
    AlertDialog(
        onDismissRequest = close,
        title = { Text("Save a place here") },
        text = {
            Column {
                Text("%.5f, %.5f".format(lat, lon), style = MaterialTheme.typography.bodySmall)
                OutlinedTextField(value = name, onValueChange = { name = it }, label = { Text("Name") }, singleLine = true)
                Text("In library", style = MaterialTheme.typography.bodySmall, modifier = Modifier.padding(top = 8.dp))
                writable.forEach { l ->
                    Row(Modifier.clickable { ns = l.name }, verticalAlignment = Alignment.CenterVertically) {
                        RadioButton(selected = ns == l.name, onClick = { ns = l.name })
                        Text(l.name)
                    }
                }
                if (writable.isEmpty()) Text("Map may add to none of its libraries (Choose libraries…, ☰).")
                if (error.isNotEmpty()) Text(error, color = MaterialTheme.colorScheme.error)
            }
        },
        confirmButton = {
            Button(enabled = name.isNotBlank() && ns != null, onClick = {
                scope.launch {
                    val r = JSONObject(core { Core.savePlace(ns!!, name, lat, lon, "") })
                    if (r.has("error")) error = r.getString("error") else saved(ns!!)
                }
            }) { Text("Save") }
        },
        dismissButton = { TextButton(onClick = close) { Text("Cancel") } },
    )
}

/**
 * Searching the address libraries and the places here (retifs only, no
 * internet service), typed straight into the bar on top of the map; what's
 * found shows under it ([SearchResults]) until one is chosen.
 */
class Searching {
    var text by mutableStateOf("")
    var found by mutableStateOf<List<JSONObject>>(emptyList())
    var note by mutableStateOf("")
    var open by mutableStateOf(false)
    var go: (JSONObject) -> Unit = {}
}

@Composable
fun SearchField(s: Searching, modifier: Modifier, go: (Double, Double, Double) -> Unit) {
    val focus = LocalFocusManager.current
    s.go = { f ->
        s.text = f.getString("name")
        s.open = false
        focus.clearFocus()
        go(f.getDouble("lat"), f.getDouble("lon"), f.optDouble("zoom", 17.0))
    }
    LaunchedEffect(s.text, s.open) {
        if (!s.open || s.text.trim().length < 3) {
            s.found = emptyList()
            s.note = ""
            return@LaunchedEffect
        }
        delay(400)
        // A library held lazily asks its peers: a search can take seconds.
        s.note = "Searching…"
        val r = core { Core.find(s.text) }
        if (r.trimStart().startsWith("{")) {
            s.note = JSONObject(r).optString("error")
        } else {
            val a = JSONArray(r)
            s.found = (0 until a.length()).map { a.getJSONObject(it) }
            s.note = if (s.found.isEmpty()) "Nothing found" else ""
        }
    }
    Row(modifier, verticalAlignment = Alignment.CenterVertically) {
        BasicTextField(
            value = s.text,
            onValueChange = { s.text = it; s.open = true },
            singleLine = true,
            textStyle = MaterialTheme.typography.bodyLarge.copy(color = MaterialTheme.colorScheme.onSurface),
            cursorBrush = SolidColor(MaterialTheme.colorScheme.primary),
            keyboardOptions = KeyboardOptions(imeAction = ImeAction.Search),
            keyboardActions = KeyboardActions(onSearch = { s.found.firstOrNull()?.let(s.go) }),
            modifier = Modifier.weight(1f).padding(vertical = 14.dp).onFocusChanged { if (it.isFocused) s.open = true },
            decorationBox = { inner ->
                if (s.text.isEmpty()) Text("Search addresses and places", color = MaterialTheme.colorScheme.onSurfaceVariant)
                inner()
            },
        )
        if (s.text.isNotEmpty()) {
            IconButton(onClick = { s.text = ""; s.open = false; focus.clearFocus() }) { Icon(Icons.Filled.Close, "Clear") }
        } else {
            Icon(Icons.Filled.Search, null, Modifier.padding(horizontal = 12.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
}

/** What the search bar found, under it. */
@Composable
fun SearchResults(atlas: Atlas, s: Searching) {
    val focus = LocalFocusManager.current
    BackHandler(s.open) {
        s.open = false
        focus.clearFocus()
    }
    if (!s.open || (s.found.isEmpty() && s.note.isEmpty())) return
    Surface(shape = RoundedCornerShape(16.dp), tonalElevation = 3.dp, shadowElevation = 3.dp, modifier = Modifier.fillMaxWidth().padding(top = 4.dp)) {
        Column(Modifier.padding(horizontal = 16.dp, vertical = 4.dp)) {
            LazyColumn(Modifier.heightIn(max = 360.dp)) {
                items(s.found) { f ->
                    Text(f.getString("name"), Modifier.fillMaxWidth().clickable { s.go(f) }.padding(vertical = 10.dp))
                }
            }
            if (s.note.isNotEmpty()) Text(s.note, style = MaterialTheme.typography.bodySmall, modifier = Modifier.padding(vertical = 8.dp))
            if (atlas.libs.none { it.address }) Text("Addresses are searched in address libraries you've joined.", style = MaterialTheme.typography.bodySmall, modifier = Modifier.padding(bottom = 8.dp))
        }
    }
}

/** Public map libraries heard on the network; joining is done in retifs. */
@Composable
fun Announces(close: () -> Unit) {
    val context = LocalContext.current
    val heard by produceState<JSONObject?>(null) { value = runCatching { JSONObject(core { Core.heard() }) }.getOrNull() }
    AlertDialog(
        onDismissRequest = close,
        title = { Text("Announces") },
        text = {
            LazyColumn(Modifier.heightIn(max = 480.dp)) {
                val usable = heard?.optJSONArray("usable")?.let { a -> (0 until a.length()).map { a.getJSONObject(it) } } ?: emptyList()
                if (heard == null) item { Text("Listening…") }
                if (heard?.has("error") == true) item { Text(heard!!.getString("error"), color = MaterialTheme.colorScheme.error) }
                items(usable) { l ->
                    Column(Modifier.padding(vertical = 6.dp)) {
                        Text(l.optString("name"), style = MaterialTheme.typography.titleSmall)
                        if (l.optString("about").isNotEmpty()) Text(l.optString("about"), style = MaterialTheme.typography.bodySmall)
                        val here = l.optString("here").takeUnless { l.isNull("here") }
                        Text(if (here != null) "joined as $here" else "${l.optInt("hops")} hop(s) away", style = MaterialTheme.typography.bodySmall)
                    }
                }
            }
        },
        confirmButton = { Retifs.open(context)?.let { i -> TextButton(onClick = { context.startActivity(i) }) { Text("Join in retifs") } } },
        dismissButton = { TextButton(onClick = close) { Text("Close") } },
    )
}

/** The libraries: Announces above, each library a toggle. */
@Composable
fun Drawer(atlas: Atlas, announces: () -> Unit, letIn: () -> Unit) {
    LazyColumn(Modifier.padding(vertical = 8.dp)) {
        item {
            OutlinedButton(onClick = announces, modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp)) { Text("Announces") }
            Text("Libraries", style = MaterialTheme.typography.titleSmall, modifier = Modifier.padding(start = 16.dp, top = 16.dp, bottom = 4.dp))
        }
        // Only libraries with something to show here (a map, places, addresses).
        items(atlas.libs.filter { it.maps.isNotEmpty() || it.places > 0 || it.byCell || it.address }, key = { it.name }) { l ->
            Row(Modifier.fillMaxWidth().clickable { atlas.toggle(l.name) }.padding(horizontal = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                Checkbox(checked = l.name !in atlas.hidden, onCheckedChange = { atlas.toggle(l.name) })
                Column {
                    Text(l.name)
                    val what = when {
                        l.maps.isNotEmpty() -> "map"
                        l.address -> "addresses (searched)"
                        l.byCell -> "places, as looked at"
                        else -> "${l.places} place(s)"
                    }
                    val access = if (l.writable) "read and write" else "read only"
                    Text("$what · $access · ${l.records} records, ${l.peers} peer(s)" + if (l.lazy) " · fetched as needed" else "", style = MaterialTheme.typography.bodySmall)
                }
            }
        }
        item {
            HorizontalDivider(Modifier.padding(vertical = 8.dp))
            Text("Map sees only the libraries retifs let it in to.", style = MaterialTheme.typography.bodySmall, modifier = Modifier.padding(horizontal = 16.dp))
            Box(Modifier.padding(16.dp)) { OutlinedButton(onClick = letIn) { Text("Choose libraries…") } }
        }
    }
}

/** retifs can't be used yet: why, and what to do. */
@Composable
fun Problem(p: Retifs.Problem, letIn: () -> Unit, retry: () -> Unit) {
    val context = LocalContext.current
    Column(Modifier.fillMaxSize().statusBarsPadding().padding(24.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        when (p) {
            Retifs.Problem.NOT_INSTALLED -> {
                Text("retifs isn't installed", style = MaterialTheme.typography.titleMedium)
                Text("Map shows the maps and places in your retifs libraries. Install the retifs app (from the retifs-installers library) first.")
            }
            Retifs.Problem.NOT_LET_IN -> {
                Text("Let Map use retifs", style = MaterialTheme.typography.titleMedium)
                Text("retifs asks which of your libraries Map may see, and whether it may add to them (to save places).")
                Button(onClick = letIn) { Text("Choose libraries…") }
            }
            Retifs.Problem.NOT_RUNNING -> {
                Text("retifs isn't running", style = MaterialTheme.typography.titleMedium)
                Text("Open retifs: it starts once Reticulum (Prns) is up.")
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Retifs.open(context)?.let { i -> OutlinedButton(onClick = { context.startActivity(i) }) { Text("Open retifs") } }
                    Button(onClick = retry) { Text("Try again") }
                }
            }
        }
    }
}
