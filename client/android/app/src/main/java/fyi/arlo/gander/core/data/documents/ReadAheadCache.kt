package fyi.arlo.gander.core.data.documents

import java.io.IOException
import java.util.concurrent.CancellationException
import java.util.concurrent.CompletableFuture
import java.util.concurrent.ExecutionException
import java.util.concurrent.Executor
import java.util.concurrent.TimeUnit
import java.util.concurrent.TimeoutException
import java.util.concurrent.atomic.AtomicLong

/** A blocking source of byte ranges of one file version (the host's `/file` with `Range`). */
interface RangeFetcher {
    /**
     * Bytes `[offset, offset + length)`. [length] never runs past the end of the file, so anything
     * shorter is an error. Gives up after [timeoutMillis] with a [ReadTimeoutException]. Throws
     * [FileChangedException] when the host's file is no longer the version first read.
     */
    fun fetch(offset: Long, length: Int, timeoutMillis: Long): ByteArray

    /** Aborts every fetch in flight and refuses new ones (the fd was cancelled or released). */
    fun cancel()
}

/** A read of a Files-app document failed; the provider turns every one of these into `EIO`. */
open class DocumentReadException(message: String, cause: Throwable? = null) : IOException(message, cause)

/** The file changed on the host while it was open: serving more bytes would mix two versions. */
class FileChangedException(message: String = "the file changed on the computer") : DocumentReadException(message)

/** One read waited longer than its deadline (the platform itself never times a read out). */
class ReadTimeoutException : DocumentReadException("the computer did not answer in time")

/** The fd was cancelled (CancellationSignal) or released while a read was waiting. */
class ReadCancelledException : DocumentReadException("the read was cancelled")

/**
 * Sizes of [ReadAheadCache] (WP-60, from the Files-app spike, `2026-11-01-files-app-spike-findings.md`):
 * the first read after a jump fetches exactly what was asked, a stream that continues grows from
 * [firstWindow] to [maxWindow], at most [maxStreams] streams are kept and all blocks together
 * (prefetches included) stay under [memoryBudget].
 */
data class ReadAheadConfig(
    val firstWindow: Int = 64 * KIB,
    val maxWindow: Int = MIB,
    val maxStreams: Int = 4,
    val memoryBudget: Long = 4L * MIB,
    /**
     * A read starting at most this far past a stream's end continues that stream: players skip the
     * other track's chunk (forward gaps of 8 to 36 KiB measured), and the gap is fetched with it.
     */
    val joinDistance: Long = 64L * KIB,
    /** Per read, including waiting for a prefetch: the platform never gives up on its own (75 s measured). */
    val readTimeoutMillis: Long = 20_000,
) {
    init {
        require(firstWindow in 1..maxWindow) { "firstWindow must be in 1..maxWindow" }
        require(maxStreams >= 1) { "at least one stream" }
        require(memoryBudget >= 2L * maxWindow) { "the budget must hold a block and its prefetch" }
    }

    companion object {
        const val KIB = 1024
        const val MIB = 1024 * 1024
    }
}

/** What one fd did; logged (counts only) when it is released and read by the instrumented test. */
data class ReadStats(
    val reads: Long,
    val bytesServed: Long,
    val requests: Long,
    val bytesFetched: Long,
    val prefetches: Long,
    val prefetchHits: Long,
    val jumps: Long,
    val maxResidentBytes: Long,
) {
    /** Bytes fetched per byte served (1.0 is perfect; prefetch past the last read makes it larger). */
    val overFetch: Double get() = if (bytesServed == 0L) 0.0 else bytesFetched.toDouble() / bytesServed
}

/**
 * The block cache in front of one open document (one proxy fd). Not thread safe: [read] runs on
 * the fd's own handler thread, one call at a time (measured: never more than one in flight per fd).
 * Prefetches run on [prefetchExecutor]; [cancel] may be called from any thread.
 *
 * The model is a small set of *streams*, one per reader position (a player's audio and video
 * demuxers sit about 2 MiB apart; a seek starts a new one):
 *
 * - A read inside a stream's block is copied from memory. Once the reader is past half of a block
 *   of a stream that has continued at least twice, the next block is prefetched (size doubling up
 *   to [ReadAheadConfig.maxWindow]) so the callback thread rarely waits on the network.
 * - A read that starts at or shortly after a stream's end ([ReadAheadConfig.joinDistance])
 *   continues it: one request from the stream's end, at least the stream's next window size.
 * - Anything else is a jump: exactly the requested range is fetched (a seek's first read is 4 to
 *   28 KiB at an unrelated offset; a full block there would be wasted), as a new stream; the least
 *   recently used stream goes when there are too many or the memory budget would be exceeded.
 * - Prefetch starts once a stream has been continued twice, so a stray random read never pulls a
 *   window it does not use.
 *
 * Every failure is a [DocumentReadException]. A [FileChangedException] poisons the cache: every
 * later read fails too, so a reader never gets bytes of two versions.
 */
