package fyi.arlo.gander.core.data.preview

import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File

/**
 * The tile pyramid against `protocol/test-vectors/tiles.json`, the file the Rust host's
 * `gander-core` runs too (`tiles.rs`). Gradle passes its repository path as `gander.tileVectors`
 * and re-runs this test when the file changes, so host and client cannot drift apart unnoticed.
 */
class TilePyramidTest {
    private val root: JsonObject by lazy {
        val path = System.getProperty("gander.tileVectors")
            ?: error("system property gander.tileVectors is not set (run through Gradle)")
        Json.parseToJsonElement(File(path).readText(Charsets.UTF_8)).jsonObject
    }

    private fun JsonElement.pair(): Pair<Int, Int> = jsonArray.let { it[0].jsonPrimitive.int to it[1].jsonPrimitive.int }

    private fun JsonElement.quad(): List<Int> = jsonArray.map { it.jsonPrimitive.int }

    @Test
    fun theVectorFileIsTheExpectedOne() {
        assertEquals(1, root.getValue("version").jsonPrimitive.int)
        assertEquals(TILE_SIZE, root.getValue("tile_size").jsonPrimitive.int)
        val ids = root.getValue("vectors").jsonArray.map { it.jsonObject.getValue("id").jsonPrimitive.content }
        assertEquals("ids are unique", ids.size, ids.toSet().size)
        assertTrue("the full vector set", ids.size >= 8)
        listOf("image-6000x4500", "pdf-a1-300dpi", "image-513x1", "image-32768x1", "image-over-the-limit")
            .forEach { assertTrue("vector $it is in tiles.json", it in ids) }
    }

    @Test
    fun everyVectorGivesTheExpectedLevelsSizesGridsAndTiles() {
        for (element in root.getValue("vectors").jsonArray) {
            val v = element.jsonObject
            val id = v.getValue("id").jsonPrimitive.content
            val (w, h) = v.getValue("top").pair()
            val pyramid = TilePyramid.of(w, h)
            if (v.getValue("levels") is JsonNull) {
                assertNull("$id has no pyramid", pyramid)
                continue
            }
            assertNotNull("$id has a pyramid", pyramid)
            pyramid!!
            assertEquals("$id levels", v.getValue("levels").jsonPrimitive.int, pyramid.levels)
            val sizes = v.getValue("level_sizes").jsonArray
            val grids = v.getValue("grids").jsonArray
            assertEquals("$id level count", pyramid.levels, sizes.size)
            for (level in 0 until pyramid.levels) {
                val size = sizes[level].pair()
                assertEquals("$id level $level size", size, pyramid.levelSize(level)!!.let { it.width to it.height })
                val grid = grids[level].pair()
                assertEquals("$id level $level grid", grid, pyramid.grid(level)!!.let { it.width to it.height })
            }
            assertNull("$id: past the top level", pyramid.levelSize(pyramid.levels))
            assertNull(pyramid.levelSize(-1))
            for (t in v.getValue("tiles").jsonArray.map { it.jsonObject }) {
                val level = t.getValue("level").jsonPrimitive.int
                val x = t.getValue("x").jsonPrimitive.int
                val y = t.getValue("y").jsonPrimitive.int
                val got = pyramid.tile(level, x, y)
                val expect = t.getValue("expect")
                if (expect is JsonNull) {
                    assertNull("$id tile $level/$x/$y does not exist", got)
                    continue
                }
                assertNotNull("$id tile $level/$x/$y exists", got)
                got!!
                assertEquals("$id $level/$x/$y size", expect.jsonObject.getValue("size").pair(), got.width to got.height)
                assertEquals(
                    "$id $level/$x/$y source",
                    expect.jsonObject.getValue("src").quad(),
                    listOf(got.srcX, got.srcY, got.srcWidth, got.srcHeight),
                )
            }
        }
    }

    @Test
    fun theTilesOfTheTopLevelCoverEveryPixelOnce() {
        for ((w, h) in listOf(6000 to 4500, 513 to 1, 7016 to 9933, 1 to 1, 512 to 512, 33 to 4097)) {
            val p = TilePyramid.of(w, h)!!
            val top = p.levels - 1
            val grid = p.grid(top)!!
            var covered = 0L
            for (y in 0 until grid.height) for (x in 0 until grid.width) {
                val t = p.tile(top, x, y)!!
                assertEquals("the top level is 1:1", t.width to t.height, t.srcWidth to t.srcHeight)
                covered += t.width.toLong() * t.height
            }
            assertEquals("${w}x$h", w.toLong() * h, covered)
        }
    }

    @Test
    fun levelZeroIsOneTileAndEachLevelDoublesTheOneBelow() {
        for ((w, h) in listOf(6000 to 4500, 4000 to 3000, 32768 to 32768, 100 to 100, 9933 to 7016)) {
            val p = TilePyramid.of(w, h)!!
            assertEquals("${w}x$h", 1 to 1, p.grid(0)!!.let { it.width to it.height })
            for (level in 1 until p.levels) {
                val below = p.levelSize(level - 1)!!
                val above = p.levelSize(level)!!
                assertEquals((above.width + 1) / 2, below.width)
                assertEquals((above.height + 1) / 2, below.height)
                assertEquals(0.5, p.levelScale(level - 1) / p.levelScale(level), 0.0)
            }
            assertEquals(1.0, p.levelScale(p.levels - 1), 0.0)
        }
    }

    @Test
    fun unusablePagesHaveNoPyramid() {
        assertNull(TilePyramid.of(0, 10))
        assertNull(TilePyramid.of(10, 0))
        assertNull(TilePyramid.of(TilePyramid.MAX_TOP_SIDE + 1, 10))
        assertEquals(7, TilePyramid.of(TilePyramid.MAX_TOP_SIDE, 1)!!.levels)
        assertEquals(1, TilePyramid.of(512, 512)!!.levels)
        assertEquals(2, TilePyramid.of(513, 1)!!.levels)
    }
}
