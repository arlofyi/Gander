package fyi.arlo.gander.core.net

import okhttp3.ConnectionSpec
import okhttp3.Dispatcher
import okhttp3.OkHttpClient
import okhttp3.TlsVersion
import java.security.SecureRandom
import java.time.Duration
import javax.net.ssl.SSLContext
import javax.net.ssl.X509TrustManager

/**
 * The OkHttp clients for ONE paired host (client spec, "Networking layer"):
 *
 * - [api]: JSON calls and the `/events` WebSocket. Connect 5 s, read 30 s, WebSocket ping 20 s.
 * - [transfer]: file bytes, thumbnails, previews, Media3 and Coil. No overall call timeout; the
 *   60 s read timeout is the idle watchdog (OkHttp's read timeout is per read, so a stream that
 *   keeps delivering bytes never trips it, one that stalls for 60 s fails).
 *
 * Both share one connection pool and one dispatcher limited to 8 in-flight requests (the host's
 * per-device limit), trust only [pin], verify the peer with [PinnedHostnameVerifier], speak TLS
 * 1.3 only, never follow redirects, keep no cookies and no cache, and add the Gander headers.
 * There is deliberately no logging interceptor: headers carry the token.
 */
class GanderHttpClients(
    val pin: SpkiPin,
    val endpoint: HostEndpoint,
    appVersion: String,
    token: () -> BearerToken?,
    base: OkHttpClient = OkHttpClient(),
) {
    val trustManager: GanderTrustManager = GanderTrustManager(pin)

    private val dispatcher = Dispatcher().apply {
        maxRequests = MAX_CONCURRENT_REQUESTS
        maxRequestsPerHost = MAX_CONCURRENT_REQUESTS
    }

    /** JSON API and WebSocket client. */
    val api: OkHttpClient = configure(base.newBuilder(), trustManager, PinnedHostnameVerifier(pin))
        .dispatcher(dispatcher)
        .addInterceptor(GanderHeadersInterceptor(appVersion, token))
        .addInterceptor(AddressFallbackInterceptor(endpoint))
        .connectTimeout(CONNECT_TIMEOUT)
        .readTimeout(API_READ_TIMEOUT)
        .writeTimeout(API_READ_TIMEOUT)
        .pingInterval(WEBSOCKET_PING)
        .build()

    /** Byte-stream client (fetch, thumbnails, previews, Media3 `OkHttpDataSource`, Coil). */
    val transfer: OkHttpClient = api.newBuilder()
        .readTimeout(TRANSFER_IDLE_TIMEOUT)
        .writeTimeout(TRANSFER_IDLE_TIMEOUT)
        .callTimeout(Duration.ZERO)
        .pingInterval(Duration.ZERO)
        .build()

    companion object {
        const val MAX_CONCURRENT_REQUESTS = 8
        val CONNECT_TIMEOUT: Duration = Duration.ofSeconds(5)
        val API_READ_TIMEOUT: Duration = Duration.ofSeconds(30)
        val TRANSFER_IDLE_TIMEOUT: Duration = Duration.ofSeconds(60)
        val WEBSOCKET_PING: Duration = Duration.ofSeconds(20)

        /** The host is TLS 1.3 only (protocol spec, Transport). */
        val TLS_13_ONLY: ConnectionSpec = ConnectionSpec.Builder(ConnectionSpec.RESTRICTED_TLS)
            .tlsVersions(TlsVersion.TLS_1_3)
            .build()

        /**
         * A client for pairing flow B's first contact (`GET /v1/host`, `POST /v1/pair`, polling):
         * it accepts any certificate and records it in [trustManager] so the caller can compare
         * `host_id` and derive the SAS. No token is ever attached. Build a fresh one per pairing
         * attempt so a resumed TLS session cannot skip the observation.
         */
        fun firstContact(
            appVersion: String,
            trustManager: ObservingTrustManager,
            base: OkHttpClient = OkHttpClient(),
        ): OkHttpClient = configure(
            base.newBuilder(),
            trustManager,
            { _, session ->
                // Accept only the exact certificate the trust manager just observed.
                val seen = trustManager.observedLeaf
                val peer = runCatching { session?.peerCertificates?.firstOrNull() }.getOrNull()
                seen != null && peer == seen
            },
        )
            .addInterceptor(GanderHeadersInterceptor(appVersion) { null })
            .connectTimeout(CONNECT_TIMEOUT)
            .readTimeout(API_READ_TIMEOUT)
            .build()

        private fun configure(
            builder: OkHttpClient.Builder,
            trustManager: X509TrustManager,
            hostnameVerifier: javax.net.ssl.HostnameVerifier,
        ): OkHttpClient.Builder {
            val context = SSLContext.getInstance("TLS")
            context.init(null, arrayOf(trustManager), SecureRandom())
            return builder
                .sslSocketFactory(context.socketFactory, trustManager)
                .hostnameVerifier(hostnameVerifier)
                .connectionSpecs(listOf(TLS_13_ONLY))
                .followRedirects(false)
                .followSslRedirects(false)
                .cache(null)
                .retryOnConnectionFailure(true)
        }
    }
}
