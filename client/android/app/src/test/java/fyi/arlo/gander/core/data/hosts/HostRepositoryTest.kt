package fyi.arlo.gander.core.data.hosts

import fyi.arlo.gander.core.net.BearerToken
import fyi.arlo.gander.core.net.protocol.RelayPath
import fyi.arlo.gander.testing.SampleData
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Test

class HostRepositoryTest {
    private val token = BearerToken("test-token-not-a-secret")

    @Test
    fun addStoresTheHostAndItsTokenAndForgetRemovesBoth() = runBlocking {
        val repo = InMemoryHostRepository()
        repo.add(SampleData.host(), token)
        assertEquals(listOf("studio-pc"), repo.hosts.first().map { it.name })
        assertEquals(token, repo.token(SampleData.HOST_ID))

        repo.forget(SampleData.HOST_ID)
        assertEquals(emptyList<PairedHost>(), repo.hosts.first())
        assertNull(repo.token(SampleData.HOST_ID))
        assertNull(repo.get(SampleData.HOST_ID))
    }

    @Test
    fun renameTrimsAndIgnoresBlankNames() = runBlocking {
        val repo = InMemoryHostRepository(listOf(SampleData.host()))
        repo.rename(SampleData.HOST_ID, "  Office PC ")
        assertEquals("Office PC", repo.get(SampleData.HOST_ID)!!.name)
        repo.rename(SampleData.HOST_ID, "   ")
        assertEquals("Office PC", repo.get(SampleData.HOST_ID)!!.name)
    }

    @Test
    fun rotatingTheTokenReplacesItAtOnce() = runBlocking {
        val repo = InMemoryHostRepository()
        repo.add(SampleData.host(), token)
        repo.updateToken(SampleData.HOST_ID, BearerToken("rotated"))
        assertEquals(BearerToken("rotated"), repo.token(SampleData.HOST_ID))
    }

    @Test
    fun recordContactWithMeRefreshesAddressesAndCapabilitiesButKeepsTheUserName() = runBlocking {
        val repo = InMemoryHostRepository(listOf(SampleData.host(name = "My PC", pairedAs = null)))
        repo.recordContact(SampleData.HOST_ID, atMillis = 42L, me = SampleData.me(deviceName = "Tablet", hostName = "studio-pc"))
        val host = repo.get(SampleData.HOST_ID)!!
        assertEquals("My PC", host.name) // the host calls itself studio-pc; the rename on the phone stays
        assertEquals(listOf("100.101.102.103", "studio-pc.tail1234.ts.net"), host.addresses)
        assertEquals("Tablet", host.pairedAs)
        assertEquals(42L, host.lastSeenAtMillis)
        assertNotNull(host.capabilities)
    }

    @Test
    fun recordContactWithoutMeOnlyStampsLastSeen() = runBlocking {
        val repo = InMemoryHostRepository(listOf(SampleData.host()))
        repo.recordContact(SampleData.HOST_ID, atMillis = 7L)
        val host = repo.get(SampleData.HOST_ID)!!
        assertEquals(7L, host.lastSeenAtMillis)
        assertNull(host.capabilities)
        assertEquals(listOf("100.101.102.103", "fd7a:115c:a1e0::1"), host.addresses)
    }

    @Test
    fun theHostNeverPrintsItsToken() {
        val host = SampleData.host()
        assertFalse(host.toString().contains("token"))
        assertFalse(host.toString().contains(token.value))
    }

    @Test
    fun relayedPathsAreDerpAndPeerRelayOnly() {
        assertEquals(false, HostStatus.Online(relay = RelayPath.DIRECT).isRelayed)
        assertEquals(false, HostStatus.Online(relay = null).isRelayed)
        assertEquals(true, HostStatus.Online(relay = RelayPath.DERP).isRelayed)
        assertEquals(true, HostStatus.Online(relay = RelayPath.PEER_RELAY).isRelayed)
    }
}
