package fyi.arlo.gander.core.data.thumbs

import coil3.decode.DataSource
import coil3.disk.DiskCache
import fyi.arlo.gander.core.net.BearerToken
import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.ThumbPriority
import fyi.arlo.gander.core.net.TestHosts
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import okhttp3.tls.HeldCertificate
import okio.Buffer
import okio.ByteString.Companion.encodeUtf8
import okio.Path.Companion.toOkioPath
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import java.util.concurrent.TimeUnit

/** The thumbnail request and its answers against a pinned MockWebServer on 127.0.0.1. */
class ClientThumbSourceTest {
    @get:Rule
    val folder = TemporaryFolder()

    private lateinit var host: HeldCertificate
    private lateinit var server: MockWebServer
    private val token = "tok_thumb_secret_value"

    @Before
    fun setUp() {
        host = TestHosts.selfSigned()
        server = TestHosts.httpsServer(host)
    }

    @After
    fun tearDown() {
        server.close()
    }

    private fun source() = ClientThumbSource { TestHosts.clients(server, TestHosts.pinOf(host), BearerToken(token)) }

    private fun ref(path: String = "Photos/a.jpg", size: Int = 256) = ThumbRef("host_A", "shr_1", path, "v1", size)

    private fun webp(text: String = "webp-bytes", contentType: String = "image/webp") =
        MockResponse.Builder().code(200).setHeader("Content-Type", contentType).body(text).build()

    private fun error(code: Int, body: String, vararg headers: Pair<String, String>) =
        MockResponse.Builder().code(code).setHeader("Content-Type", "application/json").apply {
            headers.forEach { (k, v) -> setHeader(k, v) }
        }.body(body).build()

    @Test
    fun theRequestNamesShareFileSizeAndPriorityAndCarriesTheToken() = runBlocking<Unit> {
        server.enqueue(webp())
        val data = source().load(ref("Photos/été & more #1.jpg", size = 128), ThumbPriority.PREFETCH)
        assertEquals("webp-bytes".encodeUtf8(), data.bytes)
        assertEquals("image/webp", data.mimeType)

        val request = server.takeRequest()
        assertEquals("GET", request.method)
        assertEquals("/v1/shares/shr_1/thumb", request.url.encodedPath)
        assertEquals("Photos/été & more #1.jpg", request.url.queryParameter("path"))
        assertEquals("128", request.url.queryParameter("size"))
        assertEquals("prefetch", request.url.queryParameter("priority"))
        assertEquals("Bearer $token", request.headers["Authorization"])
        assertEquals("1", request.headers["X-Gander-Protocol"])
        assertNull(request.headers["Origin"])
        assertFalse("the token never goes in the URL", request.url.toString().contains(token))
    }

    @Test
    fun theVisiblePriorityIsSentToo() = runBlocking<Unit> {
        server.enqueue(webp())
        source().load(ref(), ThumbPriority.VISIBLE)
        assertEquals("visible", server.takeRequest().url.queryParameter("priority"))
    }

    @Test
    fun aContentTypeWithParametersIsReducedToTheMimeType() = runBlocking<Unit> {
        server.enqueue(webp(contentType = "image/jpeg; charset=binary"))
        assertEquals("image/jpeg", source().load(ref(), ThumbPriority.VISIBLE).mimeType)
    }

    @Test
    fun generatingBecomesATypedErrorWithTheHostsRetryHint() = runBlocking<Unit> {
        server.enqueue(error(503, """{"error":{"code":"generating","message":"Queued.","retry_after_ms":1400}}"""))
        try {
            source().load(ref(), ThumbPriority.VISIBLE)
            fail("expected Generating")
        } catch (e: GanderError.Host.Generating) {
            assertEquals(503, e.status)
            assertEquals(1400, e.retryAfterMs)
        }
    }

    @Test
    fun theRetryAfterHeaderIsUsedWhenTheBodyHasNoHint() = runBlocking<Unit> {
        server.enqueue(
            error(503, """{"error":{"code":"generating","message":"Queued.","retry_after_ms":0}}""", "Retry-After" to "2"),
        )
        try {
            source().load(ref(), ThumbPriority.VISIBLE)
            fail("expected Generating")
        } catch (e: GanderError.Host.Generating) {
            assertEquals(2000, e.retryAfterMs)
        }
    }

