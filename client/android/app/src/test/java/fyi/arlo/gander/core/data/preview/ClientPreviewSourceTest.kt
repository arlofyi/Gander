package fyi.arlo.gander.core.data.preview

import fyi.arlo.gander.core.net.BearerToken
import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.TestHosts
import fyi.arlo.gander.core.net.protocol.PreviewKind
import kotlinx.coroutines.runBlocking
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import okhttp3.tls.HeldCertificate
import okio.Buffer
import okio.ByteString.Companion.encodeUtf8
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Before
import org.junit.Test

/** The three preview requests and their answers against a pinned MockWebServer on 127.0.0.1. */
class ClientPreviewSourceTest {
    private lateinit var host: HeldCertificate
    private lateinit var server: MockWebServer
    private val token = "tok_preview_secret_value"

    @Before
    fun setUp() {
        host = TestHosts.selfSigned()
        server = TestHosts.httpsServer(host)
    }

    @After
    fun tearDown() {
        server.close()
    }

    private fun source() = ClientPreviewSource { TestHosts.clients(server, TestHosts.pinOf(host), BearerToken(token)) }

    private fun json(code: Int, body: String, vararg headers: Pair<String, String>) =
        MockResponse.Builder().code(code).setHeader("Content-Type", "application/json").apply {
            headers.forEach { (k, v) -> setHeader(k, v) }
        }.body(body).build()

    private fun ref(page: Int = 2, width: Int = 1080) =
        PreviewPageRef("host_A", "shr_1", "Docs/été & more #1.pdf", "v1", page, width)

    @Test
    fun infoIsReadAsJsonWithTheTokenInTheHeaderOnly() = runBlocking<Unit> {
        server.enqueue(
            json(
                200,
                """{"kind":"pdf","pages":42,"width":1240,"height":1754,"text_bytes":null,"renderer":"windows_pdf","estimated_ms":400}""",
            ),
        )
        val info = source().info("host_A", "shr_1", "Docs/Report.pdf")
        assertEquals(PreviewKind.PDF, info.kind)
        assertEquals(42, info.pages)
        assertEquals(400, info.estimatedMs)

        val request = server.takeRequest()
        assertEquals("/v1/shares/shr_1/preview/info", request.url.encodedPath)
        assertEquals("Docs/Report.pdf", request.url.queryParameter("path"))
        assertEquals("Bearer $token", request.headers["Authorization"])
        assertFalse(request.url.toString().contains(token))
    }

    @Test
    fun aWordDocumentsPageCountMayBeNullUntilItsFirstRender() = runBlocking<Unit> {
        server.enqueue(
            json(
                200,
                """{"kind":"office","pages":null,"width":null,"height":null,"text_bytes":null,"renderer":"office_com","estimated_ms":15000}""",
            ),
        )
        val info = source().info("host_A", "shr_1", "Docs/Contract.docx")
        assertEquals(PreviewKind.OFFICE, info.kind)
        assertNull(info.pages)
        assertEquals(15_000, info.estimatedMs)
    }

    @Test
    fun aPageRequestNamesPageAndWidthAndReturnsBytesMimeAndEtag() = runBlocking<Unit> {
        server.enqueue(
            MockResponse.Builder().code(200)
                .setHeader("Content-Type", "image/webp")
                .setHeader("ETag", "\"p-00ff\"")
                .setHeader("Cache-Control", "private, max-age=31536000, immutable")
                .body("page-bytes").build(),
        )
        val data = source().page(ref(page = 2, width = 720))
        assertEquals("page-bytes".encodeUtf8(), data.bytes)
        assertEquals("image/webp", data.mimeType)
        assertEquals("\"p-00ff\"", data.etag)

        val request = server.takeRequest()
        assertEquals("GET", request.method)
        assertEquals("/v1/shares/shr_1/preview", request.url.encodedPath)
        assertEquals("Docs/été & more #1.pdf", request.url.queryParameter("path"))
        assertEquals("2", request.url.queryParameter("page"))
        assertEquals("720", request.url.queryParameter("width"))
        assertEquals("Bearer $token", request.headers["Authorization"])
        assertEquals("1", request.headers["X-Gander-Protocol"])
        assertNull(request.headers["Origin"])
        assertFalse(request.url.toString().contains(token))
    }

