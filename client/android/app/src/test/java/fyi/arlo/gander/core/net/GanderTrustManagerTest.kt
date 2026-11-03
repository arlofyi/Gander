package fyi.arlo.gander.core.net

import kotlinx.coroutines.runBlocking
import mockwebserver3.MockResponse
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.tls.HandshakeCertificates
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test
import java.security.cert.CertificateException
import java.security.cert.X509Certificate

/** The WP-21 verification: pinned accepted, other self-signed rejected, system-trusted rejected. */
class GanderTrustManagerTest {
    private val hostHello = """{"protocol":[1],"pairing_open":false,"host_id":"x"}"""

    @Test
    fun pinnedSelfSignedCertificateIsAccepted() = runBlocking<Unit> {
        val host = TestHosts.selfSigned()
        TestHosts.httpsServer(host).use { server ->
            server.enqueue(MockResponse.Builder().body(hostHello).build())
            val clients = TestHosts.clients(server, TestHosts.pinOf(host))
            val hello = GanderApi(clients).host()
            assertEquals(listOf(1), hello.protocol)
            val recorded = server.takeRequest()
            assertEquals("TLSv1.3", recorded.handshake?.tlsVersion?.javaName)
        }
    }

    @Test
    fun otherSelfSignedCertificateIsRejected() = runBlocking<Unit> {
        val paired = TestHosts.selfSigned("paired-host")
        val impostor = TestHosts.selfSigned("impostor")
        TestHosts.httpsServer(impostor).use { server ->
            server.enqueue(MockResponse.Builder().body(hostHello).build())
            val clients = TestHosts.clients(server, TestHosts.pinOf(paired))
            try {
                GanderApi(clients).host()
                fail("an unpinned certificate was accepted")
            } catch (e: GanderError.PinMismatch) {
                assertEquals(TestHosts.pinOf(paired), e.expected)
                assertEquals(TestHosts.pinOf(impostor), e.actual)
            }
            assertEquals("no request may reach an impostor", 0, server.requestCount)
        }
    }

    @Test
    fun systemTrustedCertificateIsRejected() = runBlocking<Unit> {
        val trusted = TestHosts.caSigned()
        TestHosts.httpsServer(trusted.leaf, trusted.ca.certificate).use { server ->
            // Control: a client that trusts the platform roots plus this CA (what "system
            // trusted" means to an ordinary client) accepts the server.
            val systemLike = HandshakeCertificates.Builder()
                .addPlatformTrustedCertificates()
                .addTrustedCertificate(trusted.ca.certificate)
                .build()
            val ordinary = OkHttpClient.Builder()
                .sslSocketFactory(systemLike.sslSocketFactory(), systemLike.trustManager)
                .build()
            server.enqueue(MockResponse.Builder().body(hostHello).build())
            ordinary.newCall(Request.Builder().url("https://127.0.0.1:${server.port}/v1/host").build()).execute().use {
                assertTrue("control client should trust the CA-signed server", it.isSuccessful)
            }

            // Gander, paired with a different host, must refuse it: no system trust at all.
            server.enqueue(MockResponse.Builder().body(hostHello).build())
            val gander = TestHosts.clients(server, TestHosts.pinOf(TestHosts.selfSigned()))
            try {
                GanderApi(gander).host()
                fail("a CA-trusted certificate was accepted without a pin match")
            } catch (_: GanderError.PinMismatch) {
                // expected
            }
        }
    }

    @Test
    fun pinningTheIssuingCaDoesNotAcceptItsLeaf() {
        val trusted = TestHosts.caSigned()
        val manager = GanderTrustManager(TestHosts.pinOf(trusted.ca))
        try {
            manager.checkServerTrusted(arrayOf(trusted.leaf.certificate, trusted.ca.certificate), "ECDHE_ECDSA")
            fail("the pin must apply to the leaf, not to a CA in the chain")
        } catch (_: SpkiPinMismatchException) {
            // expected
        }
    }

    @Test
    fun trustManagerRefusesEmptyChainsAndClientAuth() {
        val manager = GanderTrustManager(TestHosts.pinOf(TestHosts.selfSigned()))
        assertEquals(0, manager.acceptedIssuers.size)
        assertThrows<CertificateException> { manager.checkServerTrusted(arrayOf(), "ECDHE_ECDSA") }
        assertThrows<CertificateException> { manager.checkServerTrusted(null, "ECDHE_ECDSA") }
        val cert = TestHosts.selfSigned().certificate
        assertThrows<CertificateException> { manager.checkClientTrusted(arrayOf(cert), "ECDHE_ECDSA") }
    }

    @Test
    fun hostIdRoundTripsAndRejectsGarbage() {
        val held = TestHosts.selfSigned()
        val pin = SpkiPin.of(held.certificate)
        assertEquals(43, pin.hostId.length) // 32 bytes base64url without padding
        assertFalse(pin.hostId.contains('='))
        assertEquals(pin, SpkiPin.fromHostId(pin.hostId))
        assertTrue(pin.matches(held.certificate))
        assertThrows<IllegalArgumentException> { SpkiPin.fromHostId("not base64!") }
        assertThrows<IllegalArgumentException> { SpkiPin.fromHostId("AAAA") }
    }

    @Test
    fun hostnameVerifierAcceptsOnlyThePinnedPeer() = runBlocking<Unit> {
        val host = TestHosts.selfSigned()
        TestHosts.httpsServer(host).use { server ->
            server.enqueue(MockResponse.Builder().body(hostHello).build())
            // Pair the verifier with a client that trusts the server another way, so the
            // verifier's own decision is what is tested.
            val trustIt = HandshakeCertificates.Builder().addTrustedCertificate(host.certificate).build()
            val other = TestHosts.pinOf(TestHosts.selfSigned())
            val client = OkHttpClient.Builder()
                .sslSocketFactory(trustIt.sslSocketFactory(), trustIt.trustManager)
                .hostnameVerifier(PinnedHostnameVerifier(other))
                .build()
            try {
                client.newCall(Request.Builder().url("https://127.0.0.1:${server.port}/v1/host").build()).execute().close()
                fail("hostname verifier accepted a peer that is not the pinned host")
            } catch (_: javax.net.ssl.SSLPeerUnverifiedException) {
                // expected
            }
            assertFalse(PinnedHostnameVerifier(other).verify("127.0.0.1", null))
        }
    }

    @Test
    fun firstContactObservesTheCertificateItSaw() = runBlocking<Unit> {
        val host = TestHosts.selfSigned()
        TestHosts.httpsServer(host).use { server ->
            server.enqueue(MockResponse.Builder().body(hostHello).build())
            val observer = ObservingTrustManager()
            assertNull(observer.observedPin)
            val client = GanderHttpClients.firstContact("0.0.0-test", observer)
            client.newCall(Request.Builder().url("https://127.0.0.1:${server.port}/v1/host").build()).execute().use {
                assertTrue(it.isSuccessful)
            }
            assertEquals(TestHosts.pinOf(host), observer.observedPin)
            val leaf: X509Certificate? = observer.observedLeaf
            assertEquals(host.certificate, leaf)
            assertNull("first contact never sends a token", server.takeRequest().headers["Authorization"])
        }
    }
}

inline fun <reified T : Throwable> assertThrows(block: () -> Unit) {
    try {
        block()
    } catch (e: Throwable) {
        if (e is T) return
        throw AssertionError("expected ${T::class.java.simpleName}, got $e", e)
    }
    throw AssertionError("expected ${T::class.java.simpleName}, nothing thrown")
}
