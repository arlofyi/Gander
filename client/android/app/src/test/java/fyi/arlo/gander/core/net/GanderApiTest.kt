package fyi.arlo.gander.core.net

import fyi.arlo.gander.core.net.protocol.ErrorCode
import fyi.arlo.gander.core.net.protocol.PairRequest
import fyi.arlo.gander.core.net.protocol.PairingDevice
import kotlinx.coroutines.runBlocking
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import okhttp3.tls.HeldCertificate
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Before
import org.junit.Test
import java.net.ServerSocket

/** Interceptors, address fallback and response mapping against a pinned MockWebServer. */
class GanderApiTest {
    private lateinit var host: HeldCertificate
    private lateinit var server: MockWebServer

    @Before
    fun setUp() {
        host = TestHosts.selfSigned()
        server = TestHosts.httpsServer(host)
    }

    @After
    fun tearDown() {
        server.close()
    }

    private fun json(code: Int, body: String) =
        MockResponse.Builder().code(code).setHeader("Content-Type", "application/json").body(body).build()

    @Test
    fun everyRequestCarriesBearerProtocolAndUserAgent() = runBlocking<Unit> {
        server.enqueue(json(200, """{"shares":[]}"""))
        val api = GanderApi(TestHosts.clients(server, TestHosts.pinOf(host), BearerToken("tok_abc")))
        assertTrue(api.shares().shares.isEmpty())
        val request = server.takeRequest()
        assertEquals("Bearer tok_abc", request.headers["Authorization"])
        assertEquals("1", request.headers["X-Gander-Protocol"])
        assertEquals("Gander-Android/0.0.0-test", request.headers["User-Agent"])
        assertNull(request.headers["Origin"])
        assertNull(request.headers["Cookie"])
        assertEquals("/v1/shares", request.url.encodedPath)
    }

    @Test
    fun noTokenMeansNoAuthorizationHeader() = runBlocking<Unit> {
        server.enqueue(json(200, """{"protocol":[1],"pairing_open":true,"host_id":"x"}"""))
        GanderApi(TestHosts.clients(server, TestHosts.pinOf(host), token = null)).host()
        assertNull(server.takeRequest().headers["Authorization"])
    }

    @Test
    fun tokenNeverAppearsInToStringOrErrors() = runBlocking<Unit> {
        val secret = "s3cr3t-token-value"
        assertFalse(BearerToken(secret).toString().contains(secret))
        server.enqueue(json(401, """{"error":{"code":"token_revoked","message":"Revoked.","retry_after_ms":0}}"""))
        try {
            GanderApi(TestHosts.clients(server, TestHosts.pinOf(host), BearerToken(secret))).me()
            fail("expected TokenRevoked")
        } catch (e: GanderError.Host.TokenRevoked) {
            assertFalse(e.toString().contains(secret))
            assertFalse(e.stackTraceToString().contains(secret))
        }
    }

    @Test
    fun errorBodiesMapToTypedErrors() = runBlocking<Unit> {
        server.enqueue(
            json(503, """{"error":{"code":"generating","message":"Wait.","retry_after_ms":1500}}"""),
        )
        val api = GanderApi(TestHosts.clients(server, TestHosts.pinOf(host)))
        try {
            api.previewInfo("shr_1", "a/b.pdf")
            fail("expected Generating")
        } catch (e: GanderError.Host.Generating) {
            assertEquals(1500L, e.retryAfterMs)
            assertEquals(503, e.status)
            assertEquals(ErrorCode.GENERATING, e.code)
        }
        val recorded = server.takeRequest()
        assertEquals("/v1/shares/shr_1/preview/info", recorded.url.encodedPath)
        assertEquals("a/b.pdf", recorded.url.queryParameter("path"))
    }

    @Test
    fun listHonoursIfNoneMatch() = runBlocking<Unit> {
        server.enqueue(MockResponse.Builder().code(304).build())
        val api = GanderApi(TestHosts.clients(server, TestHosts.pinOf(host)))
        val result = api.list("shr_1", "Clients/Acme", limit = 200, ifNoneMatch = "W/\"5f2a9c\"")
        assertEquals(Listing.NotModified, result)
        val recorded = server.takeRequest()
        assertEquals("W/\"5f2a9c\"", recorded.headers["If-None-Match"])
        assertEquals("200", recorded.url.queryParameter("limit"))
        assertEquals("Clients/Acme", recorded.url.queryParameter("path"))
    }

    @Test
    fun malformedSuccessBodyIsAProtocolError() = runBlocking<Unit> {
        server.enqueue(json(200, """{"shares": "not a list"}"""))
        try {
            GanderApi(TestHosts.clients(server, TestHosts.pinOf(host))).shares()
            fail("expected MalformedResponse")
        } catch (_: GanderError.MalformedResponse) {
            // expected
        }
    }

