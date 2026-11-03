package fyi.arlo.gander.core.data.thumbs

import android.content.Context
import coil3.Extras
import coil3.ImageLoader
import coil3.disk.DiskCache
import coil3.fetch.FetchResult
import coil3.fetch.Fetcher
import coil3.fetch.SourceFetchResult
import coil3.getExtra
import coil3.key.Keyer
import coil3.request.ImageRequest
import coil3.request.Options
import fyi.arlo.gander.core.data.preview.PreviewPageComponents
import fyi.arlo.gander.core.data.preview.PreviewPageFetcher
import fyi.arlo.gander.core.data.preview.PreviewPageKeyer
import fyi.arlo.gander.core.data.preview.PreviewTileFetcher
import fyi.arlo.gander.core.data.preview.PreviewTileKeyer
import fyi.arlo.gander.core.net.ThumbPriority
import okio.Path.Companion.toOkioPath
import java.io.File

/** Coil `Extras` key carrying a request's [ThumbPriority]; absent means [ThumbPriority.VISIBLE]. */
val ThumbPriorityKey: Extras.Key<ThumbPriority> = Extras.Key(default = ThumbPriority.VISIBLE)

/** Marks a request as prefetch (ahead of the viewport) or visible (the default). */
fun ImageRequest.Builder.thumbPriority(priority: ThumbPriority): ImageRequest.Builder = apply {
    extras.set(ThumbPriorityKey, priority)
}

/** Memory cache key of a [ThumbRef]: version and size included, token never. */
class ThumbKeyer : Keyer<ThumbRef> {
    override fun key(data: ThumbRef, options: Options): String = data.cacheKey()
}

/**
 * Coil fetcher for [ThumbRef]: delegates to [ThumbLoader]. Cancelling the request (a row that
 * scrolled out of a lazy list, `Disposable.dispose()`) cancels this coroutine, which leaves the
 * scheduler queue, stops a pending retry and cancels the HTTP call.
 */
class ThumbFetcher(
    private val ref: ThumbRef,
    private val priority: ThumbPriority,
    private val loader: ThumbLoader,
    private val diskCache: DiskCache?,
    private val readDisk: Boolean,
    private val writeDisk: Boolean,
) : Fetcher {

    override suspend fun fetch(): FetchResult {
        val loaded = loader.load(ref, priority, diskCache, readDisk, writeDisk)
        return SourceFetchResult(loaded.source, loaded.mimeType, loaded.dataSource)
    }

    class Factory(private val loader: ThumbLoader) : Fetcher.Factory<ThumbRef> {
        override fun create(data: ThumbRef, options: Options, imageLoader: ImageLoader): Fetcher = ThumbFetcher(
            ref = data,
            priority = options.getExtra(ThumbPriorityKey),
            loader = loader,
            diskCache = imageLoader.diskCache,
            readDisk = options.diskCachePolicy.readEnabled,
            writeDisk = options.diskCachePolicy.writeEnabled,
        )
    }
}

/**
 * The app's Coil [ImageLoader]: the [ThumbRef] fetcher and keyer, and a disk cache for thumbnails
 * under the app's private cache directory (the system may clear it; thumbnails are re-fetched).
 * With [previews] the same loader also draws preview pages ([PreviewPageRef], WP-26), which keep
 * their own disk cache so a long document does not push thumbnails out of theirs.
 */
fun createThumbnailImageLoader(
    context: Context,
    loader: ThumbLoader,
    cacheDir: File = File(context.cacheDir, "thumbs"),
    maxDiskBytes: Long = DEFAULT_THUMB_DISK_BYTES,
    previews: PreviewPageComponents? = null,
): ImageLoader = ImageLoader.Builder(context)
    .components {
        add(ThumbKeyer())
        add(ThumbFetcher.Factory(loader))
        if (previews != null) {
            add(PreviewPageKeyer())
            add(PreviewPageFetcher.Factory(previews.loader, previews.diskCache))
            if (previews.tiles != null) {
                add(PreviewTileKeyer())
                add(PreviewTileFetcher.Factory(previews.tiles, previews.diskCache))
            }
        }
    }
    .diskCache {
        DiskCache.Builder()
            .directory(cacheDir.toOkioPath())
            .maxSizeBytes(maxDiskBytes)
            .build()
    }
    .build()

/** Thumbnails are 10 to 60 KB each; this holds a few thousand. */
const val DEFAULT_THUMB_DISK_BYTES: Long = 128L * 1024 * 1024
