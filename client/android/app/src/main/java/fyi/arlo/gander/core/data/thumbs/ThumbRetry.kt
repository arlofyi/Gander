package fyi.arlo.gander.core.data.thumbs

import fyi.arlo.gander.core.net.GanderError
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.delay

/**
 * How a thumbnail request answers "not ready yet". Client spec: `503 generating` is retried after
 * `retry_after_ms` up to 3 times, then the type glyph stays. A `429` carries the same kind of
 * hint and is treated the same way.
 *
 * The wait is [retryAfterMs] clamped to [minDelayMs]..[maxDelayMs]. The upper bound keeps a long
 * queue (the host says `200 ms x position`) from parking a row for minutes, and from letting the
 * host's queue forget the job: it drops a waiting job that is not re-requested for 10 s, and a
 * repeated request for a queued job only reports its position again.
 */
data class ThumbRetryPolicy(
    val maxRetries: Int = 3,
    val defaultDelayMs: Long = 500,
    val minDelayMs: Long = 100,
    val maxDelayMs: Long = 5_000,
) {
    init {
        require(maxRetries >= 0 && minDelayMs in 0..maxDelayMs) { "invalid retry policy" }
    }

    fun isRetryable(error: Throwable): Boolean =
        error is GanderError.Host.Generating || error is GanderError.Host.TooManyRequests

    fun delayFor(error: GanderError.Host): Long =
        (if (error.retryAfterMs > 0) error.retryAfterMs else defaultDelayMs).coerceIn(minDelayMs, maxDelayMs)

    /**
     * Runs [block] (one attempt, [attempt] counts from 0), retrying a retryable [GanderError.Host]
     * until [maxRetries] retries were used, then rethrows it. Anything else, and cancellation,
     * propagates at once; a cancelled caller never starts another attempt. [sleep] replaces the
     * real wait (`delay`) in tests.
     */
    suspend fun <T> run(
        sleep: (suspend (millis: Long) -> Unit)? = null,
        block: suspend (attempt: Int) -> T,
    ): T {
        var attempt = 0
        while (true) {
            try {
                return block(attempt)
            } catch (e: CancellationException) {
                throw e
            } catch (e: GanderError.Host) {
                if (!isRetryable(e) || attempt >= maxRetries) throw e
                attempt++
                if (sleep == null) delay(delayFor(e)) else sleep(delayFor(e))
            }
        }
    }
}

