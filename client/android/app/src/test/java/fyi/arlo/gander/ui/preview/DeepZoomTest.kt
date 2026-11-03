package fyi.arlo.gander.ui.preview

import fyi.arlo.gander.core.data.preview.TilePyramid
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** Level choice and the visible tile set of deep zoom, on the JVM. */
class DeepZoomTest {
    // A1 drawing at 300 dpi: 7016 x 9933, 6 levels; the base page is 1080 px wide.
    private val a1 = TilePyramid.of(7016, 9933)!!
    private val a1BitmapW = 1080
    private val a1BitmapH = 1529

    // A 6000 x 4500 photo, 5 levels; base 1080 x 810.
    private val photo = TilePyramid.of(6000, 4500)!!

    private fun plan(
        pyramid: TilePyramid,
        viewportW: Float,
        viewportH: Float,
        bitmapW: Int,
        bitmapH: Int,
        transform: ZoomTransform,
        maxTiles: Int = DeepZoom.MAX_VISIBLE_TILES,
    ) = DeepZoom.plan(pyramid, viewportW, viewportH, bitmapW, bitmapH, transform, maxTiles)

    private fun a1Plan(transform: ZoomTransform) = plan(a1, 1080f, 1800f, a1BitmapW, a1BitmapH, transform)

    /** The transform that shows the content's corner at [scale]. */
    private fun cornerOffset(scale: Float, vw: Float, vh: Float, right: Boolean, bottom: Boolean): ZoomTransform {
        val maxX = vw * (scale - 1f) / 2f
        val maxY = vh * (scale - 1f) / 2f
        return ZoomTransform(scale, if (right) -maxX else maxX, if (bottom) -maxY else maxY)
    }

    // Level choice --------------------------------------------------------------------------------

    @Test
    fun theSmallestSharpEnoughLevelIsChosen() {
        // levels of a 6000 x 4500 photo have scales 1/16, 1/8, 1/4, 1/2 and 1
        assertEquals(0, DeepZoom.levelFor(photo, 0.0625))
        assertEquals("within the tolerance of level 0", 0, DeepZoom.levelFor(photo, 0.07))
        assertEquals(1, DeepZoom.levelFor(photo, 0.10))
        assertEquals(2, DeepZoom.levelFor(photo, 0.25))
        assertEquals(3, DeepZoom.levelFor(photo, 0.5))
        assertEquals("only a hair past half a pixel keeps level 3", 3, DeepZoom.levelFor(photo, 0.55))
        assertEquals(4, DeepZoom.levelFor(photo, 0.7))
        assertEquals("the top level is as sharp as it gets", 4, DeepZoom.levelFor(photo, 3.0))
        assertEquals(0, DeepZoom.levelFor(photo, 0.0))
    }

    @Test
    fun theLevelFollowsTheZoom() {
        fun levelAt(zoom: Float) = plan(photo, 1080f, 2000f, 1080, 810, ZoomTransform(zoom))?.level
        assertEquals(2, levelAt(1.6f)) // 0.288 screen px per photo px
        assertEquals(3, levelAt(2f)) // 0.36
        assertEquals(4, levelAt(4f)) // 0.72
        assertEquals(4, levelAt(5f))
    }

    // When tiles are not needed ------------------------------------------------------------------

    @Test
    fun theBasePageIsEnoughAtOneTimesAndUpToTheThreshold() {
        assertNull(a1Plan(ZoomTransform(1f)))
        assertNull("exactly at the threshold", a1Plan(ZoomTransform(1.5f)))
        assertNotNull(a1Plan(ZoomTransform(1.51f)))
    }

    @Test
    fun aBaseThatIsAlreadyAtNaturalResolutionNeverNeedsTiles() {
        val small = TilePyramid.of(1000, 1400)!!
        assertNull(plan(small, 1000f, 1400f, 1000, 1400, ZoomTransform(4f)))
        assertNull("a base wider than the top level", plan(small, 1000f, 1400f, 1200, 1680, ZoomTransform(4f)))
        assertNotNull(plan(small, 1000f, 1400f, 800, 1120, ZoomTransform(4f)))
    }

    @Test
    fun anUnmeasuredViewportOrBitmapGivesNothing() {
        assertNull(plan(a1, 0f, 1800f, 1080, 1529, ZoomTransform(3f)))
        assertNull(plan(a1, 1080f, 1800f, 0, 0, ZoomTransform(3f)))
    }

