package fyi.arlo.gander.ui.settings

import fyi.arlo.gander.core.data.UiError
import fyi.arlo.gander.core.data.UiErrorKind
import fyi.arlo.gander.core.data.browse.InMemoryBrowserPrefs
import fyi.arlo.gander.core.data.cache.FakeDerivedCaches
import fyi.arlo.gander.core.data.hosts.HostForgetter
import fyi.arlo.gander.core.data.hosts.InMemoryHostRepository
import fyi.arlo.gander.core.data.hosts.TokenRotator
import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.testing.MainDispatcherRule
import fyi.arlo.gander.testing.SampleData
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import java.io.IOException

@OptIn(ExperimentalCoroutinesApi::class)
class HostDetailsViewModelTest {
    @get:Rule
    val main = MainDispatcherRule()

    private val studio = SampleData.host(name = "studio-pc")
    private val hosts = InMemoryHostRepository(listOf(studio))
    private val caches = FakeDerivedCaches(size = 10)
    private val purged = mutableListOf<String>()
    private val forgetter = HostForgetter(hosts, caches, InMemoryBrowserPrefs(), purgeFetched = { purged += it })
    private var rotation: suspend () -> Unit = {}
    private var rotatedFor: String? = null
    private val rotator = TokenRotator { host ->
        rotatedFor = host.id
        rotation()
    }

    private fun viewModel(hostId: String = studio.id) = HostDetailsViewModel(hostId, hosts, rotator, forgetter)

    private fun TestScope.collect(vm: HostDetailsViewModel) {
        backgroundScope.launch { vm.uiState.collect {} }
        runCurrent()
    }

    @Test
    fun theComputerIsShownOnceLoaded() = runTest {
        val vm = viewModel()
        assertFalse(vm.uiState.value.loaded)
        collect(vm)
        assertTrue(vm.uiState.value.loaded)
        assertEquals("studio-pc", vm.uiState.value.host?.name)
        assertFalse(vm.uiState.value.forgotten)
    }

    @Test
    fun rotatingShowsProgressThenSuccess() = runTest {
        val gate = CompletableDeferred<Unit>()
        rotation = { gate.await() }
        val vm = viewModel()
        collect(vm)

        vm.rotateToken()
        runCurrent()
        assertEquals(RotateOutcome.Working, vm.uiState.value.rotate)
        assertEquals(studio.id, rotatedFor)

        // a second press while one is running does nothing
        rotatedFor = null
        vm.rotateToken()
        runCurrent()
        assertNull(rotatedFor)

        gate.complete(Unit)
        runCurrent()
        assertEquals(RotateOutcome.Done, vm.uiState.value.rotate)

        vm.dismissRotateMessage()
        runCurrent()
        assertEquals(RotateOutcome.None, vm.uiState.value.rotate)
    }

    @Test
    fun rotationFailuresSayWhy() = runTest {
        val vm = viewModel()
        collect(vm)

        rotation = { throw GanderError.Unreachable(listOf("100.64.0.1"), null) }
        vm.rotateToken()
        runCurrent()
        assertEquals(RotateOutcome.Failed(UiError(UiErrorKind.OFFLINE)), vm.uiState.value.rotate)

        rotation = { throw GanderError.PinMismatch(SampleData.pin(), null, null) }
        vm.rotateToken()
        runCurrent()
        assertEquals(RotateOutcome.Failed(UiError(UiErrorKind.WRONG_COMPUTER)), vm.uiState.value.rotate)

        rotation = { throw IOException("disk full") }
        vm.rotateToken()
        runCurrent()
        assertEquals(RotateOutcome.Failed(UiError(UiErrorKind.OTHER)), vm.uiState.value.rotate)
    }

    @Test
    fun forgettingRemovesTheComputerAndEverythingSavedFromItAndClosesTheScreen() = runTest {
        val vm = viewModel()
        collect(vm)

        vm.forget()
        runCurrent()

        assertEquals(listOf(studio.id), purged)
        assertEquals(1, caches.clears)
        assertNull(hosts.get(studio.id))
        assertTrue(vm.uiState.value.forgotten)
        assertNull(vm.uiState.value.host)
    }

    @Test
    fun aComputerForgottenElsewhereClosesTheScreen() = runTest {
        val vm = viewModel()
        collect(vm)
        assertNotNull(vm.uiState.value.host)

        hosts.forget(studio.id)
        runCurrent()

        assertTrue(vm.uiState.value.forgotten)
    }

    @Test
    fun anUnknownComputerIsTreatedAsGone() = runTest {
        val vm = viewModel(hostId = "nobody")
        collect(vm)
        assertTrue(vm.uiState.value.loaded)
        assertTrue(vm.uiState.value.forgotten)
        // and rotating it does nothing
        vm.rotateToken()
        runCurrent()
        assertNull(rotatedFor)
    }
}
