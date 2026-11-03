package fyi.arlo.gander.core.data

import fyi.arlo.gander.core.net.GanderError

/** Why something could not be shown, in the terms the UI explains (client spec, honesty rules). */
enum class UiErrorKind {
    /** No address of the computer answered (off, asleep, VPN down, wrong network). */
    OFFLINE,

    /** The certificate is not the paired computer's. Never bypassable. */
    WRONG_COMPUTER,

    /** TLS failed for a reason other than the pin. */
    SECURE_CONNECTION,

    /** The token was revoked or is no longer valid: the owner removed this phone. */
    REVOKED,

    /** The share's permissions do not allow this. */
    NO_PERMISSION,

    /** The computer refused this phone's private-network identity. */
    IDENTITY_MISMATCH,

    /** The host row is gone (forgotten while a screen for it was open). */
    NOT_PAIRED,

    /** The share was removed or renamed on the computer. */
    SHARE_GONE,

    /** The folder or file is gone, hidden or not allowed. */
    NOT_FOUND,

    /** The computer is busy (too many requests); try again shortly. */
    BUSY,

    /** The computer answered with something this app version does not understand. */
    INCOMPATIBLE,

    OTHER,
}

/**
 * An error ready for the UI: a [kind] the screen maps to a localised sentence, plus the host's
 * own English [detail] when it has one (never paths or secrets, protocol rule).
 */
data class UiError(val kind: UiErrorKind, val detail: String? = null) {
    /** True when showing a previously cached listing instead is honest (the host is just not there). */
    val isConnectivity: Boolean get() = kind == UiErrorKind.OFFLINE
}

/** Maps a transport or protocol failure to what the user is told. */
fun GanderError.toUiError(): UiError = also { fyi.arlo.gander.core.DebugLog.failure("request", it) }.let { e -> e.toUiErrorInner() }

private fun GanderError.toUiErrorInner(): UiError = when (this) {
    is GanderError.Unreachable, is GanderError.Timeout, is GanderError.Network -> UiError(UiErrorKind.OFFLINE)
    is GanderError.PinMismatch -> UiError(UiErrorKind.WRONG_COMPUTER)
    is GanderError.Tls -> UiError(UiErrorKind.SECURE_CONNECTION)
    is GanderError.MalformedResponse -> UiError(UiErrorKind.INCOMPATIBLE)
    is GanderError.Host.TokenRevoked,
    is GanderError.Host.TokenInvalid,
    is GanderError.Host.TokenMissing,
    -> UiError(UiErrorKind.REVOKED)
    is GanderError.Host.PermissionDenied, is GanderError.Host.Forbidden ->
        UiError(UiErrorKind.NO_PERMISSION, hostMessage.takeIf { it.isNotBlank() })
    is GanderError.Host.NetworkIdentityMismatch -> UiError(UiErrorKind.IDENTITY_MISMATCH)
    is GanderError.Host.ShareNotFound -> UiError(UiErrorKind.SHARE_GONE)
    is GanderError.Host.NotFound, is GanderError.Host.BadPath -> UiError(UiErrorKind.NOT_FOUND)
    is GanderError.Host.TooManyRequests -> UiError(UiErrorKind.BUSY)
    is GanderError.Host -> UiError(UiErrorKind.OTHER, hostMessage.takeIf { it.isNotBlank() })
}
