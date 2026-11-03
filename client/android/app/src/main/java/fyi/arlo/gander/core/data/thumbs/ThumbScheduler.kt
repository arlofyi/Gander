package fyi.arlo.gander.core.data.thumbs

import fyi.arlo.gander.core.net.ThumbPriority
import kotlinx.coroutines.CancellableContinuation
import kotlinx.coroutines.suspendCancellableCoroutine
import java.util.ArrayDeque
import kotlin.coroutines.resume

/**
 * Limits how many thumbnail requests one host sees at once and lets rows that are on screen go
 * before prefetched ones. OkHttp's dispatcher is first come first served and shared with list
 * pages and previews (8 in flight per host), so without this a screenful of thumbnails could
 * starve a listing request, and prefetch could queue ahead of the row the user is looking at.
 *
 * A waiter that is cancelled (the row scrolled away) leaves the queue at once; a running one
 * frees its slot when its coroutine is cancelled, because [run] releases in `finally`.
 */
class ThumbScheduler(private val maxConcurrent: Int = DEFAULT_CONCURRENCY) {
    init {
        require(maxConcurrent > 0) { "need at least one slot" }
    }

    private class Waiter(val continuation: CancellableContinuation<Unit>)

    private val lock = Any()
    private var running = 0
    private val visible = ArrayDeque<Waiter>()
    private val prefetch = ArrayDeque<Waiter>()

    /** Runs [block] once a slot is free; [ThumbPriority.VISIBLE] waiters are served first. */
    suspend fun <T> run(priority: ThumbPriority, block: suspend () -> T): T {
        acquire(priority)
        try {
            return block()
        } finally {
            release()
        }
    }

    private suspend fun acquire(priority: ThumbPriority) {
        suspendCancellableCoroutine { continuation ->
            val waiter = Waiter(continuation)
            val granted = synchronized(lock) {
                if (running < maxConcurrent && visible.isEmpty() && prefetch.isEmpty()) {
                    running++
                    true
                } else {
                    (if (priority == ThumbPriority.VISIBLE) visible else prefetch).addLast(waiter)
                    false
                }
            }
            if (granted) {
                continuation.resume(Unit) { _, _, _ -> release() }
            } else {
                continuation.invokeOnCancellation {
                    synchronized(lock) {
                        visible.remove(waiter)
                        prefetch.remove(waiter)
                    }
                }
            }
        }
    }

    private fun release() {
        val next = synchronized(lock) {
            val waiter = visible.pollFirst() ?: prefetch.pollFirst()
            // Hand the slot over without freeing it, so a newcomer cannot jump the queue.
            if (waiter == null) running--
            waiter
        }
        // If the waiter was cancelled in the meantime the callback gives the slot on again.
        next?.continuation?.resume(Unit) { _, _, _ -> release() }
    }

    /** Waiting requests, for tests and diagnostics. */
    fun queued(): Int = synchronized(lock) { visible.size + prefetch.size }

    companion object {
        /**
         * Half of the host's 8 in-flight slots per device; the rest stays free for listings and
         * previews. The host itself renders two thumbnails at a time.
         */
        const val DEFAULT_CONCURRENCY = 4
    }
}
