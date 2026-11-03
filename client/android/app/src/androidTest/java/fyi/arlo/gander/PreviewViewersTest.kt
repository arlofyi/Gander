package fyi.arlo.gander

import android.graphics.Bitmap
import android.graphics.Color
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertTextContains
import androidx.compose.ui.test.assertTextEquals
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTouchInput
import androidx.compose.ui.test.swipeLeft
import androidx.lifecycle.SavedStateHandle
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import coil3.ImageLoader
import fyi.arlo.gander.core.data.actions.InMemoryActionRefusals
import fyi.arlo.gander.core.data.browse.EntryTarget
import fyi.arlo.gander.core.data.hosts.InMemoryHostRepository
import fyi.arlo.gander.core.data.preview.CoilPagePrefetcher
import fyi.arlo.gander.core.data.preview.PreviewPageComponents
import fyi.arlo.gander.core.data.preview.PreviewPageData
import fyi.arlo.gander.core.data.preview.PreviewPageLoader
import fyi.arlo.gander.core.data.preview.PreviewSource
import fyi.arlo.gander.core.data.preview.TextChunk
import fyi.arlo.gander.core.data.preview.VideoPlayerFactory
import fyi.arlo.gander.core.data.preview.createPreviewDiskCache
import fyi.arlo.gander.core.data.thumbs.ThumbLoader
import fyi.arlo.gander.core.data.thumbs.ThumbSource
import fyi.arlo.gander.core.data.thumbs.createThumbnailImageLoader
import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.protocol.Entry
import fyi.arlo.gander.core.net.protocol.PreviewKind
import fyi.arlo.gander.testing.FakePreviewSource
import fyi.arlo.gander.testing.SampleData
import fyi.arlo.gander.ui.preview.PagesViewModel
import fyi.arlo.gander.ui.preview.PagesViewerScreen
import fyi.arlo.gander.ui.preview.PreviewTags
import fyi.arlo.gander.ui.preview.SinglePageViewModel
import fyi.arlo.gander.ui.preview.ImageViewerScreen
import fyi.arlo.gander.ui.preview.TextViewModel
import fyi.arlo.gander.ui.preview.TextViewerScreen
import fyi.arlo.gander.ui.preview.VideoViewModel
import fyi.arlo.gander.ui.preview.VideoViewerScreen
import fyi.arlo.gander.ui.theme.GanderTheme
import okio.ByteString
import okio.ByteString.Companion.toByteString
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import java.io.ByteArrayOutputStream
import java.io.File
import java.util.UUID

/**
 * The preview viewers (WP-26) over a scripted host and the real Coil loader, fetcher and disk
 * cache. Compiled by `assembleDebugAndroidTest`; they run on the emulator in WP-29. The logic
 * behind them (page window, retries, text paging) is covered by JVM tests; what the emulator adds
 * is that the pager, the zoom layer, the lazy text list and the poster really draw it.
 */
@RunWith(AndroidJUnit4::class)
class PreviewViewersTest {
    @get:Rule
    val composeRule = createComposeRule()

    private val context = InstrumentationRegistry.getInstrumentation().targetContext
    private val host = SampleData.host()
    private val hosts = InMemoryHostRepository(listOf(host))
    private val refusals = InMemoryActionRefusals()
    private val source = FakePreviewSource()
    private val dirs = mutableListOf<File>()
    private var loader: ImageLoader? = null

    @After
    fun tearDown() {
        loader?.shutdown()
        dirs.forEach { it.deleteRecursively() }
    }

    private fun png(color: Int): ByteString {
        val bitmap = Bitmap.createBitmap(120, 170, Bitmap.Config.ARGB_8888).apply { eraseColor(color) }
        return ByteArrayOutputStream().use {
            bitmap.compress(Bitmap.CompressFormat.PNG, 100, it)
            it.toByteArray().toByteString()
        }
    }

    private fun imageLoader(sourceForPages: PreviewSource = source): ImageLoader {
        val id = UUID.randomUUID()
        val thumbs = File(context.cacheDir, "viewers-thumbs-$id").also { dirs += it }
        val previews = File(context.cacheDir, "viewers-previews-$id").also { dirs += it }
        return createThumbnailImageLoader(
            context = context,
            loader = ThumbLoader(ThumbSource { _, _ -> throw GanderError.Host.UnsupportedType(415, "no thumbnails here") }),
            cacheDir = thumbs,
            previews = PreviewPageComponents(
                loader = PreviewPageLoader(sourceForPages),
                diskCache = createPreviewDiskCache(context, previews),
            ),
        ).also { loader = it }
    }

    private fun target(entry: Entry) = EntryTarget(host.id, "shr_projects", "Docs/${entry.name}", entry)

    private fun waitForTag(tag: String, timeoutMillis: Long = 10_000) {
        composeRule.waitUntil(timeoutMillis) {
            composeRule.onAllNodes(hasTestTag(tag)).fetchSemanticsNodes().isNotEmpty()
        }
    }

    // Pages ---------------------------------------------------------------------------------------

