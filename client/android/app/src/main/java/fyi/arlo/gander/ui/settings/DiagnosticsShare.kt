package fyi.arlo.gander.ui.settings

import android.content.ActivityNotFoundException
import android.content.ClipData
import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.core.content.FileProvider
import fyi.arlo.gander.BuildConfig
import java.io.File
import java.io.IOException
import java.time.LocalDateTime
import java.time.format.DateTimeFormatter

/**
 * Hands the redacted diagnostics report to the share sheet. The text is written to one file under
 * `cacheDir/diagnostics/` (the system may clear it; only the newest report is kept) and leaves the
 * app as a `content://` URI with a one-off read grant, through its own `FileProvider`
 * (`<applicationId>.diagnostics`, `res/xml/diagnostics_paths.xml`) so that the fetched-files provider
 * keeps exposing `fetched/` and nothing else.
 */
object DiagnosticsShare {
    const val AUTHORITY = BuildConfig.APPLICATION_ID + ".diagnostics"
    const val DIR = "diagnostics"

    private val STAMP = DateTimeFormatter.ofPattern("yyyyMMdd-HHmmss")

    /** Writes [text] as the only report file and returns it. Blocking: call from a background dispatcher. */
    @Throws(IOException::class)
    fun write(context: Context, text: String, now: LocalDateTime = LocalDateTime.now()): File {
        val dir = File(context.cacheDir, DIR)
        if (!dir.isDirectory && !dir.mkdirs()) throw IOException("cannot create the diagnostics folder")
        dir.listFiles()?.forEach { it.delete() }
        val file = File(dir, "gander-diagnostics-${STAMP.format(now)}.txt")
        file.writeText(text, Charsets.UTF_8)
        return file
    }

    /** `ACTION_SEND` of [file] as `text/plain`, wrapped in a chooser. */
    fun sendIntent(context: Context, file: File, chooserTitle: CharSequence, subject: CharSequence): Intent {
        val uri: Uri = FileProvider.getUriForFile(context, AUTHORITY, file)
        val send = Intent(Intent.ACTION_SEND)
            .setType("text/plain")
            .putExtra(Intent.EXTRA_STREAM, uri)
            .putExtra(Intent.EXTRA_SUBJECT, subject)
            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        // The ClipData carries the grant through the chooser to the chosen app.
        send.clipData = ClipData.newRawUri(file.name, uri)
        return Intent.createChooser(send, chooserTitle)
    }

    /** Opens the share sheet. False when no app can take a text file. */
    fun send(context: Context, file: File, chooserTitle: CharSequence, subject: CharSequence): Boolean = try {
        val intent = sendIntent(context, file, chooserTitle, subject)
        if (context !is android.app.Activity) intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        context.startActivity(intent)
        true
    } catch (_: ActivityNotFoundException) {
        false
    }
}
