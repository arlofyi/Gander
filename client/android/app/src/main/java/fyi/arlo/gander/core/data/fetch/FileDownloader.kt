package fyi.arlo.gander.core.data.fetch

import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.await
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import okhttp3.HttpUrl
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import java.io.EOFException
import java.io.File
import java.io.IOException
import java.io.RandomAccessFile

/** Where a download stands: what `Range: bytes=<bytesDone>-` and `If-Range: <etag>` resume from. */
data class ResumePoint(val bytesDone: Long, val totalBytes: Long?, val etag: String?) {
    companion object {
        val START = ResumePoint(0, null, null)
    }
}

/** Writing the downloaded bytes on the phone failed (disk full, storage gone). Not a host problem. */
class LocalStorageException(cause: IOException) : IOException("cannot write the downloaded file", cause)

/** The file changed on the host more often than the engine is willing to start over. */
class FileChangedTooOftenException : Exception("the file kept changing on the host")

/**
 * One file over HTTP into one local file, resumable (client spec, "Fetch engine"):
 *
 * - The request carries `Accept-Encoding: identity` (a range of a compressed body would be
 *   meaningless) and, when there are bytes and an ETag to resume from, `Range: bytes=<n>-` plus
 *   `If-Range: <etag>`.
 * - 206 appends after checking `Content-Range` starts at `n` and the ETag is unchanged.
 * - 412 `file_changed` (this host's answer to a stale `If-Range`), or a 200 to a ranged request
 *   with a different ETag (plain HTTP's answer), means the file changed: [Listener.onFileChanged]
 *   is told (it may throw to give up) and the download starts again from zero.
 * - A 206 that does not fit, a 200 with the same ETag or a 416: start over quietly (at most
 *   [maxQuietRestarts] times, then the host is treated as incompatible).
 *
 * The offset never exceeds what is on disk: resuming takes the smaller of the persisted count and
 * the file length, so a crash between a write and a progress save cannot leave a hole.
 *
 * Errors are [GanderError] (host or transport), [LocalStorageException] (the phone), or whatever
 * [Listener.onFileChanged] throws. Cancelling the coroutine cancels the HTTP call at once.
 */
