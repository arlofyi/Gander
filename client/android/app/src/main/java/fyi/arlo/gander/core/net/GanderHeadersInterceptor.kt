package fyi.arlo.gander.core.net

import okhttp3.Interceptor
import okhttp3.Response

/**
 * A per-device bearer token. It is a secret: [toString] never prints it, and nothing in this
 * package logs headers. Read [value] only to put it on the wire or into the encrypted store.
 */
class BearerToken(val value: String) {
    init {
        require(value.isNotBlank() && value.none { it.isWhitespace() || it.code < 0x21 }) {
            "token is not a base64url string"
        }
    }

    override fun toString(): String = "BearerToken(<redacted>)"

    override fun equals(other: Any?): Boolean = other is BearerToken && other.value == value

    override fun hashCode(): Int = value.hashCode()
}

/**
 * Adds the headers every Gander request carries: `Authorization: Bearer` when a token is
 * available, `X-Gander-Protocol: 1` and `User-Agent: Gander-Android/<version>`. Strips the
 * headers that make the host refuse a request as browser traffic (`Origin`, `Sec-Fetch-Mode`,
 * `Cookie`); nothing here should set them, this only makes sure.
 *
 * [token] is read per request, so a rotated token takes effect without rebuilding the client.
 * It returns null before pairing completes (the pairing endpoints need no token).
 */
class GanderHeadersInterceptor(
    private val appVersion: String,
    private val token: () -> BearerToken?,
) : Interceptor {
    override fun intercept(chain: Interceptor.Chain): Response {
        val builder = chain.request().newBuilder()
            .removeHeader("Origin")
            .removeHeader("Sec-Fetch-Mode")
            .removeHeader("Cookie")
            .header(PROTOCOL_HEADER, PROTOCOL_VERSION)
            .header("User-Agent", "Gander-Android/$appVersion")
        token()?.let { builder.header("Authorization", "Bearer ${it.value}") }
        return chain.proceed(builder.build())
    }

    companion object {
        const val PROTOCOL_HEADER = "X-Gander-Protocol"
        const val PROTOCOL_VERSION = "1"
    }
}
