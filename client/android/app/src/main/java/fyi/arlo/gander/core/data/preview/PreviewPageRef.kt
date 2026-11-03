package fyi.arlo.gander.core.data.preview

/**
 * The Coil model of one preview page: which file on which host, which page, at which pixel width,
 * and which version of the file. [PreviewPageFetcher] turns it into bytes; [cacheKey] is the
 * memory and disk cache key ("pages cached by etag+page+width", client spec).
 *
 * [version] is `"<mtime>#<size>"` of the entry ([fyi.arlo.gander.core.data.thumbs.thumbVersion]),
 * the inputs the host derives its strong ETag from, so an edited file gets new keys. The bearer
 * token is not part of this class and never enters a cache key, a log line or the disk cache.
 */
data class PreviewPageRef(
    val hostId: String,
    val shareId: String,
    /** Share-relative path of the file. */
    val path: String,
    val version: String,
    /** 1-based, as in `GET /preview?page=`. */
    val page: Int,
    /** The `width` query value; see [PreviewWidths.pick]. */
    val widthPx: Int,
    /**
     * Whether the page may be kept on disk. False for Office documents: until the real render
     * succeeds the host answers page 1 with a low-fidelity embedded thumbnail under the same URL
     * (and a different ETag the client cannot see), and a copy of that on disk would outlive the
     * real render. The host has its own cache, so asking again is cheap.
     */
    val persistent: Boolean = true,
) {
    init {
        require(page >= 1) { "pages are numbered from 1" }
        require(widthPx > 0) { "preview width must be positive" }
    }

    /**
     * `gander-preview/1|<host>|<share>|<width>|<page>|<version>|<path>`. The path is last because
     * it is the only part that may contain `|`.
     */
    fun cacheKey(): String = "gander-preview/1|$hostId|$shareId|$widthPx|$page|$version|$path"

    /** File names can be private: they stay out of logs. */
    override fun toString(): String = "PreviewPageRef(host=$hostId, share=$shareId, page=$page, width=$widthPx)"
}

/** Which `width` to ask the host for. */
object PreviewWidths {
    /** `capabilities.preview_max_width` of every v1 host; the host clamps anything larger. */
    const val DEFAULT_MAX = 2048

    /** Below this a page is not readable; also keeps a not-yet-measured layout from asking for 1 px. */
    const val MIN = 64

    /**
     * The width of the viewport in pixels (client spec: "request width = screen width px"),
     * clamped to what the host renders: [hostMax] is `capabilities.preview_max_width`, null while
     * the host's capabilities are not known.
     */
    fun pick(viewportPx: Int, hostMax: Int?): Int {
        val max = (hostMax ?: DEFAULT_MAX).coerceAtLeast(MIN)
        return viewportPx.coerceIn(MIN, max)
    }
}
