package fyi.arlo.gander.core.data.preview

import fyi.arlo.gander.core.net.GanderError
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.delay

/**
 * How a preview request answers "not ready yet". Thumbnails are retried three times (client spec),
 * but a preview can take much longer: the host renders one page at a time per device, and a cold
 * Office render is "10-20 s" (preview-renderers spec, `estimated_ms`). So the retries of a preview
 * are bounded by a time budget, not a count: `503 generating` (and `429`) is retried after
 * `retry_after_ms` until [budgetMs] has passed since the first attempt, then the error surfaces
 * and the viewer offers Retry.
 *
 * The wait is `retry_after_ms` clamped to [minDelayMs]..[maxDelayMs]. The upper bound keeps the
 * request alive on the host, which drops a queued job that is not asked for again for a while.
 */
data class PreviewRetryPolicy(
    val budgetMs: Long = 90_000,
    val defaultDelayMs: Long = 700,
    val minDelayMs: Long = 250,
    val maxDelayMs: Long = 3_000,
) {
    init {
        require(budgetMs >= 0 && minDelayMs in 0..maxDelayMs) { "invalid retry policy" }
    }

    fun isRetryable(error: Throwable): Boolean =
        error is GanderError.Host.Generating || error is GanderError.Host.TooManyRequests

    fun delayFor(error: GanderError.Host): Long =
        (if (error.retryAfterMs > 0) error.retryAfterMs else defaultDelayMs).coerceIn(minDelayMs, maxDelayMs)

    /**
     * Runs [block] (one attempt), retrying a retryable [GanderError.Host] while the wait would
     * still end inside the budget, then rethrows it. Anything else, and cancellation, propagates
     * at once. [onWait] is told before every wait (the UI can say "still rendering"); [sleep] and
     * [now] (milliseconds, monotonic) replace the real clock in tests.
     */
    suspend fun <T> run(
        sleep: (suspend (millis: Long) -> Unit)? = null,
        now: () -> Long = ::monotonicMillis,
        onWait: ((error: GanderError.Host, delayMs: Long) -> Unit)? = null,
        block: suspend () -> T,
    ): T {
        val start = now()
        while (true) {
            try {
                return block()
            } catch (e: CancellationException) {
                throw e
            } catch (e: GanderError.Host) {
                if (!isRetryable(e)) throw e
                val wait = delayFor(e)
                if (now() - start + wait > budgetMs) throw e
                onWait?.invoke(e, wait)
                if (sleep == null) delay(wait) else sleep(wait)
            }
        }
    }

    private companion object {
        fun monotonicMillis(): Long = System.nanoTime() / 1_000_000
    }
}
