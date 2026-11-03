package fyi.arlo.gander.core.data.preview

import fyi.arlo.gander.core.net.GanderError
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

class PreviewRetryPolicyTest {
    private fun generating(retryMs: Long) = GanderError.Host.Generating(503, "queued", retryMs)

    /** A clock that only moves when the policy "sleeps". */
    private class FakeClock {
        var millis = 0L
        val waits = mutableListOf<Long>()
        val sleep: suspend (Long) -> Unit = {
            waits += it
            millis += it
        }
        val now: () -> Long = { millis }
    }

    @Test
    fun theWaitFollowsTheHostsHintWithinBounds() {
        val policy = PreviewRetryPolicy()
        assertEquals(1_500, policy.delayFor(generating(1_500)))
        assertEquals("too eager", 250, policy.delayFor(generating(10)))
        assertEquals("too patient", 3_000, policy.delayFor(generating(60_000)))
        assertEquals("no hint", 700, policy.delayFor(generating(0)))
        assertEquals("429 carries a hint too", 2_000, policy.delayFor(GanderError.Host.TooManyRequests(429, "", 2_000)))
    }

    @Test
    fun onlyNotReadyAndBusyAreRetryable() {
        val policy = PreviewRetryPolicy()
        assertTrue(policy.isRetryable(generating(1)))
        assertTrue(policy.isRetryable(GanderError.Host.TooManyRequests(429, "", 1)))
        assertFalse(policy.isRetryable(GanderError.Host.UnsupportedType(415, "")))
        assertFalse(policy.isRetryable(GanderError.Host.Internal(500, "")))
        assertFalse(policy.isRetryable(GanderError.Unreachable(listOf("a"), null)))
        assertFalse(policy.isRetryable(IllegalStateException()))
    }

    @Test
    fun aPageThatTakesAWhileIsWaitedForUntilItIsReady() = runBlocking {
        val clock = FakeClock()
        var calls = 0
        // a cold Office render: five answers of "still working" before the page
        val result = PreviewRetryPolicy().run(clock.sleep, clock.now) {
            if (++calls <= 5) throw generating(2_000)
            "page"
        }
        assertEquals("page", result)
        assertEquals(6, calls)
        assertEquals(List(5) { 2_000L }, clock.waits)
    }

    @Test
    fun theBudgetEndsTheWaitingNotACount() = runBlocking {
        val clock = FakeClock()
        var calls = 0
        val policy = PreviewRetryPolicy(budgetMs = 2_000)
        try {
            policy.run(clock.sleep, clock.now) {
                calls++
                throw generating(500)
            }
            fail("expected Generating")
        } catch (e: GanderError.Host.Generating) {
            assertEquals(503, e.status)
        }
        // waits at 0, 500, 1000, 1500 ms fit; the one that would end at 2500 does not
        assertEquals(listOf(500L, 500L, 500L, 500L), clock.waits)
        assertEquals(5, calls)
    }

    @Test
    fun timeSpentInTheRequestsCountsAgainstTheBudget() = runBlocking {
        val clock = FakeClock()
        var calls = 0
        val policy = PreviewRetryPolicy(budgetMs = 5_000)
        try {
            policy.run(clock.sleep, clock.now) {
                calls++
                clock.millis += 1_500 // the host held the request for its 1.5 s wait
                throw generating(1_000)
            }
            fail("expected Generating")
        } catch (_: GanderError.Host.Generating) {
        }
        // attempt 1 ends at 1.5 s (wait to 2.5), attempt 2 ends at 4 s (wait would end at 5.0, not past
        // the budget: waits), attempt 3 ends at 6.5 s and its wait is past the budget
        assertEquals(3, calls)
        assertEquals(listOf(1_000L, 1_000L), clock.waits)
    }

    @Test
    fun otherErrorsAndCancellationPropagateAtOnce() = runBlocking {
        val clock = FakeClock()
        val policy = PreviewRetryPolicy()
        var calls = 0
        val unsupported = runCatching {
            policy.run(clock.sleep, clock.now) {
                calls++
                throw GanderError.Host.UnsupportedType(415, "")
            }
        }.exceptionOrNull()
        assertTrue(unsupported is GanderError.Host.UnsupportedType)
        assertEquals(1, calls)

        val cancelled = runCatching {
            policy.run(clock.sleep, clock.now) { throw CancellationException("scrolled away") }
        }.exceptionOrNull()
        assertTrue(cancelled is CancellationException)
        assertTrue(clock.waits.isEmpty())
    }

    @Test
    fun theCallerIsToldBeforeEveryWait() = runBlocking {
        val clock = FakeClock()
        val told = mutableListOf<Long>()
        var calls = 0
        PreviewRetryPolicy().run(clock.sleep, clock.now, onWait = { _, delay -> told += delay }) {
            if (++calls < 3) throw generating(800)
        }
        assertEquals(listOf(800L, 800L), told)
        assertEquals(told, clock.waits)
    }
}
