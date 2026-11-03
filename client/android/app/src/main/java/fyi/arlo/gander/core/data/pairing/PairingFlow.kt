package fyi.arlo.gander.core.data.pairing

import fyi.arlo.gander.core.data.hosts.PairedHost

/** What the waiting screen shows once the computer has the request. */
data class WaitingForApproval(
    /** The computer's name (flow A) or the address that was typed (flow B). */
    val hostLabel: String,
    /** Derived on the phone from the certificate it saw; never the string the computer sent. */
    val sas: SasCode,
    val method: PairingMethod,
)

/**
 * Pairs this phone with a computer. The ViewModel talks to this interface; [PairingService] is the
 * real implementation and tests script it.
 */
interface PairingFlow {
    /**
     * Runs the whole flow: connect, verify the identity, send the pairing request, call
     * [onWaiting] with the code to show, poll until the owner decides, then store the host and its
     * token. Returns the stored host. Throws [PairingFailure] for every expected failure;
     * cancelling the coroutine abandons the attempt (the computer times the request out itself).
     */
    suspend fun pair(target: PairingTarget, onWaiting: (WaitingForApproval) -> Unit): PairedHost
}

/**
 * An expected way for pairing to end badly. The message never contains the QR secret, a token or
 * the scanned URI; [detail] is the computer's own English text when it sent one (never paths or
 * secrets, protocol rule).
 */
class PairingFailure(
    val kind: Kind,
    val detail: String? = null,
    val retryAfterMs: Long = 0,
    cause: Throwable? = null,
) : Exception("pairing failed: ${kind.name.lowercase()}", cause) {

    enum class Kind {
        /** The "Add a device" window is not open (404 `pairing_closed`). */
        CLOSED,

        /** Five failed attempts in this window (429). */
        RATE_LIMITED,

        /** Another device is waiting for approval (409). */
        IN_PROGRESS,

        /** The computer refused the code: wrong or expired secret, or a peer it does not admit (403). */
        CODE_REJECTED,

        /** The certificate is not the one the code or `GET /v1/host` named. Never bypassable. */
        WRONG_COMPUTER,

        /** The code the phone derived is not the one the computer reported: someone is in the middle, or a bug. */
        SAS_MISMATCH,

        /** The owner chose Deny. */
        DENIED,

        /** The request timed out, was cancelled or was already collected (410, or no answer in time). */
        EXPIRED,

        /** No address answered, or the connection kept failing. */
        UNREACHABLE,

        /** TLS failed for a reason other than the pin. */
        SECURE_CONNECTION,

        /** The computer answered in a way this app version does not understand. */
        INCOMPATIBLE,

        /** The owner approved, but the host and token could not be saved on this phone. */
        STORAGE,

        /**
         * A bug in this app, not anything the computer or the network did: an exception nobody
         * expected. [detail] carries only the exception's class name, which helps a bug report and
         * never contains addresses, codes or tokens.
         */
        APP_ERROR,

        OTHER,
    }

    /**
     * True if trying the same target again can work without scanning or typing anything: the
     * computer simply was not reachable.
     */
    val canRetrySameTarget: Boolean get() = kind == Kind.UNREACHABLE

    companion object {
        /**
         * Maps an exception that is neither a [PairingFailure] nor a cancellation to the closest
         * kind, so the screen never falls back to a bare "Pairing failed.". The exception's
         * message is dropped (it may carry a URL with the code in it); only its class name is kept.
         */
        fun fromUnexpected(e: Throwable): PairingFailure = when (e) {
            is PairingFailure -> e
            is java.io.IOException -> PairingFailure(Kind.UNREACHABLE, cause = e)
            is kotlinx.serialization.SerializationException -> PairingFailure(Kind.INCOMPATIBLE, cause = e)
            is java.security.GeneralSecurityException -> PairingFailure(Kind.STORAGE, cause = e)
            else -> PairingFailure(Kind.APP_ERROR, detail = e.javaClass.simpleName.ifBlank { null }, cause = e)
        }
    }
}
