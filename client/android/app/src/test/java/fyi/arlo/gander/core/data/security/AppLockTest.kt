package fyi.arlo.gander.core.data.security

import fyi.arlo.gander.core.data.settings.LockConfig
import org.junit.Assert.assertEquals
import org.junit.Test

/** The app lock's rules: cold start, the background timeout, and turning the lock on and off. */
class AppLockTest {
    private var clock = 1_000L
    private val lock = AppLock { clock }
    private val minute = 60_000L

    private val on = LockConfig(enabled = true, minutes = 1)
    private val off = LockConfig(enabled = false, minutes = 1)

    private fun state() = lock.state.value

    @Test
    fun theStateIsUnknownUntilTheSettingsAreRead() {
        assertEquals(LockState.UNKNOWN, state())
        lock.onForeground() // nothing to decide with yet
        assertEquals(LockState.UNKNOWN, state())
    }

    @Test
    fun aColdStartIsLockedWhenTheLockIsOn() {
        lock.onConfig(on)
        assertEquals(LockState.LOCKED, state())
    }

    @Test
    fun aColdStartIsOpenWhenTheLockIsOff() {
        lock.onConfig(off)
        assertEquals(LockState.UNLOCKED, state())
    }

    @Test
    fun unlockingOpensTheAppUntilItHasBeenAwayLongEnough() {
        lock.onConfig(on)
        lock.unlock()
        assertEquals(LockState.UNLOCKED, state())

        // away for 30 seconds: still open
        lock.onBackground()
        clock += minute / 2
        lock.onForeground()
        assertEquals(LockState.UNLOCKED, state())

        // away for exactly a minute: locked (the timeout is "at least")
        lock.onBackground()
        clock += minute
        lock.onForeground()
        assertEquals(LockState.LOCKED, state())
    }

    @Test
    fun theTimeoutComesFromTheSettings() {
        lock.onConfig(LockConfig(enabled = true, minutes = 5))
        lock.unlock()
        lock.onBackground()
        clock += 4 * minute
        lock.onForeground()
        assertEquals(LockState.UNLOCKED, state())

        lock.onBackground()
        clock += 5 * minute
        lock.onForeground()
        assertEquals(LockState.LOCKED, state())
    }

    @Test
    fun zeroMinutesLocksEveryTimeTheAppLeavesTheScreen() {
        lock.onConfig(LockConfig(enabled = true, minutes = 0))
        lock.unlock()
        lock.onBackground()
        lock.onForeground() // same instant
        assertEquals(LockState.LOCKED, state())
    }

    @Test
    fun comingBackWhileLockedStaysLockedAndTheClockDoesNotRestartTheStay() {
        lock.onConfig(on)
        lock.onBackground() // locked at cold start: nothing to note
        clock += 10 * minute
        lock.onForeground()
        assertEquals(LockState.LOCKED, state())
    }

    @Test
    fun aForegroundWithoutABackgroundBeforeItIsNotAStay() {
        // a configuration change: the activity restarts but never reports leaving
        lock.onConfig(on)
        lock.unlock()
        clock += 10 * minute
        lock.onForeground()
        assertEquals(LockState.UNLOCKED, state())
    }

    @Test
    fun turningTheLockOffUnlocksAtOnce() {
        lock.onConfig(on)
        assertEquals(LockState.LOCKED, state())
        lock.onConfig(off)
        assertEquals(LockState.UNLOCKED, state())
    }

    @Test
    fun turningTheLockOnInsideTheAppKeepsItOpen() {
        lock.onConfig(off)
        lock.onConfig(on)
        assertEquals(LockState.UNLOCKED, state())

        // ... and it locks after the next stay away
        lock.onBackground()
        clock += 2 * minute
        lock.onForeground()
        assertEquals(LockState.LOCKED, state())
    }

    @Test
    fun aStayWhileTheLockWasOffDoesNotLockWhenItIsTurnedOnLater() {
        lock.onConfig(off)
        lock.onBackground()
        clock += 10 * minute
        lock.onForeground() // lock is off: nothing happens, the stay is forgotten
        lock.onConfig(on)
        lock.onForeground()
        assertEquals(LockState.UNLOCKED, state())
    }

    @Test
    fun changingTheTimeoutDoesNotLockAnOpenApp() {
        lock.onConfig(on)
        lock.unlock()
        lock.onConfig(LockConfig(enabled = true, minutes = 15))
        assertEquals(LockState.UNLOCKED, state())
    }

    @Test
    fun theFilesAppProviderSeesTheLockWithoutAnActivity() {
        // A process started by the Files app: no activity told the lock anything yet.
        assertEquals(true, lock.hidesContent(lockEnabled = true))
        assertEquals(false, lock.hidesContent(lockEnabled = false))
    }

    @Test
    fun theFilesAppProviderTreatsALongBackgroundStayAsLocked() {
        lock.onConfig(on)
        lock.unlock()
        assertEquals(false, lock.hidesContent(lockEnabled = true))
        lock.onBackground()
        clock += minute - 1
        assertEquals(false, lock.hidesContent(lockEnabled = true))
        clock += 1
        // The state machine still says UNLOCKED (it only re-locks on the way back to the screen).
        assertEquals(LockState.UNLOCKED, state())
        assertEquals(true, lock.hidesContent(lockEnabled = true))
        lock.onForeground()
        assertEquals(LockState.LOCKED, state())
        assertEquals(true, lock.hidesContent(lockEnabled = true))
    }
}