    @Test
    fun theFirstPageDrawsWithItsCounterAndTheNextOneIsPrefetched() {
        source.infoHandler = { FakePreviewSource.pdfInfo(pages = 3) }
        source.pageHandler = { PreviewPageData(png(Color.RED), "image/png", "\"p\"") }
        val imageLoader = imageLoader()
        val entry = SampleData.file("Report.pdf", size = 2_000_000, preview = PreviewKind.PDF)
        val viewModel = PagesViewModel(
            target = target(entry),
            hosts = hosts,
            source = source,
            prefetcher = CoilPagePrefetcher(context, imageLoader),
            refusalLog = refusals,
            saved = SavedStateHandle(),
        )

        composeRule.setContent {
            GanderTheme {
                PagesViewerScreen(viewModel, "Report.pdf", imageLoader, onBack = {}, onFetch = {})
            }
        }
        waitForTag(PreviewTags.pageLoaded(1))
        composeRule.onNodeWithTag(PreviewTags.PAGE_COUNTER).assertTextEquals("1 / 3")
        composeRule.onNodeWithTag(PreviewTags.FETCH).assertIsDisplayed()

        // prefetch n+1 starts once page 1 is there
        composeRule.waitUntil(5_000) { source.pageCalls.any { it.page == 2 } }
        assertEquals(1, source.pageCalls.count { it.page == 1 })

        composeRule.onNodeWithTag(PreviewTags.PAGER).performTouchInput { swipeLeft() }
        waitForTag(PreviewTags.pageLoaded(2))
        composeRule.onNodeWithTag(PreviewTags.PAGE_COUNTER).assertTextEquals("2 / 3")
        assertEquals("page 2 came from the prefetch, not a second request", 1, source.pageCalls.count { it.page == 2 })
    }

    @Test
    fun aFileTheComputerCannotRenderIsExplainedWithAWayOut() {
        source.infoHandler = { throw GanderError.Host.UnsupportedType(415, "no renderer") }
        val imageLoader = imageLoader()
        val entry = SampleData.file("Report.pdf", preview = PreviewKind.PDF)
        val viewModel = PagesViewModel(
            target = target(entry),
            hosts = hosts,
            source = source,
            prefetcher = CoilPagePrefetcher(context, imageLoader),
            refusalLog = refusals,
            saved = SavedStateHandle(),
        )
        var fetched = 0
        composeRule.setContent {
            GanderTheme {
                PagesViewerScreen(viewModel, "Report.pdf", imageLoader, onBack = {}, onFetch = { fetched++ })
            }
        }
        waitForTag(PreviewTags.FAILURE)
        composeRule.onNodeWithTag(PreviewTags.FETCH).performClick()
        assertEquals(1, fetched)
    }

    // Image ---------------------------------------------------------------------------------------

    @Test
    fun theImageDrawsAndOffersTheOriginal() {
        source.pageHandler = { PreviewPageData(png(Color.BLUE), "image/png", "\"p\"") }
        val imageLoader = imageLoader()
        val entry = SampleData.file("Holiday.jpg", size = 3_500_000, preview = PreviewKind.IMAGE)
        val viewModel = SinglePageViewModel(target(entry), hosts, refusals)
        var fetched = 0
        composeRule.setContent {
            GanderTheme {
                ImageViewerScreen(viewModel, "Holiday.jpg", imageLoader, onBack = {}, onFetch = { fetched++ })
            }
        }
        waitForTag(PreviewTags.IMAGE_LOADED)
        composeRule.onNode(hasText("View original (Fetch)")).performClick()
        assertEquals(1, fetched)
        assertTrue("one page request at the viewport width", source.pageCalls.single().widthPx > 0)
    }

    // Text ----------------------------------------------------------------------------------------

    @Test
    fun theTextShowsItsEncodingAndLoadsTheNextSlice() {
        source.textHandler = { call ->
            if (call.offset == 0L) {
                TextChunk("héllo\nwörld", 0, 13, "utf-16le", 26, hasMore = true)
            } else {
                TextChunk("\nmore text", call.offset, 26, "utf-16le", 26, hasMore = false)
            }
        }
        val entry = SampleData.file("notes.txt", preview = PreviewKind.TEXT)
        val viewModel = TextViewModel(target(entry), source, refusals, sliceBytes = 100)
        composeRule.setContent {
            GanderTheme {
                TextViewerScreen(viewModel, "notes.txt", onBack = {}, onFetch = {})
            }
        }
        waitForTag(PreviewTags.TEXT_ENCODING)
        composeRule.onNodeWithTag(PreviewTags.TEXT_ENCODING).assertTextContains("UTF-16 LE", substring = true)
        // the short first slice leaves the end of the list on screen, so the next slice is read by itself
        composeRule.waitUntil(5_000) {
            composeRule.onAllNodes(hasText("more text", substring = true)).fetchSemanticsNodes().isNotEmpty()
        }
        assertEquals(listOf(0L, 13L), source.textCalls.map { it.offset })
    }

    // Video ---------------------------------------------------------------------------------------

    @Test
    fun theVideoShowsItsPosterAndNothingPlaysUntilTheUserAsks() {
        source.pageHandler = { PreviewPageData(png(Color.GREEN), "image/png", "\"p\"") }
        val imageLoader = imageLoader()
        val entry = SampleData.file("Clip.mp4", size = 90_000_000, preview = PreviewKind.VIDEO_POSTER)
        val viewModel = VideoViewModel(target(entry), hosts, refusals, SavedStateHandle())
        val factory = VideoPlayerFactory { _, _, _, _ -> throw IllegalStateException("the player is built on play") }
        composeRule.setContent {
            GanderTheme {
                VideoViewerScreen(
                    viewModel = viewModel,
                    target = target(entry),
                    title = "Clip.mp4",
                    imageLoader = imageLoader,
                    playerFactory = factory,
                    onBack = {},
                    onFetch = {},
                )
            }
        }
        waitForTag(PreviewTags.POSTER_LOADED)
        composeRule.onNodeWithTag(PreviewTags.VIDEO_PLAY).assertIsDisplayed()
    }
}
