package fyi.arlo.gander.core.data.documents

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.Executor
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.random.Random

/**
 * The per-fd block cache of the Files-app provider (WP-60) against the read patterns the
 * Files-app spike measured: bulk sequential 128 KiB reads (copy), a player's two demux readers
 * about 2 MiB apart with small forward gaps, small random preads, and seeks.
 */
class ReadAheadCacheTest {
    private val kib = 1024
    private val mib = 1024 * 1024

    /** Deterministic content: byte at offset o. */
    private fun byteAt(o: Long): Byte = ((o * 131 + (o shr 20)) % 251).toByte()

    private inner class FakeFetcher(private val size: Long) : RangeFetcher {
        val requests = CopyOnWriteArrayList<Pair<Long, Int>>()
        val cancelled = AtomicBoolean(false)
        var failAt: Int? = null
        var failWith: () -> Exception = { FileChangedException() }

        override fun fetch(offset: Long, length: Int, timeoutMillis: Long): ByteArray {
            assertTrue("in range", offset >= 0 && offset + length <= size)
            requests += offset to length
            if (failAt == requests.size) throw failWith()
            return ByteArray(length) { byteAt(offset + it) }
        }

        override fun cancel() {
            cancelled.set(true)
        }
    }

    private val inline = Executor { it.run() }

    private fun cache(size: Long, fetcher: RangeFetcher, executor: Executor = inline, config: ReadAheadConfig = ReadAheadConfig()) =
        ReadAheadCache(size, fetcher, executor, config)

    private fun readAndCheck(cache: ReadAheadCache, offset: Long, size: Int, fileSize: Long) {
        val out = ByteArray(size)
        val n = cache.read(offset, size, out)
        val expected = minOf(size.toLong(), maxOf(0L, fileSize - offset)).toInt()
        assertEquals("bytes returned at $offset", expected, n)
        assertArrayEquals("content at $offset", ByteArray(n) { byteAt(offset + it) }, out.copyOf(n))
    }

    @Test
    fun aBulkSequentialReaderGetsGrowingBlocksAndNoOverFetch() {
        val size = 30_811_338L // the spike's test file
        val fetcher = FakeFetcher(size)
        val c = cache(size, fetcher)
        var pos = 0L
        while (pos < size) {
            readAndCheck(c, pos, 128 * kib, size)
            pos += 128 * kib
        }
        val stats = c.stats()
        assertEquals(size, stats.bytesServed)
        assertEquals("every byte fetched once", size, stats.bytesFetched)
        // 236 kernel reads (spike) become about 35 requests: 1 MiB blocks after a short ramp.
        assertTrue("requests: ${stats.requests}", stats.requests <= 40)
        assertTrue("resident ${stats.maxResidentBytes}", stats.maxResidentBytes <= 4L * mib)
    }

    @Test
    fun theFirstReadAfterAJumpFetchesExactlyThatThenTheWindowGrows() {
        val size = 20L * mib
        val fetcher = FakeFetcher(size)
        val c = cache(size, fetcher)
        val start = 7L * mib + 12_345
        var pos = start
        while (pos < start + 3 * mib) {
            readAndCheck(c, pos, 4 * kib, size)
            pos += 4 * kib
        }
        val lengths = fetcher.requests.map { it.second }
        assertEquals(listOf(4 * kib, 64 * kib, 128 * kib, 256 * kib, 512 * kib, mib), lengths.take(6))
        assertEquals(start, fetcher.requests[0].first)
        assertTrue("never above 1 MiB", lengths.all { it <= mib })
    }

    @Test
    fun smallRandomPreadsCostExactlyWhatTheyAskFor() {
        val size = 50L * mib
        val fetcher = FakeFetcher(size)
        val c = cache(size, fetcher)
        val random = Random(47)
        repeat(200) { readAndCheck(c, random.nextLong(size - 8 * kib), 8 * kib, size) }
        val stats = c.stats()
        println("random 8 KiB preads: $stats overFetch=${stats.overFetch}")
        // A pread that happens to land just past an earlier one looks like a player's forward gap and
        // continues that stream (a few of 200 here, 64 KiB each); none of them prefetches.
        assertEquals("prefetches", 0L, stats.prefetches)
        assertTrue("over-fetch ${stats.overFetch}", stats.overFetch <= 1.3)
        assertTrue(stats.maxResidentBytes <= 4L * mib)
    }

