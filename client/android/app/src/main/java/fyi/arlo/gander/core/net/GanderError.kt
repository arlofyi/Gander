package fyi.arlo.gander.core.net

import fyi.arlo.gander.core.net.protocol.ErrorBody
import fyi.arlo.gander.core.net.protocol.ErrorCode
import kotlinx.serialization.SerializationException
import java.io.IOException
import java.io.InterruptedIOException
import javax.net.ssl.SSLException

/**
 * Everything that can go wrong talking to a host, as the UI needs it: one subclass per protocol
 * error code (so a `when` over [Host] is exhaustive and every refusal can be explained in the
 * sheet, client spec "Honesty rules") plus the transport failures.
 *
 * Messages never contain tokens, secrets or file contents. [Host.hostMessage] is the host's
 * English text, which the protocol guarantees is free of paths and secrets; screens should show
 * a localised string chosen by type and use it only as detail.
 */
sealed class GanderError(message: String, cause: Throwable? = null) : Exception(message, cause) {

    /** The host answered with a protocol error. */
    sealed class Host(
        val status: Int,
        val code: ErrorCode,
        val hostMessage: String,
        val retryAfterMs: Long,
    ) : GanderError("host refused: ${code.wire.ifEmpty { "unknown" }} ($status)") {
        // 400
        class BadRequest(s: Int, m: String) : Host(s, ErrorCode.BAD_REQUEST, m, 0)
        class BadHost(s: Int, m: String) : Host(s, ErrorCode.BAD_HOST, m, 0)
        class BadPath(s: Int, m: String) : Host(s, ErrorCode.BAD_PATH, m, 0)

        // 401: the token is no good. TokenRevoked: forget the host after telling the user why.
        class TokenMissing(s: Int, m: String) : Host(s, ErrorCode.TOKEN_MISSING, m, 0)
        class TokenInvalid(s: Int, m: String) : Host(s, ErrorCode.TOKEN_INVALID, m, 0)
        class TokenRevoked(s: Int, m: String) : Host(s, ErrorCode.TOKEN_REVOKED, m, 0)

        // 403
        class Forbidden(s: Int, m: String) : Host(s, ErrorCode.FORBIDDEN, m, 0)
        class BrowserRequest(s: Int, m: String) : Host(s, ErrorCode.BROWSER_REQUEST, m, 0)
        class NetworkIdentityMismatch(s: Int, m: String) : Host(s, ErrorCode.NETWORK_IDENTITY_MISMATCH, m, 0)

        /** [hostMessage] names the missing permission. */
        class PermissionDenied(s: Int, m: String) : Host(s, ErrorCode.PERMISSION_DENIED, m, 0)

        // 404
        class ShareNotFound(s: Int, m: String) : Host(s, ErrorCode.SHARE_NOT_FOUND, m, 0)
        class NotFound(s: Int, m: String) : Host(s, ErrorCode.NOT_FOUND, m, 0)
        class PairingClosed(s: Int, m: String) : Host(s, ErrorCode.PAIRING_CLOSED, m, 0)

        // 409, 410
        class StreamBusy(s: Int, m: String) : Host(s, ErrorCode.STREAM_BUSY, m, 0)
        class PairingInProgress(s: Int, m: String) : Host(s, ErrorCode.PAIRING_IN_PROGRESS, m, 0)
        class PairingExpired(s: Int, m: String) : Host(s, ErrorCode.PAIRING_EXPIRED, m, 0)

        // 412: stream preconditions and resumed downloads
        class HostLocked(s: Int, m: String) : Host(s, ErrorCode.HOST_LOCKED, m, 0)
        class HostUnattended(s: Int, m: String) : Host(s, ErrorCode.HOST_UNATTENDED, m, 0)
        class AppUnavailable(s: Int, m: String) : Host(s, ErrorCode.APP_UNAVAILABLE, m, 0)

        /** A resumed download's file changed on the host: restart from zero after telling the user. */
        class FileChanged(s: Int, m: String) : Host(s, ErrorCode.FILE_CHANGED, m, 0)

        // 413, 415, 416, 422
        class TooLarge(s: Int, m: String) : Host(s, ErrorCode.TOO_LARGE, m, 0)
        class UnsupportedType(s: Int, m: String) : Host(s, ErrorCode.UNSUPPORTED_TYPE, m, 0)
        class RangeNotSatisfiable(s: Int, m: String) : Host(s, ErrorCode.RANGE_NOT_SATISFIABLE, m, 0)

        /** [hostMessage] says why in user terms. */
        class StreamRefused(s: Int, m: String) : Host(s, ErrorCode.STREAM_REFUSED, m, 0)

        // 429, 500, 503
        class TooManyRequests(s: Int, m: String, retry: Long) : Host(s, ErrorCode.TOO_MANY_REQUESTS, m, retry)
        class PairingRateLimited(s: Int, m: String, retry: Long) : Host(s, ErrorCode.PAIRING_RATE_LIMITED, m, retry)
        class Internal(s: Int, m: String) : Host(s, ErrorCode.INTERNAL, m, 0)

        /** Thumbnail, preview or hash not ready; retry after [retryAfterMs]. */
        class Generating(s: Int, m: String, retry: Long) : Host(s, ErrorCode.GENERATING, m, retry)

        /** `POST /stream` before stream mode exists on the host (`capabilities.stream.enabled` is false). */
        class StreamUnavailable(s: Int, m: String) : Host(s, ErrorCode.STREAM_UNAVAILABLE, m, 0)

        /**
         * An error status whose body is not a protocol error, or carries a code this client does
         * not know (a newer host). [rawCode] is null when the body was not an `ErrorBody`.
         */
        class Unrecognised(s: Int, val rawCode: String?, m: String, retry: Long) :
            Host(s, ErrorCode.UNKNOWN, m, retry)
    }

