package fyi.arlo.gander.ui.hosts

import fyi.arlo.gander.core.data.UiError
import fyi.arlo.gander.core.data.UiErrorKind
import fyi.arlo.gander.core.data.hosts.HostProber
import fyi.arlo.gander.core.data.hosts.HostStatus
import fyi.arlo.gander.core.data.hosts.InMemoryHostRepository
import fyi.arlo.gander.core.data.hosts.PairedHost
import fyi.arlo.gander.core.data.hosts.PrivateNetworkMonitor
import fyi.arlo.gander.core.net.protocol.RelayPath
import fyi.arlo.gander.testing.MainDispatcherRule
import fyi.arlo.gander.testing.SampleData
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

@OptIn(ExperimentalCoroutinesApi::class)
class HostsViewModelTest {
    @get:Rule
    val main = MainDispatcherRule()

    private class FakeProber(private val repo: InMemoryHostRepository) : HostProber {
        val results = mutableMapOf<String, HostStatus>()
        val probed = mutableListOf<String>()
        var latencyMs = 0L

        override suspend fun probe(host: PairedHost): HostStatus {
            probed += host.id
            if (latencyMs > 0) delay(latencyMs)
            val status = results[host.id] ?: HostStatus.Online()
            // like the real prober, a successful probe updates the stored host (last seen)
            if (status is HostStatus.Online) repo.recordContact(host.id, atMillis = 77L)
            return status
        }
    }

    private class FakeNetwork(val up: MutableStateFlow<Boolean> = MutableStateFlow(false)) : PrivateNetworkMonitor {
        override val vpnActive: Flow<Boolean> = up
    }

    private val studio = SampleData.host(name = "studio-pc")
    private val laptop = SampleData.host(name = "laptop", id = "laptop-id")
    private val repo = InMemoryHostRepository(listOf(studio))
    private val prober = FakeProber(repo)
    private val network = FakeNetwork()

    private fun viewModel() = HostsViewModel(repo, prober, network, pingIntervalMs = 20_000)

    private fun kotlinx.coroutines.test.TestScope.collectState(vm: HostsViewModel) {
        backgroundScope.launch { vm.uiState.collect {} }
    }

    @Test
    fun hostsStartAsCheckingUntilTheFirstProbeLands() = runTest {
        val vm = viewModel()
        collectState(vm)
        runCurrent()
        val state = vm.uiState.value
        assertTrue(state.loaded)
        assertEquals(listOf("studio-pc"), state.cards.map { it.host.name })
        assertEquals(HostStatus.Checking, state.cards.single().status)
    }

    @Test
    fun theEmptyStateIsOnlyReportedOnceTheListHasLoaded() = runTest {
        val vm = HostsViewModel(InMemoryHostRepository(), prober, network)
        assertFalse(vm.uiState.value.loaded)
        collectState(vm)
        runCurrent()
        assertTrue(vm.uiState.value.loaded)
        assertTrue(vm.uiState.value.cards.isEmpty())
    }

    @Test
    fun probesEvery20SecondsWhileVisible() = runTest {
        val vm = viewModel()
        collectState(vm)
        backgroundScope.launch { vm.pingWhileVisible() }
        runCurrent()
        assertEquals(1, prober.probed.size)

        advanceTimeBy(19_999)
        runCurrent()
        assertEquals(1, prober.probed.size)
        advanceTimeBy(2)
        runCurrent()
        assertEquals(2, prober.probed.size)
        advanceTimeBy(20_000)
        runCurrent()
        assertEquals(3, prober.probed.size)
    }

    @Test
    fun aProbeThatUpdatesTheStoredHostDoesNotRestartTheLoop() = runTest {
        val vm = viewModel()
        collectState(vm)
        backgroundScope.launch { vm.pingWhileVisible() }
        runCurrent()
        // the probe recorded last-seen, which re-emits the host list; that must not trigger another probe
        runCurrent()
        assertEquals(1, prober.probed.size)
        assertEquals(77L, repo.get(studio.id)!!.lastSeenAtMillis)
    }

    @Test
    fun aNewlyPairedHostIsProbedAtOnce() = runTest {
        val vm = viewModel()
        collectState(vm)
        backgroundScope.launch { vm.pingWhileVisible() }
        runCurrent()
        repo.add(laptop, fyi.arlo.gander.core.net.BearerToken("t"))
        runCurrent()
        assertTrue("laptop-id" in prober.probed)
        assertEquals(2, vm.uiState.value.cards.size)
    }

    @Test
    fun statusReachesTheCardWithRelayAndOfflineStates() = runTest {
        repo.add(laptop, fyi.arlo.gander.core.net.BearerToken("t"))
        prober.results[studio.id] = HostStatus.Online(relay = RelayPath.DERP)
        prober.results[laptop.id] = HostStatus.Unavailable(UiError(UiErrorKind.OFFLINE))
        val vm = viewModel()
        collectState(vm)
        backgroundScope.launch { vm.pingWhileVisible() }
        runCurrent()

        val byName = vm.uiState.value.cards.associate { it.host.name to it.status }
        assertTrue((byName.getValue("studio-pc") as HostStatus.Online).isRelayed)
        assertEquals(UiErrorKind.OFFLINE, (byName.getValue("laptop") as HostStatus.Unavailable).error.kind)
    }

    @Test
    fun hostsAreProbedInParallelAndEachStatusLandsWhenItAnswers() = runTest {
        repo.add(laptop, fyi.arlo.gander.core.net.BearerToken("t"))
        prober.latencyMs = 5_000
        val vm = viewModel()
        collectState(vm)
        backgroundScope.launch { vm.pingWhileVisible() }
        runCurrent()
        assertEquals(2, prober.probed.size) // both started before either finished
        advanceTimeBy(5_001)
        runCurrent()
        assertTrue(vm.uiState.value.cards.all { it.status is HostStatus.Online })
    }

    @Test
    fun pullToRefreshProbesNowAndShowsTheSpinnerWhileItRuns() = runTest {
        prober.latencyMs = 1_000
        val vm = viewModel()
        collectState(vm)
        runCurrent()
        vm.refresh()
        runCurrent()
        assertTrue(vm.uiState.value.refreshing)
        advanceTimeBy(1_001)
        runCurrent()
        assertFalse(vm.uiState.value.refreshing)
        assertEquals(1, prober.probed.size)
    }

    @Test
    fun renameAndForgetGoThroughTheRepository() = runTest {
        repo.add(laptop, fyi.arlo.gander.core.net.BearerToken("t"))
        val vm = viewModel()
        collectState(vm)
        runCurrent()
        vm.rename(laptop.id, "Office laptop")
        runCurrent()
        assertEquals(listOf("studio-pc", "Office laptop"), vm.uiState.value.cards.map { it.host.name })
        vm.forget(laptop.id)
        runCurrent()
        assertEquals(listOf("studio-pc"), vm.uiState.value.cards.map { it.host.name })
        assertEquals(null, repo.token(laptop.id))
    }

    @Test
    fun thePrivateNetworkHintFollowsTheVpnState() = runTest {
        val vm = viewModel()
        collectState(vm)
        runCurrent()
        assertFalse(vm.uiState.value.privateNetworkUp)
        network.up.value = true
        runCurrent()
        assertTrue(vm.uiState.value.privateNetworkUp)
    }
}
