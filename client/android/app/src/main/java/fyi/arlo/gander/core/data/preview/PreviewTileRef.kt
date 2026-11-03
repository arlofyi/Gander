package fyi.arlo.gander.core.data.preview

/**
 * The Coil model of one deep-zoom tile: which file on which host, which page, and which tile of
 * its pyramid ([TilePyramid]). [PreviewTileFetcher] turns it into bytes; [cacheKey] is the memory
 * and disk cache key.
 *
 * [version] is `"<mtime>#<size>"` of the entry ([fyi.arlo.gander.core.data.thumbs.thumbVersion]),
 * the inputs the host derives its tile keys from, so an edited file gets new keys. The bearer
 * token is not part of this class and never enters a cache key, a log line or the disk cache.
 */
data class PreviewTileRef(
    val hostId: String,
    val shareId: String,
    /** Share-relative path of the file. */
    val path: String,
    val version: String,
    /** 1-based, as in `GET /preview/tile?page=`. */
    val page: Int,
    val level: Int,
    /** Tile column from the left. */
    val x: Int,
    /** Tile row from the top. */
    val y: Int,
) {
    init {
        require(page >= 1) { "pages are numbered from 1" }
        require(level >= 0 && x >= 0 && y >= 0) { "tile coordinates are not negative" }
    }

    /**
     * `gander-tile/1|<host>|<share>|<page>|<level>|<x>|<y>|<version>|<path>`. The path is last
     * because it is the only part that may contain `|`.
     */
    fun cacheKey(): String = "gander-tile/1|$hostId|$shareId|$page|$level|$x|$y|$version|$path"

    /** File names can be private: they stay out of logs. */
    override fun toString(): String =
        "PreviewTileRef(host=$hostId, share=$shareId, page=$page, level=$level, x=$x, y=$y)"
}
