package fyi.arlo.gander.core.data.pairing

import fyi.arlo.gander.core.data.hosts.HostRepository
import fyi.arlo.gander.core.data.hosts.InMemoryHostRepository
import fyi.arlo.gander.core.data.hosts.PairedHost
import fyi.arlo.gander.core.net.BearerToken
import fyi.arlo.gander.core.net.TestHosts
import fyi.arlo.gander.core.net.protocol.HostOs
import fyi.arlo.gander.core.net.protocol.PairingDevice
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Before
import org.junit.Test
import java.net.ServerSocket

/**
 * Pairing flows A and B end to end against a scripted host over real TLS 1.3 on loopback:
 * what is sent and in which order, what is pinned, which code is shown, what is stored, and every
 * way it can end.
 */
class PairingServiceTest {
    private lateinit var host: FakePairingHost
    private lateinit var repo: InMemoryHostRepository
    private val device = PairingDevice(name = "Pixel 9", platform = "android", model = "Pixel 9", appVersion = "0.0.0-test")

    @Before
    fun setUp() {
        host = FakePairingHost()
        repo = InMemoryHostRepository()
    }

    @After
    fun tearDown() {
        host.shutdown()
    }

    private fun service(
        hosts: HostRepository = repo,
        pollIntervalMs: Long = 5,
        approvalTimeoutMs: Long = 10_000,
    ) = PairingService(
        hosts = hosts,
        device = { device },
        appVersion = "0.0.0-test",
        pollIntervalMs = pollIntervalMs,
        approvalTimeoutMs = approvalTimeoutMs,
    )

    private fun manual(port: Int = host.server.port) = PairingTarget.Manual(ManualAddress("127.0.0.1", port))

    private fun qr(
        hostId: String = host.pin.hostId,
        secret: String = SECRET,
        port: Int = host.server.port,
        addresses: List<String> = listOf("127.0.0.1"),
    ) = PairingTarget.Qr(PairingInvite(hostId, addresses, port, "Studio PC", PairingSecret(secret), magicDns = null))

    private fun failureOf(target: PairingTarget, service: PairingService = service()): PairingFailure = runBlocking {
        try {
            service.pair(target) {}
            fail("pairing should have failed")
            throw AssertionError()
        } catch (e: PairingFailure) {
            e
        }
    }

    // Flow B ------------------------------------------------------------------------------------