class FileDownloader(
    private val clock: () -> Long = System::currentTimeMillis,
    private val bufferSize: Int = 64 * 1024,
    private val progressEveryBytes: Long = 1024L * 1024L,
    private val progressEveryMs: Long = 500,
    private val maxQuietRestarts: Int = 2,
) {
    interface Listener {
        /** Bytes are on disk up to [point]; persist it. Also called once when an attempt ends. */
        suspend fun onProgress(point: ResumePoint)

        /** The host's file is a different version now; the download restarts from zero. Throw to stop. */
        suspend fun onFileChanged()
    }

    private sealed interface Attempt {
        data class Done(val point: ResumePoint) : Attempt

        data class Restart(val changed: Boolean) : Attempt
    }

    suspend fun download(
        client: OkHttpClient,
        url: HttpUrl,
        file: File,
        from: ResumePoint,
        listener: Listener,
    ): ResumePoint = withContext(Dispatchers.IO) { loop(client, url, file, from, listener) }

    private suspend fun loop(
        client: OkHttpClient,
        url: HttpUrl,
        file: File,
        from: ResumePoint,
        listener: Listener,
    ): ResumePoint {
        var point = startingPoint(file, from)
        var quietRestarts = 0
        while (true) {
            when (val outcome = attempt(client, url, file, point, listener)) {
                is Attempt.Done -> return outcome.point
                is Attempt.Restart -> {
                    if (outcome.changed) {
                        listener.onFileChanged()
                    } else {
                        quietRestarts += 1
                        if (quietRestarts > maxQuietRestarts) {
                            throw GanderError.MalformedResponse(IOException("host does not honour the byte range"))
                        }
                    }
                    point = ResumePoint.START
                    truncate(file, 0)
                    listener.onProgress(point)
                }
            }
        }
    }

    private fun startingPoint(file: File, from: ResumePoint): ResumePoint {
        val onDisk = if (file.isFile) file.length() else 0L
        val offset = if (from.etag == null) 0L else minOf(onDisk, from.bytesDone).coerceAtLeast(0)
        truncate(file, offset)
        return if (offset == 0L) ResumePoint(0, from.totalBytes, from.etag) else from.copy(bytesDone = offset)
    }

    private suspend fun attempt(
        client: OkHttpClient,
        url: HttpUrl,
        file: File,
        point: ResumePoint,
        listener: Listener,
    ): Attempt = coroutineScope {
        val ranged = point.bytesDone > 0 && point.etag != null
        val request = Request.Builder().url(url).header("Accept-Encoding", "identity").apply {
            if (ranged) {
                header("Range", "bytes=${point.bytesDone}-")
                header("If-Range", point.etag!!)
            }
        }.build()
        val call = client.newCall(request)
        // Cancelling this coroutine must unblock a socket read at once, not after the idle timeout.
        val canceller = launch(Dispatchers.Unconfined) {
            try {
                awaitCancellation()
            } finally {
                call.cancel()
            }
        }
        try {
            val response = try {
                call.await()
            } catch (e: IOException) {
                currentCoroutineContext().ensureActive()
                throw GanderError.fromThrowable(e)
            }
            response.use { handle(it, ranged, file, point, listener) }
        } finally {
            canceller.cancel()
        }
    }

    private suspend fun handle(
        response: Response,
        ranged: Boolean,
        file: File,
        point: ResumePoint,
        listener: Listener,
    ): Attempt {
        val etag = response.header("ETag")
        return when (response.code) {
            206 -> {
                val range = parseContentRange(response.header("Content-Range"))
                when {
                    !ranged || range == null || range.start != point.bytesDone -> Attempt.Restart(changed = false)
                    etag != null && etag != point.etag -> Attempt.Restart(changed = true)
                    else -> {
                        val total = range.total ?: point.totalBytes
                        receive(response, file, point.bytesDone, total, point.etag, listener)
                    }
                }
            }
            200 -> {
                if (ranged) {
                    // The host sent the whole file instead of the range: a newer version (ETag
                    // differs) restarts with the user told; otherwise it just did not do ranges.
                    if (etag != point.etag) listener.onFileChanged()
                }
                val length = response.body.contentLength().takeIf { it >= 0 }
                receive(response, file, 0, length, etag, listener)
            }
            412 -> Attempt.Restart(changed = true)
            416 -> if (ranged) Attempt.Restart(changed = false) else throw errorOf(response)
            else -> throw errorOf(response)
        }
    }

    private suspend fun receive(
        response: Response,
        file: File,
        offset: Long,
        total: Long?,
        etag: String?,
        listener: Listener,
    ): Attempt {
        var done = offset
        try {
            val raf = try {
                RandomAccessFile(file, "rw").also {
                    it.setLength(offset)
                    it.seek(offset)
                }
            } catch (e: IOException) {
                throw LocalStorageException(e)
            }
            raf.use {
                val source = response.body.source()
                val buffer = ByteArray(bufferSize)
                var reportedBytes = done
                var reportedAt = clock()
                while (true) {
                    currentCoroutineContext().ensureActive()
                    val n = try {
                        source.read(buffer)
                    } catch (e: IOException) {
                        currentCoroutineContext().ensureActive()
                        throw GanderError.fromThrowable(e)
                    }
                    if (n < 0) break
                    try {
                        raf.write(buffer, 0, n)
                    } catch (e: IOException) {
                        throw LocalStorageException(e)
                    }
                    done += n
                    if (total != null && done > total) {
                        throw GanderError.MalformedResponse(IOException("host sent more bytes than announced"))
                    }
                    val now = clock()
                    if (done - reportedBytes >= progressEveryBytes || now - reportedAt >= progressEveryMs) {
                        listener.onProgress(ResumePoint(done, total, etag))
                        reportedBytes = done
                        reportedAt = now
                    }
                }
            }
        } finally {
            // Whatever happened, what is on disk is resumable from here.
            withContext(NonCancellable) { listener.onProgress(ResumePoint(done, total, etag)) }
        }
        if (total != null && done < total) {
            throw GanderError.Network(EOFException("connection closed after $done of $total bytes"))
        }
        return Attempt.Done(ResumePoint(done, total ?: done, etag))
    }

    private fun errorOf(response: Response): GanderError.Host {
        val body = try {
            response.peekBody(MAX_ERROR_BODY).string()
        } catch (e: CancellationException) {
            throw e
        } catch (_: IOException) {
            ""
        }
        return GanderError.fromResponse(response.code, body, response.header("Retry-After"))
    }

    private fun truncate(file: File, length: Long) {
        try {
            if (length == 0L) {
                if (file.exists() && !file.delete()) RandomAccessFile(file, "rw").use { it.setLength(0) }
            } else {
                RandomAccessFile(file, "rw").use { it.setLength(length) }
            }
        } catch (e: IOException) {
            throw LocalStorageException(e)
        }
    }

    /** `bytes <start>-<end>/<total|*>`. */
    internal data class ContentRange(val start: Long, val end: Long, val total: Long?)

    companion object {
        private const val MAX_ERROR_BODY = 64L * 1024
        private val CONTENT_RANGE = Regex("""^\s*bytes\s+(\d+)-(\d+)/(\d+|\*)\s*$""")

        internal fun parseContentRange(header: String?): ContentRange? {
            val m = header?.let { CONTENT_RANGE.matchEntire(it) } ?: return null
            val start = m.groupValues[1].toLongOrNull() ?: return null
            val end = m.groupValues[2].toLongOrNull() ?: return null
            val total = m.groupValues[3].takeIf { it != "*" }?.toLongOrNull()
            if (end < start || (total != null && end >= total)) return null
            return ContentRange(start, end, total)
        }
    }
}
