package fyi.arlo.gander.ui.lock

import androidx.biometric.BiometricManager
import androidx.biometric.BiometricManager.Authenticators.BIOMETRIC_WEAK
import androidx.biometric.BiometricManager.Authenticators.DEVICE_CREDENTIAL
import androidx.biometric.BiometricPrompt
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.core.content.ContextCompat
import androidx.fragment.app.FragmentActivity

/** Can this phone check the user right now? */
enum class AuthAvailability {
    AVAILABLE,

    /** No screen lock or biometric is set up (or the phone has none): the app lock cannot work. */
    NOT_SET_UP,

    /** Temporarily not usable (sensor busy, security update pending): try again. */
    UNAVAILABLE,
}

sealed interface AuthResult {
    data object Success : AuthResult

    /** The user backed out of the prompt. */
    data object Cancelled : AuthResult

    /** The prompt closed without success (too many attempts, hardware error). */
    data object Failed : AuthResult
}

/**
 * Asks the user to prove it is them with a fingerprint or face, or the screen lock PIN, pattern or
 * password as the fallback (`BIOMETRIC_WEAK or DEVICE_CREDENTIAL`, the combination that works from
 * API 26). The app lock and Settings use this; tests use a fake.
 */
interface DeviceAuthenticator {
    fun availability(): AuthAvailability

    /** Shows the system prompt. [onResult] is called once, on the main thread. */
    fun authenticate(title: String, subtitle: String?, onResult: (AuthResult) -> Unit)
}

/** The authenticator for code that has no activity (previews, tests that do not care): never available. */
object NoDeviceAuthenticator : DeviceAuthenticator {
    override fun availability() = AuthAvailability.NOT_SET_UP

    override fun authenticate(title: String, subtitle: String?, onResult: (AuthResult) -> Unit) =
        onResult(AuthResult.Failed)
}

/** `MainActivity` provides the real one; screens read it from here instead of taking an activity. */
val LocalDeviceAuthenticator = staticCompositionLocalOf<DeviceAuthenticator> { NoDeviceAuthenticator }

/**
 * [DeviceAuthenticator] over `BiometricPrompt`. Create it in the activity's `onCreate`, before the
 * activity is started: the prompt registers with the activity's lifecycle so it can re-attach after
 * a rotation, which only works if it exists by then. One prompt object is reused; each
 * [authenticate] only swaps who gets the answer.
 */
class BiometricDeviceAuthenticator(private val activity: FragmentActivity) : DeviceAuthenticator {

    private var pending: ((AuthResult) -> Unit)? = null

    private val prompt = BiometricPrompt(
        activity,
        ContextCompat.getMainExecutor(activity),
        object : BiometricPrompt.AuthenticationCallback() {
            override fun onAuthenticationSucceeded(result: BiometricPrompt.AuthenticationResult) = finish(AuthResult.Success)

            override fun onAuthenticationError(errorCode: Int, errString: CharSequence) = finish(
                when (errorCode) {
                    BiometricPrompt.ERROR_USER_CANCELED,
                    BiometricPrompt.ERROR_NEGATIVE_BUTTON,
                    BiometricPrompt.ERROR_CANCELED,
                    -> AuthResult.Cancelled
                    else -> AuthResult.Failed
                },
            )

            // A wrong finger or face (onAuthenticationFailed) lets the user try again inside the prompt.
        },
    )

    private fun finish(result: AuthResult) {
        val callback = pending ?: return
        pending = null
        callback(result)
    }

    override fun availability(): AuthAvailability = when (BiometricManager.from(activity).canAuthenticate(ALLOWED)) {
        BiometricManager.BIOMETRIC_SUCCESS -> AuthAvailability.AVAILABLE
        BiometricManager.BIOMETRIC_ERROR_NONE_ENROLLED,
        BiometricManager.BIOMETRIC_ERROR_NO_HARDWARE,
        -> AuthAvailability.NOT_SET_UP
        else -> AuthAvailability.UNAVAILABLE
    }

    override fun authenticate(title: String, subtitle: String?, onResult: (AuthResult) -> Unit) {
        pending = onResult
        val info = BiometricPrompt.PromptInfo.Builder()
            .setTitle(title)
            .apply { if (subtitle != null) setSubtitle(subtitle) }
            .setAllowedAuthenticators(ALLOWED)
            .setConfirmationRequired(false)
            .build()
        prompt.authenticate(info)
    }

    private companion object {
        /** No negative button is set: with a device credential allowed the prompt has its own "Use PIN". */
        const val ALLOWED = BIOMETRIC_WEAK or DEVICE_CREDENTIAL
    }
}
