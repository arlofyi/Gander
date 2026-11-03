package fyi.arlo.gander.core.data.actions

import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.webkit.MimeTypeMap
import androidx.core.net.toUri
import fyi.arlo.gander.core.net.protocol.Entry
import java.util.Locale

/**
 * Whether this phone has an app that can open a file of this type, for rule 3 of the default-action
 * table ("phone has a handler for the MIME type"). An interface so the file sheet's ViewModel
 * is tested without `PackageManager`.
 */
fun interface PhoneHandlerLookup {
    /** [mime] is what the host reported (may be null); [ext] is the lower-case extension, if any. */
    fun hasHandler(mime: String?, ext: String?): Boolean
}

/** No handler for anything; the default in tests and previews. */
object NoPhoneHandlers : PhoneHandlerLookup {
    override fun hasHandler(mime: String?, ext: String?) = false
}

fun PhoneHandlerLookup.hasHandlerFor(entry: Entry): Boolean = hasHandler(entry.mime, entry.extension())

/**
 * Asks `PackageManager` who would take an `ACTION_VIEW` for the type, the same intent shape
 * open-with uses (WP-27: a `content://` URI plus the MIME type). Needs the `<queries>` entry in the
 * manifest for package visibility on Android 11+; without it every lookup would answer "no".
 */
class PackageManagerHandlerLookup(context: Context) : PhoneHandlerLookup {
    private val packageManager = context.applicationContext.packageManager

    override fun hasHandler(mime: String?, ext: String?): Boolean {
        val type = mime?.trim()?.lowercase(Locale.ROOT)?.takeIf { it.contains('/') && it != GENERIC_BINARY }
            ?: ext?.let { MimeTypeMap.getSingleton().getMimeTypeFromExtension(it) }
            ?: return false
        val intent = Intent(Intent.ACTION_VIEW).setDataAndType(PROBE_URI.toUri(), type)
        return try {
            queryActivities(intent).isNotEmpty()
        } catch (_: RuntimeException) {
            // A transaction failure from the package service: say "no handler" rather than crash a sheet.
            false
        }
    }

    private fun queryActivities(intent: Intent) =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            packageManager.queryIntentActivities(
                intent,
                PackageManager.ResolveInfoFlags.of(PackageManager.MATCH_DEFAULT_ONLY.toLong()),
            )
        } else {
            @Suppress("DEPRECATION")
            packageManager.queryIntentActivities(intent, PackageManager.MATCH_DEFAULT_ONLY)
        }

    private companion object {
        /** Only used to shape the query; nothing is ever opened through it. */
        const val PROBE_URI = "content://fyi.arlo.gander.fileprovider/probe"

        /** Every app that opens "anything" would match: that is not a handler for this file. */
        const val GENERIC_BINARY = "application/octet-stream"
    }
}
