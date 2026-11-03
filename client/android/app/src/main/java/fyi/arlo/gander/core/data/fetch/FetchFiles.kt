package fyi.arlo.gander.core.data.fetch

import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.withContext
import java.io.File
import java.io.IOException

/**
 * Where fetched bytes live (client spec: "write into app-private storage, expose via
 * FileProvider"). Everything is under `<filesDir>/fetched/`, which is the only folder the
 * `FileProvider` exposes and which backup and device transfer exclude:
 *
 * ```
 * fetched/j<job id>/download.part   while the job runs (Range resume appends here)
 * fetched/j<job id>/<safe name>     once finished and checked; this path goes into cached_files
 * ```
 *
 * One folder per job keeps two files of the same name apart and makes cleanup a folder delete.
 * The host's file name is never used as a path: [safeName] turns it into one segment.
 */
class FetchFiles(private val filesDir: File) {
    val root: File = File(filesDir, DIR)

    fun jobDir(jobId: Long): File = File(root, "j$jobId")

    fun partialFile(jobId: Long): File = File(jobDir(jobId), PARTIAL_NAME)

    /** `fetched/j<id>/<safe name>`, relative to `filesDir`. */
    fun finalRelativePath(jobId: Long, name: String): String = "$DIR/j$jobId/${safeName(name)}"

    /**
     * The file at a stored relative path, or null when the path does not point inside [root]
     * (a damaged or foreign row must never make the app open or delete something else).
     */
    fun resolve(relativePath: String): File? {
        if (relativePath.isEmpty()) return null
        val file = File(filesDir, relativePath)
        val rootCanonical = runCatching { root.canonicalFile }.getOrNull() ?: return null
        val canonical = runCatching { file.canonicalFile }.getOrNull() ?: return null
        return canonical.takeIf { it.path.startsWith(rootCanonical.path + File.separator) }
    }

    /** Creates the job's folder and returns the partial file (not created). */
    @Throws(IOException::class)
    fun preparePartial(jobId: Long): File {
        val dir = jobDir(jobId)
        if (!dir.isDirectory && !dir.mkdirs()) throw IOException("cannot create the download folder")
        return partialFile(jobId)
    }

    fun deleteJobDir(jobId: Long) {
        jobDir(jobId).deleteRecursively()
    }

    /** Deletes the folder holding the file at [relativePath] (a cached copy being replaced or removed). */
    fun deleteContaining(relativePath: String) {
        val file = resolve(relativePath) ?: return
        val dir = file.parentFile ?: return
        if (dir.parentFile?.canonicalPath == root.canonicalPath) dir.deleteRecursively() else file.delete()
    }

    /** Removes job folders whose id is not in [keep]. Returns how many were removed. */
    fun sweep(keep: Set<Long>): Int {
        val dirs = root.listFiles() ?: return 0
        var removed = 0
        for (dir in dirs) {
            val id = dir.name.removePrefix("j").toLongOrNull()
            if (!dir.name.startsWith("j") || id == null || id !in keep) {
                if (dir.deleteRecursively()) removed += 1
            }
        }
        return removed
    }

    /**
     * Bytes the app may still write. Deliberately `usableSpace`, not `StorageManager.getAllocatableBytes`:
     * the check should be conservative and must not count other apps' clearable caches as ours.
     */
    @Suppress("UsableSpace")
    fun usableSpace(): Long = (if (root.exists()) root else filesDir).usableSpace

