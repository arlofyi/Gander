package fyi.arlo.gander

import android.graphics.Bitmap
import android.graphics.Color
import androidx.compose.foundation.layout.Column
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.test.assertCountEquals
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import coil3.ImageLoader
import coil3.request.ImageRequest
import coil3.request.SuccessResult
import fyi.arlo.gander.core.data.thumbs.ThumbData
import fyi.arlo.gander.core.data.thumbs.ThumbLoader
import fyi.arlo.gander.core.data.thumbs.ThumbRef
import fyi.arlo.gander.core.data.thumbs.ThumbSource
import fyi.arlo.gander.core.data.thumbs.createThumbnailImageLoader
import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.protocol.PreviewKind
import fyi.arlo.gander.testing.SampleData.file
import fyi.arlo.gander.ui.browser.BrowserTags
import fyi.arlo.gander.ui.browser.EntryDisplay
import fyi.arlo.gander.ui.browser.EntryGridCell
import fyi.arlo.gander.ui.browser.EntryRow
import fyi.arlo.gander.ui.browser.LocalThumbnailScope
import fyi.arlo.gander.ui.browser.ThumbnailScope
import fyi.arlo.gander.ui.browser.ThumbnailTags
import fyi.arlo.gander.ui.theme.GanderTheme
import kotlinx.coroutines.runBlocking
import okio.ByteString
import okio.ByteString.Companion.toByteString
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import java.io.ByteArrayOutputStream
import java.io.File
import java.util.UUID
import java.util.concurrent.CopyOnWriteArrayList

/**
 * Thumbnails in the browser rows and grid cells, through the real Coil loader, fetcher, disk cache
 * and Compose layer, with a scripted [ThumbSource] standing in for the host. Runs on the emulator
 * in WP-29. The same check against the real host is [HostThumbnailsFixtureTest], which needs the
 * host Browse API (WP-14) and is deferred to WP-29 too.
 */
@RunWith(AndroidJUnit4::class)
class ThumbnailsTest {
    @get:Rule
    val composeRule = createComposeRule()

    private val context = InstrumentationRegistry.getInstrumentation().targetContext
    private val cacheDir = File(context.cacheDir, "thumbs-test-${UUID.randomUUID()}")
    private val asked = CopyOnWriteArrayList<String>()
    private val attempts = HashMap<String, Int>()
    private val refs = CopyOnWriteArrayList<ThumbRef>()

    private fun png(color: Int): ByteString {
        val bitmap = Bitmap.createBitmap(64, 64, Bitmap.Config.ARGB_8888).apply { eraseColor(color) }
        return ByteArrayOutputStream().also { bitmap.compress(Bitmap.CompressFormat.PNG, 100, it) }
            .toByteArray().toByteString()
    }

    /** a.jpg is "queued" on its first request (503), b.png answers at once, c.jpg cannot be rendered. */
    private val source = ThumbSource { ref, _ ->
        asked += ref.path
        refs += ref
        val attempt = synchronized(attempts) { (attempts[ref.path] ?: 0).also { attempts[ref.path] = it + 1 } }
        when (ref.path) {
            "Photos/a.jpg" -> if (attempt == 0) {
                throw GanderError.Host.Generating(503, "queued", 200)
            } else {
                ThumbData(png(Color.RED), "image/png")
            }
            "Photos/b.png" -> ThumbData(png(Color.BLUE), "image/png")
            else -> throw GanderError.Host.UnsupportedType(415, "no thumbnail for this type")
        }
    }

    private val loader: ImageLoader by lazy { createThumbnailImageLoader(context, ThumbLoader(source), cacheDir) }

    @After
    fun tearDown() {
        loader.shutdown()
        cacheDir.deleteRecursively()
    }

    private val a = file("a.jpg", preview = PreviewKind.IMAGE)
    private val b = file("b.png", preview = PreviewKind.IMAGE)
    private val c = file("c.jpg", preview = PreviewKind.IMAGE)
    private val notes = file("notes.md")

