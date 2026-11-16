package org.retifs.map

import android.app.ActivityManager
import android.app.ApplicationExitInfo
import android.content.Context
import android.os.Build
import java.io.File

/**
 * Crash and trouble reports (retifs ADR-0036): this app's crashes go to
 * retifs, which keeps them in the user's crash library (`retifs-crashlogs`,
 * once the device is in it).
 *
 * A crash is written to this app's files at once (the process is ending)
 * and sent on the next start, with what Android recorded about other ends
 * of this app: native crashes, ANRs, being killed while in use.
 *
 * The same file is in every retifs Android app; only the package differs.
 */
object CrashReports {
    private lateinit var dir: File
    private lateinit var header: String
    private const val KEEP = 50
    /** A report's text at most (retifsd takes up to 256 KiB). */
    private const val MAX_TEXT = 200_000

    /**
     * Call once at start, after the core can connect. [nativeDir] gives the
     * core the folder (and header) its own panics are written to; [send]
     * hands a report (kind, source, text) to retifs: true once it took it.
     */
    fun install(context: Context, nativeDir: (String, String) -> Unit, send: (String, String, String) -> Boolean) {
        val app = context.applicationContext
        dir = File(app.filesDir, "crash").apply { mkdirs() }
        val version = runCatching { app.packageManager.getPackageInfo(app.packageName, 0).versionName }.getOrNull() ?: "?"
        header = "app: ${app.packageName} $version\n" +
            "android: ${Build.VERSION.RELEASE} (API ${Build.VERSION.SDK_INT})\n" +
            "device: ${Build.MANUFACTURER} ${Build.MODEL}\n\n"
        runCatching { nativeDir(dir.path, header) }
        val previous = Thread.getDefaultUncaughtExceptionHandler()
        Thread.setDefaultUncaughtExceptionHandler { thread, e ->
            runCatching { write("app_crash", System.currentTimeMillis(), "Uncaught exception in thread ${thread.name}:\n${e.stackTraceToString()}") }
            previous?.uncaughtException(thread, e)
        }
        Thread({
            runCatching { exits(app) }
            runCatching { deliver(app, send) }
        }, "crash reports").start()
    }

    private fun write(kind: String, at: Long, text: String) {
        val f = File(dir, "$at-$kind.txt")
        val part = File(dir, ".${f.name}.part")
        part.writeText(header + text.take(MAX_TEXT))
        part.renameTo(f)
    }

    /** What Android recorded about this app's ends since the last look (Android 11+). */
    private fun exits(context: Context) {
        if (Build.VERSION.SDK_INT < 30) return
        val prefs = context.getSharedPreferences("crash_reports", Context.MODE_PRIVATE)
        val seen = prefs.getLong("exits_seen", System.currentTimeMillis() - 3 * 86_400_000L)
        val am = context.getSystemService(ActivityManager::class.java) ?: return
        var newest = seen
        for (x in am.getHistoricalProcessExitReasons(context.packageName, 0, 16)) {
            if (x.timestamp <= seen) continue
            newest = maxOf(newest, x.timestamp)
            val kind = kindOf(x) ?: continue
            val trace = if (x.reason == ApplicationExitInfo.REASON_ANR) {
                runCatching { x.traceInputStream?.use { String(it.readBytes()) } }.getOrNull()
            } else null
            write(
                kind,
                x.timestamp,
                "Android ended process ${x.processName} (pid ${x.pid}): $kind\n" +
                    "description: ${x.description}\n" +
                    "status: ${x.status}  importance: ${x.importance}  pss: ${x.pss} kB  rss: ${x.rss} kB\n" +
                    (trace?.let { "\n--- trace ---\n$it" } ?: ""),
            )
        }
        prefs.edit().putLong("exits_seen", newest).apply()
    }

    /**
     * The ends worth a report: crashes outside Kotlin (those in Kotlin are
     * written as they happen), ANRs, and being killed while the user had it
     * in use (in front, playing, syncing), not as a cached app.
     */
    private fun kindOf(x: ApplicationExitInfo): String? {
        val inUse = x.importance <= ActivityManager.RunningAppProcessInfo.IMPORTANCE_PERCEPTIBLE
        return when (x.reason) {
            ApplicationExitInfo.REASON_CRASH_NATIVE -> "native_crash"
            ApplicationExitInfo.REASON_ANR -> "anr"
            ApplicationExitInfo.REASON_INITIALIZATION_FAILURE -> "init_failure"
            ApplicationExitInfo.REASON_EXCESSIVE_RESOURCE_USAGE -> "excessive_use"
            ApplicationExitInfo.REASON_DEPENDENCY_DIED -> "dependency_died"
            ApplicationExitInfo.REASON_LOW_MEMORY -> if (inUse) "low_memory" else null
            ApplicationExitInfo.REASON_SIGNALED -> if (inUse) "killed" else null
            else -> null
        }
    }

    /** The waiting reports to retifs, oldest first; kept if it can't take them now. */
    private fun deliver(context: Context, send: (String, String, String) -> Boolean) {
        val files = dir.listFiles { f -> f.name.endsWith(".txt") }?.sortedBy { it.name } ?: return
        files.dropLast(KEEP).forEach { it.delete() }
        for (f in files.takeLast(KEEP)) {
            val kind = f.name.substringAfter('-').removeSuffix(".txt")
            if (!send(kind, context.packageName, f.readText())) return
            f.delete()
        }
    }
}
