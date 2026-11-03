package fyi.arlo.gander.core.data.thumbs

import fyi.arlo.gander.core.net.GanderError
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.currentTime
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

@OptIn(ExperimentalCoroutinesApi::class)
class ThumbRetryPolicyTest {

    private fun generating(retryAfterMs: Long) = GanderError.Host.Generating(503, "queued", retryAfterMs)

    private val policy = ThumbRetryPolicy()

    @Test
    fun aThumbnailThatIsReadyOnTheThirdTryIsReturned() = runTest {
        val waits = mutableListOf<Long>()
        var calls = 0
        val result = policy.run(sleep = { waits += it }) { attempt ->
            calls++
            if (attempt < 2) throw generating(retryAfterMs = 400 * (attempt + 1L))
            "bytes"
        }
        assertEquals("bytes", result)
        assertEquals(3, calls)
        assertEquals("waits follow retry_after_ms of each answer", listOf(400L, 800L), waits)
    }

    @Test
    fun afterThreeRetriesTheHostsAnswerIsRethrownSoTheGlyphStays() = runTest {
        var calls = 0
        val waits = mutableListOf<Long>()
        try {
            policy.run(sleep = { waits += it }) {
                calls++
                throw generating(retryAfterMs = 200)
            }
            fail("expected Generating")
        } catch (e: GanderError.Host.Generating) {
            assertEquals(503, e.status)
        }
        assertEquals("1 try + 3 retries (client spec)", 4, calls)
        assertEquals(listOf(200L, 200L, 200L), waits)
    }

    @Test
    fun theWaitIsClampedAndFallsBackWhenTheHostGivesNone() {
        assertEquals(500, policy.delayFor(generating(0)))
        assertEquals("0 ms hint is no hint", 500, policy.delayFor(generating(-1)))
        assertEquals("a tiny hint still waits a moment", 100, policy.delayFor(generating(10)))
        assertEquals(1500, policy.delayFor(generating(1500)))
        // 200 ms x queue position 400: do not park the row for 80 s (the host forgets jobs after 10 s)
        assertEquals(5_000, policy.delayFor(generating(80_000)))
    }

    @Test
    fun tooManyRequestsIsRetriedLikeGenerating() = runTest {
        var calls = 0
        val result = policy.run(sleep = {}) { attempt ->
            calls++
            if (attempt == 0) throw GanderError.Host.TooManyRequests(429, "slow down", 1000)
            "ok"
        }
        assertEquals("ok", result)
        assertEquals(2, calls)
    }

    @Test
    fun answersThatWillNotChangeAreNotRetried() = runTest {
        val refusals = listOf(
            GanderError.Host.UnsupportedType(415, "no thumbnail for this type"),
            GanderError.Host.NotFound(404, "gone"),
            GanderError.Host.BadRequest(400, "size"),
            GanderError.Host.TokenRevoked(401, "revoked"),
            GanderError.Host.Internal(500, "boom"),
        )
        for (refusal in refusals) {
            var calls = 0
            try {
                policy.run(sleep = { fail("must not wait for ${refusal.code}") }) {
                    calls++
                    throw refusal
                }
                fail("expected the refusal")
            } catch (e: GanderError.Host) {
                assertSame(refusal, e)
            }
            assertEquals(1, calls)
        }
    }

    @Test
    fun networkFailuresPropagateAtOnce() = runTest {
        var calls = 0
        try {
            policy.run(sleep = { fail("no wait") }) {
                calls++
                throw GanderError.Unreachable(listOf("100.64.0.1"), null)
            }
            fail("expected Unreachable")
        } catch (e: GanderError.Unreachable) {
            assertEquals(1, calls)
        }
    }

    @Test
    fun theRealWaitIsVirtualTimeUnderRunTest() = runTest {
        var calls = 0
        val job = launch {
            policy.run { attempt ->
                calls++
                if (attempt < 2) throw generating(retryAfterMs = 1_000)
            }
        }
        runCurrent()
        assertEquals(1, calls)
        advanceTimeBy(999)
        runCurrent()
        assertEquals("still inside the first wait", 1, calls)
        advanceTimeBy(1)
        runCurrent()
        assertEquals(2, calls)
        advanceTimeBy(1_000)
        runCurrent()
        assertEquals(3, calls)
        assertTrue(job.isCompleted)
        assertEquals(2_000L, currentTime)
    }

    @Test
    fun cancellingDuringTheWaitStopsRetrying() = runTest {
        var calls = 0
        val job = launch {
            policy.run { _ ->
                calls++
                throw generating(retryAfterMs = 1_000)
            }
        }
        runCurrent()
        assertEquals(1, calls)
        job.cancel()
        advanceTimeBy(60_000)
        runCurrent()
        assertEquals("a row that scrolled away never asks again", 1, calls)
        assertTrue(job.isCancelled)
    }

    @Test
    fun cancellationFromTheAttemptItselfIsNotSwallowed() = runTest {
        var calls = 0
        try {
            policy.run(sleep = {}) {
                calls++
                throw CancellationException("row left the screen")
            }
            fail("expected cancellation")
        } catch (e: CancellationException) {
            assertEquals("row left the screen", e.message)
        }
        assertEquals(1, calls)
    }

    @Test
    fun anInvalidPolicyIsRefused() {
        try {
            ThumbRetryPolicy(maxRetries = -1)
            fail("negative retries")
        } catch (_: IllegalArgumentException) {
        }
        try {
            ThumbRetryPolicy(minDelayMs = 10, maxDelayMs = 5)
            fail("min above max")
        } catch (_: IllegalArgumentException) {
        }
    }
}
