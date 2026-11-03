package fyi.arlo.gander.core.data.security

import fyi.arlo.gander.core.data.settings.LockConfig
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/** Whether the app's content may be shown. [UNKNOWN] lasts from process start until the settings are read. */
enum class LockState { UNKNOWN, LOCKED, UNLOCKED }

/**
 * The app lock's state machine (client spec, "Security on the device": optional biometric lock
 * gating the UI after a while in the background). Pure Kotlin on a monotonic clock, driven from the
 * main thread by `MainActivity`:
 *
 * - **Cold start** (a new process, including one the system killed in the background): locked
 *   when the lock is on, as soon as the settings are known ([onConfig]).
 * - **Background**: [onBackground] notes the moment, [onForeground] locks again when the stay was
 *   at least [LockConfig.timeoutMillis] (0 = every time). A configuration change is not a
 *   background stay; the activity does not report it.
 * - **Turning the lock off** unlocks at once; **turning it on** keeps the app unlocked (the user just
 *   proved they can unlock: Settings asks for the device credential first).
 * - [unlock] is called after a successful biometric or device-credential check.
 */
class AppLock(private val elapsedMillis: () -> Long) {
    private val _state = MutableStateFlow(LockState.UNKNOWN)
    val state: StateFlow<LockState> = _state.asStateFlow()

    // Written on the main thread, read by the Files-app provider on a binder thread.
    @Volatile private var config: LockConfig? = null

    @Volatile private var leftAt: Long? = null

    /** The settings are known or changed. */
    fun onConfig(new: LockConfig) {
        config = new
        when {
            !new.enabled -> {
                leftAt = null
                _state.value = LockState.UNLOCKED
            }
            _state.value == LockState.UNKNOWN -> _state.value = LockState.LOCKED
        }
    }

    /** The app is no longer on screen (`onStop`, not for a configuration change). */
    fun onBackground() {
        if (_state.value == LockState.UNLOCKED && leftAt == null) leftAt = elapsedMillis()
    }

    /** The app is back on screen (`onStart`). */
    fun onForeground() {
        val left = leftAt ?: return
        leftAt = null
        val current = config ?: return
        if (current.enabled && _state.value == LockState.UNLOCKED && elapsedMillis() - left >= current.timeoutMillis) {
            _state.value = LockState.LOCKED
        }
    }

    /**
     * Whether content must stay hidden right now, for callers that are not the UI (the Files-app
     * provider, WP-60): [lockEnabled] is the stored setting, which the provider reads itself because
     * this machine only learns it once an activity started. Locked when the lock is on and the app
     * is not unlocked, or when it is unlocked but has been in the background for the timeout (the
     * state only flips back to [LockState.LOCKED] when the app returns to the screen).
     */
    fun hidesContent(lockEnabled: Boolean): Boolean {
        if (!lockEnabled) return false
        if (_state.value != LockState.UNLOCKED) return true
        val left = leftAt ?: return false
        val timeout = config?.timeoutMillis ?: 0L
        return elapsedMillis() - left >= timeout
    }

    fun unlock() {
        leftAt = null
        _state.value = LockState.UNLOCKED
    }
}
