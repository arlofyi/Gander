package fyi.arlo.gander.core.data.thumbs

import coil3.decode.DataSource
import coil3.decode.ImageSource
import coil3.disk.DiskCache
import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.ThumbPriority
import kotlinx.coroutines.CancellationException
import okio.Buffer
import okio.FileSystem
import java.util.concurrent.ConcurrentHashMap

/** What the Coil decoder reads: an image from the disk cache, or the bytes just fetched. */
class LoadedThumb(val source: ImageSource, val mimeType: String?, val dataSource: DataSource)

/**
 * Everything between "Coil wants this [ThumbRef]" and "here are its bytes", without Coil's
 * fetcher types so it runs in plain JVM tests: disk cache lookup, then the host through a
 * per-host [ThumbScheduler] and [ThumbRetryPolicy], then a write to the disk cache.
 *
 * Disk entries are immutable by [ThumbRef.cacheKey] (it carries the file's version and the pixel
 * size), so a hit is never revalidated and an edited file simply gets a new key.
 */
class ThumbLoader(
    private val source: ThumbSource,
    private val retry: ThumbRetryPolicy = ThumbRetryPolicy(),
    private val schedulerFor: (hostId: String) -> ThumbScheduler = PerHostSchedulers(),
    private val sleep: (suspend (millis: Long) -> Unit)? = null,
    private val failures: ThumbFailureMemo = ThumbFailureMemo(),
) {

    /**
     * Loads [ref]. [diskCache] may be null; [readDisk] and [writeDisk] mirror Coil's
     * `CachePolicy` of the request. Throws what [ThumbSource.load] throws once retries are used up
     * (the UI then keeps the type glyph), and rethrows a remembered refusal without asking again.
     */
    suspend fun load(
        ref: ThumbRef,
        priority: ThumbPriority,
        diskCache: DiskCache?,
        readDisk: Boolean = true,
        writeDisk: Boolean = true,
    ): LoadedThumb {
        val key = ref.cacheKey()
        if (diskCache != null && readDisk) readFromDisk(diskCache, key)?.let { return it }
        failures.get(key)?.let { throw it }
        val data = try {
            val scheduler = schedulerFor(ref.hostId)
            retry.run(sleep) { scheduler.run(priority) { source.load(ref, priority) } }
        } catch (e: CancellationException) {
            throw e
        } catch (e: GanderError.Host) {
            if (ThumbFailureMemo.isPermanent(e)) failures.put(key, e)
            throw e
        }
        return if (diskCache != null && writeDisk) {
            writeToDisk(diskCache, key, data) ?: fromBuffer(data)
        } else {
            fromBuffer(data)
        }
    }
}

// Disk cache and in-memory helpers, shared with the preview page loader (same entry layout:
// data = the image bytes, metadata = the MIME type).
internal fun readFromDisk(cache: DiskCache, key: String): LoadedThumb? {
    val snapshot = cache.openSnapshot(key) ?: return null
    return try {
        val mime = runCatching { cache.fileSystem.read(snapshot.metadata) { readUtf8() } }.getOrNull()
            ?.takeIf { it.isNotBlank() }
        LoadedThumb(
            source = ImageSource(
                file = snapshot.data,
                fileSystem = cache.fileSystem,
                diskCacheKey = key,
                closeable = snapshot,
            ),
            mimeType = mime,
            dataSource = DataSource.DISK,
        )
    } catch (e: Exception) {
        snapshot.close()
        null
    }
}

/** Null if the entry could not be written (another editor holds it, or the disk is full). */
internal fun writeToDisk(cache: DiskCache, key: String, data: ThumbData): LoadedThumb? {
    val editor = cache.openEditor(key) ?: return null
    return try {
        cache.fileSystem.write(editor.data) { write(data.bytes) }
        cache.fileSystem.write(editor.metadata) { writeUtf8(data.mimeType.orEmpty()) }
        val snapshot = editor.commitAndOpenSnapshot() ?: return null
        LoadedThumb(
            source = ImageSource(
                file = snapshot.data,
                fileSystem = cache.fileSystem,
                diskCacheKey = key,
                closeable = snapshot,
            ),
            mimeType = data.mimeType,
            dataSource = DataSource.NETWORK,
        )
    } catch (e: Exception) {
        runCatching { editor.abort() }
        null
    }
}

internal fun fromBuffer(data: ThumbData): LoadedThumb = LoadedThumb(
    source = ImageSource(Buffer().write(data.bytes), FileSystem.SYSTEM),
    mimeType = data.mimeType,
    dataSource = DataSource.NETWORK,
)

/** One [ThumbScheduler] per host, created on first use. */
class PerHostSchedulers(
    private val concurrency: Int = ThumbScheduler.DEFAULT_CONCURRENCY,
) : (String) -> ThumbScheduler {
    private val schedulers = ConcurrentHashMap<String, ThumbScheduler>()

    override fun invoke(hostId: String): ThumbScheduler =
        schedulers.getOrPut(hostId) { ThumbScheduler(concurrency) }
}

/**
 * Remembers, for this process, thumbnails the host refused for good (415 not thumbnailable, 413,
 * 404, 400, 403), so scrolling back over a file the host predicted wrongly does not make it spawn
 * a render worker again for every pass. Keyed by [ThumbRef.cacheKey], so an edited file (new
 * version) is asked again. "Not ready" and network failures are never remembered.
 */
class ThumbFailureMemo(private val maxEntries: Int = 512) {
    private val entries = object : LinkedHashMap<String, GanderError.Host>(16, 0.75f, true) {
        override fun removeEldestEntry(eldest: MutableMap.MutableEntry<String, GanderError.Host>?) =
            size > maxEntries
    }

    fun get(key: String): GanderError.Host? = synchronized(entries) { entries[key] }

    fun put(key: String, error: GanderError.Host) {
        synchronized(entries) { entries[key] = error }
    }

    companion object {
        fun isPermanent(error: GanderError.Host): Boolean = when (error) {
            is GanderError.Host.UnsupportedType,
            is GanderError.Host.TooLarge,
            is GanderError.Host.NotFound,
            is GanderError.Host.BadPath,
            is GanderError.Host.BadRequest,
            is GanderError.Host.PermissionDenied,
            is GanderError.Host.Forbidden,
            -> true
            else -> false
        }
    }
}
