package fyi.arlo.gander.core.data.documents

import fyi.arlo.gander.core.net.GanderError
import okhttp3.Call
import okhttp3.HttpUrl
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import java.io.IOException
import java.io.InterruptedIOException
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.TimeUnit

/** The host refused a range request (revoked token, no fetch permission, file gone...). */
class HostRefusedException(val error: GanderError) : DocumentReadException("the computer refused the read", error)

/**
 * [RangeFetcher] over a host's `GET /shares/{share}/file` (protocol v1, "Fetch") on the pinned
 * `transfer` client. Each fetch is one synchronous request:
 *
 * - `Range: bytes=<a>-<b>` and `Accept-Encoding: identity`; once a response carried an `ETag`,
 *   every later request sends it back as `If-Range`.
 * - 206 must start at `a`, report the size the document was opened with as its total, carry the
 *   same `ETag` as before and deliver every byte; anything else fails the read.
 * - 412 (`file_changed`, the host's answer to a stale `If-Range`), a 200 to a request that had an
 *   `If-Range` (plain HTTP's answer), a different `ETag` or a different total size mean the file
 *   changed: [FileChangedException], which the cache makes permanent for this fd.
 * - Any other status is a [HostRefusedException] carrying the decoded [GanderError].
 *
 * Every call has its own deadline (`Call.timeout`), so a stalled host fails the read instead of
 * hanging the reader. [cancel] aborts the calls in flight. Nothing here logs: URLs carry paths
 * and the token travels in headers.
 */
class HostRangeFetcher(
    private val client: OkHttpClient,
    private val url: HttpUrl,
    private val fileSize: Long,
) : RangeFetcher {

    @Volatile private var etag: String? = null

    @Volatile private var cancelled = false
    private val inFlight = ConcurrentHashMap.newKeySet<Call>()

    override fun fetch(offset: Long, length: Int, timeoutMillis: Long): ByteArray {
        if (cancelled) throw ReadCancelledException()
        require(length > 0 && offset >= 0 && offset + length <= fileSize) { "range outside the file" }
        val sentEtag = etag
        val request = Request.Builder().url(url)
            .header("Range", "bytes=$offset-${offset + length - 1}")
            .header("Accept-Encoding", "identity")
            .apply { if (sentEtag != null) header("If-Range", sentEtag) }
            .build()
        val call = client.newCall(request)
        call.timeout().timeout(timeoutMillis.coerceAtLeast(1), TimeUnit.MILLISECONDS)
        inFlight += call
        try {
            if (cancelled) throw ReadCancelledException()
            return call.execute().use { response -> handle(response, offset, length, sentEtag) }
        } catch (e: DocumentReadException) {
            throw e
        } catch (e: InterruptedIOException) {
            if (cancelled) throw ReadCancelledException()
            throw ReadTimeoutException()
        } catch (e: IOException) {
            if (cancelled) throw ReadCancelledException()
            throw DocumentReadException("the connection to the computer failed", GanderError.fromThrowable(e))
        } catch (e: GanderError) {
            // The address-fallback interceptor reports "no address answered" this way.
            throw DocumentReadException("the computer could not be reached", e)
        } finally {
            inFlight -= call
        }
    }

    override fun cancel() {
        cancelled = true
        inFlight.forEach { it.cancel() }
    }

    private fun handle(response: Response, offset: Long, length: Int, sentEtag: String?): ByteArray {
        val responseEtag = response.header("ETag")
        when (response.code) {
            206 -> Unit
            412 -> throw FileChangedException()
            200 -> {
                if (sentEtag != null || (responseEtag != null && etag != null && responseEtag != etag)) {
                    throw FileChangedException()
                }
                throw DocumentReadException("the computer does not answer byte ranges")
            }
            else -> {
                val body = runCatching { response.body.string().take(MAX_ERROR_BODY) }.getOrNull()
                throw HostRefusedException(GanderError.fromResponse(response.code, body, response.header("Retry-After")))
            }
        }
        val range = ContentRange.parse(response.header("Content-Range"))
            ?: throw DocumentReadException("the computer sent no usable Content-Range")
        if (range.total != null && range.total != fileSize) throw FileChangedException()
        if (range.first != offset || range.last < range.first || range.length > length) {
            throw DocumentReadException("the computer sent a different range")
        }
        if (responseEtag != null) {
            synchronized(this) {
                val known = etag
                if (known == null) etag = responseEtag else if (known != responseEtag) throw FileChangedException()
            }
        }
        val expected = minOf(length.toLong(), fileSize - offset).toInt()
        if (range.length.toInt() != expected) throw DocumentReadException("the computer sent a short range")
        val bytes = ByteArray(expected)
        try {
            response.body.source().readFully(bytes)
        } catch (e: java.io.EOFException) {
            throw DocumentReadException("the computer closed the connection early", e)
        }
        return bytes
    }

    private companion object {
        const val MAX_ERROR_BODY = 4096
    }
}

/** `Content-Range: bytes <first>-<last>/<total|*>` (RFC 9110 14.4). */
data class ContentRange(val first: Long, val last: Long, val total: Long?) {
    val length: Long get() = last - first + 1

    companion object {
        private val PATTERN = Regex("""bytes\s+(\d+)-(\d+)/(\d+|\*)""")

        fun parse(header: String?): ContentRange? {
            val match = PATTERN.matchEntire(header?.trim() ?: return null) ?: return null
            val (first, last, total) = match.destructured
            return ContentRange(
                first.toLongOrNull() ?: return null,
                last.toLongOrNull() ?: return null,
                if (total == "*") null else total.toLongOrNull() ?: return null,
            )
        }
    }
}