    companion object {
        const val DIR = "fetched"
        const val PARTIAL_NAME = "download.part"
        private const val MAX_NAME_CHARS = 120
        private val FORBIDDEN = setOf('/', '\\', ':', '*', '?', '"', '<', '>', '|')

        /** The job id encoded in a `fetched/j<id>/...` path, or null. */
        fun jobIdOf(relativePath: String): Long? =
            relativePath.split('/').getOrNull(1)?.removePrefix("j")?.toLongOrNull()

        /**
         * One safe path segment from a host file name: separators, reserved and control
         * characters become `_`, leading and trailing dots and spaces go, the length is capped
         * (keeping the extension). Never empty, never `.` or `..`.
         */
        fun safeName(name: String): String {
            val cleaned = buildString {
                for (ch in name) append(if (ch < ' ' || ch == '\u007f' || ch in FORBIDDEN) '_' else ch)
            }.trim(' ', '.')
            val base = cleaned.ifEmpty { "file" }
            if (base.length <= MAX_NAME_CHARS) return base
            val dot = base.lastIndexOf('.')
            val ext = if (dot > 0 && base.length - dot <= 16) base.substring(dot) else ""
            return base.substring(0, MAX_NAME_CHARS - ext.length).trimEnd(' ', '.') + ext
        }
    }
}

/**
 * The cached files with their bytes: the API WP-28's cache, Storage screen and Settings build
 * on, and what the file sheet's "saved copy" lookup reads.
 */
class OfflineFiles(
    private val store: CachedFileStore,
    private val files: FetchFiles,
    private val clock: () -> Long = System::currentTimeMillis,
) {
    val all = store.cachedFiles

    suspend fun get(key: FileKey): CachedFile? = store.cached(key)

    /** The file on disk of [cached], or null when it is gone (deleted, or a damaged row). */
    fun fileOf(cached: CachedFile): File? = files.resolve(cached.localPath)?.takeIf { it.isFile }

    /** The cached copy of [key] if it exists on disk; also counts as a use (LRU). */
    suspend fun open(key: FileKey): Pair<CachedFile, File>? {
        val cached = store.cached(key) ?: return null
        val file = fileOf(cached) ?: return null
        store.touch(key, clock())
        return cached to file
    }

    /** "Keep offline". */
    suspend fun setPinned(key: FileKey, pinned: Boolean): Boolean = store.setPinned(key, pinned)

    /**
     * Deletes the copy (row and bytes). Not cancellable half way: a row without its bytes is
     * cleaned up by the engine's sweep, but bytes without a row would sit there until then.
     */
    suspend fun remove(key: FileKey) = withContext(NonCancellable) {
        val cached = store.cached(key) ?: return@withContext
        store.deleteCached(key)
        files.deleteContaining(cached.localPath)
    }

    /** Bytes used by cached files (sizes from the rows). */
    suspend fun usedBytes(): Long = all.first().sumOf { it.size }

    /**
     * Deletes every copy that is not kept offline, of one computer ([hostId]) or of all of them.
     * Returns how many were removed and the bytes they held.
     */
    suspend fun removeUnpinned(hostId: String? = null): Removed {
        var count = 0
        var bytes = 0L
        for (cached in all.first()) {
            if (cached.pinned || (hostId != null && cached.key.hostId != hostId)) continue
            if (removeIfUnpinned(cached.key)) {
                count += 1
                bytes += cached.size
            }
        }
        return Removed(count, bytes)
    }

    /**
     * Brings the cache under [capBytes] by deleting the least recently used copies that are not
     * kept offline (see [CachePolicy] for the exact rule). Pinned copies are never touched, even
     * when they alone exceed the cap.
     */
    suspend fun evictToCap(capBytes: Long): EvictionResult {
        val plan = CachePolicy.plan(all.first(), capBytes, clock())
        var freed = 0L
        val evicted = mutableListOf<CachedFile>()
        for (victim in plan.victims) {
            // Re-read at delete time: "Keep offline" may have been switched on since the plan.
            if (removeIfUnpinned(victim.key)) {
                evicted += victim
                freed += victim.size
            }
        }
        return EvictionResult(evicted, freed, plan.usedBytes - freed, plan.retryInMillis)
    }

    private suspend fun removeIfUnpinned(key: FileKey): Boolean = withContext(NonCancellable) {
        val current = store.cached(key) ?: return@withContext false
        if (current.pinned) return@withContext false
        store.deleteCached(key)
        files.deleteContaining(current.localPath)
        true
    }
}

/** What a bulk removal deleted. */
data class Removed(val count: Int, val bytes: Long)
