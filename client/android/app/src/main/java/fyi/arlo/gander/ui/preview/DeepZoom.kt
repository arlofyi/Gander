package fyi.arlo.gander.ui.preview

import fyi.arlo.gander.core.data.preview.TILE_SIZE
import fyi.arlo.gander.core.data.preview.TilePyramid
import kotlin.math.ceil
import kotlin.math.floor
import kotlin.math.min

/** One tile of a [TilePyramid]: level, column, row. */
data class TileKey(val level: Int, val x: Int, val y: Int)

/** What to show at the current zoom: the pyramid [level] chosen and the [tiles] on screen, nearest the centre first. */
data class TilePlan(val level: Int, val tiles: List<TileKey>)

/** Where the page sits in the viewport before any zoom: a fit of the base bitmap, centred. */
data class PageRect(val left: Float, val top: Float, val width: Float, val height: Float)

/**
 * The arithmetic of deep zoom (WP-67), apart from Compose so it is tested on the JVM.
 *
 * The viewer shows the host's preview page (the *base* bitmap, `bitmapWidth` x `bitmapHeight`
 * pixels, at most 2048 wide) fitted into the viewport and zoomed by [ZoomTransform]. Once the
 * picture is magnified well past one screen pixel per bitmap pixel, [plan] picks the level of the
 * tile pyramid that is sharp enough and lists the tiles that intersect the viewport. The base
 * stays underneath as the placeholder, so a tile that has not arrived yet costs nothing visible.
 */
object DeepZoom {
    /** Tiles are used once one base-bitmap pixel covers more than this many screen pixels. */
    const val ENGAGE_ABOVE = 1.5f

    /**
     * A level counts as sharp enough when its scale times this reaches the screen scale needed,
     * so a magnification that is only a hair past a level does not bump to the next one (which
     * has four times the tiles).
     */
    const val LEVEL_TOLERANCE = 1.25

    /** The zoom limit when tiles can show more than the base page: one top-level pixel per screen pixel, at most this. */
    const val MAX_ZOOM_WITH_TILES = 12f

    /** Most tiles requested at once: about two screens of 512 px tiles, nearest the centre first. */
    const val MAX_VISIBLE_TILES = 24

    /** Scale that fits a [bitmapWidth] x [bitmapHeight] picture into the viewport (what `ContentScale.Fit` does). */
    fun fitScale(viewportWidth: Float, viewportHeight: Float, bitmapWidth: Int, bitmapHeight: Int): Float =
        min(viewportWidth / bitmapWidth, viewportHeight / bitmapHeight)

    /** The page's rectangle in the viewport at zoom 1, in viewport pixels. */
    fun pageRect(viewportWidth: Float, viewportHeight: Float, bitmapWidth: Int, bitmapHeight: Int): PageRect {
        val fit = fitScale(viewportWidth, viewportHeight, bitmapWidth, bitmapHeight)
        val w = bitmapWidth * fit
        val h = bitmapHeight * fit
        return PageRect((viewportWidth - w) / 2f, (viewportHeight - h) / 2f, w, h)
    }

    /**
     * How far the viewer may zoom a picture that has tiles: up to the point where one top-level
     * (natural resolution) pixel covers one screen pixel, never less than [ZoomMath.MAX_SCALE] and at
     * most [MAX_ZOOM_WITH_TILES]. Without tiles, or when the base page is already natural, the
     * ordinary limit stands.
     */
    fun maxZoomFor(
        pyramid: TilePyramid,
        viewportWidth: Float,
        viewportHeight: Float,
        bitmapWidth: Int,
        bitmapHeight: Int,
    ): Float {
        if (viewportWidth <= 0f || viewportHeight <= 0f || bitmapWidth <= 0 || bitmapHeight <= 0) return ZoomMath.MAX_SCALE
        if (pyramid.topWidth <= bitmapWidth) return ZoomMath.MAX_SCALE
        val fit = fitScale(viewportWidth, viewportHeight, bitmapWidth, bitmapHeight)
        val natural = pyramid.topWidth / (fit * bitmapWidth)
        return natural.coerceIn(ZoomMath.MAX_SCALE, MAX_ZOOM_WITH_TILES)
    }

    /**
     * The smallest level whose scale (pixels per top-level pixel: 1 at the top, 1/2 below it...)
     * times [LEVEL_TOLERANCE] reaches [screenPxPerTopPx], the screen pixels one top-level pixel
     * covers; the top level when even that is not enough.
     */
    fun levelFor(pyramid: TilePyramid, screenPxPerTopPx: Double): Int {
        for (level in 0 until pyramid.levels) {
            if (pyramid.levelScale(level) * LEVEL_TOLERANCE >= screenPxPerTopPx) return level
        }
        return pyramid.levels - 1
    }