    private fun show(content: @Composable () -> Unit) {
        composeRule.setContent {
            GanderTheme {
                CompositionLocalProvider(
                    LocalThumbnailScope provides ThumbnailScope("host_A", "shr_1", imageLoader = loader),
                ) { Column { content() } }
            }
        }
    }

    // The row is one merged clickable node, so the thumbnail's tag lives in the unmerged tree only.
    private fun loaded(path: String) = hasTestTag(ThumbnailTags.loaded(path))

    @Test
    fun thumbnailsOfImagesAppearInRowsWithinFiveSeconds() {
        show {
            EntryRow(EntryDisplay(a, "Photos/a.jpg"), onClick = {}, onLongClick = {})
            EntryRow(EntryDisplay(b, "Photos/b.png"), onClick = {}, onLongClick = {})
            EntryRow(EntryDisplay(notes, "Photos/notes.md"), onClick = {}, onLongClick = {})
        }
        composeRule.waitUntil(timeoutMillis = 5_000) {
            composeRule.onAllNodes(loaded("Photos/a.jpg"), useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty() &&
                composeRule.onAllNodes(loaded("Photos/b.png"), useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty()
        }
        composeRule.onNodeWithTag(ThumbnailTags.loaded("Photos/a.jpg"), useUnmergedTree = true).assertIsDisplayed()
        composeRule.onNodeWithTag(ThumbnailTags.loaded("Photos/b.png"), useUnmergedTree = true).assertIsDisplayed()

        // a.jpg was queued on the host once (503) and retried; the text file was never asked for
        assertEquals(2, asked.count { it == "Photos/a.jpg" })
        assertFalse("Photos/notes.md" in asked)
        composeRule.onAllNodes(loaded("Photos/notes.md"), useUnmergedTree = true).assertCountEquals(0)
    }

    @Test
    fun gridCellsShowThumbnailsToo() {
        show {
            EntryGridCell(EntryDisplay(b, "Photos/b.png"), onClick = {}, onLongClick = {})
        }
        composeRule.waitUntil(timeoutMillis = 5_000) {
            composeRule.onAllNodes(loaded("Photos/b.png"), useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty()
        }
    }

    @Test
    fun aFileTheHostCannotRenderKeepsItsTypeGlyph() {
        show {
            EntryRow(EntryDisplay(c, "Photos/c.jpg"), onClick = {}, onLongClick = {})
        }
        composeRule.waitUntil(timeoutMillis = 5_000) { "Photos/c.jpg" in asked }
        composeRule.waitForIdle()
        composeRule.onNodeWithTag(BrowserTags.entry("Photos/c.jpg")).assertIsDisplayed()
        composeRule.onAllNodes(loaded("Photos/c.jpg"), useUnmergedTree = true).assertCountEquals(0)
    }

    @Test
    fun aSecondLookComesFromTheCachesAndAnEditedFileIsFetchedAgain() {
        show {
            EntryRow(EntryDisplay(b, "Photos/b.png"), onClick = {}, onLongClick = {})
        }
        composeRule.waitUntil(timeoutMillis = 5_000) {
            composeRule.onAllNodes(loaded("Photos/b.png"), useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty()
        }
        assertEquals(1, asked.count { it == "Photos/b.png" })
        val ref = refs.first { it.path == "Photos/b.png" }

        val again = runBlocking { loader.execute(ImageRequest.Builder(context).data(ref).build()) }
        assertTrue(again is SuccessResult)
        assertEquals("served from memory or disk, not the host", 1, asked.count { it == "Photos/b.png" })

        // the file was edited on the computer: new mtime, new key, new request
        val edited = runBlocking { loader.execute(ImageRequest.Builder(context).data(ref.copy(version = "2026-11-02T00:00:00Z#2048")).build()) }
        assertTrue(edited is SuccessResult)
        assertEquals(2, asked.count { it == "Photos/b.png" })
    }
}
