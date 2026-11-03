package fyi.arlo.gander.core.data.preview

import fyi.arlo.gander.core.net.GanderApi
import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.GanderHttpClients
import fyi.arlo.gander.core.net.await
import fyi.arlo.gander.core.net.protocol.PreviewInfo
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import okhttp3.Headers
import okhttp3.OkHttpClient
import okhttp3.Request
import okio.ByteString
import okio.ByteString.Companion.toByteString

/** The bytes of one preview page and what the host said they are (`image/webp` or `image/jpeg`). */
class PreviewPageData(val bytes: ByteString, val mimeType: String?, val etag: String?)

/**
 * One slice of a text file, decoded to UTF-8 by the host (`GET /preview/text`).
 *
 * [nextOffset] is where the next slice starts; use it, not `offset + text.length`, to continue
 * (offsets count bytes of the decoded text, and the host moves a slice to a character boundary).
 */
data class TextChunk(
    val text: String,
    /** The offset this slice was asked for. */
    val offset: Long,
    val nextOffset: Long,
    /** `X-Gander-Text-Encoding` as sent (`utf-8`, `utf-16le`, `utf-16be`, `windows-1252`); null if absent. */
    val encoding: String?,
    /** `X-Gander-Text-Total` (bytes of decoded text); null if absent or unreadable. */
    val totalBytes: Long?,
    /** More text follows [nextOffset]. */
    val hasMore: Boolean,
) {
    companion object {
        /**
         * Builds a chunk from the body and headers of `GET /preview/text`. Handles the headers that
         * may be missing: without `X-Gander-Text-Next` the next offset is `offset + body size`;
         * without `X-Gander-Text-Total` more text is assumed to follow a non-empty slice (an empty
         * slice ends the text).
         */
        fun fromResponse(offset: Long, body: ByteString, headers: Headers): TextChunk {
            val total = headers[TEXT_TOTAL_HEADER]?.trim()?.toLongOrNull()?.takeIf { it >= 0 }
            val next = headers[TEXT_NEXT_HEADER]?.trim()?.toLongOrNull()?.takeIf { it >= offset }
                ?: (offset + body.size)
            val hasMore = body.size > 0 && next > offset && (total == null || next < total)
            return TextChunk(
                text = body.utf8(),
                offset = offset,
                nextOffset = next,
                encoding = headers[TEXT_ENCODING_HEADER]?.trim()?.takeIf { it.isNotEmpty() },
                totalBytes = total,
                hasMore = hasMore,
            )
        }
    }
}

/**
 * Where preview data comes from: the JSON `preview/info`, page images and text slices of one
 * host. Fakes stand in for a host in tests. Every function throws [GanderError] and nothing else,
 * and cancels the request on the wire when the calling coroutine is cancelled.
 */
interface PreviewSource {
    suspend fun info(hostId: String, shareId: String, path: String): PreviewInfo

    /** One attempt at one page; `503 generating` surfaces as [GanderError.Host.Generating]. */
    suspend fun page(ref: PreviewPageRef): PreviewPageData

    /**
     * One attempt at one deep-zoom tile (WP-67); `503 generating` surfaces as
     * [GanderError.Host.Generating], a tile that does not exist as `BadRequest`, a file without
     * tiles as `UnsupportedType`.
     */
    suspend fun tile(ref: PreviewTileRef): PreviewPageData

    suspend fun text(hostId: String, shareId: String, path: String, offset: Long, limit: Int): TextChunk
}

/**
 * Previews over each host's pinned clients, so a wrong certificate fails the handshake before the
 * token is sent. [clientsFor] returns the host's clients (the app wires `AppGraph.hostClients`).
 * Pages and text go over the `transfer` client (60 s idle watchdog); `info` is JSON on `api`.
 */
