package fyi.arlo.gander.core.data.preview

import coil3.decode.DataSource
import coil3.disk.DiskCache
import fyi.arlo.gander.core.data.thumbs.LoadedThumb
import fyi.arlo.gander.core.data.thumbs.ThumbFailureMemo
import fyi.arlo.gander.core.data.thumbs.ThumbScheduler
import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.ThumbPriority
import fyi.arlo.gander.testing.FakePreviewSource
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import okio.ByteString
import okio.ByteString.Companion.encodeUtf8
import okio.Path.Companion.toOkioPath
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

/** The part of the page fetcher that is not Coil: disk cache, the retry budget, scheduling, remembered refusals. */
@OptIn(ExperimentalCoroutinesApi::class)
class PreviewPageLoaderTest {
    @get:Rule
    val folder = TemporaryFolder()

    private lateinit var disk: DiskCache
    private val source = FakePreviewSource()
    private var millis = 0L
    private val waits = mutableListOf<Long>()

    @Before
    fun setUp() {
        disk = DiskCache.Builder().directory(folder.newFolder("previews").toOkioPath()).maxSizeBytes(1L shl 20).build()
    }

    @After
    fun tearDown() {
        disk.shutdown()
    }

    private fun ref(page: Int = 1, width: Int = 1080, version: String = "v1", persistent: Boolean = true) =
        PreviewPageRef("host_A", "shr_1", "Docs/Report.pdf", version, page, width, persistent)

    private fun loader(
        retry: PreviewRetryPolicy = PreviewRetryPolicy(),
        memo: ThumbFailureMemo = ThumbFailureMemo(),
        scheduler: ThumbScheduler = ThumbScheduler(1),
    ) = PreviewPageLoader(
        source = source,
        retry = retry,
        schedulerFor = { scheduler },
        sleep = {
            waits += it
            millis += it
        },
        now = { millis },
        failures = memo,
    )

    private fun page(text: String, mime: String? = "image/webp") =
        PreviewPageData(text.encodeUtf8(), mime, "\"e\"")

    private fun generating(retryMs: Long = 400) = GanderError.Host.Generating(503, "queued", retryMs)

    private fun LoadedThumb.read(): ByteString = this.source.source().use { it.readByteString() }

    @Test
    fun theFirstLoadFetchesAndTheSecondComesFromDisk() = runTest {
        source.pageHandler = { page("webp-bytes") }
        val loader = loader()

        val first = loader.load(ref(), ThumbPriority.VISIBLE, disk)
        assertEquals(DataSource.NETWORK, first.dataSource)
        assertEquals("image/webp", first.mimeType)
        assertEquals("webp-bytes".encodeUtf8(), first.read())
        first.source.close()

        val second = loader.load(ref(), ThumbPriority.VISIBLE, disk)
        assertEquals(DataSource.DISK, second.dataSource)
        assertEquals("the type survives the disk", "image/webp", second.mimeType)
        assertEquals("webp-bytes".encodeUtf8(), second.read())
        second.source.close()
        assertEquals("the host was asked once", 1, source.pageCalls.size)
    }

    @Test
    fun anotherPageWidthOrFileVersionIsAnotherPicture() = runTest {
        source.pageHandler = { page("p${it.page}w${it.widthPx}${it.version}") }
        val loader = loader()
        loader.load(ref(), ThumbPriority.VISIBLE, disk).source.close()
        loader.load(ref(page = 2), ThumbPriority.VISIBLE, disk).source.close()
        loader.load(ref(width = 720), ThumbPriority.VISIBLE, disk).source.close()
        loader.load(ref(version = "v2"), ThumbPriority.VISIBLE, disk).source.close()
        assertEquals(4, source.pageCalls.size)
        // and the first one is still served from disk
        val again = loader.load(ref(), ThumbPriority.VISIBLE, disk)
        assertEquals(DataSource.DISK, again.dataSource)
        again.source.close()
        assertEquals(4, source.pageCalls.size)
    }

    @Test
    fun aPageStillBeingRenderedIsWaitedForThenCached() = runTest {
        var calls = 0
        source.pageHandler = {
            if (++calls <= 6) throw generating(2_000)
            page("rendered")
        }
        val loader = loader()
        val loaded = loader.load(ref(), ThumbPriority.VISIBLE, disk)
        assertEquals("rendered".encodeUtf8(), loaded.read())
        loaded.source.close()
        assertEquals("far more than the three retries of a thumbnail", 7, source.pageCalls.size)
        assertEquals(List(6) { 2_000L }, waits)
        val again = loader.load(ref(), ThumbPriority.VISIBLE, disk)
        assertEquals(DataSource.DISK, again.dataSource)
        again.source.close()
    }