    @Test
    fun otherRefusalsKeepTheirTypes() = runBlocking<Unit> {
        server.enqueue(error(415, """{"error":{"code":"unsupported_type","message":"No thumbnail.","retry_after_ms":0}}"""))
        server.enqueue(error(401, """{"error":{"code":"token_revoked","message":"Revoked.","retry_after_ms":0}}"""))
        server.enqueue(MockResponse.Builder().code(502).body("<html>proxy</html>").build())
        val source = source()
        assertTrue(runCatching { source.load(ref(), ThumbPriority.VISIBLE) }.exceptionOrNull() is GanderError.Host.UnsupportedType)
        assertTrue(runCatching { source.load(ref(), ThumbPriority.VISIBLE) }.exceptionOrNull() is GanderError.Host.TokenRevoked)
        val odd = runCatching { source.load(ref(), ThumbPriority.VISIBLE) }.exceptionOrNull()
        assertTrue(odd is GanderError.Host.Unrecognised)
    }

    @Test
    fun aBodyOverTheLimitIsRefused() = runBlocking<Unit> {
        val big = Buffer().write(ByteArray((ClientThumbSource.MAX_THUMB_BYTES + 1).toInt()))
        server.enqueue(MockResponse.Builder().code(200).setHeader("Content-Type", "image/webp").body(big).build())
        try {
            source().load(ref(), ThumbPriority.VISIBLE)
            fail("expected MalformedResponse")
        } catch (_: GanderError.MalformedResponse) {
        }
    }

    @Test
    fun aHostWithAnotherCertificateGetsNoRequestAndNoToken() = runBlocking<Unit> {
        val impostor = TestHosts.selfSigned("impostor")
        val clients = TestHosts.clients(server, TestHosts.pinOf(impostor), BearerToken(token))
        try {
            ClientThumbSource { clients }.load(ref(), ThumbPriority.VISIBLE)
            fail("expected PinMismatch")
        } catch (e: GanderError.PinMismatch) {
            assertFalse(e.stackTraceToString().contains(token))
        }
        assertEquals("the handshake failed before any request", 0, server.requestCount)
    }

    @Test
    fun cancellingTheCallerClosesTheRequestOnTheWire() = runBlocking<Unit> {
        // the host holds the answer back for 5 s: the row scrolls away long before
        server.enqueue(MockResponse.Builder().code(200).headersDelay(5, TimeUnit.SECONDS).body("late").build())
        val clients = TestHosts.clients(server, TestHosts.pinOf(host), BearerToken(token))
        val dispatcher = clients.transfer.dispatcher
        val job = launch(Dispatchers.IO) { ClientThumbSource { clients }.load(ref(), ThumbPriority.VISIBLE) }
        assertTrue("the request reached the host", server.takeRequest(10, TimeUnit.SECONDS) != null)
        assertEquals(1, dispatcher.runningCallsCount())

        job.cancel()
        withTimeout(2_000) { job.join() }
        assertTrue(job.isCancelled)
        // OkHttp's call was cancelled too (its socket closed), not left waiting for the host
        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(3)
        while (dispatcher.runningCallsCount() > 0 && System.nanoTime() < deadline) Thread.sleep(20)
        assertEquals(0, dispatcher.runningCallsCount())
    }

    @Test
    fun throughTheLoaderAQueuedThumbnailIsRetriedOverRealHttpAndCachedWithoutTheToken() = runBlocking<Unit> {
        val busy = error(503, """{"error":{"code":"generating","message":"Queued.","retry_after_ms":400}}""")
        server.enqueue(busy)
        server.enqueue(busy)
        server.enqueue(webp("finally"))
        val waits = mutableListOf<Long>()
        val cacheDir = folder.newFolder("thumbs")
        val disk = DiskCache.Builder().directory(cacheDir.toOkioPath()).maxSizeBytes(1L shl 20).build()
        try {
            val loader = ThumbLoader(source(), ThumbRetryPolicy(), { ThumbScheduler(2) }, { waits += it })

            val loaded = loader.load(ref(), ThumbPriority.VISIBLE, disk)
            assertEquals(DataSource.NETWORK, loaded.dataSource)
            assertEquals("finally".encodeUtf8(), loaded.source.source().use { it.readByteString() })
            loaded.source.close()

            assertEquals(3, server.requestCount)
            assertEquals(listOf(400L, 400L), waits)
            repeat(3) { assertEquals("visible", server.takeRequest().url.queryParameter("priority")) }

            // a second look is served from disk
            val again = loader.load(ref(), ThumbPriority.VISIBLE, disk)
            assertEquals(DataSource.DISK, again.dataSource)
            again.source.close()
            assertEquals(3, server.requestCount)

            // nothing under the cache directory mentions the token or the Authorization header
            val files = cacheDir.walkTopDown().filter { it.isFile }.toList()
            assertTrue(files.isNotEmpty())
            files.forEach { file ->
                assertFalse(file.name, file.name.contains(token))
                val text = file.readBytes().toString(Charsets.ISO_8859_1)
                assertFalse(file.name, text.contains(token) || text.contains("Authorization"))
            }
        } finally {
            disk.shutdown()
        }
    }
}
