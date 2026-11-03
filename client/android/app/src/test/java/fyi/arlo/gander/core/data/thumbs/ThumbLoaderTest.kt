package fyi.arlo.gander.core.data.thumbs

import coil3.decode.DataSource
import coil3.disk.DiskCache
import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.ThumbPriority
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
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

/** The part of the fetcher that is not Coil: disk cache, retries, scheduling, remembered refusals. */
@OptIn(ExperimentalCoroutinesApi::class)
class ThumbLoaderTest {
    @get:Rule
    val folder = TemporaryFolder()

    private lateinit var disk: DiskCache

    @Before
    fun setUp() {
        disk = DiskCache.Builder().directory(folder.newFolder("thumbs").toOkioPath()).maxSizeBytes(1L shl 20).build()
    }

    @After
    fun tearDown() {
        disk.shutdown()
    }

    private fun ref(path: String = "Photos/a.jpg", version: String = "v1", size: Int = 256) =
        ThumbRef("host_A", "shr_1", path, version, size)

    /** Answers from [script] in order; the last answer repeats. Counts calls and records priorities. */
    private class ScriptedSource(private vararg val script: () -> ThumbData) : ThumbSource {
        var calls = 0
        val priorities = mutableListOf<ThumbPriority>()

        override suspend fun load(ref: ThumbRef, priority: ThumbPriority): ThumbData {
            priorities += priority
            return script[minOf(calls++, script.lastIndex)]()
        }
    }

    private fun bytes(text: String, mime: String? = "image/webp") = { ThumbData(text.encodeUtf8(), mime) }

    private fun generating(retryMs: Long = 300) = { throw GanderError.Host.Generating(503, "queued", retryMs) }

    private fun newLoader(
        source: ThumbSource,
        waits: MutableList<Long> = mutableListOf(),
        memo: ThumbFailureMemo = ThumbFailureMemo(),
    ) =
        ThumbLoader(
            source = source,
            retry = ThumbRetryPolicy(),
            schedulerFor = { ThumbScheduler(2) },
            sleep = { waits += it },
            failures = memo,
        )

    private fun LoadedThumb.read(): ByteString = source.source().use { it.readByteString() }

    @Test
    fun theFirstLoadFetchesAndTheSecondComesFromDisk() = runTest {
        val source = ScriptedSource(bytes("webp-bytes"))
        val loader = newLoader(source)

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

        assertEquals("the host was asked once", 1, source.calls)
    }

    @Test
    fun anEditedFileGetsANewThumbnailAndTheOldEntryIsLeftAlone() = runTest {
        val source = ScriptedSource(bytes("old"), bytes("new"))
        val loader = newLoader(source)

        loader.load(ref(version = "mtime-1#10"), ThumbPriority.VISIBLE, disk).source.close()
        val edited = loader.load(ref(version = "mtime-2#10"), ThumbPriority.VISIBLE, disk)
        assertEquals(DataSource.NETWORK, edited.dataSource)
        assertEquals("new".encodeUtf8(), edited.read())
        edited.source.close()
        assertEquals(2, source.calls)

        // the other size of the same file is a different picture too
        loader.load(ref(version = "mtime-2#10", size = 128), ThumbPriority.VISIBLE, disk).source.close()
        assertEquals(3, source.calls)

        // and the first version is still served from disk
        val old = loader.load(ref(version = "mtime-1#10"), ThumbPriority.VISIBLE, disk)
        assertEquals(DataSource.DISK, old.dataSource)
        assertEquals("old".encodeUtf8(), old.read())
        old.source.close()
        assertEquals(3, source.calls)
    }

    @Test
    fun aThumbnailThatIsQueuedOnTheHostIsRetriedThenCached() = runTest {
        val waits = mutableListOf<Long>()
        val source = ScriptedSource(generating(300), generating(600), bytes("ready"))
        val loader = newLoader(source, waits)

        val loaded = loader.load(ref(), ThumbPriority.VISIBLE, disk)
        assertEquals("ready".encodeUtf8(), loaded.read())
        loaded.source.close()
        assertEquals(3, source.calls)
        assertEquals(listOf(300L, 600L), waits)

        assertEquals(DataSource.DISK, loader.load(ref(), ThumbPriority.VISIBLE, disk).also { it.source.close() }.dataSource)
        assertEquals(3, source.calls)
    }

    @Test
    fun whenTheHostStaysBusyTheErrorSurfacesAndNothingIsCachedOrRemembered() = runTest {
        val source = ScriptedSource(generating())
        val loader = newLoader(source)
        try {
            loader.load(ref(), ThumbPriority.VISIBLE, disk)
            fail("expected Generating")
        } catch (e: GanderError.Host.Generating) {
            assertEquals(503, e.status)
        }
        assertEquals("1 try + 3 retries", 4, source.calls)
        assertNull("nothing half written", disk.openSnapshot(ref().cacheKey()))

        // scrolling back later asks again
        try {
            loader.load(ref(), ThumbPriority.VISIBLE, disk)
            fail("expected Generating again")
        } catch (_: GanderError.Host.Generating) {
        }
        assertEquals(8, source.calls)
    }

