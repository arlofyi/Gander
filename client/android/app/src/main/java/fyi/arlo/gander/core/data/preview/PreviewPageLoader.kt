package fyi.arlo.gander.core.data.preview

import coil3.disk.DiskCache
import fyi.arlo.gander.core.data.thumbs.LoadedThumb
import fyi.arlo.gander.core.data.thumbs.PerHostSchedulers
import fyi.arlo.gander.core.data.thumbs.ThumbData
import fyi.arlo.gander.core.data.thumbs.ThumbFailureMemo
import fyi.arlo.gander.core.data.thumbs.ThumbScheduler
import fyi.arlo.gander.core.data.thumbs.fromBuffer
import fyi.arlo.gander.core.data.thumbs.readFromDisk
import fyi.arlo.gander.core.data.thumbs.writeToDisk
import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.ThumbPriority
import kotlinx.coroutines.CancellationException

/**
 * Everything between "Coil wants this [PreviewPageRef]" and "here are its bytes", without Coil's
 * fetcher types so it runs in plain JVM tests: disk cache lookup, then the host through a
 * per-host [ThumbScheduler] and [PreviewRetryPolicy], then a write to the disk cache. The same
 * shape as the thumbnail loader, with two differences:
 *
 * - the retries are bounded by time, not count (a page can take 10-20 s to render), and
 * - the scheduler has one slot per host, matching the host's preview queue (1 per device): the
 *   page on screen goes first, prefetched neighbours wait behind it.
 *
 * Disk entries are immutable by [PreviewPageRef.cacheKey] (file version, page, width), so a hit
 * is never revalidated. A page of a [PreviewPageRef.persistent] false ref never touches the disk.
 */
class PreviewPageLoader(
    private val source: PreviewSource,
    private val retry: PreviewRetryPolicy = PreviewRetryPolicy(),
    private val schedulerFor: (hostId: String) -> ThumbScheduler = PerHostSchedulers(HOST_PREVIEW_SLOTS),
    private val sleep: (suspend (millis: Long) -> Unit)? = null,
    private val now: () -> Long = { System.nanoTime() / 1_000_000 },
    private val failures: ThumbFailureMemo = ThumbFailureMemo(),
) {

    /**
     * Loads [ref]. [diskCache] may be null; [readDisk] and [writeDisk] mirror Coil's `CachePolicy`
     * of the request. Throws what [PreviewSource.page] throws once the retry budget is used up,
     * and rethrows a remembered refusal (415, 413...) without asking again.
     */
    suspend fun load(
        ref: PreviewPageRef,
        priority: ThumbPriority,
        diskCache: DiskCache?,
        readDisk: Boolean = true,
        writeDisk: Boolean = true,
    ): LoadedThumb {
        val cache = diskCache.takeIf { ref.persistent }
        val key = ref.cacheKey()
        if (cache != null && readDisk) readFromDisk(cache, key)?.let { return it }
        failures.get(key)?.let { throw it }
        val data = try {
            val scheduler = schedulerFor(ref.hostId)
            retry.run(sleep, now) { scheduler.run(priority) { source.page(ref) } }
        } catch (e: CancellationException) {
            throw e
        } catch (e: GanderError.Host) {
            if (ThumbFailureMemo.isPermanent(e)) failures.put(key, e)
            throw e
        }
        val thumb = ThumbData(data.bytes, data.mimeType)
        return if (cache != null && writeDisk) {
            writeToDisk(cache, key, thumb) ?: fromBuffer(thumb)
        } else {
            fromBuffer(thumb)
        }
    }

    companion object {
        /** The host renders one preview at a time per device (preview-renderers spec, "Principles"). */
        const val HOST_PREVIEW_SLOTS = 1
    }
}