    @Test
    fun twoDemuxReadersTwoMebibytesApartKeepTheirOwnStreams() {
        // Player pattern (spike): ~8 KiB reads, forward gaps of 8 to 20 KiB, two readers ~2 MiB apart.
        val size = 30L * mib
        val fetcher = FakeFetcher(size)
        val c = cache(size, fetcher)
        val random = Random(5)
        var video = 400L * kib
        var audio = video + 2 * mib
        var reads = 0
        while (audio < 20L * mib) {
            val v = listOf(4, 8, 8, 12, 16, 20)[random.nextInt(6)] * kib
            readAndCheck(c, video, v, size)
            video += v + random.nextInt(8, 21) * kib
            val a = listOf(4, 8, 8)[random.nextInt(3)] * kib
            readAndCheck(c, audio, a, size)
            audio += a + random.nextInt(8, 21) * kib
            reads += 2
        }
        val stats = c.stats()
        assertTrue("requests ${stats.requests} for $reads reads", stats.requests * 20 < reads)
        // The gaps are fetched (one request spans them); a single 1 MiB window measured 2.42x.
        val spanned = (video - 400L * kib) + (audio - 400L * kib - 2 * mib)
        println("two demux readers: $stats reads=$reads spanned=$spanned ratio=${stats.bytesFetched.toDouble() / spanned}")
        // What the readers stepped over is fetched too (one request spans the small gaps); on top of
        // that only the blocks still ahead of each reader when it stops (at most 2 MiB per stream).
        assertTrue("fetched ${stats.bytesFetched} for $spanned spanned", stats.bytesFetched <= spanned * 115 / 100)
        assertTrue("resident ${stats.maxResidentBytes}", stats.maxResidentBytes <= 4L * mib)
        assertTrue("prefetch was used", stats.prefetchHits > 0)
    }

    @Test
    fun aSeekStartsANewStreamAndOldOnesAreEvictedWithinTheBudget() {
        val size = 64L * mib
        val fetcher = FakeFetcher(size)
        val c = cache(size, fetcher)
        for (seek in listOf(0L, 10L, 20L, 30L, 40L, 50L, 5L).map { it * mib }) {
            var pos = seek
            repeat(300) {
                readAndCheck(c, pos, 8 * kib, size)
                pos += 8 * kib
            }
        }
        assertTrue("resident ${c.stats().maxResidentBytes}", c.stats().maxResidentBytes <= 4L * mib)
        assertEquals(7L, c.stats().jumps)
    }

    @Test
    fun readsAtAndAcrossTheEndAreClamped() {
        val size = 100_000L
        val c = cache(size, FakeFetcher(size))
        readAndCheck(c, 0, 131_072, size)
        readAndCheck(c, 99_000, 8 * kib, size)
        assertEquals(0, c.read(size, 4096, ByteArray(4096)))
        assertEquals(0, c.read(size + 10, 4096, ByteArray(4096)))
        val empty = cache(0, FakeFetcher(0))
        assertEquals(0, empty.read(0, 4096, ByteArray(4096)))
    }

    @Test
    fun aChangedFileFailsThisAndEveryLaterRead() {
        val size = 8L * mib
        val fetcher = FakeFetcher(size).apply { failAt = 3 }
        val c = cache(size, fetcher)
        var pos = 0L
        var failed = false
        while (pos < size && !failed) {
            try {
                readAndCheck(c, pos, 128 * kib, size)
            } catch (_: FileChangedException) {
                failed = true
            }
            pos += 128 * kib
        }
        assertTrue("the change was reported", failed)
        // Even a range that would be served from memory now fails: no mixing of versions.
        try {
            c.read(0, 4096, ByteArray(4096))
            fail("a read after a change must fail")
        } catch (_: FileChangedException) {
        }
    }

    @Test
    fun anOrdinaryFailureFailsOnlyThatRead() {
        val size = 4L * mib
        val fetcher = FakeFetcher(size).apply {
            failAt = 1
            failWith = { DocumentReadException("connection reset") }
        }
        val c = cache(size, fetcher)
        try {
            c.read(0, 4096, ByteArray(4096))
            fail("first read should fail")
        } catch (e: DocumentReadException) {
            assertTrue(e !is FileChangedException)
        }
        readAndCheck(c, 0, 4096, size)
    }

    @Test
    fun aPrefetchThatNeverArrivesTimesTheReadOut() {
        val size = 8L * mib
        val fetcher = FakeFetcher(size)
        // Prefetches are queued but never run: the reader must give up at its deadline.
        val c = cache(size, fetcher, executor = Executor { }, config = ReadAheadConfig(readTimeoutMillis = 200))
        var pos = 0L
        val started = System.nanoTime()
        try {
            while (pos < size) {
                readAndCheck(c, pos, 128 * kib, size)
                pos += 128 * kib
            }
            fail("expected a timeout")
        } catch (_: ReadTimeoutException) {
        }
        val ms = (System.nanoTime() - started) / 1_000_000
        assertTrue("gave up after about the deadline ($ms ms)", ms in 150..5_000)
    }

    @Test
    fun cancellingAbortsTheFetcherAndFailsLaterReads() {
        val size = 4L * mib
        val fetcher = FakeFetcher(size)
        val c = cache(size, fetcher)
        readAndCheck(c, 0, 4096, size)
        c.cancel()
        assertTrue(fetcher.cancelled.get())
        try {
            c.read(0, 4096, ByteArray(4096))
            fail("read after cancel")
        } catch (_: ReadCancelledException) {
        }
    }

    @Test
    fun aRequestLargerThanTheBufferIsRefused() {
        val c = cache(1000, FakeFetcher(1000))
        try {
            c.read(0, 10, ByteArray(5))
            fail("size above the buffer")
        } catch (_: DocumentReadException) {
        }
    }
}