class ClientPreviewSource(
    private val clientsFor: suspend (hostId: String) -> GanderHttpClients,
) : PreviewSource {

    /**
     * [clientsFor] fails with an `IllegalStateException` for a host that is not paired (any more,
     * forgotten while a viewer was open); callers only handle [GanderError], so it becomes the
     * "no address answered" error, which says "can't reach this computer".
     */
    private suspend fun clients(hostId: String): GanderHttpClients = try {
        clientsFor(hostId)
    } catch (e: IllegalStateException) {
        throw GanderError.Unreachable(emptyList(), e)
    }

    override suspend fun info(hostId: String, shareId: String, path: String): PreviewInfo =
        GanderApi(clients(hostId)).previewInfo(shareId, path)

    override suspend fun page(ref: PreviewPageRef): PreviewPageData {
        val clients = clients(ref.hostId)
        val request = GanderApi(clients).previewPageRequest(ref.shareId, ref.path, ref.page, ref.widthPx)
        val body = clients.transfer.fetchBody(request, MAX_PAGE_BYTES)
        return PreviewPageData(body.bytes, body.mimeType, body.headers["ETag"])
    }

    override suspend fun tile(ref: PreviewTileRef): PreviewPageData {
        val clients = clients(ref.hostId)
        val request = GanderApi(clients).previewTileRequest(ref.shareId, ref.path, ref.page, ref.level, ref.x, ref.y)
        val body = clients.transfer.fetchBody(request, MAX_TILE_BYTES)
        return PreviewPageData(body.bytes, body.mimeType, body.headers["ETag"])
    }

    override suspend fun text(hostId: String, shareId: String, path: String, offset: Long, limit: Int): TextChunk {
        val clients = clients(hostId)
        val request = GanderApi(clients).previewTextRequest(shareId, path, offset, limit)
        val body = clients.transfer.fetchBody(request, MAX_TEXT_BYTES)
        return TextChunk.fromResponse(offset, body.bytes, body.headers)
    }

    companion object {
        /** A page is 100-250 KB, a 2048 px photo a few MB; refuse anything absurd rather than buffer it. */
        const val MAX_PAGE_BYTES = 24L * 1024 * 1024

        /** A tile is a 512 px WebP, tens of KB; refuse anything absurd. */
        const val MAX_TILE_BYTES = 2L * 1024 * 1024

        /** The host caps one slice at 1 MiB. */
        const val MAX_TEXT_BYTES = 2L * 1024 * 1024
    }
}

const val TEXT_ENCODING_HEADER = "X-Gander-Text-Encoding"
const val TEXT_TOTAL_HEADER = "X-Gander-Text-Total"

/** Added by the host's router when it can tell where a slice really ended; not guaranteed to exist. */
const val TEXT_NEXT_HEADER = "X-Gander-Text-Next"

internal class FetchedBody(val bytes: ByteString, val mimeType: String?, val headers: Headers)

/** GET [request] and buffer at most [limit] bytes; protocol errors become [GanderError]s. */
internal suspend fun OkHttpClient.fetchBody(request: Request, limit: Long): FetchedBody {
    try {
        return newCall(request).await().use { response ->
            if (!response.isSuccessful) {
                val body = response.peekBody(MAX_ERROR_BYTES).string()
                throw GanderError.fromResponse(response.code, body, response.header("Retry-After"))
            }
            if (response.body.contentLength() > limit) {
                throw GanderError.MalformedResponse(java.io.IOException("preview over $limit bytes"))
            }
            val source = response.body.source()
            source.request(limit + 1)
            if (source.buffer.size > limit) {
                throw GanderError.MalformedResponse(java.io.IOException("preview over $limit bytes"))
            }
            val bytes = source.buffer.readByteArray().toByteString()
            currentCoroutineContext().ensureActive()
            FetchedBody(
                bytes = bytes,
                mimeType = response.header("Content-Type")?.substringBefore(';')?.trim()?.ifEmpty { null },
                headers = response.headers,
            )
        }
    } catch (e: CancellationException) {
        throw e
    } catch (e: GanderError) {
        throw e
    } catch (e: Exception) {
        throw GanderError.fromThrowable(e)
    }
}

/** Error bodies are small JSON; read no more than this of one. */
private const val MAX_ERROR_BYTES = 64L * 1024
