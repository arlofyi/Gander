package fyi.arlo.gander.core

import android.util.Log
import fyi.arlo.gander.BuildConfig

/**
 * Debug-build-only diagnostics for failures the UI deliberately reduces to a short message.
 *
 * Release builds log nothing. Debug builds log the exception class chain and messages under the
 * tag `Gander` so a developer can read them with `adb logcat -s Gander`. Tokens never appear here:
 * they travel in headers, not in exception messages or URLs.
 */
object DebugLog {
    private const val TAG = "Gander"

    fun failure(where: String, t: Throwable) {
        if (!BuildConfig.DEBUG) return
        runCatching { Log.w(TAG, "$where failed: ${describe(t)}", t) }
    }

    /** A debug-only line with counts or states; never a path, name, token or file content. */
    fun note(message: String) {
        if (!BuildConfig.DEBUG) return
        runCatching { Log.d(TAG, message) }
    }

    private fun describe(t: Throwable): String =
        generateSequence(t) { it.cause?.takeIf { c -> c !== it } }
            .take(8)
            .joinToString(" <- ") { "${it.javaClass.simpleName}(${it.message?.take(300)})" }
}
