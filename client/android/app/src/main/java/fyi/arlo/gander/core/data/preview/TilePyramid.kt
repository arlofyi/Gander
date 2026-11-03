package fyi.arlo.gander.core.data.preview

/** Edge length of a full deep-zoom tile in pixels. */
const val TILE_SIZE: Int = 512

/** Where one tile sits and what it shows (see [TilePyramid.tile]). */
data class TileGeometry(
    val level: Int,
    val x: Int,
    val y: Int,
    /** Width of the tile image in pixels: [TILE_SIZE], or less on the right edge. */
    val width: Int,
    /** Height of the tile image in pixels: [TILE_SIZE], or less on the bottom edge. */
    val height: Int,
    /** Left edge of the area the tile shows, in top-level pixels. */
    val srcX: Int,
    val srcY: Int,
    /** Width of the area the tile shows, in top-level pixels (clipped to the page). */
    val srcWidth: Int,
    val srcHeight: Int,
)

/** Size of one level in pixels, or the number of tile columns and rows of it. */
data class IntSize2(val width: Int, val height: Int)

/**
 * The deep-zoom tile pyramid of one page (protocol `GET /preview/tile`, WP-67). Pure arithmetic
 * shared with the host (`host/gander-core/src/tiles.rs`) and checked against the same vectors
 * (`protocol/test-vectors/tiles.json`):
 *
 * - the top level is the page at natural resolution ([topWidth] x [topHeight], from
 *   `PreviewInfo.tile_pages`), the last level;
 * - every level below it is half the size of the one above, rounded up, down to level 0, the
 *   smallest level whose longest side fits in [TILE_SIZE] (one tile);
 * - a level is cut into [TILE_SIZE] squares from the top left; tiles on the right and bottom edge
 *   are smaller, never padded.
 */
class TilePyramid private constructor(
    val topWidth: Int,
    val topHeight: Int,
    val levels: Int,
) {
    /** Size of [level] in pixels; null past the top level. */
    fun levelSize(level: Int): IntSize2? {
        if (level !in 0 until levels) return null
        val k = levels - 1 - level
        return IntSize2(ceilShift(topWidth, k), ceilShift(topHeight, k))
    }

    /** Tile columns (`width`) and rows (`height`) of [level]. */
    fun grid(level: Int): IntSize2? {
        val size = levelSize(level) ?: return null
        return IntSize2(ceilDiv(size.width, TILE_SIZE), ceilDiv(size.height, TILE_SIZE))
    }

    /** Tile ([x], [y]) of [level]; null when the level or the position does not exist. */
    fun tile(level: Int, x: Int, y: Int): TileGeometry? {
        val size = levelSize(level) ?: return null
        val grid = grid(level) ?: return null
        if (x !in 0 until grid.width || y !in 0 until grid.height) return null
        val k = levels - 1 - level
        val x0 = x * TILE_SIZE
        val y0 = y * TILE_SIZE
        val x1 = minOf(x0 + TILE_SIZE, size.width)
        val y1 = minOf(y0 + TILE_SIZE, size.height)
        // Top-level coordinates, clipped to the page: a rounded-up level can be a little larger
        // than the page it stands for.
        val sx0 = x0 shl k
        val sy0 = y0 shl k
        val sx1 = minOf(x1 shl k, topWidth)
        val sy1 = minOf(y1 shl k, topHeight)
        return TileGeometry(level, x, y, x1 - x0, y1 - y0, sx0, sy0, sx1 - sx0, sy1 - sy0)
    }

    /** How many pixels a level has per top-level pixel: 1 at the top, 1/2 below it, and so on. */
    fun levelScale(level: Int): Double = 1.0 / (1 shl (levels - 1 - level))

    override fun toString(): String = "TilePyramid(${topWidth}x$topHeight, $levels levels)"

    companion object {
        /** Longest side of a top level the host serves. */
        const val MAX_TOP_SIDE = 32_768

        /** The pyramid of a page whose top level is [topWidth] x [topHeight]; null when it cannot be tiled. */
        fun of(topWidth: Int, topHeight: Int): TilePyramid? {
            val longest = maxOf(topWidth, topHeight)
            if (topWidth <= 0 || topHeight <= 0 || longest > MAX_TOP_SIDE) return null
            var levels = 1
            while (ceilShift(longest, levels - 1) > TILE_SIZE) levels++
            return TilePyramid(topWidth, topHeight, levels)
        }

        private fun ceilDiv(a: Int, b: Int): Int = (a + b - 1) / b

        private fun ceilShift(a: Int, k: Int): Int = ((a.toLong() + (1L shl k) - 1) shr k).toInt()
    }
}
