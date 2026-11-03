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
 * The deep-zoom twin of [PreviewPageLoader] (WP-67): everything between "Coil wants this
 * [PreviewTileRef]" and "here are its bytes", without Coil's fetcher types so it runs in plain JVM
 * tests. Disk cache lookup (entries are immutable by [PreviewTileRef.cacheKey], which carries the
 * file's `mtime#size`), then the host through the per-host scheduler and [PreviewRetryPolicy],
 * then a write to the disk cache.
 *
 * Differences from a page:
 *
 * - [schedulerFor] should be the very scheduler the page loader uses (the app wires one
 *   [PerHostSchedulers] into both), so the host's single preview slot per device is shared: a tile
 *   queues behind a page the user is waiting for, and a tile that scrolled away leaves the queue
 *   when its request is cancelled (its composable left the screen).
 * - the retry budget is shorter ([TILE_BUDGET_MS]): a tile that took half a minute is no longer
 *   what the user is looking at.
 * - a refusal that will not change (the file has no tiles, the tile does not exist) is remembered
 *   so the viewer never asks the host for it again.
 */
class PreviewTileLoader(
    private val source: PreviewSource,
    private val retry: PreviewRetryPolicy = PreviewRetryPolicy(budgetMs = TILE_BUDGET_MS),
    private val schedulerFor: (hostId: String) -> ThumbScheduler = PerHostSchedulers(PreviewPageLoader.HOST_PREVIEW_SLOTS),
    private val sleep: (suspend (millis: Long) -> Unit)? = null,
    private val now: () -> Long = { System.nanoTime() / 1_000_000 },
    private val failures: ThumbFailureMemo = ThumbFailureMemo(),
) {

    /**
     * Loads [ref]. [diskCache] may be null; [readDisk] and [writeDisk] mirror Coil's `CachePolicy`.
     * Throws what [PreviewSource.tile] throws once the retry budget is used up, and rethrows a
     * remembered refusal without asking again.
     */
    suspend fun load(
        ref: PreviewTileRef,
        diskCache: DiskCache?,
        readDisk: Boolean = true,
        writeDisk: Boolean = true,
    ): LoadedThumb {
        val key = ref.cacheKey()
        if (diskCache != null && readDisk) readFromDisk(diskCache, key)?.let { return it }
        failures.get(key)?.let { throw it }
        val data = try {
            val scheduler = schedulerFor(ref.hostId)
            retry.run(sleep, now) { scheduler.run(ThumbPriority.VISIBLE) { source.tile(ref) } }
        } catch (e: CancellationException) {
            throw e
        } catch (e: GanderError.Host) {
            if (ThumbFailureMemo.isPermanent(e)) failures.put(key, e)
            throw e
        }
        val thumb = ThumbData(data.bytes, data.mimeType)
        return if (diskCache != null && writeDisk) {
            writeToDisk(diskCache, key, thumb) ?: fromBuffer(thumb)
        } else {
            fromBuffer(thumb)
        }
    }

    companion object {
        /** How long a tile is waited for when the host's queue is busy. */
        const val TILE_BUDGET_MS = 30_000L
    }
}
