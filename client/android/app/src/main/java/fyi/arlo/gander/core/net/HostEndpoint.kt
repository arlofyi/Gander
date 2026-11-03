package fyi.arlo.gander.core.net

import okhttp3.HttpUrl
import okhttp3.Interceptor
import okhttp3.Response
import java.io.IOException
import java.io.InterruptedIOException
import java.net.ConnectException
import java.net.NoRouteToHostException
import java.net.SocketTimeoutException
import java.net.UnknownHostException

/**
 * Where a host can be reached: its addresses in preference order (tailnet IPv4, tailnet IPv6,
 * MagicDNS name) and the port. Remembers the address that last worked and tries it first.
 *
 * Thread-safe. Refresh with [updateAddresses] from `/v1/me` `host.addresses` (plus `magic_dns`)
 * on every connect, as the client spec requires.
 */
class HostEndpoint(addresses: List<String>, val port: Int = DEFAULT_PORT) {
    init {
        require(port in 1..65535) { "port out of range" }
    }

    @Volatile
    private var state = State(normalise(addresses), preferred = null)

    /** Every known address, the last one that worked first. */
    val candidates: List<String>
        get() = state.let { s ->
            val preferred = s.preferred
            if (preferred == null) s.addresses else listOf(preferred) + (s.addresses - preferred)
        }

    /** The address requests are built against right now. */
    val current: String get() = candidates.first()

    /** Records that [address] answered; later requests start with it. */
    fun markWorking(address: String) {
        synchronized(this) {
            if (address in state.addresses) state = state.copy(preferred = address)
        }
    }

    /**
     * Replaces the address list (from `/v1/me`). The remembered address is kept if the host still
     * lists it. An empty list is ignored: a host that reports no address is still the one we
     * just reached.
     */
    fun updateAddresses(addresses: List<String>) {
        val next = normalise(addresses, allowEmpty = true)
        if (next.isEmpty()) return
        synchronized(this) {
            state = State(next, state.preferred?.takeIf { it in next })
        }
    }

    /** `https://<current>:<port>/v1/` */
    fun baseUrl(address: String = current): HttpUrl =
        HttpUrl.Builder().scheme("https").host(address).port(port).addPathSegment("v1").addPathSegment("").build()

    /** True if [url] points at one of this host's addresses and port. */
    fun owns(url: HttpUrl): Boolean = url.port == port && canonical(url.host) in state.addresses.map(::canonical)

    override fun toString(): String = "HostEndpoint(candidates=$candidates, port=$port)"

    private data class State(val addresses: List<String>, val preferred: String?)

    private fun normalise(addresses: List<String>, allowEmpty: Boolean = false): List<String> {
        val cleaned = addresses.map { it.trim().removePrefix("[").removeSuffix("]") }
            .filter { it.isNotEmpty() }
            .distinct()
        require(allowEmpty || cleaned.isNotEmpty()) { "a host needs at least one address" }
        return cleaned
    }

    /** HttpUrl canonicalises IPv6 literals; compare in that form. */
    private fun canonical(host: String): String =
        HttpUrl.Builder().scheme("https").host(host).build().host

    companion object {
        /** "GANDR" on a keypad (protocol spec, Transport). */
        const val DEFAULT_PORT = 42637
    }
}

/**
 * Application interceptor that implements [HostEndpoint] fallback: if connecting to the request's
 * address fails, the same request is retried against the host's other addresses in order, and
 * the first that answers becomes the remembered one. Only failures before any byte was sent are
 * retried (connection refused, no route, unknown host, connect timeout), so POSTs are safe.
 * TLS failures (pin mismatch) are never retried: a wrong certificate is not a routing problem.
 *
 * The URL host is rewritten, not just the socket address, because the host checks the `Host`
 * header against the addresses it serves (protocol spec, `bad_host`).
 */
class AddressFallbackInterceptor(private val endpoint: HostEndpoint) : Interceptor {
    override fun intercept(chain: Interceptor.Chain): Response {
        val original = chain.request()
        if (!endpoint.owns(original.url)) return chain.proceed(original)

        val first = original.url.host
        val order = listOf(first) + endpoint.candidates.filterNot { sameHost(it, first) }
        val failures = mutableListOf<IOException>()
        for (address in order) {
            val request = if (address == first) {
                original
            } else {
                original.newBuilder().url(original.url.newBuilder().host(address).build()).build()
            }
            try {
                val response = chain.proceed(request)
                endpoint.markWorking(address)
                return response
            } catch (e: IOException) {
                if (!isConnectFailure(e) || chain.call().isCanceled()) throw e
                failures += e
            }
        }
        throw HostUnreachableException(order, failures)
    }

    private fun sameHost(a: String, b: String): Boolean =
        HttpUrl.Builder().scheme("https").host(a).build().host == HttpUrl.Builder().scheme("https").host(b).build().host

    private fun isConnectFailure(e: IOException): Boolean = when (e) {
        is ConnectException, is NoRouteToHostException, is UnknownHostException -> true
        // OkHttp reports connect timeouts as SocketTimeoutException("Connect timed out"); a read
        // timeout says "timeout" or "Read timed out" and must not be replayed.
        is SocketTimeoutException -> e.message?.contains("connect", ignoreCase = true) == true
        is InterruptedIOException -> false
        else -> false
    }
}

/** Every address of a host failed to connect. Mapped to [GanderError.Unreachable]. */
class HostUnreachableException(val tried: List<String>, val failures: List<IOException>) :
    IOException("host unreachable on ${tried.size} address(es)", failures.lastOrNull()) {
    init {
        failures.dropLast(1).forEach(::addSuppressed)
    }
}