    @Test
    fun manualFlowPairsAndStoresTheHostPinAndToken() = runBlocking {
        host.polls += FakePairingHost.approved(host.pin.hostId)
        val waiting = mutableListOf<WaitingForApproval>()

        val paired = service().pair(manual()) { waiting += it }

        // GET /host (first contact), then the pinned POST and the poll.
        assertEquals(
            listOf("GET /v1/host", "POST /v1/pair", "GET /v1/pair/pair_test"),
            host.seen.map { "${it.method} ${it.path}" },
        )
        host.seen.forEach { assertNull("no token before pairing", it.headers["authorization"]) }
        val post = host.seen[1]
        assertFalse("flow B sends no secret", post.body.contains("secret"))
        assertTrue(post.body.contains(""""platform":"android""""))
        assertTrue(post.body.contains(""""name":"Pixel 9""""))

        // The code on screen is derived from the certificate this phone saw.
        assertEquals(1, waiting.size)
        assertEquals(PairingMethod.MANUAL, waiting[0].method)
        assertEquals("127.0.0.1", waiting[0].hostLabel)
        val deviceNonce = Base64Url.decodeFixed(
            Regex(""""nonce":"([^"]+)"""").find(post.body)!!.groupValues[1],
            Sas.NONCE_BYTES,
        )!!
        assertEquals(Sas.derive(host.cert.certificate.publicKey.encoded, deviceNonce, host.hostNonce), waiting[0].sas)

        // Stored: host, pin, token. The address that worked comes first.
        assertEquals(host.pin.hostId, paired.id)
        assertEquals(host.pin, paired.pin)
        assertEquals("studio-pc", paired.name)
        assertEquals(HostOs.WINDOWS, paired.os)
        assertEquals("Pixel 9", paired.pairedAs)
        assertEquals(host.server.port, paired.port)
        assertEquals(listOf("127.0.0.1", "100.101.102.103", "studio-pc.tail1.ts.net"), paired.addresses)
        assertEquals(listOf(paired), repo.hosts.first())
        assertEquals(BearerToken(FakePairingHost.TOKEN), repo.token(paired.id))
    }

    @Test
    fun manualFlowRefusesAHostWhoseIdIsNotItsCertificate() {
        host.claimedHostId = TestHosts.pinOf(TestHosts.selfSigned()).hostId // names someone else's key
        val failure = failureOf(manual())
        assertEquals(PairingFailure.Kind.WRONG_COMPUTER, failure.kind)
        assertEquals(listOf("GET /v1/host"), host.seen.map { "${it.method} ${it.path}" }) // never got to POST
        assertEquals(emptyList<PairedHost>(), runBlocking { repo.hosts.first() })
    }

    @Test
    fun manualFlowStopsWhenTheReportedCodeIsNotTheOneThePhoneDerived() {
        // A man in the middle relays to the real host (whose SPKI the code is derived from) but
        // presents its own certificate to the phone: the phone derives a different code.
        host.reportedSpki = TestHosts.selfSigned().keyPair.public.encoded
        var shown = false
        val failure = runBlocking {
            try {
                service().pair(manual()) { shown = true }
                null
            } catch (e: PairingFailure) {
                e
            }
        }
        assertEquals(PairingFailure.Kind.SAS_MISMATCH, failure?.kind)
        assertFalse("no code is shown for a mismatch", shown)
        assertTrue(runBlocking { repo.hosts.first() }.isEmpty())
        assertEquals(listOf("GET /v1/host", "POST /v1/pair"), host.seen.map { "${it.method} ${it.path}" })
    }

    // Flow A ------------------------------------------------------------------------------------

    @Test
    fun qrFlowSendsTheSecretToThePinnedHostAndSkipsTheHelloCall() = runBlocking {
        host.polls += FakePairingHost.approved(host.pin.hostId)
        val waiting = mutableListOf<WaitingForApproval>()

        val paired = service().pair(qr()) { waiting += it }

        assertEquals(listOf("POST /v1/pair", "GET /v1/pair/pair_test"), host.seen.map { "${it.method} ${it.path}" })
        assertTrue(host.seen[0].body.contains(""""secret":"$SECRET""""))
        assertNull(host.seen[0].headers["authorization"])
        assertEquals(PairingMethod.QR, waiting.single().method)
        assertEquals("Studio PC", waiting.single().hostLabel)
        assertEquals(host.pin, paired.pin)
        assertEquals(BearerToken(FakePairingHost.TOKEN), repo.token(paired.id))
    }

    @Test
    fun qrFlowNeverSendsTheSecretToAComputerWithAnotherCertificate() {
        val otherId = TestHosts.pinOf(TestHosts.selfSigned()).hostId // the code names a different key
        val failure = failureOf(qr(hostId = otherId))
        assertEquals(PairingFailure.Kind.WRONG_COMPUTER, failure.kind)
        // The TLS handshake was refused: not one HTTP request, so no secret, reached the server.
        assertEquals(0, host.server.requestCount)
        assertTrue(host.seen.isEmpty())
        assertFalse(failure.toString().contains(SECRET))
        assertFalse(failure.cause?.message.orEmpty().contains(SECRET))
    }

    @Test
    fun qrFlowFallsThroughToTheNextAddressOnConnectFailure() = runBlocking {
        host.polls += FakePairingHost.approved(host.pin.hostId)
        // The fake listens on 127.0.0.1 only; nothing answers on ::1, so the first address fails
        // to connect and the second is used. That address is what gets remembered first.
        val paired = service().pair(qr(addresses = listOf("::1", "127.0.0.1"))) {}
        assertEquals("127.0.0.1", paired.addresses.first())
    }

    // Outcomes ----------------------------------------------------------------------------------

    @Test
    fun denyEndsPairingAndStoresNothing() {
        host.polls += FakePairingHost.json(202, """{"state":"pending"}""")
        host.polls += FakePairingHost.DENIED
        assertEquals(PairingFailure.Kind.DENIED, failureOf(manual()).kind)
        assertTrue(runBlocking { repo.hosts.first() }.isEmpty())
    }

    @Test
    fun anExpiredRequestEndsPairing() {
        host.polls += FakePairingHost.EXPIRED
        assertEquals(PairingFailure.Kind.EXPIRED, failureOf(manual()).kind)
    }

    @Test
    fun noDecisionBeforeTheDeadlineIsAnExpiry() {
        val failure = failureOf(manual(), service(pollIntervalMs = 10, approvalTimeoutMs = 150))
        assertEquals(PairingFailure.Kind.EXPIRED, failure.kind)
        assertTrue("it polled while waiting", host.seen.count { it.path == "/v1/pair/pair_test" } >= 2)
    }

    @Test
    fun aBriefHostErrorWhilePollingDoesNotAbandonPairing() = runBlocking {
        repeat(3) { host.polls += FakePairingHost.json(500, FakePairingHost.error("internal", "Internal error.")) }
        host.polls += FakePairingHost.approved(host.pin.hostId)
        val paired = service().pair(manual()) {}
        assertEquals(host.pin.hostId, paired.id)
    }

    @Test
    fun aHostThatKeepsFailingEndsPairing() {
        repeat(20) { host.polls += FakePairingHost.json(500, FakePairingHost.error("internal", "Internal error.")) }
        assertEquals(PairingFailure.Kind.OTHER, failureOf(manual()).kind)
    }

    @Test
    fun startRefusalsMapToTheirOwnFailures() {
        fun kindFor(code: Int, error: String, retryMs: Long = 0, message: String = "x"): PairingFailure {
            host.startOverride = FakePairingHost.json(code, FakePairingHost.error(error, message, retryMs))
            return failureOf(manual())
        }
        assertEquals(PairingFailure.Kind.CLOSED, kindFor(404, "pairing_closed").kind)
        kindFor(429, "pairing_rate_limited", retryMs = 30_000).also {
            assertEquals(PairingFailure.Kind.RATE_LIMITED, it.kind)
            assertEquals(30_000, it.retryAfterMs)
        }
        assertEquals(PairingFailure.Kind.IN_PROGRESS, kindFor(409, "pairing_in_progress").kind)
        kindFor(403, "forbidden", message = "This pairing code is not valid.").also {
            assertEquals(PairingFailure.Kind.CODE_REJECTED, it.kind)
            assertEquals("This pairing code is not valid.", it.detail)
        }
        assertEquals(PairingFailure.Kind.OTHER, kindFor(400, "bad_host").kind)
        assertTrue(runBlocking { repo.hosts.first() }.isEmpty())
    }

    @Test
    fun aMalformedHostAnswerIsIncompatible() {
        host.startOverride = FakePairingHost.json(200, """{"pairing_id":"p","sas":"123 456","host_nonce":"not-a-nonce","expires_at":"x"}""")
        assertEquals(PairingFailure.Kind.INCOMPATIBLE, failureOf(manual()).kind)
        host.startOverride = FakePairingHost.json(200, "not json")
        assertEquals(PairingFailure.Kind.INCOMPATIBLE, failureOf(manual()).kind)
    }

    @Test
    fun anApprovalForAnotherHostIdIsNotStored() {
        host.polls += FakePairingHost.approved(TestHosts.pinOf(TestHosts.selfSigned()).hostId)
        assertEquals(PairingFailure.Kind.WRONG_COMPUTER, failureOf(manual()).kind)
        assertTrue(runBlocking { repo.hosts.first() }.isEmpty())
        assertNull(repo.token(host.pin.hostId))
    }

    @Test
    fun aComputerThatIsNotThereIsUnreachable() {
        val failure = failureOf(manual(port = closedPort()))
        assertEquals(PairingFailure.Kind.UNREACHABLE, failure.kind)
        assertTrue(failure.canRetrySameTarget)
    }

    @Test
    fun aStorageFailureIsReportedAsSuch() {
        host.polls += FakePairingHost.approved(host.pin.hostId)
        val failing = object : HostRepository by repo {
            override suspend fun add(host: PairedHost, token: BearerToken) = throw IllegalStateException("disk full")
        }
        val failure = failureOf(manual(), service(hosts = failing))
        assertEquals(PairingFailure.Kind.STORAGE, failure.kind)
        assertNotNull(failure.cause)
        assertFalse(failure.message.orEmpty().contains(FakePairingHost.TOKEN))
    }

    @Test
    fun anUnexpectedExceptionLeavesAsAPairingFailureNotARawException() {
        host.polls += FakePairingHost.approved(host.pin.hostId)
        val failure = runBlocking {
            try {
                service().pair(manual()) { throw IllegalStateException("https://127.0.0.1/v1/pair secret") }
                fail("pairing should have failed")
                throw AssertionError()
            } catch (e: PairingFailure) {
                e
            }
        }
        assertEquals(PairingFailure.Kind.APP_ERROR, failure.kind)
        assertEquals("IllegalStateException", failure.detail)
        assertFalse(failure.message.orEmpty().contains("secret"))
    }

    @Test
    fun noFailureMessageCarriesTheSecretOrTheToken() {
        host.startOverride = FakePairingHost.json(403, FakePairingHost.error("forbidden", "This pairing code is not valid."))
        val failure = failureOf(qr())
        val text = failure.toString() + failure.message + failure.detail + failure.cause?.message
        assertFalse(text.contains(SECRET))
        assertFalse(text.contains(FakePairingHost.TOKEN))
    }

    private fun closedPort(): Int = ServerSocket(0, 1, TestHosts.LOOPBACK).use { it.localPort }

    private companion object {
        /** 32 bytes of 0x01, base64url: an obvious placeholder, not a real window secret. */
        val SECRET: String = Base64Url.encode(ByteArray(32) { 1 })
    }
}