    @Test
    fun pairingPollDistinguishesPendingDeniedApprovedAndExpired() = runBlocking<Unit> {
        val api = GanderApi(TestHosts.clients(server, TestHosts.pinOf(host), token = null))
        server.enqueue(json(200, """{"pairing_id":"pr_1","sas":"482 913","host_nonce":"n","expires_at":"2026-11-02T20:13:03Z"}"""))
        val started = api.startPairing(
            PairRequest(device = PairingDevice("Pixel 9", "android", "Pixel 9", "0.1.0"), nonce = "q3Jx0bS1n9Vw2kLp8Zt4Yg"),
        )
        assertEquals("482 913", started.sas)
        val sent = server.takeRequest().body!!.utf8()
        assertFalse("flow B sends no secret field", sent.contains("secret"))

        server.enqueue(json(202, """{"state":"pending"}"""))
        server.enqueue(json(403, """{"state":"denied"}"""))
        server.enqueue(
            json(200, """{"token":"PLACEHOLDER","device_id":"dev_1","host":{"id":"h","name":"studio-pc","os":"windows","os_version":"10","app_version":"0.1.0","addresses":["100.101.102.103"],"magic_dns":null}}"""),
        )
        server.enqueue(json(410, """{"error":{"code":"pairing_expired","message":"Expired.","retry_after_ms":0}}"""))
        assertEquals(PairPoll.Pending, api.pollPairing("pr_1"))
        assertEquals(PairPoll.Denied, api.pollPairing("pr_1"))
        val approved = api.pollPairing("pr_1") as PairPoll.Approved
        assertEquals("dev_1", approved.approved.deviceId)
        assertFalse(approved.toString().contains("PLACEHOLDER"))
        try {
            api.pollPairing("pr_1")
            fail("expected PairingExpired")
        } catch (_: GanderError.Host.PairingExpired) {
            // expected
        }
    }

    @Test
    fun meRefreshesTheEndpointAddresses() = runBlocking<Unit> {
        server.enqueue(json(200, ProtocolExamples.exampleJson("MeResponse")))
        val clients = TestHosts.clients(server, TestHosts.pinOf(host))
        GanderApi(clients).me()
        // The host reported tailnet addresses and a MagicDNS name. 127.0.0.1 is no longer
        // listed, so the endpoint now leads with the host's first address.
        assertEquals(
            listOf("100.101.102.103", "fd7a:115c:a1e0::1", "studio-pc.tail1234.ts.net"),
            clients.endpoint.candidates,
        )
    }

    @Test
    fun fallsBackToTheNextAddressAndRemembersIt() = runBlocking<Unit> {
        // 127.0.0.2 is loopback too, but nothing listens there on this port: connection refused.
        val clients = TestHosts.clients(server, TestHosts.pinOf(host), addresses = listOf("127.0.0.2", "127.0.0.1"))
        server.enqueue(json(200, """{"shares":[]}"""))
        server.enqueue(json(200, """{"shares":[]}"""))
        val api = GanderApi(clients)
        assertEquals("127.0.0.2", clients.endpoint.current)
        api.shares()
        assertEquals("127.0.0.1", clients.endpoint.current)
        assertEquals("127.0.0.1", server.takeRequest().url.host)
        api.shares()
        assertEquals(2, server.requestCount)
    }

    @Test
    fun allAddressesFailingIsUnreachable() = runBlocking<Unit> {
        val closedPort = ServerSocket(0, 1, TestHosts.LOOPBACK).use { it.localPort }
        val clients = GanderHttpClients(
            pin = TestHosts.pinOf(host),
            endpoint = HostEndpoint(listOf("127.0.0.1", "127.0.0.2"), closedPort),
            appVersion = "0.0.0-test",
            token = { null },
        )
        try {
            GanderApi(clients).host()
            fail("expected Unreachable")
        } catch (e: GanderError.Unreachable) {
            assertEquals(listOf("127.0.0.1", "127.0.0.2"), e.tried)
        }
    }

    @Test
    fun endpointKeepsPreferredAddressAcrossUpdates() {
        val endpoint = HostEndpoint(listOf("100.1.1.1", "[fd7a:115c:a1e0::1]", "pc.ts.net"), 42637)
        assertEquals(listOf("100.1.1.1", "fd7a:115c:a1e0::1", "pc.ts.net"), endpoint.candidates)
        endpoint.markWorking("pc.ts.net")
        assertEquals("pc.ts.net", endpoint.current)
        endpoint.updateAddresses(listOf("100.1.1.2", "pc.ts.net"))
        assertEquals(listOf("pc.ts.net", "100.1.1.2"), endpoint.candidates)
        endpoint.updateAddresses(emptyList())
        assertEquals(listOf("pc.ts.net", "100.1.1.2"), endpoint.candidates)
        endpoint.markWorking("10.0.0.9") // not one of the host's addresses: ignored
        assertEquals("pc.ts.net", endpoint.current)
        assertEquals("https://[fd7a:115c:a1e0::1]:42637/v1/", endpoint.baseUrl("fd7a:115c:a1e0::1").toString())
        assertThrows<IllegalArgumentException> { HostEndpoint(emptyList()) }
        assertThrows<IllegalArgumentException> { HostEndpoint(listOf("a"), 0) }
    }
}