    @Test
    fun aTileRequestNamesPageLevelColumnAndRowAndReturnsBytesMimeAndEtag() = runBlocking<Unit> {
        server.enqueue(
            MockResponse.Builder().code(200)
                .setHeader("Content-Type", "image/webp")
                .setHeader("ETag", "\"t-00ff\"")
                .setHeader("Cache-Control", "private, max-age=31536000, immutable")
                .body("tile-bytes").build(),
        )
        val tile = PreviewTileRef("host_A", "shr_1", "Docs/été & more #1.pdf", "v1", page = 3, level = 4, x = 6, y = 9)
        val data = source().tile(tile)
        assertEquals("tile-bytes".encodeUtf8(), data.bytes)
        assertEquals("image/webp", data.mimeType)
        assertEquals("\"t-00ff\"", data.etag)

        val request = server.takeRequest()
        assertEquals("GET", request.method)
        assertEquals("/v1/shares/shr_1/preview/tile", request.url.encodedPath)
        assertEquals("Docs/été & more #1.pdf", request.url.queryParameter("path"))
        assertEquals("3", request.url.queryParameter("page"))
        assertEquals("4", request.url.queryParameter("level"))
        assertEquals("6", request.url.queryParameter("x"))
        assertEquals("9", request.url.queryParameter("y"))
        assertEquals("Bearer $token", request.headers["Authorization"])
        assertFalse(request.url.toString().contains(token))
    }

    @Test
    fun aTileThatDoesNotExistOrHasNoRendererKeepsItsType() = runBlocking<Unit> {
        server.enqueue(json(400, """{"error":{"code":"bad_request","message":"tile out of range","retry_after_ms":0}}"""))
        server.enqueue(json(415, """{"error":{"code":"unsupported_type","message":"No tiles.","retry_after_ms":0}}"""))
        server.enqueue(json(503, """{"error":{"code":"generating","message":"Rendering.","retry_after_ms":800}}"""))
        val tile = PreviewTileRef("host_A", "shr_1", "a.pdf", "v1", 1, 0, 0, 0)
        val source = source()
        assertTrue(runCatching { source.tile(tile) }.exceptionOrNull() is GanderError.Host.BadRequest)
        assertTrue(runCatching { source.tile(tile) }.exceptionOrNull() is GanderError.Host.UnsupportedType)
        assertEquals(800, (runCatching { source.tile(tile) }.exceptionOrNull() as GanderError.Host.Generating).retryAfterMs)
    }

    @Test
    fun aTileOverTheLimitIsRefused() = runBlocking<Unit> {
        val big = Buffer().write(ByteArray((ClientPreviewSource.MAX_TILE_BYTES + 1).toInt()))
        server.enqueue(MockResponse.Builder().code(200).setHeader("Content-Type", "image/webp").body(big).build())
        try {
            source().tile(PreviewTileRef("host_A", "shr_1", "a.pdf", "v1", 1, 0, 0, 0))
            fail("expected MalformedResponse")
        } catch (_: GanderError.MalformedResponse) {
        }
    }

    @Test
    fun aPageStillBeingRenderedIsATypedErrorWithTheHostsHint() = runBlocking<Unit> {
        server.enqueue(json(503, """{"error":{"code":"generating","message":"Rendering.","retry_after_ms":1500}}"""))
        try {
            source().page(ref())
            fail("expected Generating")
        } catch (e: GanderError.Host.Generating) {
            assertEquals(1500, e.retryAfterMs)
        }
    }

    @Test
    fun theRetryAfterHeaderIsUsedWhenTheBodyHasNoHint() = runBlocking<Unit> {
        server.enqueue(
            json(503, """{"error":{"code":"generating","message":"Rendering.","retry_after_ms":0}}""", "Retry-After" to "2"),
        )
        val error = runCatching { source().page(ref()) }.exceptionOrNull()
        assertEquals(2000, (error as GanderError.Host.Generating).retryAfterMs)
    }