class ReadAheadCache(
    private val fileSize: Long,
    private val fetcher: RangeFetcher,
    private val prefetchExecutor: Executor,
    private val config: ReadAheadConfig = ReadAheadConfig(),
    private val nanoTime: () -> Long = System::nanoTime,
) {
    private class Ahead(val start: Long, val size: Int, val future: CompletableFuture<ByteArray>) {
        val end: Long get() = start + size
    }

    private class Stream(var start: Long, var data: ByteArray, var nextSize: Int, var lastUse: Long) {
        val end: Long get() = start + data.size

        /** Times the stream was continued; from [PREFETCH_AFTER] on it is worth prefetching for. */
        var continuations = 0
        var ahead: Ahead? = null

        fun contains(pos: Long) = pos >= start && pos < end
    }

    private val streams = ArrayList<Stream>()
    private var tick = 0L

    @Volatile private var failure: DocumentReadException? = null

    private var reads = 0L
    private var bytesServed = 0L
    private var jumps = 0L
    private var prefetchHits = 0L
    private var maxResident = 0L
    private val requests = AtomicLong()
    private val bytesFetched = AtomicLong()
    private val prefetches = AtomicLong()

    init {
        require(fileSize >= 0) { "negative file size" }
    }

    /**
     * Fills `out[0 until n]` with the bytes at [offset] and returns `n`: [size] clamped to the end
     * of the file (0 at or past the end).
     */
    fun read(offset: Long, size: Int, out: ByteArray): Int {
        failure?.let { throw it }
        if (offset < 0 || size < 0 || size > out.size) throw DocumentReadException("bad read request")
        if (offset >= fileSize || size == 0) return 0
        val n = minOf(size.toLong(), fileSize - offset).toInt()
        val deadline = nanoTime() + TimeUnit.MILLISECONDS.toNanos(config.readTimeoutMillis)
        try {
            var done = 0
            while (done < n) done += serve(offset + done, n - done, out, done, deadline)
        } catch (e: FileChangedException) {
            failure = e
            throw e
        }
        reads += 1
        bytesServed += n
        return n
    }

    /** Stops everything: in-flight fetches are aborted and every later read fails. Any thread. */
    fun cancel() {
        if (failure == null) failure = ReadCancelledException()
        fetcher.cancel()
    }

    /** [cancel] plus dropping the blocks; call from the read thread when the fd is released. */
    fun close() {
        cancel()
        streams.clear()
    }

    fun stats(): ReadStats = ReadStats(
        reads = reads,
        bytesServed = bytesServed,
        requests = requests.get(),
        bytesFetched = bytesFetched.get(),
        prefetches = prefetches.get(),
        prefetchHits = prefetchHits,
        jumps = jumps,
        maxResidentBytes = maxResident,
    )

    // One step of a read: serves what one block can, returns how many bytes that was (>= 1).
    private fun serve(pos: Long, need: Int, out: ByteArray, outOff: Int, deadline: Long): Int {
        streams.firstOrNull { it.contains(pos) }?.let { return copy(it, pos, need, out, outOff) }

        // A prefetched block that covers the position: wait for it and make it the stream's block.
        streams.firstOrNull { s -> s.ahead.let { it != null && pos >= it.start && pos < it.end } }?.let { s ->
            val ahead = s.ahead!!
            s.ahead = null
            val bytes = await(ahead, deadline)
            if (bytes != null) {
                s.start = ahead.start
                s.data = bytes
                prefetchHits += 1
                noteResident()
                return copy(s, pos, need, out, outOff)
            }
            // The prefetch failed for a reason worth one more try: fall through to a fetch now.
        }

        // Continuing a stream: from its end (filling a small gap) through the read, at least a window.
        val near = streams.filter { pos >= it.end && pos - it.end <= config.joinDistance }.maxByOrNull { it.end }
        if (near != null) {
            near.ahead = null
            val from = near.end
            val size = maxOf(near.nextSize.toLong(), pos + need - from).coerceAtMost(fileSize - from).toInt()
            near.nextSize = minOf(near.nextSize * 2, config.maxWindow)
            near.continuations += 1
            makeRoom(size.toLong(), keep = near, freeing = near.data.size.toLong())
            near.data = fetchNow(from, size, deadline)
            near.start = from
            noteResident()
            return copy(near, pos, need, out, outOff)
        }

        // A jump: exactly the requested range, as a new stream.
        jumps += 1
        if (streams.size >= config.maxStreams) evictLru(keep = null)
        makeRoom(need.toLong(), keep = null, freeing = 0)
        val stream = Stream(pos, fetchNow(pos, need, deadline), config.firstWindow, ++tick)
        streams += stream
        noteResident()
        return copy(stream, pos, need, out, outOff)
    }

    private fun copy(s: Stream, pos: Long, need: Int, out: ByteArray, outOff: Int): Int {
        val count = minOf(need.toLong(), s.end - pos).toInt()
        System.arraycopy(s.data, (pos - s.start).toInt(), out, outOff, count)
        s.lastUse = ++tick
        maybePrefetch(s, pos + count)
        return count
    }

    private fun maybePrefetch(s: Stream, readEnd: Long) {
        if (s.continuations < PREFETCH_AFTER || s.ahead != null || s.end >= fileSize) return
        if (readEnd - s.start < s.data.size / 2) return
        val size = minOf(s.nextSize.toLong(), fileSize - s.end).toInt()
        if (!makeRoom(size.toLong(), keep = s, freeing = 0)) return
        val start = s.end
        prefetches.incrementAndGet()
        requests.incrementAndGet()
        bytesFetched.addAndGet(size.toLong())
        val future = CompletableFuture.supplyAsync(
            { checked(fetcher.fetch(start, size, config.readTimeoutMillis), size) },
            prefetchExecutor,
        )
        s.ahead = Ahead(start, size, future)
        s.nextSize = minOf(s.nextSize * 2, config.maxWindow)
        noteResident()
    }

    /** The prefetched bytes, or null when the prefetch failed in a way a direct fetch may fix. */
    private fun await(ahead: Ahead, deadline: Long): ByteArray? {
        val remaining = deadline - nanoTime()
        if (remaining <= 0) throw ReadTimeoutException()
        return try {
            ahead.future.get(remaining, TimeUnit.NANOSECONDS)
        } catch (_: TimeoutException) {
            throw ReadTimeoutException()
        } catch (e: ExecutionException) {
            when (val cause = e.cause) {
                is FileChangedException -> throw cause
                is ReadCancelledException -> throw cause
                else -> {
                    failure?.let { throw it }
                    null
                }
            }
        } catch (_: CancellationException) {
            failure?.let { throw it }
            null
        } catch (_: InterruptedException) {
            Thread.currentThread().interrupt()
            throw ReadCancelledException()
        }
    }

    private fun fetchNow(start: Long, size: Int, deadline: Long): ByteArray {
        failure?.let { throw it }
        val remainingMs = TimeUnit.NANOSECONDS.toMillis(deadline - nanoTime())
        if (remainingMs <= 0) throw ReadTimeoutException()
        requests.incrementAndGet()
        bytesFetched.addAndGet(size.toLong())
        return checked(fetcher.fetch(start, size, remainingMs), size)
    }

    private fun checked(bytes: ByteArray, size: Int): ByteArray {
        if (bytes.size != size) throw DocumentReadException("the computer sent ${bytes.size} of $size bytes")
        return bytes
    }

    private fun resident(): Long = streams.sumOf { it.data.size.toLong() + (it.ahead?.size ?: 0) }

    private fun noteResident() {
        maxResident = maxOf(maxResident, resident())
    }

    /**
     * Evicts least recently used streams (never [keep]) until [extra] more bytes fit in the budget,
     * counting [freeing] bytes that are about to be replaced. False when they cannot fit.
     */
    private fun makeRoom(extra: Long, keep: Stream?, freeing: Long): Boolean {
        while (resident() - freeing + extra > config.memoryBudget) {
            if (!evictLru(keep)) return false
        }
        return true
    }

    private fun evictLru(keep: Stream?): Boolean {
        val victim = streams.filter { it !== keep }.minByOrNull { it.lastUse } ?: return false
        victim.ahead = null // a prefetch in flight finishes on its own and is dropped
        streams.remove(victim)
        return true
    }

    private companion object {
        /**
         * Continuations before a stream prefetches. One can be chance (a random pread that lands just
         * past an earlier one looks like a player's forward gap); two in a row is a reader moving on.
         */
        const val PREFETCH_AFTER = 2
    }
}