    @Test
    fun whenTheBudgetIsUsedUpTheErrorSurfacesAndNothingIsCachedOrRemembered() = runTest {
        source.pageHandler = { throw generating(500) }
        val loader = loader(PreviewRetryPolicy(budgetMs = 1_000))
        try {
            loader.load(ref(), ThumbPriority.VISIBLE, disk)
            fail("expected Generating")
        } catch (e: GanderError.Host.Generating) {
            assertEquals(503, e.status)
        }
        assertEquals("one try and two retries", 3, source.pageCalls.size)
        assertNull("nothing half written", disk.openSnapshot(ref().cacheKey()))

        // Retry asks the host again
        millis = 0
        runCatching { loader.load(ref(), ThumbPriority.VISIBLE, disk) }
        assertEquals(6, source.pageCalls.size)
    }

    @Test
    fun aRefusalThatWillNotChangeIsAskedOnlyOncePerVersionOfTheFile() = runTest {
        source.pageHandler = { throw GanderError.Host.UnsupportedType(415, "no renderer") }
        val loader = loader()
        val first = runCatching { loader.load(ref(), ThumbPriority.VISIBLE, disk) }.exceptionOrNull()
        assertTrue(first is GanderError.Host.UnsupportedType)
        val again = runCatching { loader.load(ref(), ThumbPriority.VISIBLE, disk) }.exceptionOrNull()
        assertSame("the remembered refusal", first, again)
        assertEquals(1, source.pageCalls.size)

        runCatching { loader.load(ref(version = "v2"), ThumbPriority.VISIBLE, disk) }
        assertEquals("an edited file is asked again", 2, source.pageCalls.size)
    }

    @Test
    fun officePagesNeverTouchTheDiskBecauseTheHostMayAnswerWithAPlaceholderFirst() = runTest {
        var answer = "low-fidelity thumbnail"
        source.pageHandler = { page(answer) }
        val loader = loader()
        val office = ref(persistent = false)

        val first = loader.load(office, ThumbPriority.VISIBLE, disk)
        assertEquals(DataSource.NETWORK, first.dataSource)
        first.source.close()
        assertNull("not written", disk.openSnapshot(office.cacheKey()))

        // a minute later the host has the real render, and the phone gets it instead of a stale copy
        answer = "real render"
        val second = loader.load(office, ThumbPriority.VISIBLE, disk)
        assertEquals("real render".encodeUtf8(), second.read())
        second.source.close()
        assertEquals(2, source.pageCalls.size)
    }

    @Test
    fun theCachePoliciesOfTheRequestAreHonoured() = runTest {
        var n = 0
        source.pageHandler = { page("v${++n}") }
        val loader = loader()

        loader.load(ref(), ThumbPriority.VISIBLE, disk, writeDisk = false).source.close()
        assertNull(disk.openSnapshot(ref().cacheKey()))

        loader.load(ref(), ThumbPriority.VISIBLE, disk).source.close()
        val skipped = loader.load(ref(), ThumbPriority.VISIBLE, disk, readDisk = false)
        assertEquals(DataSource.NETWORK, skipped.dataSource)
        assertEquals("v3".encodeUtf8(), skipped.read())
        skipped.source.close()
    }

    @Test
    fun withoutADiskCacheTheBytesComeStraightFromTheHost() = runTest {
        source.pageHandler = { page("bytes", mime = null) }
        val loaded = loader().load(ref(), ThumbPriority.VISIBLE, diskCache = null)
        assertEquals(DataSource.NETWORK, loaded.dataSource)
        assertNull(loaded.mimeType)
        assertEquals("bytes".encodeUtf8(), loaded.read())
        loaded.source.close()
    }

    @Test
    fun theVisiblePageGoesBeforeAPrefetchedOneWhenTheHostsSingleSlotIsBusy() = runTest {
        val release = CompletableDeferred<Unit>()
        val order = mutableListOf<Int>()
        source.pageHandler = {
            order += it.page
            if (it.page == 1) release.await()
            page("p${it.page}")
        }
        val loader = loader(scheduler = ThumbScheduler(1))

        val running = launch { loader.load(ref(page = 1), ThumbPriority.VISIBLE, null).source.close() }
        val prefetch = launch { loader.load(ref(page = 3), ThumbPriority.PREFETCH, null).source.close() }
        val visible = launch { loader.load(ref(page = 2), ThumbPriority.VISIBLE, null).source.close() }
        runCurrent()
        assertEquals("one request in flight for the single slot", listOf(1), order)

        release.complete(Unit)
        runCurrent()
        running.join()
        prefetch.join()
        visible.join()
        assertEquals("the page on screen overtook the prefetch", listOf(1, 2, 3), order)
    }

    @Test
    fun cancellingALoadLeavesNoTraceAndFreesTheSlot() = runTest {
        val release = CompletableDeferred<Unit>()
        source.pageHandler = {
            release.await()
            page("late")
        }
        val scheduler = ThumbScheduler(1)
        val loader = loader(scheduler = scheduler)

        val job = launch { loader.load(ref(), ThumbPriority.VISIBLE, disk) }
        val queued = launch { loader.load(ref(page = 2), ThumbPriority.PREFETCH, disk) }
        runCurrent()
        assertEquals(1, scheduler.queued())

        queued.cancel()
        job.cancel()
        runCurrent()
        assertEquals(0, scheduler.queued())
        assertNull(disk.openSnapshot(ref().cacheKey()))
        assertTrue(job.isCancelled)
    }
}