    /**
     * The level and the tiles to show, or null while the base page is enough: the zoom has not
     * passed [ENGAGE_ABOVE] screen pixels per base pixel, or the top level is no sharper than the
     * base bitmap (the host already sent the page at natural resolution).
     */
    fun plan(
        pyramid: TilePyramid,
        viewportWidth: Float,
        viewportHeight: Float,
        bitmapWidth: Int,
        bitmapHeight: Int,
        transform: ZoomTransform,
        maxTiles: Int = MAX_VISIBLE_TILES,
    ): TilePlan? {
        if (viewportWidth <= 0f || viewportHeight <= 0f || bitmapWidth <= 0 || bitmapHeight <= 0) return null
        if (pyramid.topWidth <= bitmapWidth) return null
        val fit = fitScale(viewportWidth, viewportHeight, bitmapWidth, bitmapHeight)
        val displayedPerBitmapPx = fit * transform.scale
        if (displayedPerBitmapPx <= ENGAGE_ABOVE) return null

        val screenPerTopPx = displayedPerBitmapPx.toDouble() * bitmapWidth / pyramid.topWidth
        val level = levelFor(pyramid, screenPerTopPx)
        val size = pyramid.levelSize(level) ?: return null
        val grid = pyramid.grid(level) ?: return null

        val page = pageRect(viewportWidth, viewportHeight, bitmapWidth, bitmapHeight)
        val z = transform.scale.toDouble()
        val cx = viewportWidth / 2.0
        val cy = viewportHeight / 2.0
        // The viewport as seen from the content: screen = centre + (point - centre) * zoom + offset.
        val x0 = cx + (0.0 - cx - transform.offsetX) / z
        val x1 = cx + (viewportWidth - cx - transform.offsetX) / z
        val y0 = cy + (0.0 - cy - transform.offsetY) / z
        val y1 = cy + (viewportHeight - cy - transform.offsetY) / z
        // As fractions of the page, clipped to it.
        val u0 = ((x0 - page.left) / page.width).coerceIn(0.0, 1.0)
        val u1 = ((x1 - page.left) / page.width).coerceIn(0.0, 1.0)
        val v0 = ((y0 - page.top) / page.height).coerceIn(0.0, 1.0)
        val v1 = ((y1 - page.top) / page.height).coerceIn(0.0, 1.0)
        if (u1 <= u0 || v1 <= v0) return TilePlan(level, emptyList())

        val firstCol = floor(u0 * size.width / TILE_SIZE).toInt().coerceIn(0, grid.width - 1)
        val lastCol = (ceil(u1 * size.width / TILE_SIZE).toInt() - 1).coerceIn(firstCol, grid.width - 1)
        val firstRow = floor(v0 * size.height / TILE_SIZE).toInt().coerceIn(0, grid.height - 1)
        val lastRow = (ceil(v1 * size.height / TILE_SIZE).toInt() - 1).coerceIn(firstRow, grid.height - 1)

        val ranked = ArrayList<Pair<Double, TileKey>>()
        for (row in firstRow..lastRow) {
            for (col in firstCol..lastCol) {
                val tile = pyramid.tile(level, col, row) ?: continue
                // Distance of the tile's centre from the viewport's centre, on screen.
                val px = page.left + (tile.srcX + tile.srcWidth / 2.0) / pyramid.topWidth * page.width
                val py = page.top + (tile.srcY + tile.srcHeight / 2.0) / pyramid.topHeight * page.height
                val sx = cx + (px - cx) * z + transform.offsetX
                val sy = cy + (py - cy) * z + transform.offsetY
                ranked += ((sx - cx) * (sx - cx) + (sy - cy) * (sy - cy)) to TileKey(level, col, row)
            }
        }
        ranked.sortBy { it.first }
        return TilePlan(level, ranked.take(maxTiles).map { it.second })
    }

    /**
     * Where tile [key] sits in the page's own (pre-zoom) coordinate space, as left, top, right,
     * bottom in viewport pixels at zoom 1. Edge tiles are placed by the area they show, so the
     * tiles of a level tile the page without gaps.
     */
    fun placement(pyramid: TilePyramid, key: TileKey, page: PageRect): FloatArray? {
        val tile = pyramid.tile(key.level, key.x, key.y) ?: return null
        val left = page.left + tile.srcX.toFloat() / pyramid.topWidth * page.width
        val top = page.top + tile.srcY.toFloat() / pyramid.topHeight * page.height
        val right = page.left + (tile.srcX + tile.srcWidth).toFloat() / pyramid.topWidth * page.width
        val bottom = page.top + (tile.srcY + tile.srcHeight).toFloat() / pyramid.topHeight * page.height
        return floatArrayOf(left, top, right, bottom)
    }
}

/**
 * The pyramid of one `preview/info` `tile_pages` entry, or null when the client cannot tile it (an
 * impossible size) or disagrees with the host about how many levels it has (a host that cuts its
 * pyramid differently must not get tiles drawn at the wrong place).
 */
internal fun tilePyramidOf(page: fyi.arlo.gander.core.net.protocol.TilePage): TilePyramid? =
    TilePyramid.of(page.width, page.height)?.takeIf { it.levels == page.levels }