    /** No address of the host accepted a connection (offline, VPN down, wrong network). */
    class Unreachable(val tried: List<String>, cause: Throwable?) :
        GanderError("host unreachable (${tried.size} address(es) tried)", cause)

    /**
     * The certificate is not the paired host's. Never retried and never bypassable: "This is not
     * the computer you paired with". [actual] is the pin that was presented, for diagnostics.
     */
    class PinMismatch(val expected: SpkiPin, val actual: SpkiPin?, cause: Throwable?) :
        GanderError("host certificate does not match the paired host", cause)

    /** TLS failed for another reason (protocol version, handshake abort). */
    class Tls(cause: Throwable) : GanderError("TLS handshake failed", cause)

    /** Connected, but the host did not answer in time. */
    class Timeout(cause: Throwable) : GanderError("host did not answer in time", cause)

    /** Any other I/O failure mid-request (connection reset, stream closed). */
    class Network(cause: Throwable) : GanderError("connection to host failed", cause)

    /** A success response whose body does not match the protocol. */
    class MalformedResponse(cause: Throwable) : GanderError("host response does not match protocol v1", cause)

    companion object {
        /**
         * Maps an error response. [body] is the raw response text (may be empty); [retryAfterHeader]
         * is the `Retry-After` header in seconds, used when the body has no `retry_after_ms`.
         */
        fun fromResponse(status: Int, body: String?, retryAfterHeader: String? = null): Host {
            val parsed = body?.takeIf { it.isNotBlank() }?.let {
                try {
                    GanderJson.json.decodeFromString(ErrorBody.serializer(), it)
                } catch (_: SerializationException) {
                    null
                } catch (_: IllegalArgumentException) {
                    null
                }
            }
            val headerRetry = retryAfterHeader?.trim()?.toLongOrNull()?.let { it * 1000 } ?: 0
            if (parsed == null) return Host.Unrecognised(status, null, "", headerRetry)
            val d = parsed.error
            val retry = if (d.retryAfterMs > 0) d.retryAfterMs else headerRetry
            val m = d.message
            return when (d.code) {
                ErrorCode.BAD_REQUEST -> Host.BadRequest(status, m)
                ErrorCode.BAD_HOST -> Host.BadHost(status, m)
                ErrorCode.BAD_PATH -> Host.BadPath(status, m)
                ErrorCode.TOKEN_MISSING -> Host.TokenMissing(status, m)
                ErrorCode.TOKEN_INVALID -> Host.TokenInvalid(status, m)
                ErrorCode.TOKEN_REVOKED -> Host.TokenRevoked(status, m)
                ErrorCode.FORBIDDEN -> Host.Forbidden(status, m)
                ErrorCode.BROWSER_REQUEST -> Host.BrowserRequest(status, m)
                ErrorCode.NETWORK_IDENTITY_MISMATCH -> Host.NetworkIdentityMismatch(status, m)
                ErrorCode.PERMISSION_DENIED -> Host.PermissionDenied(status, m)
                ErrorCode.SHARE_NOT_FOUND -> Host.ShareNotFound(status, m)
                ErrorCode.NOT_FOUND -> Host.NotFound(status, m)
                ErrorCode.PAIRING_CLOSED -> Host.PairingClosed(status, m)
                ErrorCode.STREAM_BUSY -> Host.StreamBusy(status, m)
                ErrorCode.PAIRING_IN_PROGRESS -> Host.PairingInProgress(status, m)
                ErrorCode.PAIRING_EXPIRED -> Host.PairingExpired(status, m)
                ErrorCode.HOST_LOCKED -> Host.HostLocked(status, m)
                ErrorCode.HOST_UNATTENDED -> Host.HostUnattended(status, m)
                ErrorCode.APP_UNAVAILABLE -> Host.AppUnavailable(status, m)
                ErrorCode.FILE_CHANGED -> Host.FileChanged(status, m)
                ErrorCode.TOO_LARGE -> Host.TooLarge(status, m)
                ErrorCode.UNSUPPORTED_TYPE -> Host.UnsupportedType(status, m)
                ErrorCode.RANGE_NOT_SATISFIABLE -> Host.RangeNotSatisfiable(status, m)
                ErrorCode.STREAM_REFUSED -> Host.StreamRefused(status, m)
                ErrorCode.TOO_MANY_REQUESTS -> Host.TooManyRequests(status, m, retry)
                ErrorCode.PAIRING_RATE_LIMITED -> Host.PairingRateLimited(status, m, retry)
                ErrorCode.INTERNAL -> Host.Internal(status, m)
                ErrorCode.GENERATING -> Host.Generating(status, m, retry)
                ErrorCode.STREAM_UNAVAILABLE -> Host.StreamUnavailable(status, m)
                ErrorCode.UNKNOWN -> Host.Unrecognised(status, rawCode(body), m, retry)
            }
        }

        /** Maps an exception from an OkHttp call. A [GanderError] passes through unchanged. */
        fun fromThrowable(t: Throwable): GanderError {
            if (t is GanderError) return t
            causeChain(t).filterIsInstance<SpkiPinMismatchException>().firstOrNull()?.let {
                return PinMismatch(it.expected, it.actual, t)
            }
            return when (t) {
                is HostUnreachableException -> Unreachable(t.tried, t)
                is SSLException -> Tls(t)
                is InterruptedIOException -> Timeout(t)
                is SerializationException, is IllegalArgumentException -> MalformedResponse(t)
                is IOException -> Network(t)
                else -> Network(t)
            }
        }

        private fun causeChain(t: Throwable): Sequence<Throwable> =
            generateSequence(t) { it.cause?.takeIf { c -> c !== it } }.take(MAX_CAUSE_DEPTH)

        private fun rawCode(body: String?): String? = body?.let {
            Regex("\"code\"\\s*:\\s*\"([a-z_]{1,64})\"").find(it)?.groupValues?.get(1)
        }

        private const val MAX_CAUSE_DEPTH = 16
    }
}
