package fyi.arlo.gander.ui.browser

import fyi.arlo.gander.core.data.UiErrorKind
import fyi.arlo.gander.core.data.browse.DefaultBrowseRepository
import fyi.arlo.gander.core.data.browse.InMemorySnapshotStore
import fyi.arlo.gander.core.data.hosts.InMemoryHostRepository
import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.protocol.ServerEvent
import fyi.arlo.gander.testing.FakeHostConnector
import fyi.arlo.gander.testing.MainDispatcherRule
import fyi.arlo.gander.testing.SampleData
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

@OptIn(ExperimentalCoroutinesApi::class)
class SharesViewModelTest {
    @get:Rule
    val main = MainDispatcherRule()

    private val host = SampleData.host()
    private val connector = FakeHostConnector()
    private val api get() = connector.api
    private val snapshots = InMemorySnapshotStore()
    private var now = 1_000L
    private val repository = DefaultBrowseRepository(
        connector,
        snapshots,
        snapshots,
        clock = { now },
        reconnectBaseMs = 1_000,
    )
    private val hosts = InMemoryHostRepository(listOf(host))

    private fun viewModel() = SharesViewModel(host.id, hosts, repository)

    @Test
    fun listsTheSharesOfTheHost() = runTest {
        api.shares = listOf(SampleData.share(name = "Projects"), SampleData.share(id = "shr_b", name = "Photos"))
        val state = viewModel().uiState.value
        assertEquals("studio-pc", state.hostName)
        assertEquals(listOf("Projects", "Photos"), state.shares.map { it.name })
        assertFalse(state.loading)
        assertNull(state.cachedAtMillis)
        assertNull(state.error)
    }

    @Test
    fun anUnreachableHostShowsTheStoredSharesWithTheirAge() = runTest {
        viewModel() // stores the shares at now = 1_000
        now = 50_000
        api.failure = GanderError.Unreachable(host.addresses, null)
        val state = viewModel().uiState.value
        assertEquals(listOf("Projects"), state.shares.map { it.name })
        assertEquals(1_000L, state.cachedAtMillis)
        assertNull(state.error)
    }

    @Test
    fun offlineWithNothingStoredIsAnExplainedError() = runTest {
        api.failure = GanderError.Unreachable(host.addresses, null)
        val state = viewModel().uiState.value
        assertTrue(state.shares.isEmpty())
        assertFalse(state.loading)
        assertEquals(UiErrorKind.OFFLINE, state.error?.kind)
    }

    @Test
    fun aRevokedTokenIsExplainedNotHidden() = runTest {
        viewModel()
        api.failure = GanderError.fromResponse(401, """{"error":{"code":"token_revoked","message":"m","retry_after_ms":0}}""")
        val vm = viewModel()
        assertEquals(UiErrorKind.REVOKED, vm.uiState.value.error?.kind)
        // the stored list is not shown as if nothing happened
        assertEquals(1, vm.uiState.value.shares.size) // the instant snapshot, with the error beside it
    }

    @Test
    fun sharesUpdatedReloadsTheList() = runTest {
        val vm = viewModel()
        backgroundScope.launch { vm.watchEvents() }
        runCurrent()
        assertEquals(1, api.sharesCalls)

        api.shares = listOf(SampleData.share(name = "Projects"), SampleData.share(id = "shr_new", name = "New share"))
        connector.events.emit(ServerEvent.SharesUpdated)
        runCurrent()
        assertEquals(2, api.sharesCalls)
        assertEquals(listOf("Projects", "New share"), vm.uiState.value.shares.map { it.name })
    }

    @Test
    fun pullToRefreshReloads() = runTest {
        val vm = viewModel()
        api.shares = listOf(SampleData.share(name = "Renamed"))
        vm.refresh()
        assertEquals(listOf("Renamed"), vm.uiState.value.shares.map { it.name })
        assertFalse(vm.uiState.value.refreshing)
    }

    @Test
    fun aForgottenHostIsReported() = runTest {
        val vm = SharesViewModel(host.id, InMemoryHostRepository(), repository)
        assertEquals(UiErrorKind.NOT_PAIRED, vm.uiState.value.error?.kind)
        assertFalse(vm.uiState.value.loading)
    }

    @Test
    fun aRevokedDeviceEventIsShown() = runTest {
        val vm = viewModel()
        backgroundScope.launch { vm.watchEvents() }
        runCurrent()
        connector.events.emit(ServerEvent.DeviceRevoked)
        runCurrent()
        assertEquals(UiErrorKind.REVOKED, vm.uiState.value.error?.kind)
    }
}