    @Test
    fun aRefusalThatWillNotChangeIsAskedOnlyOncePerVersionOfTheFile() = runTest {
        val unsupported = { throw GanderError.Host.UnsupportedType(415, "no thumbnail for this type") }
        val source = ScriptedSource(unsupported)
        val loader = newLoader(source)

        val first = runCatching { loader.load(ref(), ThumbPriority.VISIBLE, disk) }.exceptionOrNull()
        assertTrue(first is GanderError.Host.UnsupportedType)
        assertEquals(1, source.calls)

        val again = runCatching { loader.load(ref(), ThumbPriority.VISIBLE, disk) }.exceptionOrNull()
        assertSame("the remembered refusal", first, again)
        assertEquals("no second request to the host", 1, source.calls)

        // the file was edited: the host may be able to render it now
        runCatching { loader.load(ref(version = "v2"), ThumbPriority.VISIBLE, disk) }
        assertEquals(2, source.calls)
    }

    @Test
    fun onlyPermanentRefusalsAreRemembered() {
        val permanent = listOf(
            GanderError.Host.UnsupportedType(415, ""),
            GanderError.Host.TooLarge(413, ""),
            GanderError.Host.NotFound(404, ""),
            GanderError.Host.BadPath(400, ""),
            GanderError.Host.BadRequest(400, ""),
            GanderError.Host.PermissionDenied(403, ""),
            GanderError.Host.Forbidden(403, ""),
        )
        permanent.forEach { assertTrue(it.code.wire, ThumbFailureMemo.isPermanent(it)) }
        val transient = listOf(
            GanderError.Host.Generating(503, "", 100),
            GanderError.Host.TooManyRequests(429, "", 100),
            GanderError.Host.Internal(500, ""),
            GanderError.Host.TokenInvalid(401, ""),
            GanderError.Host.ShareNotFound(404, ""),
        )
        transient.forEach { assertTrue(it.code.wire, !ThumbFailureMemo.isPermanent(it)) }
    }

    @Test
    fun theRememberedRefusalsAreBounded() {
        val memo = ThumbFailureMemo(maxEntries = 2)
        val error = GanderError.Host.UnsupportedType(415, "")
        memo.put("a", error)
        memo.put("b", error)
        memo.put("c", error)
        assertNull(memo.get("a"))
        assertNotNull(memo.get("b"))
        assertNotNull(memo.get("c"))
    }

    @Test
    fun thePriorityReachesTheSource() = runTest {
        val source = ScriptedSource(bytes("x"))
        val loader = newLoader(source)
        loader.load(ref("a.jpg"), ThumbPriority.PREFETCH, disk).source.close()
        loader.load(ref("b.jpg"), ThumbPriority.VISIBLE, disk).source.close()
        assertEquals(listOf(ThumbPriority.PREFETCH, ThumbPriority.VISIBLE), source.priorities)
    }

    @Test
    fun theCachePoliciesOfTheRequestAreHonoured() = runTest {
        val source = ScriptedSource(bytes("one"), bytes("two"), bytes("three"))
        val loader = newLoader(source)

        // write disabled: fetched, not stored
        loader.load(ref(), ThumbPriority.VISIBLE, disk, writeDisk = false).source.close()
        assertNull(disk.openSnapshot(ref().cacheKey()))

        // stored by a normal load
        loader.load(ref(), ThumbPriority.VISIBLE, disk).source.close()
        assertEquals(2, source.calls)

        // read disabled: goes to the host although the disk has it
        val skipped = loader.load(ref(), ThumbPriority.VISIBLE, disk, readDisk = false)
        assertEquals(DataSource.NETWORK, skipped.dataSource)
        assertEquals("three".encodeUtf8(), skipped.read())
        skipped.source.close()
        assertEquals(3, source.calls)
    }

    @Test
    fun withoutADiskCacheTheBytesComeStraightFromTheHost() = runTest {
        val source = ScriptedSource(bytes("bytes", mime = null))
        val loaded = newLoader(source).load(ref(), ThumbPriority.VISIBLE, diskCache = null)
        assertEquals(DataSource.NETWORK, loaded.dataSource)
        assertNull(loaded.mimeType)
        assertEquals("bytes".encodeUtf8(), loaded.read())
        loaded.source.close()
    }

    @Test
    fun cancellingALoadLeavesNoTraceAndFreesTheScheduler() = runTest {
        val release = CompletableDeferred<Unit>()
        var calls = 0
        val source = ThumbSource { _, _ ->
            calls++
            release.await()
            ThumbData("late".encodeUtf8(), "image/webp")
        }
        val scheduler = ThumbScheduler(1)
        val memo = ThumbFailureMemo()
        val loader = ThumbLoader(source, ThumbRetryPolicy(), { scheduler }, { }, memo)

        val job = launch { loader.load(ref(), ThumbPriority.VISIBLE, disk) }
        val queued = launch { loader.load(ref("b.jpg"), ThumbPriority.VISIBLE, disk) }
        runCurrent()
        assertEquals("one running, one waiting for the single slot", 1, calls)
        assertEquals(1, scheduler.queued())

        job.cancel()
        queued.cancel()
        runCurrent()
        assertTrue(job.isCancelled && queued.isCancelled)
        assertEquals("the cancelled waiter never started", 1, calls)
        assertEquals(0, scheduler.queued())
        assertNull(memo.get(ref().cacheKey()))
        assertNull(disk.openSnapshot(ref().cacheKey()))

        // the slot is free again
        release.complete(Unit)
        val done = loader.load(ref("c.jpg"), ThumbPriority.VISIBLE, disk)
        assertEquals("late".encodeUtf8(), done.read())
        done.source.close()
    }
}
