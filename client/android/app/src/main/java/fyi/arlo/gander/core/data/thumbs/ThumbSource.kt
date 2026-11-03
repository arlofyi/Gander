package fyi.arlo.gander.core.data.thumbs

import fyi.arlo.gander.core.net.GanderApi
import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.GanderHttpClients
import fyi.arlo.gander.core.net.ThumbPriority
import fyi.arlo.gander.core.net.await
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import okhttp3.OkHttpClient
import okhttp3.Request
import okio.ByteString
import okio.ByteString.Companion.toByteString

/** The bytes of one thumbnail and what the host said they are (`image/webp`, fallback `image/jpeg`). */
class ThumbData(val bytes: ByteString, val mimeType: String?)

/** Where thumbnail bytes come from. Fakes stand in for a host in tests. */
fun interface ThumbSource {
    /**
     * One attempt. Throws [GanderError] for anything the host or the network did (so
     * [ThumbRetryPolicy] and the UI can tell "not ready" from "never"), and cancels the request
     * on the wire when the calling coroutine is cancelled.
     */
    suspend fun load(ref: ThumbRef, priority: ThumbPriority): ThumbData
}

/** The host a thumbnail was requested for is not paired (any more). */
class UnknownThumbHostException(hostId: String) : IllegalStateException("no paired host $hostId")

/**
 * Fetches thumbnails from a host over its pinned `transfer` client, so a wrong certificate fails
 * the handshake before the token is sent. [clientsFor] returns the host's clients (the app wires
 * `PinnedHostConnector.clients`) or throws [UnknownThumbHostException].
 */
class ClientThumbSource(
    private val clientsFor: suspend (hostId: String) -> GanderHttpClients,
) : ThumbSource {

    override suspend fun load(ref: ThumbRef, priority: ThumbPriority): ThumbData {
        val clients = clientsFor(ref.hostId)
        val request = GanderApi(clients).thumbRequest(ref.shareId, ref.path, ref.sizePx, priority)
        return clients.transfer.fetchThumb(request)
    }

    companion object {
        /** A 512 px WebP is tens of kilobytes; refuse anything absurd rather than buffer it. */
        const val MAX_THUMB_BYTES = 8L * 1024 * 1024
    }
}

internal suspend fun OkHttpClient.fetchThumb(request: Request): ThumbData {
    try {
        return newCall(request).await().use { response ->
            if (!response.isSuccessful) {
                val body = response.peekBody(MAX_ERROR_BYTES).string()
                throw GanderError.fromResponse(response.code, body, response.header("Retry-After"))
            }
            val limit = ClientThumbSource.MAX_THUMB_BYTES
            if (response.body.contentLength() > limit) {
                throw GanderError.MalformedResponse(java.io.IOException("thumbnail over $limit bytes"))
            }
            val source = response.body.source()
            source.request(limit + 1)
            if (source.buffer.size > limit) {
                throw GanderError.MalformedResponse(java.io.IOException("thumbnail over $limit bytes"))
            }
            val bytes = source.buffer.readByteArray().toByteString()
            currentCoroutineContext().ensureActive()
            ThumbData(bytes, response.header("Content-Type")?.substringBefore(';')?.trim()?.ifEmpty { null })
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
