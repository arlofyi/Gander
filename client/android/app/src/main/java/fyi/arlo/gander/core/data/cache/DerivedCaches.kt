package fyi.arlo.gander.core.data.cache

import coil3.ImageLoader
import coil3.disk.DiskCache
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/**
 * The caches that can always be rebuilt from the computer: the thumbnail disk cache (128 MiB,
 * WP-24) and the preview page disk cache (256 MiB, WP-26), plus Coil's memory cache. Unlike
 * fetched files they are not kept offline, and the system may clear them on its own.
 *
 * Coil's disk cache hashes its keys into file names, so it cannot be emptied one computer at a
 * time: [clear] empties all of it, and forgetting a computer uses that on purpose (nothing of its
 * files may stay on the phone; the other computers' thumbnails are fetched again when needed).
 */
interface DerivedCaches {
    /** Bytes the disk caches hold right now. */
    suspend fun sizeBytes(): Long

    /** Empties the thumbnail and preview disk caches and the in-memory image cache. */
    suspend fun clear()
}

/** [DerivedCaches] over the app's Coil loader and the preview page disk cache. */
class CoilDerivedCaches(
    private val imageLoader: () -> ImageLoader,
    private val previewDiskCache: () -> DiskCache,
) : DerivedCaches {

    override suspend fun sizeBytes(): Long = withContext(Dispatchers.IO) {
        (imageLoader().diskCache?.size ?: 0L) + previewDiskCache().size
    }

    override suspend fun clear() {
        withContext(Dispatchers.IO) {
            val loader = imageLoader()
            loader.memoryCache?.clear()
            loader.diskCache?.clear()
            previewDiskCache().clear()
        }
    }
}

/** Test and preview implementation. */
class FakeDerivedCaches(var size: Long = 0) : DerivedCaches {
    var clears = 0
        private set

    override suspend fun sizeBytes(): Long = size

    override suspend fun clear() {
        clears += 1
        size = 0
    }
}