    @Test
    fun refusalsKeepTheirTypes() = runBlocking<Unit> {
        server.enqueue(json(415, """{"error":{"code":"unsupported_type","message":"No renderer.","retry_after_ms":0}}"""))
        server.enqueue(json(413, """{"error":{"code":"too_large","message":"Too wide.","retry_after_ms":0}}"""))
        server.enqueue(json(403, """{"error":{"code":"permission_denied","message":"Needs preview.","retry_after_ms":0}}"""))
        val source = source()
        assertTrue(runCatching { source.page(ref()) }.exceptionOrNull() is GanderError.Host.UnsupportedType)
        assertTrue(runCatching { source.page(ref()) }.exceptionOrNull() is GanderError.Host.TooLarge)
        assertTrue(runCatching { source.page(ref()) }.exceptionOrNull() is GanderError.Host.PermissionDenied)
    }

    @Test
    fun aPageOverTheLimitIsRefused() = runBlocking<Unit> {
        val big = Buffer().write(ByteArray((ClientPreviewSource.MAX_PAGE_BYTES + 1).toInt()))
        server.enqueue(MockResponse.Builder().code(200).setHeader("Content-Type", "image/webp").body(big).build())
        try {
            source().page(ref())
            fail("expected MalformedResponse")
        } catch (_: GanderError.MalformedResponse) {
        }
    }

    @Test
    fun aTextRequestNamesOffsetAndLimitAndReadsTheEncodingHeaders() = runBlocking<Unit> {
        server.enqueue(
            MockResponse.Builder().code(200)
                .setHeader("Content-Type", "text/plain; charset=utf-8")
                .setHeader("X-Gander-Text-Encoding", "utf-16le")
                .setHeader("X-Gander-Text-Total", "300")
                .setHeader("X-Gander-Text-Next", "123")
                .body("héllo wörld").build(),
        )
        val chunk = source().text("host_A", "shr_1", "Notes/log.txt", offset = 100, limit = 65_536)
        assertEquals("héllo wörld", chunk.text)
        assertEquals(100, chunk.offset)
        assertEquals("utf-16le", chunk.encoding)
        assertEquals(300L, chunk.totalBytes)
        assertEquals("the host said where the next slice starts", 123, chunk.nextOffset)
        assertTrue(chunk.hasMore)

        val request = server.takeRequest()
        assertEquals("/v1/shares/shr_1/preview/text", request.url.encodedPath)
        assertEquals("Notes/log.txt", request.url.queryParameter("path"))
        assertEquals("100", request.url.queryParameter("offset"))
        assertEquals("65536", request.url.queryParameter("limit"))
        assertEquals("Bearer $token", request.headers["Authorization"])
    }

    @Test
    fun aBinaryFileAsTextIsUnsupported() = runBlocking<Unit> {
        server.enqueue(json(415, """{"error":{"code":"unsupported_type","message":"Binary file.","retry_after_ms":0}}"""))
        val error = runCatching { source().text("host_A", "shr_1", "a.bin", 0, 1024) }.exceptionOrNull()
        assertTrue(error is GanderError.Host.UnsupportedType)
    }

    @Test
    fun aHostWithAnotherCertificateGetsNoRequestAndNoToken() = runBlocking<Unit> {
        val impostor = TestHosts.selfSigned("impostor")
        val clients = TestHosts.clients(server, TestHosts.pinOf(impostor), BearerToken(token))
        val source = ClientPreviewSource { clients }
        assertTrue(runCatching { source.page(ref()) }.exceptionOrNull() is GanderError.PinMismatch)
        assertTrue(runCatching { source.text("host_A", "shr_1", "a.txt", 0, 10) }.exceptionOrNull() is GanderError.PinMismatch)
        assertTrue(runCatching { source.info("host_A", "shr_1", "a.pdf") }.exceptionOrNull() is GanderError.PinMismatch)
        assertEquals("the handshake failed before any request", 0, server.requestCount)
    }

    @Test
    fun aForgottenHostIsAnErrorTheViewerCanExplain() = runBlocking<Unit> {
        val source = ClientPreviewSource { throw IllegalStateException("no paired host") }
        assertTrue(runCatching { source.page(ref()) }.exceptionOrNull() is GanderError.Unreachable)
        assertTrue(runCatching { source.info("h", "s", "p") }.exceptionOrNull() is GanderError.Unreachable)
        assertTrue(runCatching { source.text("h", "s", "p", 0, 1) }.exceptionOrNull() is GanderError.Unreachable)
    }
}
