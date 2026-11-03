package fyi.arlo.gander.core.data.documents

import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.TestHosts
import mockwebserver3.Dispatcher
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import mockwebserver3.RecordedRequest
import okhttp3.OkHttpClient
import okio.Buffer
import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.TimeUnit

/** `/file` range requests of the Files-app provider (WP-60): Range math, If-Range and ETag, errors. */
class HostRangeFetcherTest {
    private val content = ByteArray(300_000) { (it * 7 % 256).toByte() }

    /** A host's `/file` for closed ranges `bytes=a-b`, with a strong ETag and 412 on a stale `If-Range`. */
    private inner class RangeHost : Dispatcher() {
        @Volatile var etag = "\"v1\""

        @Volatile var total = content.size.toLong()

        @Volatile var ignoreRange = false

        @Volatile var status: Pair<Int, String>? = null

        @Volatile var delayHeadersMs = 0L
        val requests = CopyOnWriteArrayList<RecordedRequest>()

        override fun dispatch(request: RecordedRequest): MockResponse {
            requests += request
            status?.let { (code, error) ->
                return MockResponse.Builder().code(code)
                    .body("""{"error":{"code":"$error","message":"refused","retry_after_ms":0}}""").build()
            }
            val ifRange = request.headers["If-Range"]
            if (ifRange != null && ifRange != etag) {
                return MockResponse.Builder().code(412)
                    .body("""{"error":{"code":"file_changed","message":"changed","retry_after_ms":0}}""").build()
            }
            val m = Regex("""bytes=(\d+)-(\d+)""").matchEntire(request.headers["Range"].orEmpty())
            val builder = MockResponse.Builder().setHeader("ETag", etag)
            if (delayHeadersMs > 0) builder.headersDelay(delayHeadersMs, TimeUnit.MILLISECONDS)
            if (m == null || ignoreRange) return builder.code(200).body(Buffer().write(content)).build()
            val first = m.groupValues[1].toInt()
            val last = minOf(m.groupValues[2].toInt(), content.size - 1)
            return builder.code(206)
                .setHeader("Content-Range", "bytes $first-$last/$total")
                .body(Buffer().write(content, first, last - first + 1))
                .build()
        }
    }

    private val host = RangeHost()
    private val server = MockWebServer().apply {
        dispatcher = host
        start(TestHosts.LOOPBACK, 0)
    }
    private val fetcher = HostRangeFetcher(OkHttpClient(), server.url("/v1/shares/s/file?path=a.mp4"), content.size.toLong())

    @After
    fun tearDown() {
        server.close()
    }

    @Test
    fun aRangeIsAskedForExactlyAndTheETagIsSentBackAsIfRange() {
        val bytes = fetcher.fetch(1000, 8192, 5_000)
        assertArrayEquals(content.copyOfRange(1000, 9192), bytes)
        val first = host.requests[0]
        assertEquals("bytes=1000-9191", first.headers["Range"])
        assertEquals("identity", first.headers["Accept-Encoding"])
        assertNull(first.headers["If-Range"])

        val tail = fetcher.fetch(content.size - 100L, 100, 5_000)
        assertArrayEquals(content.copyOfRange(content.size - 100, content.size), tail)
        assertEquals("\"v1\"", host.requests[1].headers["If-Range"])
    }

    @Test
    fun aChangedFileIsReportedAsFileChanged() {
        fetcher.fetch(0, 4096, 5_000)
        host.etag = "\"v2\""
        try {
            fetcher.fetch(4096, 4096, 5_000)
            fail("412 must be a FileChangedException")
        } catch (_: FileChangedException) {
        }
    }

    @Test
    fun aDifferentTotalSizeMeansTheFileChanged() {
        host.total = content.size + 1L
        try {
            fetcher.fetch(0, 4096, 5_000)
            fail("a different total must be a FileChangedException")
        } catch (_: FileChangedException) {
        }
    }

    @Test
    fun aWholeBodyAnswerIsNeverServedAsARange() {
        host.ignoreRange = true
        try {
            fetcher.fetch(10, 100, 5_000)
            fail("a 200 must not be used")
        } catch (e: DocumentReadException) {
            assertTrue(e !is FileChangedException)
        }
        host.ignoreRange = false
        fetcher.fetch(0, 100, 5_000) // learns the ETag
        host.ignoreRange = true
        try {
            fetcher.fetch(10, 100, 5_000)
            fail("a 200 to an If-Range request means the file changed")
        } catch (_: FileChangedException) {
        }
    }

    @Test
    fun aRevokedTokenIsAHostRefusal() {
        host.status = 401 to "token_revoked"
        try {
            fetcher.fetch(0, 4096, 5_000)
            fail("401 must fail")
        } catch (e: HostRefusedException) {
            assertTrue(e.error is GanderError.Host.TokenRevoked)
        }
    }

    @Test
    fun aSlowHostFailsAtTheDeadline() {
        host.delayHeadersMs = 3_000
        val started = System.nanoTime()
        try {
            fetcher.fetch(0, 4096, 300)
            fail("expected a timeout")
        } catch (_: ReadTimeoutException) {
        }
        val ms = (System.nanoTime() - started) / 1_000_000
        assertTrue("took $ms ms", ms < 2_500)
    }

    @Test
    fun aCancelledFetcherRefusesNewReads() {
        fetcher.cancel()
        try {
            fetcher.fetch(0, 10, 5_000)
            fail("cancelled")
        } catch (_: ReadCancelledException) {
        }
    }

    @Test
    fun contentRangeIsParsedStrictly() {
        assertEquals(ContentRange(0, 99, 1000), ContentRange.parse("bytes 0-99/1000"))
        assertEquals(ContentRange(5, 5, null), ContentRange.parse("bytes 5-5/*"))
        assertNull(ContentRange.parse("bytes */1000"))
        assertNull(ContentRange.parse("items 0-1/2"))
        assertNull(ContentRange.parse(null))
    }
}
