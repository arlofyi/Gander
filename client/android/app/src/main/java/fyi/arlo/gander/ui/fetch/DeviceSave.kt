package fyi.arlo.gander.ui.fetch

import android.content.ContentResolver
import android.content.ContentValues
import android.net.Uri
import android.os.Build
import android.provider.MediaStore
import androidx.annotation.RequiresApi
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import java.io.File
import java.io.IOException

/**
 * "Save to device": copies a fetched file out of app-private storage into the phone's shared
 * Downloads, where the Files app, the gallery and every other app can find it.
 *
 * Fetch keeps its copy in `filesDir/fetched/` (private, evictable cache; Open with and Share hand
 * it out by `content://` grant). On a real tablet that read as "the file never downloaded to the
 * device": nothing appeared in Downloads. This is the explicit way to keep a copy there.
 *
 * Android 10+ (API 29): `MediaStore.Downloads` under `Download/Gander/`, no permission needed; the
 * system renames on a clash ("name (1).png"). Older versions: the caller asks the user where to
 * save with `ACTION_CREATE_DOCUMENT` and passes that URI to [copyTo].
 */
object DeviceSave {
    /** Folder under the shared Downloads collection. */
    const val SUBFOLDER = "Gander"

    /** `RELATIVE_PATH` for [SUBFOLDER] in Downloads. */
    val relativePath: String get() = "Download/$SUBFOLDER"

    /** True when [toDownloads] can be used (API 29+); otherwise ask with `ACTION_CREATE_DOCUMENT`. */
    val canSaveDirectly: Boolean get() = Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q

    /** The display name for the saved copy: the file's own name, never a path. */
    fun displayName(name: String): String =
        name.substringAfterLast('/').substringAfterLast('\\').trim().ifEmpty { "download" }

    /**
     * Copies [file] into Downloads/Gander as [name] with [mime]. Returns the new item's URI.
     * Throws [IOException] when the copy fails; the half-written item is removed.
     */
    @RequiresApi(Build.VERSION_CODES.Q)
    suspend fun toDownloads(resolver: ContentResolver, file: File, name: String, mime: String): Uri =
        withContext(Dispatchers.IO) {
            val values = ContentValues().apply {
                put(MediaStore.Downloads.DISPLAY_NAME, displayName(name))
                put(MediaStore.Downloads.MIME_TYPE, mime)
                put(MediaStore.Downloads.RELATIVE_PATH, relativePath)
                put(MediaStore.Downloads.IS_PENDING, 1)
            }
            val collection = MediaStore.Downloads.getContentUri(MediaStore.VOLUME_EXTERNAL_PRIMARY)
            val uri = resolver.insert(collection, values) ?: throw IOException("MediaStore refused the new download")
            try {
                copy(resolver, file, uri)
                resolver.update(uri, ContentValues().apply { put(MediaStore.Downloads.IS_PENDING, 0) }, null, null)
                uri
            } catch (e: Exception) {
                runCatching { resolver.delete(uri, null, null) }
                throw e
            }
        }

    /** Copies [file] to a URI the user picked (`ACTION_CREATE_DOCUMENT`, Android 8 and 9). */
    suspend fun copyTo(resolver: ContentResolver, file: File, target: Uri) = withContext(Dispatchers.IO) {
        copy(resolver, file, target)
    }

    private fun copy(resolver: ContentResolver, file: File, target: Uri) {
        val out = resolver.openOutputStream(target, "w") ?: throw IOException("cannot write the saved copy")
        out.use { sink -> file.inputStream().use { it.copyTo(sink) } }
    }
}
