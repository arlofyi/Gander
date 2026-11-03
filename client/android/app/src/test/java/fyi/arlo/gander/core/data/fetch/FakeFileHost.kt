package fyi.arlo.gander.core.data.fetch

import fyi.arlo.gander.core.data.fetch.hash.Blake3
import fyi.arlo.gander.core.data.fetch.hash.toHex
import mockwebserver3.Dispatcher
import mockwebserver3.MockResponse
import mockwebserver3.RecordedRequest
import mockwebserver3.SocketEffect
import okio.Buffer
import java.security.MessageDigest
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.TimeUnit

/**
 * `/file` and `/hash` of one file as the protocol spec defines them, for MockWebServer: strong
 * ETag, single `Range: bytes=n-`, `If-Range` mismatch -> 412 `file_changed`, `/hash` with
 * `503 generating`. Knobs make it misbehave the ways a real network and a real PC do.
 */
class FakeFileHost(content: ByteArray, etag: String = "\"v1\"") : Dispatcher() {
    @Volatile var content: ByteArray = content

    @Volatile var etag: String = etag

    /** One-shot: the next file response stops after this absolute offset and the socket closes. */
    @Volatile var dropAt: Long? = null

    /** Runs right after a dropped response was built (e.g. to change the file on the "host"). */
    @Volatile var afterDrop: (() -> Unit)? = null

    /** Throttle file bodies: bytes per period (null: full speed). */
    @Volatile var throttleBytes: Long? = null

    @Volatile var throttleMs: Long = 20

    /** Answer `/hash` with this hex instead of the real one (a corrupted copy, as far as the client knows). */
    @Volatile var hashOverride: String? = null

    /** Answer this many `/hash` calls with 503 `generating` first. */
    @Volatile var hashGenerating: Int = 0

    /** Answer `/file` with this error instead (status to JSON body). */
    @Volatile var fileError: Pair<Int, String>? = null

    val fileRequests = CopyOnWriteArrayList<RecordedRequest>()
    val hashRequests = CopyOnWriteArrayList<RecordedRequest>()

    override fun dispatch(request: RecordedRequest): MockResponse {
        val path = request.url.encodedPath
        return when {
            path.endsWith("/file") -> file(request)
            path.endsWith("/hash") -> hash(request)
            else -> error(404, "not_found")
        }
    }

    private fun file(request: RecordedRequest): MockResponse {
        fileRequests += request
        fileError?.let { (status, code) -> return error(status, code) }
        val bytes = content
        val tag = etag
        val range = request.headers["Range"]
        val ifRange = request.headers["If-Range"]
        val start = range?.let { Regex("""bytes=(\d+)-""").matchEntire(it)?.groupValues?.get(1)?.toLong() }
        if (start != null && ifRange != null && ifRange != tag) return error(412, "file_changed")
        if (start != null && start >= bytes.size) {
            return MockResponse.Builder().code(416).setHeader("Content-Range", "bytes */${bytes.size}")
                .body("""{"error":{"code":"range_not_satisfiable","message":"","retry_after_ms":0}}""").build()
        }
        val from = start ?: 0L
        val builder = MockResponse.Builder()
            .code(if (start != null) 206 else 200)
            .setHeader("ETag", tag)
            .setHeader("Accept-Ranges", "bytes")
            .setHeader("Content-Type", "application/octet-stream")
        if (start != null) builder.setHeader("Content-Range", "bytes $from-${bytes.size - 1}/${bytes.size}")
        val drop = dropAt
        if (drop != null && drop > from && drop < bytes.size) {
            dropAt = null
            builder.body(Buffer().write(bytes, from.toInt(), (drop - from).toInt()))
                .setHeader("Content-Length", bytes.size - from) // promises more than it sends
                .onResponseEnd(SocketEffect.CloseSocket())
            val response = builder.build()
            val hook = afterDrop
            afterDrop = null
            hook?.invoke() // may arm the next drop
            return response
        }
        builder.body(Buffer().write(bytes, from.toInt(), (bytes.size - from).toInt()))
        throttleBytes?.let { builder.throttleBody(it, throttleMs, TimeUnit.MILLISECONDS) }
        return builder.build()
    }

    private fun hash(request: RecordedRequest): MockResponse {
        hashRequests += request
        if (hashGenerating > 0) {
            hashGenerating -= 1
            return MockResponse.Builder().code(503)
                .body("""{"error":{"code":"generating","message":"The hash is being computed.","retry_after_ms":5}}""")
                .build()
        }
        val algo = request.url.queryParameter("algo") ?: "blake3"
        val bytes = content
        val hex = hashOverride ?: when (algo) {
            "sha256" -> MessageDigest.getInstance("SHA-256").digest(bytes).toHex()
            else -> Blake3.hash(bytes).toHex()
        }
        val tag = etag.replace("\"", "\\\"")
        return MockResponse.Builder().code(200).setHeader("Content-Type", "application/json")
            .body("""{"algo":"$algo","hex":"$hex","size":${bytes.size},"etag":"$tag"}""")
            .build()
    }

    private fun error(status: Int, code: String) = MockResponse.Builder().code(status)
        .setHeader("Content-Type", "application/json")
        .body("""{"error":{"code":"$code","message":"refused","retry_after_ms":0}}""")
        .build()
}
