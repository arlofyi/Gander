package fyi.arlo.gander.core.data.hosts

import fyi.arlo.gander.core.data.UiErrorKind
import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.protocol.HostHello
import fyi.arlo.gander.core.net.protocol.RelayPath
import fyi.arlo.gander.testing.FakeHostConnector
import fyi.arlo.gander.testing.SampleData
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class ConnectorHostProberTest {
    private val host = SampleData.host()
    private val connector = FakeHostConnector()
    private val api get() = connector.api
    private val repo = InMemoryHostRepository(listOf(host))
    private val prober = ConnectorHostProber(connector, repo, clock = { 500L })

    private fun hostError(status: Int, code: String) =
        GanderError.fromResponse(status, """{"error":{"code":"$code","message":"m","retry_after_ms":0}}""")

    @Test
    fun aHostThatAnswersIsOnlineAndRecordsContact() = runBlocking {
        val status = prober.probe(host)
        assertEquals(HostStatus.Online(relay = null), status)
        assertEquals(500L, repo.get(host.id)!!.lastSeenAtMillis)
        assertEquals(1, api.hostCalls)
        assertEquals(1, api.meCalls)
    }

    @Test
    fun theRelayPathComesFromMe() = runBlocking {
        api.me = SampleData.me(relayed = RelayPath.DERP)
        val status = prober.probe(host) as HostStatus.Online
        assertTrue(status.isRelayed)
        assertEquals(RelayPath.DERP, status.relay)
    }

    @Test
    fun anUnreachableHostIsUnavailableOfflineAndLastSeenIsNotTouched() = runBlocking {
        api.failure = GanderError.Unreachable(host.addresses, null)
        val status = prober.probe(host) as HostStatus.Unavailable
        assertEquals(UiErrorKind.OFFLINE, status.error.kind)
        assertNull(repo.get(host.id)!!.lastSeenAtMillis)
        assertEquals(0, api.meCalls)
    }

    @Test
    fun aDifferentHostIdIsTheWrongComputer() = runBlocking {
        api.hello = HostHello(listOf(1), false, "someone-else")
        val status = prober.probe(host) as HostStatus.Unavailable
        assertEquals(UiErrorKind.WRONG_COMPUTER, status.error.kind)
    }

    @Test
    fun aPinMismatchOnTheHandshakeIsTheWrongComputer() = runBlocking {
        api.failure = GanderError.PinMismatch(host.pin, null, null)
        assertEquals(
            UiErrorKind.WRONG_COMPUTER,
            (prober.probe(host) as HostStatus.Unavailable).error.kind,
        )
    }

    @Test
    fun aRevokedTokenOnMeMakesTheHostUnavailable() = runBlocking {
        api.meFailure = hostError(401, "token_revoked")
        val status = prober.probe(host) as HostStatus.Unavailable
        assertEquals(UiErrorKind.REVOKED, status.error.kind)
        // The Hosts screen shows "Removed on the computer" with Pair again / Forget for it.
        assertEquals(true, status.isRemovedOnHost)
        assertEquals(false, HostStatus.Unavailable(fyi.arlo.gander.core.data.UiError(UiErrorKind.OFFLINE)).isRemovedOnHost)
    }

    @Test
    fun aHostThatRefusesMeForAnotherReasonIsStillOnlineWithANotice() = runBlocking {
        api.meFailure = hostError(403, "network_identity_mismatch")
        val status = prober.probe(host) as HostStatus.Online
        assertEquals(UiErrorKind.IDENTITY_MISMATCH, status.notice!!.kind)
        assertEquals(500L, repo.get(host.id)!!.lastSeenAtMillis) // it did answer
    }
}