    @Test
    fun aFitThatShrinksThePageDelaysTheThreshold() {
        // a short viewport fits the 1080 x 1529 page at 0.5: 3x zoom is only 1.5 screen px per base px
        assertNull(plan(a1, 1080f, 764.5f, a1BitmapW, a1BitmapH, ZoomTransform(3f)))
        assertNotNull(plan(a1, 1080f, 764.5f, a1BitmapW, a1BitmapH, ZoomTransform(3.2f)))
    }

    // The visible set -----------------------------------------------------------------------------

    @Test
    fun twiceCentredShowsTheMiddleOfThePageAtLevelThree() {
        val p = a1Plan(ZoomTransform(2f))!!
        assertEquals(3, p.level)
        // level 3 is 1754 x 2484, a 4 x 5 grid; the middle half of the width and of the height
        // spans columns 0..2 and rows 0..3
        assertEquals(12, p.tiles.size)
        assertTrue(p.tiles.all { it.level == 3 })
        assertEquals(setOf(0, 1, 2), p.tiles.map { it.x }.toSet())
        assertEquals(setOf(0, 1, 2, 3), p.tiles.map { it.y }.toSet())
        assertEquals("no duplicates", p.tiles.size, p.tiles.toSet().size)
    }

    @Test
    fun theNearestTilesToTheCentreComeFirst() {
        val p = a1Plan(ZoomTransform(2f))!!
        val page = DeepZoom.pageRect(1080f, 1800f, a1BitmapW, a1BitmapH)
        val distances = p.tiles.map { t ->
            val tile = a1.tile(t.level, t.x, t.y)!!
            val cx = page.left + (tile.srcX + tile.srcWidth / 2.0) / a1.topWidth * page.width
            val cy = page.top + (tile.srcY + tile.srcHeight / 2.0) / a1.topHeight * page.height
            (cx - 540) * (cx - 540) + (cy - 900) * (cy - 900)
        }
        assertEquals("ordered by distance from the centre", distances.sorted(), distances)
        val first = p.tiles.first()
        assertTrue("first is $first", first.x in 1..2 && first.y in 1..3)
    }

    @Test
    fun panningToEachCornerShowsThatCornersTiles() {
        val vw = 1080f
        val vh = 1800f
        val scale = 3f
        val level = a1Plan(ZoomTransform(scale))!!.level
        val grid = a1.grid(level)!!

        val topLeft = a1Plan(cornerOffset(scale, vw, vh, right = false, bottom = false))!!
        assertTrue(TileKey(level, 0, 0) in topLeft.tiles)
        assertTrue("nothing from the far corner", topLeft.tiles.none { it.x == grid.width - 1 || it.y == grid.height - 1 })

        val topRight = a1Plan(cornerOffset(scale, vw, vh, right = true, bottom = false))!!
        assertTrue(TileKey(level, grid.width - 1, 0) in topRight.tiles)
        assertTrue(topRight.tiles.none { it.x == 0 || it.y == grid.height - 1 })

        val bottomLeft = a1Plan(cornerOffset(scale, vw, vh, right = false, bottom = true))!!
        assertTrue(TileKey(level, 0, grid.height - 1) in bottomLeft.tiles)
        assertTrue(bottomLeft.tiles.none { it.x == grid.width - 1 || it.y == 0 })

        val bottomRight = a1Plan(cornerOffset(scale, vw, vh, right = true, bottom = true))!!
        assertTrue(TileKey(level, grid.width - 1, grid.height - 1) in bottomRight.tiles)
        assertTrue(bottomRight.tiles.none { it.x == 0 || it.y == 0 })
    }

    @Test
    fun aTilePannedFullyOffScreenIsNoLongerInTheSet() {
        val topLeft = a1Plan(cornerOffset(3f, 1080f, 1800f, right = false, bottom = false))!!
        val bottomRight = a1Plan(cornerOffset(3f, 1080f, 1800f, right = true, bottom = true))!!
        // opposite corners share nothing: panning across leaves the first set behind entirely
        assertTrue(topLeft.tiles.toSet().intersect(bottomRight.tiles.toSet()).isEmpty())
        assertEquals(topLeft.level, bottomRight.level)
    }

