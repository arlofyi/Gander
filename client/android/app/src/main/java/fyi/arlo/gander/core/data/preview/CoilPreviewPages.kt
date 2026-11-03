package fyi.arlo.gander.core.data.preview

import android.content.Context
import coil3.ImageLoader
import coil3.disk.DiskCache
import coil3.fetch.FetchResult
import coil3.fetch.Fetcher
import coil3.fetch.SourceFetchResult
import coil3.getExtra
import coil3.key.Keyer
import coil3.request.ImageRequest
import coil3.request.Options
import coil3.size.Size
import fyi.arlo.gander.core.data.thumbs.ThumbPriorityKey
import fyi.arlo.gander.core.data.thumbs.thumbPriority
import fyi.arlo.gander.core.net.ThumbPriority
import okio.Path.Companion.toOkioPath
import java.io.File

/** Memory cache key of a [PreviewPageRef]: version, page and width included, token never. */
class PreviewPageKeyer : Keyer<PreviewPageRef> {
    override fun key(data: PreviewPageRef, options: Options): String = data.cacheKey()
}

/**
 * Coil fetcher for [PreviewPageRef]: delegates to [PreviewPageLoader]. Cancelling the request (a
 * page that left the pager, `Disposable.dispose()`) cancels this coroutine, which leaves the
 * scheduler queue, stops a pending retry and cancels the HTTP call. The request's
 * [ThumbPriority] (visible by default) decides who goes first when the host's single preview slot
 * is busy.
 */
class PreviewPageFetcher(
    private val ref: PreviewPageRef,
    private val priority: ThumbPriority,
    private val loader: PreviewPageLoader,
    private val diskCache: DiskCache?,
    private val readDisk: Boolean,
    private val writeDisk: Boolean,
) : Fetcher {

    override suspend fun fetch(): FetchResult {
        val loaded = loader.load(ref, priority, diskCache, readDisk, writeDisk)
        return SourceFetchResult(loaded.source, loaded.mimeType, loaded.dataSource)
    }

    /** [diskCache] is the preview pages' own cache, not Coil's (thumbnails keep theirs). */
    class Factory(
        private val loader: PreviewPageLoader,
        private val diskCache: DiskCache?,
    ) : Fetcher.Factory<PreviewPageRef> {
        override fun create(data: PreviewPageRef, options: Options, imageLoader: ImageLoader): Fetcher =
            PreviewPageFetcher(
                ref = data,
                priority = options.getExtra(ThumbPriorityKey),
                loader = loader,
                diskCache = diskCache,
                readDisk = options.diskCachePolicy.readEnabled,
                writeDisk = options.diskCachePolicy.writeEnabled,
            )
    }
}

/**
 * What [fyi.arlo.gander.core.data.thumbs.createThumbnailImageLoader] adds to register preview
 * pages on the app's one Coil loader: the loader of the bytes and the pages' own disk cache.
 */
class PreviewPageComponents(
    val loader: PreviewPageLoader,
    val diskCache: DiskCache,
    /** Deep-zoom tiles (WP-67); they share [diskCache] and, in the app, the page loader's scheduler. */
    val tiles: PreviewTileLoader? = null,
)

/** Pages are 100-250 KB each; this holds a few hundred. */
const val DEFAULT_PREVIEW_DISK_BYTES: Long = 256L * 1024 * 1024

/** The disk cache of preview pages under the app's private cache directory (the system may clear it). */
fun createPreviewDiskCache(
    context: Context,
    cacheDir: File = File(context.cacheDir, "previews"),
    maxDiskBytes: Long = DEFAULT_PREVIEW_DISK_BYTES,
): DiskCache = DiskCache.Builder()
    .directory(cacheDir.toOkioPath())
    .maxSizeBytes(maxDiskBytes)
    .build()

/**
 * The request that draws [ref] at full decoded size: the page is already as wide as the screen
 * (or the host allows), and zooming scales that bitmap, so Coil must not downsample it to the
 * layout bounds. Prefetch and display use the same request shape so they share a memory cache entry.
 */
fun ImageRequest.Builder.previewPage(
    ref: PreviewPageRef,
    priority: ThumbPriority = ThumbPriority.VISIBLE,
): ImageRequest.Builder = data(ref).size(Size.ORIGINAL).thumbPriority(priority)

/** Something started that can still be stopped. */
fun interface Cancelable {
    fun cancel()
}

/** Warms the caches with a page nobody is looking at yet (the pager's neighbours). */
fun interface PagePrefetcher {
    /** Starts fetching [ref] at prefetch priority; [Cancelable.cancel] stops it if it is still running. */
    fun prefetch(ref: PreviewPageRef): Cancelable

    companion object {
        /** For previews and screens without a loader. */
        val None = PagePrefetcher { Cancelable { } }
    }
}

/** [PagePrefetcher] over Coil: the page ends up in the memory and disk cache. */
class CoilPagePrefetcher(
    private val context: Context,
    private val imageLoader: ImageLoader,
) : PagePrefetcher {
    override fun prefetch(ref: PreviewPageRef): Cancelable {
        val disposable = imageLoader.enqueue(
            ImageRequest.Builder(context).previewPage(ref, ThumbPriority.PREFETCH).build(),
        )
        return Cancelable { disposable.dispose() }
    }
}

/** Memory cache key of a [PreviewTileRef]: version, page, level and position included, token never. */
class PreviewTileKeyer : Keyer<PreviewTileRef> {
    override fun key(data: PreviewTileRef, options: Options): String = data.cacheKey()
}

/**
 * Coil fetcher for [PreviewTileRef] (WP-67): delegates to [PreviewTileLoader]. Cancelling the
 * request (a tile that left the screen leaves the composition and disposes the request) cancels
 * this coroutine, which leaves the scheduler queue, stops a pending retry and cancels the HTTP
 * call.
 */
class PreviewTileFetcher(
    private val ref: PreviewTileRef,
    private val loader: PreviewTileLoader,
    private val diskCache: DiskCache?,
    private val readDisk: Boolean,
    private val writeDisk: Boolean,
) : Fetcher {

    override suspend fun fetch(): FetchResult {
        val loaded = loader.load(ref, diskCache, readDisk, writeDisk)
        return SourceFetchResult(loaded.source, loaded.mimeType, loaded.dataSource)
    }

    class Factory(
        private val loader: PreviewTileLoader,
        private val diskCache: DiskCache?,
    ) : Fetcher.Factory<PreviewTileRef> {
        override fun create(data: PreviewTileRef, options: Options, imageLoader: ImageLoader): Fetcher =
            PreviewTileFetcher(
                ref = data,
                loader = loader,
                diskCache = diskCache,
                readDisk = options.diskCachePolicy.readEnabled,
                writeDisk = options.diskCachePolicy.writeEnabled,
            )
    }
}

/** The request that draws one tile at its own decoded size: tiles are placed and scaled by the layout, never downsampled by Coil. */
fun ImageRequest.Builder.previewTile(ref: PreviewTileRef): ImageRequest.Builder = data(ref).size(Size.ORIGINAL)
