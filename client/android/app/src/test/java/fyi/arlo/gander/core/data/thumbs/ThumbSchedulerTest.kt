package fyi.arlo.gander.core.data.thumbs

import fyi.arlo.gander.core.net.ThumbPriority
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

@OptIn(ExperimentalCoroutinesApi::class)
class ThumbSchedulerTest {

    @Test
    fun atMostTheConfiguredNumberRunAtOnce() = runTest {
        val scheduler = ThumbScheduler(maxConcurrent = 2)
        val gates = List(5) { CompletableDeferred<Unit>() }
        var running = 0
        var peak = 0
        val started = mutableListOf<Int>()
        val jobs = gates.mapIndexed { i, gate ->
            launch {
                scheduler.run(ThumbPriority.VISIBLE) {
                    running++
                    peak = maxOf(peak, running)
                    started += i
                    gate.await()
                    running--
                }
            }
        }
        runCurrent()
        assertEquals(listOf(0, 1), started)
        assertEquals(3, scheduler.queued())
        gates[0].complete(Unit)
        runCurrent()
        assertEquals(listOf(0, 1, 2), started)
        gates.forEach { it.complete(Unit) }
        runCurrent()
        assertEquals(listOf(0, 1, 2, 3, 4), started)
        assertEquals(2, peak)
        assertTrue(jobs.all { it.isCompleted })
        assertEquals(0, scheduler.queued())
    }

    @Test
    fun visibleRowsGoBeforePrefetchEvenIfTheyAskedLater() = runTest {
        val scheduler = ThumbScheduler(maxConcurrent = 1)
        val gate = CompletableDeferred<Unit>()
        val order = mutableListOf<String>()
        launch { scheduler.run(ThumbPriority.VISIBLE) { gate.await() } }
        runCurrent()
        launch { scheduler.run(ThumbPriority.PREFETCH) { order += "prefetch-1" } }
        launch { scheduler.run(ThumbPriority.PREFETCH) { order += "prefetch-2" } }
        launch { scheduler.run(ThumbPriority.VISIBLE) { order += "visible-1" } }
        launch { scheduler.run(ThumbPriority.VISIBLE) { order += "visible-2" } }
        runCurrent()
        assertEquals(4, scheduler.queued())
        gate.complete(Unit)
        runCurrent()
        assertEquals(listOf("visible-1", "visible-2", "prefetch-1", "prefetch-2"), order)
    }

    @Test
    fun aRowThatScrolledAwayLeavesTheQueue() = runTest {
        val scheduler = ThumbScheduler(maxConcurrent = 1)
        val gate = CompletableDeferred<Unit>()
        val ran = mutableListOf<String>()
        launch { scheduler.run(ThumbPriority.VISIBLE) { gate.await() } }
        runCurrent()
        val gone = launch { scheduler.run(ThumbPriority.VISIBLE) { ran += "gone" } }
        launch { scheduler.run(ThumbPriority.VISIBLE) { ran += "stays" } }
        runCurrent()
        assertEquals(2, scheduler.queued())
        gone.cancel()
        runCurrent()
        assertEquals(1, scheduler.queued())
        gate.complete(Unit)
        runCurrent()
        assertEquals(listOf("stays"), ran)
    }

    @Test
    fun cancellingTheRunningRequestFreesItsSlot() = runTest {
        val scheduler = ThumbScheduler(maxConcurrent = 1)
        val ran = mutableListOf<String>()
        val first = launch { scheduler.run(ThumbPriority.VISIBLE) { CompletableDeferred<Unit>().await() } }
        runCurrent()
        launch { scheduler.run(ThumbPriority.VISIBLE) { ran += "next" } }
        runCurrent()
        assertTrue(ran.isEmpty())
        first.cancel()
        runCurrent()
        assertEquals(listOf("next"), ran)
    }

    @Test
    fun aFailingRequestFreesItsSlotToo() = runTest {
        val scheduler = ThumbScheduler(maxConcurrent = 1)
        val ran = mutableListOf<String>()
        val failed = launch {
            try {
                scheduler.run(ThumbPriority.VISIBLE) { error("host said no") }
            } catch (_: IllegalStateException) {
                ran += "failed"
            }
        }
        launch { scheduler.run(ThumbPriority.VISIBLE) { ran += "next" } }
        runCurrent()
        assertTrue(failed.isCompleted)
        assertEquals(listOf("failed", "next"), ran)
    }

    @Test
    fun aSlotHandedToACancelledWaiterPassesOn() = runTest {
        val scheduler = ThumbScheduler(maxConcurrent = 1)
        val gate = CompletableDeferred<Unit>()
        val ran = mutableListOf<String>()
        launch { scheduler.run(ThumbPriority.VISIBLE) { gate.await() } }
        runCurrent()
        val waiter = launch { scheduler.run(ThumbPriority.VISIBLE) { ran += "waiter" } }
        launch { scheduler.run(ThumbPriority.VISIBLE) { ran += "after" } }
        runCurrent()
        // release and cancel in the same instant: the slot is granted to a coroutine that is gone
        gate.complete(Unit)
        waiter.cancel()
        runCurrent()
        assertEquals(listOf("after"), ran)
        assertEquals(0, scheduler.queued())
        // and the scheduler still works afterwards
        launch { scheduler.run(ThumbPriority.PREFETCH) { ran += "later" } }
        runCurrent()
        assertEquals(listOf("after", "later"), ran)
    }
}
