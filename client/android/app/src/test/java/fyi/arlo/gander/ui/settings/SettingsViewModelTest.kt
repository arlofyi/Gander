package fyi.arlo.gander.ui.settings

import fyi.arlo.gander.core.data.actions.FileAction
import fyi.arlo.gander.core.data.actions.InMemoryDefaultActionPrefs
import fyi.arlo.gander.core.data.actions.NetworkState
import fyi.arlo.gander.core.data.actions.StreamOnlyTypes
import fyi.arlo.gander.core.data.diagnostics.DiagnosticsCollector
import fyi.arlo.gander.core.data.diagnostics.DiagnosticsDevice
import fyi.arlo.gander.core.data.fetch.VerifyPolicy
import fyi.arlo.gander.core.data.hosts.InMemoryHostRepository
import fyi.arlo.gander.core.data.settings.AppSettings
import fyi.arlo.gander.core.data.settings.InMemorySettings
import fyi.arlo.gander.testing.MainDispatcherRule
import fyi.arlo.gander.testing.SampleData
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

@OptIn(ExperimentalCoroutinesApi::class)
class SettingsViewModelTest {
    @get:Rule
    val main = MainDispatcherRule()

    private val settings = InMemorySettings()
    private val actions = InMemoryDefaultActionPrefs()
    private val studio = SampleData.host(name = "studio-pc")
    private val hosts = InMemoryHostRepository(listOf(studio))
    private var failReport = false

    private val diagnostics = DiagnosticsCollector(
        device = { DiagnosticsDevice("1.0", "16", 36, "Google", "Pixel") },
        settings = { settings.current() },
        network = { NetworkState.UNMETERED },
        privateNetworkUp = { true },
        hosts = hosts,
        cachedFiles = { emptyList() },
        jobs = { if (failReport) error("boom") else emptyList() },
        derivedCacheBytes = { 0 },
        freeBytes = { 1 },
    )

    private fun viewModel() = SettingsViewModel(settings, actions, hosts, diagnostics)

    private fun TestScope.collect(vm: SettingsViewModel) {
        backgroundScope.launch { vm.uiState.collect {} }
        runCurrent()
    }

    @Test
    fun theStateShowsTheStoredSettingsTheRememberedActionsAndThePairedComputers() = runTest {
        actions.set("pdf", FileAction.PREVIEW)
        actions.set("blend", FileAction.STREAM)
        val vm = viewModel()
        assertFalse("nothing is shown as loaded before the first read", vm.uiState.value.loaded)
        collect(vm)

        val state = vm.uiState.value
        assertTrue(state.loaded)
        assertEquals(AppSettings(), state.settings)
        assertEquals(listOf(DefaultActionRow("blend", FileAction.STREAM), DefaultActionRow("pdf", FileAction.PREVIEW)), state.defaultActions)
        assertEquals(listOf("studio-pc"), state.hosts.map { it.name })
    }

    @Test
    fun everyChangeIsWrittenAtOnceAndComesBackThroughTheState() = runTest {
        val vm = viewModel()
        collect(vm)

        vm.setVerify(VerifyPolicy.NEVER)
        vm.setCellularWarnBytes(AppSettings.CELLULAR_NEVER)
        vm.setCacheCapBytes(5L * 1024 * 1024 * 1024)
        vm.setAppLockEnabled(true)
        vm.setAppLockMinutes(15)
        runCurrent()

        val s = vm.uiState.value.settings
        assertEquals(VerifyPolicy.NEVER, s.verify)
        assertEquals(AppSettings.CELLULAR_NEVER, s.cellularWarnBytes)
        assertEquals(5L * 1024 * 1024 * 1024, s.cacheCapBytes)
        assertTrue(s.appLockEnabled)
        assertEquals(15, s.appLockMinutes)
        // and the repository (what the fetch engine reads) has them
        assertEquals(s, settings.current())
    }

    @Test
    fun streamFirstTypesAreParsedFromTheTextAndCanBeReset() = runTest {
        val vm = viewModel()
        collect(vm)

        vm.setStreamOnlyTypes("PSD, .blend  *.ai nonsense/../")
        runCurrent()
        // "nonsense/../" is not an extension and is dropped, not half-kept
        assertEquals(setOf("psd", "blend", "ai"), vm.uiState.value.settings.streamOnlyTypes)

        vm.setStreamOnlyTypes(null)
        runCurrent()
        assertEquals(StreamOnlyTypes.DEFAULT, vm.uiState.value.settings.streamOnlyTypes)
    }

    @Test
    fun rememberedActionsCanBeChangedForgottenOneByOneAndAllAtOnce() = runTest {
        actions.set("pdf", FileAction.PREVIEW)
        actions.set("zip", FileAction.FETCH)
        val vm = viewModel()
        collect(vm)

        vm.setDefaultAction("pdf", FileAction.FETCH)
        runCurrent()
        assertEquals(FileAction.FETCH, vm.uiState.value.defaultActions.first { it.ext == "pdf" }.action)

        vm.resetDefaultAction("zip")
        runCurrent()
        assertEquals(listOf("pdf"), vm.uiState.value.defaultActions.map { it.ext })

        vm.resetAllDefaultActions()
        runCurrent()
        assertTrue(vm.uiState.value.defaultActions.isEmpty())
    }

    @Test
    fun exportingBuildsARedactedReportAndHandsItOverOnce() = runTest {
        val vm = viewModel()
        collect(vm)

        vm.exportDiagnostics()
        runCurrent()

        val ready = vm.uiState.value.export as DiagnosticsExport.Ready
        assertTrue(ready.text, ready.text.startsWith("Gander diagnostics"))
        assertFalse("the computer's name never reaches the report", ready.text.contains("studio-pc"))
        assertFalse(ready.text.contains("100.101.102.103"))

        vm.exportHandled()
        runCurrent()
        assertEquals(DiagnosticsExport.Idle, vm.uiState.value.export)
    }

    @Test
    fun aFailedExportIsReportedAndCanBeDismissed() = runTest {
        failReport = true
        val vm = viewModel()
        collect(vm)

        vm.exportDiagnostics()
        runCurrent()
        assertEquals(DiagnosticsExport.Failed, vm.uiState.value.export)

        vm.exportHandled()
        runCurrent()
        assertEquals(DiagnosticsExport.Idle, vm.uiState.value.export)
    }
}
