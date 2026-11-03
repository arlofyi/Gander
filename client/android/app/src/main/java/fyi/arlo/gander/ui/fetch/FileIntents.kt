package fyi.arlo.gander.ui.fetch

import android.content.ActivityNotFoundException
import android.content.ClipData
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.webkit.MimeTypeMap
import androidx.core.content.FileProvider
import fyi.arlo.gander.BuildConfig
import fyi.arlo.gander.core.data.fetch.CachedFile
import java.io.File
import java.util.Locale

/**
 * Handing a fetched file to other apps (client spec, "Open with"): the file stays in app-private
 * storage and is exposed only through the [FileProvider] (`fetched/` and nothing else), with a
 * one-off read grant on the `content://` URI.
 *
 * - [view]: `ACTION_VIEW` with the MIME type and `FLAG_GRANT_READ_URI_PERMISSION`; no chooser is
 *   forced, the system resolver is the "Open with" (and remembers "always" itself).
 * - [share]: `ACTION_SEND` with `EXTRA_STREAM` wrapped in `createChooser`.
 */
object FileIntents {
    const val AUTHORITY = BuildConfig.APPLICATION_ID + ".fetched"

    fun uriFor(context: Context, file: File): Uri = FileProvider.getUriForFile(context, AUTHORITY, file, file.name)

    /** The host's MIME type, else one from the extension, else `application/octet-stream`. */
    fun mimeOf(name: String, hostMime: String?): String {
        hostMime?.trim()?.takeIf { it.contains('/') && it != "application/octet-stream" }?.let { return it }
        val ext = name.substringAfterLast('.', "").lowercase(Locale.ROOT)
        return MimeTypeMap.getSingleton().getMimeTypeFromExtension(ext) ?: "application/octet-stream"
    }

    fun viewIntent(context: Context, cached: CachedFile, file: File): Intent {
        val uri = uriFor(context, file)
        return Intent(Intent.ACTION_VIEW)
            .setDataAndType(uri, mimeOf(cached.name, cached.mime))
            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
    }

    fun shareIntent(context: Context, cached: CachedFile, file: File, title: CharSequence): Intent {
        val uri = uriFor(context, file)
        val send = Intent(Intent.ACTION_SEND)
            .setType(mimeOf(cached.name, cached.mime))
            .putExtra(Intent.EXTRA_STREAM, uri)
            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        // The ClipData carries the grant through the chooser to the chosen app.
        send.clipData = ClipData.newRawUri(cached.name, uri)
        return Intent.createChooser(send, title)
    }

    /** Opens with the system resolver. False when no installed app handles the type. */
    fun view(context: Context, cached: CachedFile, file: File): Boolean = start(context, viewIntent(context, cached, file))

    fun share(context: Context, cached: CachedFile, file: File, title: CharSequence): Boolean =
        start(context, shareIntent(context, cached, file, title))

    private fun start(context: Context, intent: Intent): Boolean = try {
        if (context !is android.app.Activity) intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        context.startActivity(intent)
        true
    } catch (e: ActivityNotFoundException) {
        fyi.arlo.gander.core.DebugLog.failure("open fetched file", e)
        false
    }
}
