package fyi.arlo.gander.core.net

import kotlinx.coroutines.runBlocking
import mockwebserver3.MockResponse
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.fail
import org.junit.Test

/**
 * `ObservingTrustManager(expected)` is what stops the QR secret (flow A) and the SAS exchange
 * (flow B after first contact) from reaching a computer other than the one pinned: the handshake
 * itself fails, so no request is sent. Without `expected` it stays the plain first-contact manager.
 */
class ObservingTrustManagerPinTest {
    private val hello = """{"protocol":[1],"pairing_open":true,"host_id":"x"}"""

    private fun api(server: mockwebserver3.MockWebServer, trust: ObservingTrustManager): GanderApi {
        val endpoint = HostEndpoint(listOf("127.0.0.1"), server.port)
        return GanderApi(GanderHttpClients.firstContact("0.0.0-test", trust), endpoint)
    }

    @Test
    fun theExpectedCertificateIsAcceptedAndRecorded() = runBlocking<Unit> {
        val host = TestHosts.selfSigned()
        TestHosts.httpsServer(host).use { server ->
            server.enqueue(MockResponse.Builder().body(hello).build())
            val trust = ObservingTrustManager(expected = TestHosts.pinOf(host))
            api(server, trust).host()
            assertEquals(TestHosts.pinOf(host), trust.observedPin)
            assertNotNull(trust.observedLeaf)
            assertEquals(1, server.requestCount)
        }
    }

    @Test
    fun anotherCertificateFailsTheHandshakeBeforeAnyRequest() = runBlocking<Unit> {
        val expected = TestHosts.selfSigned("expected")
        val impostor = TestHosts.selfSigned("impostor")
        TestHosts.httpsServer(impostor).use { server ->
            server.enqueue(MockResponse.Builder().body(hello).build())
            val trust = ObservingTrustManager(expected = TestHosts.pinOf(expected))
            try {
                api(server, trust).host()
                fail("a certificate that is not the expected key was accepted")
            } catch (e: GanderError.PinMismatch) {
                assertEquals(TestHosts.pinOf(expected), e.expected)
                assertEquals(TestHosts.pinOf(impostor), e.actual)
            }
            assertEquals("nothing reaches the impostor", 0, server.requestCount)
            assertNull("a refused certificate is not 'observed'", trust.observedLeaf)
        }
    }

    @Test
    fun withoutAnExpectationAnyCertificateIsObserved() = runBlocking<Unit> {
        val host = TestHosts.selfSigned()
        TestHosts.httpsServer(host).use { server ->
            server.enqueue(MockResponse.Builder().body(hello).build())
            val trust = ObservingTrustManager()
            api(server, trust).host()
            assertEquals(TestHosts.pinOf(host), trust.observedPin)
        }
    }
}
