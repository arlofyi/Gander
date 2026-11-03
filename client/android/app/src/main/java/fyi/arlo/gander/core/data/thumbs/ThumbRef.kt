package fyi.arlo.gander.core.data.thumbs

import fyi.arlo.gander.core.net.protocol.Entry
import fyi.arlo.gander.core.net.protocol.EntryFlag
import fyi.arlo.gander.core.net.protocol.EntryKind
import kotlin.math.ceil

/**
 * The Coil model of one thumbnail: which file on which host, at which pixel size, and which
 * version of the file. [ThumbFetcher] turns it into bytes; [cacheKey] is the memory and disk
 * cache key.
 *
 * [version] is what makes an edited file refresh: the listing carries no ETag, so it is
 * `"<mtime>#<size>"` of the entry (see [thumbVersion]), the same inputs the host derives its
 * strong thumbnail ETag from. The bearer token is not part of this class and never enters a
 * cache key, a log line or the disk cache (which hashes keys into file names anyway).
 */
data class ThumbRef(
    val hostId: String,
    val shareId: String,
    /** Share-relative path of the file. */
    val path: String,
    val version: String,
    /** One of the host's `capabilities.thumb_sizes`; see [ThumbSizes.pick]. */
    val sizePx: Int,
) {
    init {
        require(sizePx > 0) { "thumbnail size must be positive" }
    }

    /**
     * `gander-thumb/1|<host>|<share>|<size>|<version>|<path>`. The path is last because it is the
     * only part that may contain `|`; host ids are base64url and share ids are `shr_` ULIDs.
     */
    fun cacheKey(): String = "gander-thumb/1|$hostId|$shareId|$sizePx|$version|$path"

    /** File names can be private: they stay out of logs. */
    override fun toString(): String = "ThumbRef(host=$hostId, share=$shareId, size=$sizePx)"
}

/** `mtime#size`: changes whenever the host's own thumbnail ETag would. */
fun Entry.thumbVersion(): String = "$mtime#$size"

/** Whether to ask the host for a thumbnail at all: a file the host predicts it can render. */
fun Entry.wantsThumbnail(): Boolean = kind == EntryKind.FILE && thumb && EntryFlag.LINK !in flags

/** Picks the `size` query value from what the host renders (`capabilities.thumb_sizes`). */
object ThumbSizes {
    /** What every host renders (`host/gander-win` `THUMB_SIZES`), used until `/me` is known. */
    val DEFAULT: List<Int> = listOf(128, 256, 512)

    /** The protocol default when a host lists no size at all. */
    const val FALLBACK = 256

    /** Client spec: request size 256 on phones, 512 on tablets. Those are ceilings. */
    const val PHONE_MAX = 256
    const val TABLET_MAX = 512
    const val TABLET_MIN_SMALLEST_WIDTH_DP = 600

    /** Physical pixels of a view [dp] wide at [density] (`Resources.displayMetrics.density`). */
    fun displayPx(dp: Float, density: Float): Int = ceil(dp * density).toInt().coerceAtLeast(1)

    /**
     * The smallest size the host renders that covers [displayPx], never above 256 on a phone or
     * 512 on a tablet (when the host offers nothing that small, its smallest). The host refuses a
     * `size` that is not in its list, so this must pick from [supported], not compute one.
     */
    fun pick(displayPx: Int, supported: List<Int>, tablet: Boolean): Int {
        val sizes = supported.filter { it > 0 }.distinct().sorted()
        if (sizes.isEmpty()) return FALLBACK
        val cap = if (tablet) TABLET_MAX else PHONE_MAX
        val allowed = sizes.filter { it <= cap }.ifEmpty { listOf(sizes.first()) }
        return allowed.firstOrNull { it >= displayPx } ?: allowed.last()
    }
}