    @Test
    fun aWidePageInATallViewportShowsAllItsRowsButOnlyTheMiddleColumns() {
        val wide = TilePyramid.of(4000, 1000)!! // 4 levels, top grid 8 x 2
        val p = plan(wide, 1080f, 1800f, 1080, 270, ZoomTransform(3f))!!
        assertEquals(3, p.level)
        assertEquals(8, p.tiles.size)
        assertEquals(setOf(0, 1), p.tiles.map { it.y }.toSet())
        assertEquals(setOf(2, 3, 4, 5), p.tiles.map { it.x }.toSet())
    }

    @Test
    fun aTallPageInAWideViewportShowsAllItsColumns() {
        val tall = TilePyramid.of(1500, 6000)!! // base 270 x 1080 shown in a 2000 x 1080 viewport
        val p = plan(tall, 2000f, 1080f, 270, 1080, ZoomTransform(2f))
        // fit = 1.0, zoom 2 -> 2 screen px per base px: tiles engage; the page is 270 px wide on screen
        // and fully visible across, so every column at the chosen level is in the set
        assertNotNull(p)
        val grid = tall.grid(p!!.level)!!
        assertEquals((0 until grid.width).toSet(), p.tiles.map { it.x }.toSet())
    }

    @Test
    fun theSetIsCappedAndKeepsTheTilesNearestTheCentre() {
        // a 4000 x 4000 viewport shows the whole 6000 x 4500 photo at once: 12 x 9 tiles at the top level
        val p = plan(photo, 4000f, 4000f, 2048, 1536, ZoomTransform(1f))!!
        assertEquals(4, p.level)
        assertEquals(DeepZoom.MAX_VISIBLE_TILES, p.tiles.size)
        assertEquals(TileKey(4, 5, 4), p.tiles.first())
        val all = plan(photo, 4000f, 4000f, 2048, 1536, ZoomTransform(1f), maxTiles = 1000)!!
        assertEquals(12 * 9, all.tiles.size)
    }

    // Placement -------------------------------------------------------------------------------------

    @Test
    fun theTilesOfALevelMeetWithoutGapsAndFillThePage() {
        val page = DeepZoom.pageRect(1080f, 1800f, a1BitmapW, a1BitmapH)
        val level = 3
        val grid = a1.grid(level)!!
        var right = page.left
        for (x in 0 until grid.width) {
            val r = DeepZoom.placement(a1, TileKey(level, x, 0), page)!!
            assertEquals("tile $x starts where the last one ended", right, r[0], 0.01f)
            right = r[2]
        }
        assertEquals(page.left + page.width, right, 0.01f)
        var bottom = page.top
        for (y in 0 until grid.height) {
            val r = DeepZoom.placement(a1, TileKey(level, 0, y), page)!!
            assertEquals(bottom, r[1], 0.01f)
            bottom = r[3]
        }
        assertEquals(page.top + page.height, bottom, 0.01f)
        assertNull(DeepZoom.placement(a1, TileKey(level, grid.width, 0), page))
    }

    @Test
    fun thePageIsFittedAndCentred() {
        val tall = DeepZoom.pageRect(2000f, 1000f, 500, 1000)
        assertEquals(750f, tall.left, 0.01f)
        assertEquals(0f, tall.top, 0.01f)
        assertEquals(1000f, tall.height, 0.01f)
        assertEquals(500f, tall.width, 0.01f)
        val wide = DeepZoom.pageRect(1000f, 2000f, 1000, 500)
        assertEquals(0f, wide.left, 0.01f)
        assertEquals(750f, wide.top, 0.01f)
    }

    @Test
    fun theZoomLimitRisesToReachNaturalResolutionButNotBeyondTwelve() {
        // 6000 px photo, base 1080 wide in a 1080 wide viewport: natural resolution at about 5.6x
        val photoMax = DeepZoom.maxZoomFor(photo, 1080f, 2000f, 1080, 810)
        assertTrue("photo max $photoMax", photoMax in 5f..8f)
        // a 9933 px A1 page needs more
        val a1Max = DeepZoom.maxZoomFor(a1, 1080f, 1800f, a1BitmapW, a1BitmapH)
        assertTrue("a1 max $a1Max", a1Max > photoMax && a1Max <= DeepZoom.MAX_ZOOM_WITH_TILES)
        // a base that is already natural never raises the limit
        assertEquals(ZoomMath.MAX_SCALE, DeepZoom.maxZoomFor(TilePyramid.of(1000, 1400)!!, 1000f, 1400f, 1000, 1400), 0f)
    }
}
