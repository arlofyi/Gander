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

/** The part of the tile fetcher that is not Coil: disk cache, retries, the shared slot, remembered refusals. */
@OptIn(ExperimentalCoroutinesApi::class)
class PreviewTileLoaderTest {
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

    private fun ref(page: Int = 1, level: Int = 3, x: Int = 0, y: Int = 0, version: String = "v1") =
        PreviewTileRef("host_A", "shr_1", "Plans/A1.pdf", version, page, level, x, y)

    private fun loader(
        retry: PreviewRetryPolicy = PreviewRetryPolicy(budgetMs = PreviewTileLoader.TILE_BUDGET_MS),
        memo: ThumbFailureMemo = ThumbFailureMemo(),
        scheduler: ThumbScheduler = ThumbScheduler(1),
    ) = PreviewTileLoader(
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

    private fun tile(text: String, mime: String? = "image/webp") = PreviewPageData(text.encodeUtf8(), mime, "\"t\"")

    private fun generating(retryMs: Long = 400) = GanderError.Host.Generating(503, "queued", retryMs)

    private fun LoadedThumb.read(): ByteString = this.source.source().use { it.readByteString() }

    @Test
    fun theFirstLoadFetchesAndTheSecondComesFromDisk() = runTest {
        source.tileHandler = { tile("tile-bytes") }
        val loader = loader()

        val first = loader.load(ref(), diskCache = disk)
        assertEquals(DataSource.NETWORK, first.dataSource)
        assertEquals("image/webp", first.mimeType)
        assertEquals("tile-bytes".encodeUtf8(), first.read())
        first.source.close()

        val second = loader.load(ref(), diskCache = disk)
        assertEquals(DataSource.DISK, second.dataSource)
        assertEquals("image/webp", second.mimeType)
        assertEquals("tile-bytes".encodeUtf8(), second.read())
        second.source.close()
        assertEquals("the host was asked once", 1, source.tileCalls.size)
        assertEquals(ref(), source.tileCalls.single())
    }

    @Test
    fun anotherTileOrFileVersionIsAnotherPicture() = runTest {
        source.tileHandler = { tile("l${it.level}x${it.x}y${it.y}p${it.page}${it.version}") }
        val loader = loader()
        loader.load(ref(), diskCache = disk).source.close()
        loader.load(ref(level = 4), diskCache = disk).source.close()
        loader.load(ref(x = 1), diskCache = disk).source.close()
        loader.load(ref(y = 1), diskCache = disk).source.close()
        loader.load(ref(page = 2), diskCache = disk).source.close()
        loader.load(ref(version = "v2"), diskCache = disk).source.close()
        assertEquals(6, source.tileCalls.size)
        val again = loader.load(ref(), diskCache = disk)
        assertEquals(DataSource.DISK, again.dataSource)
        again.source.close()
        assertEquals(6, source.tileCalls.size)
    }

    @Test
    fun aTileStillBeingRenderedIsWaitedForThenCached() = runTest {
        var calls = 0
        source.tileHandler = {
            if (++calls <= 4) throw generating(2_000)
            tile("rendered")
        }
        val loader = loader()
        val loaded = loader.load(ref(), diskCache = disk)
        assertEquals("rendered".encodeUtf8(), loaded.read())
        loaded.source.close()
        assertEquals(5, source.tileCalls.size)
        assertEquals(List(4) { 2_000L }, waits)
        val again = loader.load(ref(), diskCache = disk)
        assertEquals(DataSource.DISK, again.dataSource)
        again.source.close()
    }

    @Test
    fun aHostThatStaysBusyEndsTheLoadWithinTheShorterTileBudgetAndLeavesNothingBehind() = runTest {
        source.tileHandler = { throw generating(3_000) }
        val loader = loader()
        try {
            loader.load(ref(), diskCache = disk)
            fail("expected Generating")
        } catch (e: GanderError.Host.Generating) {
            assertEquals(503, e.status)
        }
        assertTrue("less than the page budget", millis <= PreviewTileLoader.TILE_BUDGET_MS)
        assertTrue("it did wait", millis > 0)
        assertNull("nothing half written", disk.openSnapshot(ref().cacheKey()))
    }

    @Test
    fun aRefusalThatWillNotChangeIsAskedOnlyOncePerVersionOfTheFile() = runTest {
        source.tileHandler = { throw GanderError.Host.BadRequest(400, "tile out of range") }
        val loader = loader()
        val first = runCatching { loader.load(ref(), diskCache = disk) }.exceptionOrNull()
        assertTrue(first is GanderError.Host.BadRequest)
        val again = runCatching { loader.load(ref(), diskCache = disk) }.exceptionOrNull()
        assertSame("the remembered refusal", first, again)
        assertEquals(1, source.tileCalls.size)

        runCatching { loader.load(ref(version = "v2"), diskCache = disk) }
        assertEquals("an edited file is asked again", 2, source.tileCalls.size)
        runCatching { loader.load(ref(x = 1), diskCache = disk) }
        assertEquals("another tile is another question", 3, source.tileCalls.size)
    }

    @Test
    fun aFileWithoutTilesIsRememberedToo() = runTest {
        source.tileHandler = { throw GanderError.Host.UnsupportedType(415, "no tiles for this file") }
        val loader = loader()
        repeat(3) { runCatching { loader.load(ref(), diskCache = disk) } }
        assertEquals(1, source.tileCalls.size)
    }

    @Test
    fun theCachePoliciesOfTheRequestAreHonoured() = runTest {
        var n = 0
        source.tileHandler = { tile("v${++n}") }
        val loader = loader()

        loader.load(ref(), diskCache = disk, writeDisk = false).source.close()
        assertNull(disk.openSnapshot(ref().cacheKey()))

        loader.load(ref(), diskCache = disk).source.close()
        val skipped = loader.load(ref(), diskCache = disk, readDisk = false)
        assertEquals(DataSource.NETWORK, skipped.dataSource)
        assertEquals("v3".encodeUtf8(), skipped.read())
        skipped.source.close()
    }

    @Test
    fun withoutADiskCacheTheBytesComeStraightFromTheHost() = runTest {
        source.tileHandler = { tile("bytes", mime = null) }
        val loaded = loader().load(ref(), diskCache = null)
        assertEquals(DataSource.NETWORK, loaded.dataSource)
        assertNull(loaded.mimeType)
        assertEquals("bytes".encodeUtf8(), loaded.read())
        loaded.source.close()
    }

    @Test
    fun aTileWaitsForTheHostsSingleSlotAndGoesBehindAPageTheUserWaitsFor() = runTest {
        // one slot per host, shared with the page loader: a tile asked for while a page is being
        // rendered queues behind it, and runs when the page is done
        val scheduler = ThumbScheduler(1)
        val release = CompletableDeferred<Unit>()
        val order = mutableListOf<String>()
        source.tileHandler = {
            order += "tile"
            tile("t")
        }
        val pages = PreviewPageLoader(
            source = FakePreviewSource().apply {
                pageHandler = {
                    order += "page"
                    release.await()
                    PreviewPageData("p".encodeUtf8(), "image/webp", null)
                }
            },
            schedulerFor = { scheduler },
        )
        val loader = loader(scheduler = scheduler)

        val page = launch {
            pages.load(PreviewPageRef("host_A", "shr_1", "Plans/A1.pdf", "v1", 1, 1080), ThumbPriority.VISIBLE, null)
                .source.close()
        }
        val tileJob = launch { loader.load(ref(), diskCache = null).source.close() }
        runCurrent()
        assertEquals("the page holds the slot, the tile queues", listOf("page"), order)
        assertEquals(1, scheduler.queued())

        release.complete(Unit)
        runCurrent()
        page.join()
        tileJob.join()
        assertEquals(listOf("page", "tile"), order)
    }

    @Test
    fun cancellingATileThatLeftTheScreenLeavesTheQueueAndFreesTheSlot() = runTest {
        val release = CompletableDeferred<Unit>()
        source.tileHandler = {
            release.await()
            tile("late")
        }
        val scheduler = ThumbScheduler(1)
        val loader = loader(scheduler = scheduler)

        val running = launch { loader.load(ref(x = 0), diskCache = disk) }
        val queued = launch { loader.load(ref(x = 1), diskCache = disk) }
        runCurrent()
        assertEquals(1, scheduler.queued())

        queued.cancel()
        running.cancel()
        runCurrent()
        assertEquals(0, scheduler.queued())
        assertNull(disk.openSnapshot(ref(x = 0).cacheKey()))
        assertTrue(running.isCancelled)
        assertEquals("the queued tile never reached the host", 1, source.tileCalls.size)
    }
}
