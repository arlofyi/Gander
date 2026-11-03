package fyi.arlo.gander.core.data.pairing

import fyi.arlo.gander.core.net.GanderJson
import fyi.arlo.gander.core.net.TestHosts
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import mockwebserver3.Dispatcher
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import mockwebserver3.RecordedRequest
import okhttp3.tls.HeldCertificate
import java.util.concurrent.ConcurrentLinkedQueue
import java.util.concurrent.CopyOnWriteArrayList

/**
 * A scripted Gander host for the pairing tests: HTTPS on 127.0.0.1 with a per-run certificate,
 * answering `GET /v1/host`, `POST /v1/pair` and `GET /v1/pair/{id}`. The SAS it reports is derived
 * like the real host does it (its own SPKI, the phone's nonce, its own nonce), so a correct
 * phone computes the same string and a MITM test can make them differ.
 *
 * Every secret in here is generated per run or an obvious placeholder.
 */
class FakePairingHost(
    val cert: HeldCertificate = TestHosts.selfSigned(),
    /** The host id `GET /v1/host` claims. Defaults to the certificate's pin (an honest host). */
    var claimedHostId: String = TestHosts.pinOf(cert).hostId,
    /** The SPKI the reported SAS is derived from. Defaults to the certificate's own. */
    var reportedSpki: ByteArray = cert.certificate.publicKey.encoded,
) : Dispatcher() {

    class Seen(val method: String, val path: String, val headers: Map<String, String>, val body: String)

    val server: MockWebServer = TestHosts.httpsServer(cert).also { it.dispatcher = this }
    val seen = CopyOnWriteArrayList<Seen>()

    /** Answer to `POST /v1/pair`; null means the normal 200 with a derived SAS. */
    @Volatile
    var startOverride: MockResponse? = null

    /** Answers to successive polls; once exhausted every poll is 202 pending. */
    val polls = ConcurrentLinkedQueue<MockResponse>()

    val hostNonce = ByteArray(16) { (0xA0 + it).toByte() }

    val pin get() = TestHosts.pinOf(cert)

    fun shutdown() = server.close()

    override fun dispatch(request: RecordedRequest): MockResponse {
        val body = request.body?.utf8().orEmpty()
        seen += Seen(request.method, request.url.encodedPath, request.headers.toMultimap().mapValues { it.value.first() }, body)
        return when {
            request.method == "GET" && request.url.encodedPath == "/v1/host" ->
                json(200, """{"protocol":[1],"pairing_open":true,"host_id":"$claimedHostId"}""")
            request.method == "POST" && request.url.encodedPath == "/v1/pair" -> startOverride ?: started(body)
            request.method == "GET" && request.url.encodedPath.startsWith("/v1/pair/") ->
                polls.poll() ?: json(202, """{"state":"pending"}""")
            else -> json(404, error("not_found", "No such endpoint."))
        }
    }

    private fun started(body: String): MockResponse {
        val nonce = GanderJson.json.parseToJsonElement(body).jsonObject["nonce"]!!.jsonPrimitive.content
        val deviceNonce = Base64Url.decodeFixed(nonce, 16) ?: return json(400, error("bad_request", "Malformed pairing request."))
        val sas = Sas.derive(reportedSpki, deviceNonce, hostNonce).display
        return json(
            200,
            """{"pairing_id":"pair_test","sas":"$sas","host_nonce":"${Base64Url.encode(hostNonce)}","expires_at":"2026-11-02T20:13:03Z"}""",
        )
    }

    companion object {
        const val TOKEN = "tok_PLACEHOLDER_not_a_real_token"

        fun json(code: Int, body: String): MockResponse =
            MockResponse.Builder().code(code).setHeader("Content-Type", "application/json").body(body).build()

        fun error(code: String, message: String, retryAfterMs: Long = 0) =
            """{"error":{"code":"$code","message":"$message","retry_after_ms":$retryAfterMs}}"""

        fun approved(hostId: String, name: String = "studio-pc", addresses: String = """["100.101.102.103"]"""): MockResponse = json(
            200,
            """{"token":"$TOKEN","device_id":"dev_test","host":{"id":"$hostId","name":"$name","os":"windows",""" +
                """"os_version":"10.0.26200","app_version":"0.1.0","addresses":$addresses,"magic_dns":"studio-pc.tail1.ts.net"}}""",
        )

        val DENIED: MockResponse get() = json(403, """{"state":"denied"}""")
        val EXPIRED: MockResponse get() = json(410, error("pairing_expired", "This pairing request has expired."))
    }
}
