package fyi.arlo.gander.core.data.fetch

import fyi.arlo.gander.core.data.browse.EntryTarget
import fyi.arlo.gander.core.net.protocol.Entry
import fyi.arlo.gander.core.net.protocol.EntryKind
import fyi.arlo.gander.core.net.protocol.PreviewKind
import fyi.arlo.gander.core.net.protocol.StreamDecision
import fyi.arlo.gander.core.net.protocol.ZipPlan

/**
 * Folder download as a zip (WP-68, `GET /v1/zip`) rides the fetch engine as a non-resumable job.
 *
 * A zip job is an ordinary [FetchJob] (queue, notification, Downloads list, cellular rule, Save to
 * device) whose [FileKey.path] is `zip:<folder path>`. A `:` can never be part of a share path
 * (the host refuses it), so the marker cannot collide with a real file, and the job table needs no
 * new column. Its [Entry] is a synthetic file `<folder>.zip` whose size is the estimate from the
 * dry run, so the cellular rule and the progress bar have a number to work with.
 *
 * What differs from a file fetch, all in [FetchEngine]: the URL is `/v1/zip`, there is no `Range`
 * and no `If-Range` (the host has no resume for a zip), a connection that breaks starts the
 * download over from zero, and nothing is verified (there is no ETag or hash for an archive that
 * is built as it is sent).
 */
object FolderZip {
    /** Prefix of [FileKey.path] for a zip job. */
    const val MARKER = "zip:"

    /** Rough bytes the zip adds per entry (local and central headers, descriptor, name twice). */
    private const val ENTRY_OVERHEAD = 160L

    /** End-of-archive record and the folder entry itself. */
    private const val ARCHIVE_OVERHEAD = 512L

    fun isZip(key: FileKey): Boolean = key.path.startsWith(MARKER)

    /** The folder a zip job's key stands for (share-relative, `""` is the share root). */
    fun folderPath(key: FileKey): String = key.path.removePrefix(MARKER)

    /** `Folder.zip` for a folder (never a path; the folder's own name or the share's). */
    fun fileName(folderName: String): String {
        val base = FetchFiles.safeName(folderName.ifBlank { "folder" })
        return if (base.endsWith(".zip", ignoreCase = true)) base else "$base.zip"
    }

    /** What the archive will weigh: the files (stored, not compressed) plus the zip's headers. */
    fun estimatedSize(plan: ZipPlan): Long =
        plan.bytes.coerceAtLeast(0) + plan.entries.coerceAtLeast(0) * ENTRY_OVERHEAD + ARCHIVE_OVERHEAD

    /**
     * The fetch target of [folder] (an [EntryTarget] of a folder) once the dry run said [plan]:
     * hand it to the fetch queue like a file. [mtime] stamps the saved copy.
     */
    fun target(folder: EntryTarget, plan: ZipPlan, mtime: String): EntryTarget {
        val name = fileName(folder.entry.name)
        val entry = Entry(
            name = name,
            kind = EntryKind.FILE,
            size = estimatedSize(plan),
            mtime = mtime,
            ctime = null,
            ext = "zip",
            mime = "application/zip",
            flags = emptyList(),
            thumb = false,
            preview = PreviewKind.NONE,
            stream = StreamDecision.REFUSED,
        )
        return EntryTarget(folder.hostId, folder.shareId, MARKER + folder.path, entry)
    }
}
