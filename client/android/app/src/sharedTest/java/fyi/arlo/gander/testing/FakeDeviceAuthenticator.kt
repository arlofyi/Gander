package fyi.arlo.gander.testing

import fyi.arlo.gander.ui.lock.AuthAvailability
import fyi.arlo.gander.ui.lock.AuthResult
import fyi.arlo.gander.ui.lock.DeviceAuthenticator

/**
 * A [DeviceAuthenticator] that answers as scripted instead of showing the system prompt. With
 * [autoAnswer] null the callback of the last [authenticate] call is kept in [pending] so a test can
 * answer when it likes.
 */
class FakeDeviceAuthenticator(
    var availability: AuthAvailability = AuthAvailability.AVAILABLE,
    var autoAnswer: AuthResult? = AuthResult.Success,
) : DeviceAuthenticator {
    var prompts = 0
        private set
    var lastTitle: String? = null
        private set
    var pending: ((AuthResult) -> Unit)? = null

    override fun availability(): AuthAvailability = availability

    override fun authenticate(title: String, subtitle: String?, onResult: (AuthResult) -> Unit) {
        prompts += 1
        lastTitle = title
        val answer = autoAnswer
        if (answer != null) onResult(answer) else pending = onResult
    }
}
