package fyi.arlo.gander.ui.filesheet

import fyi.arlo.gander.core.data.UiError
import fyi.arlo.gander.core.data.UiErrorKind
import fyi.arlo.gander.core.data.actions.ActionHint
import fyi.arlo.gander.core.data.actions.ActionReason
import fyi.arlo.gander.core.data.actions.ActionWarning
import fyi.arlo.gander.core.data.actions.CachedFileLookup
import fyi.arlo.gander.core.data.actions.ConnectorEntryStatSource
import fyi.arlo.gander.core.data.actions.DefaultSource
import fyi.arlo.gander.core.data.actions.FileAction
import fyi.arlo.gander.core.data.actions.FixedNetworkStateMonitor
import fyi.arlo.gander.core.data.actions.InMemoryActionRefusals
import fyi.arlo.gander.core.data.actions.InMemoryDefaultActionPrefs
import fyi.arlo.gander.core.data.actions.NetworkState
import fyi.arlo.gander.core.data.actions.PhoneHandlerLookup
import fyi.arlo.gander.core.data.browse.DefaultBrowseRepository
import fyi.arlo.gander.core.data.browse.EntryTarget
import fyi.arlo.gander.core.data.browse.InMemorySnapshotStore
import fyi.arlo.gander.core.data.hosts.HostProber
import fyi.arlo.gander.core.data.hosts.HostStatus
import fyi.arlo.gander.core.data.hosts.InMemoryHostRepository
import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.protocol.Entry
import fyi.arlo.gander.core.net.protocol.EntryFlag
import fyi.arlo.gander.core.net.protocol.EntryMeta
import fyi.arlo.gander.core.net.protocol.Permission
import fyi.arlo.gander.core.net.protocol.PdfMeta
import fyi.arlo.gander.core.net.protocol.PreviewKind
import fyi.arlo.gander.core.net.protocol.RelayPath
import fyi.arlo.gander.core.net.protocol.ServerEvent
import fyi.arlo.gander.core.net.protocol.StreamDecision
import fyi.arlo.gander.testing.FakeHostConnector
import fyi.arlo.gander.testing.MainDispatcherRule
import fyi.arlo.gander.testing.SampleData
import fyi.arlo.gander.testing.StatCall
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

@OptIn(ExperimentalCoroutinesApi::class)
class FileSheetViewModelTest {
    @get:Rule
    val main = MainDispatcherRule()

    private val mib = 1024L * 1024L
    private val host = SampleData.host()
    private val connector = FakeHostConnector()
    private val api get() = connector.api
    private val snapshots = InMemorySnapshotStore()
    private val repository = DefaultBrowseRepository(connector, snapshots, snapshots, reconnectBaseMs = 1_000)
    private val stat = ConnectorEntryStatSource(connector)
    private val prefs = InMemoryDefaultActionPrefs()
    private val refusals = InMemoryActionRefusals()
    private val network = FixedNetworkStateMonitor(NetworkState.UNMETERED)
    private var hosts = InMemoryHostRepository(listOf(host))

    private val pdf = SampleData.file("Report.pdf", size = 2 * mib, preview = PreviewKind.PDF)

    private fun target(entry: Entry = pdf, folder: String = "Clients") =
        EntryTarget(host.id, "shr_projects", "$folder/${entry.name}", entry)

    private fun viewModel(
        entry: Entry = pdf,
        handlers: PhoneHandlerLookup = PhoneHandlerLookup { _, _ -> false },
        cached: CachedFileLookup = CachedFileLookup.None,
        prober: HostProber? = null,
    ): FileSheetViewModel {
        val t = target(entry)
        // The host serves what the browser listed unless a test scripts something else.
        api.folders["Clients"] = listOf(entry)
        return FileSheetViewModel(t, hosts, repository, stat, prefs, network, handlers, cached, prober, refusals)
    }

    // Loading -------------------------------------------------------------------------------------------

    @Test
    fun showsTheListingEntryAtOnceThenTheStatWithMetadata() = runTest {
        val gate = CompletableDeferred<Unit>()
        api.statGate = gate
        val fresh = pdf.copy(size = 3 * mib, meta = EntryMeta(pdf = PdfMeta(pages = 42)))
        api.statHandler = { fresh }
        val vm = viewModel()

        val before = vm.uiState.value
        assertEquals(DetailsState.Loading, before.details)
        assertEquals(2 * mib, before.entry.size)
        assertNull(before.entry.meta)
        // Actions are already usable from the listing's own preview/stream fields.
        assertEquals(FileAction.PREVIEW, before.plan.default)

        gate.complete(Unit)
        runCurrent()
        val after = vm.uiState.value
        assertEquals(DetailsState.Loaded, after.details)
        assertEquals(3 * mib, after.entry.size)
        assertEquals(42, after.entry.meta?.pdf?.pages)
        assertEquals(listOf(StatCall("shr_projects", "Clients/Report.pdf", meta = true)), api.statCalls)
        assertEquals("studio-pc", after.hostName)
    }

    @Test
    fun theSharesNameAndPermissionsFeedTheActions() = runTest {
        api.shares = listOf(SampleData.share(name = "Projects", permissions = listOf(Permission.LIST, Permission.PREVIEW)))
        val state = viewModel().uiState.value
        assertEquals("Projects", state.shareName)
        assertTrue(state.plan[FileAction.PREVIEW].enabled)
        assertEquals(ActionReason.NoPermission(Permission.FETCH), state.plan[FileAction.FETCH].reason)
        assertEquals(ActionReason.NoPermission(Permission.STREAM), state.plan[FileAction.STREAM].reason)
        assertEquals(FileAction.PREVIEW, state.plan[FileAction.FETCH].suggestion)
    }

    @Test
    fun theHostsCapabilitiesFeedTheActions() = runTest {
        val caps = SampleData.me().capabilities
        hosts = InMemoryHostRepository(listOf(host.copy(capabilities = caps.copy(stream = caps.stream.copy(enabled = false)))))
        val blend = SampleData.file("rig.blend", size = 300 * mib)
        val state = viewModel(blend).uiState.value
        assertEquals(ActionReason.StreamNotEnabled, state.plan[FileAction.STREAM].reason)
        assertEquals(FileAction.FETCH, state.plan.default)
    }

    @Test
    fun aStreamOnlyTypeDefaultsToStreamOnceTheCapabilitiesAreKnown() = runTest {
        hosts = InMemoryHostRepository(listOf(host.copy(capabilities = SampleData.me().capabilities)))
        val blend = SampleData.file("rig.blend", size = 300 * mib).copy(stream = StreamDecision.ALLOWED)
        val state = viewModel(blend).uiState.value
        assertEquals(FileAction.STREAM, state.plan.default)
    }

    @Test
    fun aHostThatCannotBeReachedKeepsTheListingEntryAndExplainsEveryAction() = runTest {
        api.failure = GanderError.Unreachable(host.addresses, null)
        val state = viewModel().uiState.value
        assertEquals(UiErrorKind.OFFLINE, (state.details as DetailsState.Failed).error.kind)
        assertEquals("Report.pdf", state.entry.name)
        assertTrue(state.plan.options.all { it.reason == ActionReason.HostUnreachable })
        assertNull(state.plan.default)
    }

    @Test
    fun aFileThatIsGoneIsSaidToBeGone() = runTest {
        api.statHandler = { throw GanderError.fromResponse(404, """{"error":{"code":"not_found","message":"gone","retry_after_ms":0}}""") }
        val state = viewModel().uiState.value
        assertEquals(UiErrorKind.NOT_FOUND, (state.details as DetailsState.Failed).error.kind)
        assertTrue(state.plan.options.all { it.reason == ActionReason.FileGone })
    }

    @Test
    fun aForgottenHostBlocksEverything() = runTest {
        hosts = InMemoryHostRepository(emptyList())
        val state = viewModel().uiState.value
        assertEquals(UiErrorKind.NOT_PAIRED, (state.details as DetailsState.Failed).error.kind)
        assertTrue(state.plan.options.none { it.enabled })
        assertTrue(api.statCalls.isEmpty())
    }

    @Test
    fun retryAsksTheHostAgain() = runTest {
        api.failure = GanderError.Unreachable(host.addresses, null)
        val vm = viewModel()
        assertTrue(vm.uiState.value.details is DetailsState.Failed)
        api.failure = null
        vm.reload()
        runCurrent()
        assertEquals(DetailsState.Loaded, vm.uiState.value.details)
        assertEquals(2, api.statCalls.size)
        assertTrue(vm.uiState.value.plan[FileAction.PREVIEW].enabled)
    }

    // Network -------------------------------------------------------------------------------------------

    @Test
    fun theDefaultFollowsTheMeteredState() = runTest {
        val deck = SampleData.file("Deck.pptx", size = 30 * mib, preview = PreviewKind.OFFICE)
        val vm = viewModel(deck, handlers = { _, ext -> ext == "pptx" })
        assertEquals(FileAction.FETCH, vm.uiState.value.plan.default)
        assertEquals(ActionHint.Fetch(30 * mib, metered = false), vm.uiState.value.plan[FileAction.FETCH].hint)

        network.state = NetworkState.METERED
        runCurrent()
        val metered = vm.uiState.value
        assertEquals(FileAction.PREVIEW, metered.plan.default)
        assertEquals(ActionHint.Fetch(30 * mib, metered = true), metered.plan[FileAction.FETCH].hint)
        assertTrue(metered.network.metered)

        network.state = NetworkState.UNMETERED
        runCurrent()
        assertEquals(FileAction.FETCH, vm.uiState.value.plan.default)
    }

    @Test
    fun aLargeFetchOnAMeteredNetworkCarriesTheCellularWarning() = runTest {
        network.state = NetworkState.METERED
        val zip = SampleData.file("backup.zip", size = 200 * mib).copy(stream = StreamDecision.REFUSED)
        val state = viewModel(zip).uiState.value
        assertEquals(FileAction.FETCH, state.plan.default)
        assertEquals(listOf(ActionWarning.CellularLarge(200 * mib)), state.plan[FileAction.FETCH].warnings)
    }

    @Test
    fun theNetworkComingBackAfterAnOfflineFailureAsksAgainByItself() = runTest {
        api.failure = GanderError.Unreachable(host.addresses, null)
        network.state = NetworkState.OFFLINE
        val vm = viewModel()
        assertEquals(ActionReason.PhoneOffline, vm.uiState.value.plan[FileAction.PREVIEW].reason)

        api.failure = null
        network.state = NetworkState.UNMETERED
        runCurrent()
        assertEquals(DetailsState.Loaded, vm.uiState.value.details)
        assertEquals(2, api.statCalls.size)
        assertTrue(vm.uiState.value.plan[FileAction.PREVIEW].enabled)
    }

    @Test
    fun aSavedCopyOpensWithoutTheNetwork() = runTest {
        api.failure = GanderError.Unreachable(host.addresses, null)
        network.state = NetworkState.OFFLINE
        val vm = viewModel(cached = CachedFileLookup { true })
        val state = vm.uiState.value
        assertTrue(state.cachedLocally)
        assertEquals(FileAction.FETCH, state.plan.default)
        assertEquals(ActionHint.SavedCopy, state.plan[FileAction.FETCH].hint)
    }

    // Remembered default -----------------------------------------------------------------------------------

    @Test
    fun aRememberedChoiceForTheExtensionBecomesTheDefault() = runTest {
        prefs.set("pdf", FileAction.FETCH)
        val state = viewModel().uiState.value
        assertEquals(FileAction.FETCH, state.plan.default)
        assertEquals(DefaultSource.REMEMBERED, state.plan.defaultSource)
        assertEquals(FileAction.FETCH, state.remembered)
        assertEquals("pdf", state.extension)
    }

    @Test
    fun aRememberedChoiceIsPerExtension() = runTest {
        prefs.set("docx", FileAction.FETCH)
        val state = viewModel().uiState.value
        assertNull(state.remembered)
        assertEquals(FileAction.PREVIEW, state.plan.default)
    }

    @Test
    fun tickingRememberStoresTheNextActionTheUserRuns() = runTest {
        val vm = viewModel()
        vm.setRememberChoice(true)
        assertTrue(vm.uiState.value.rememberChoice)

        val opened = vm.onActionClick(FileAction.FETCH)
        assertNotNull(opened)
        assertEquals(FileAction.FETCH, prefs.get("pdf"))
        // The default now follows what was remembered.
        assertEquals(FileAction.FETCH, vm.uiState.value.plan.default)
        assertEquals(DefaultSource.REMEMBERED, vm.uiState.value.plan.defaultSource)
    }

    @Test
    fun runningAnActionWithoutTheTickRemembersNothing() = runTest {
        val vm = viewModel()
        vm.onActionClick(FileAction.FETCH)
        assertNull(prefs.get("pdf"))
    }

    @Test
    fun aDisabledActionDoesNothingAndRemembersNothing() = runTest {
        api.shares = listOf(SampleData.share(permissions = listOf(Permission.LIST, Permission.PREVIEW)))
        val vm = viewModel()
        vm.setRememberChoice(true)
        assertNull(vm.onActionClick(FileAction.FETCH))
        assertNull(prefs.get("pdf"))
    }

    @Test
    fun resetForgetsTheRememberedChoice() = runTest {
        prefs.set("pdf", FileAction.STREAM)
        val vm = viewModel()
        assertEquals(FileAction.STREAM, vm.uiState.value.plan.default)
        vm.resetDefault()
        runCurrent()
        assertNull(vm.uiState.value.remembered)
        assertEquals(FileAction.PREVIEW, vm.uiState.value.plan.default)
        assertNull(prefs.get("pdf"))
    }

    @Test
    fun aFileWithoutAnExtensionCannotBeRemembered() = runTest {
        val readme = SampleData.file("README", preview = PreviewKind.TEXT).copy(ext = null)
        val vm = viewModel(readme)
        assertNull(vm.uiState.value.extension)
        vm.setRememberChoice(true)
        assertNotNull(vm.onActionClick(FileAction.PREVIEW))
        runCurrent()
        assertEquals(emptyMap<String, FileAction>(), prefs.observeAll().first())
    }

    @Test
    fun anActionHandsBackTheTargetWithTheFreshEntry() = runTest {
        val fresh = pdf.copy(size = 9 * mib, meta = EntryMeta(pdf = PdfMeta(pages = 3)))
        api.statHandler = { fresh }
        val vm = viewModel()
        val opened = vm.onActionClick(FileAction.PREVIEW)!!
        assertEquals("Clients/Report.pdf", opened.path)
        assertEquals(host.id, opened.hostId)
        assertEquals("shr_projects", opened.shareId)
        assertEquals(9 * mib, opened.entry.size)
        assertEquals(3, opened.entry.meta?.pdf?.pages)
    }

    // Host state, refusals, events ---------------------------------------------------------------------------

    @Test
    fun aLockedComputerSeenOnTheEventStreamDisablesStreamUntilItUnlocks() = runTest {
        hosts = InMemoryHostRepository(listOf(host.copy(capabilities = SampleData.me().capabilities)))
        val blend = SampleData.file("rig.blend", size = 300 * mib).copy(stream = StreamDecision.ALLOWED)
        val vm = viewModel(blend)
        backgroundScope.launch { vm.watchEvents() }
        runCurrent()
        assertEquals(FileAction.STREAM, vm.uiState.value.plan.default)

        connector.events.emit(ServerEvent.HostState(locked = true, attended = true, onBattery = false, streamAvailable = true))
        runCurrent()
        val locked = vm.uiState.value.plan
        assertEquals(ActionReason.HostLocked, locked[FileAction.STREAM].reason)
        assertEquals(FileAction.FETCH, locked.default)

        connector.events.emit(ServerEvent.HostState(locked = false, attended = true, onBattery = false, streamAvailable = true))
        runCurrent()
        assertEquals(FileAction.STREAM, vm.uiState.value.plan.default)
    }

    @Test
    fun aRefusalReportedByTheStreamViewerIsShownAndClearedWhenTheComputerUnlocks() = runTest {
        val vm = viewModel()
        backgroundScope.launch { vm.watchEvents() }
        runCurrent()

        val refusal = GanderError.fromResponse(412, """{"error":{"code":"host_locked","message":"locked","retry_after_ms":0}}""")
        vm.onActionRefused(FileAction.STREAM, refusal)
        assertEquals(ActionReason.HostLocked, vm.uiState.value.plan[FileAction.STREAM].reason)

        connector.events.emit(ServerEvent.HostState(locked = false, attended = true, onBattery = false, streamAvailable = true))
        runCurrent()
        assertTrue(vm.uiState.value.plan[FileAction.STREAM].enabled)
    }

    @Test
    fun aStreamRefusalCarriesTheHostsOwnWordsAndSurvivesUntilReload() = runTest {
        val vm = viewModel()
        val refusal = GanderError.fromResponse(
            422,
            """{"error":{"code":"stream_refused","message":"Macro-enabled documents are not opened.","retry_after_ms":0}}""",
        )
        vm.onActionRefused(FileAction.STREAM, refusal)
        assertEquals(
            ActionReason.StreamRefused("Macro-enabled documents are not opened."),
            vm.uiState.value.plan[FileAction.STREAM].reason,
        )
        assertEquals(FileAction.PREVIEW, vm.uiState.value.plan[FileAction.STREAM].suggestion)

        vm.reload()
        runCurrent()
        assertTrue(vm.uiState.value.plan[FileAction.STREAM].enabled)
    }

    @Test
    fun aRefusalReportedByAnotherScreenThroughTheSharedLogShowsOnTheSheet() = runTest {
        val vm = viewModel()
        val busy = GanderError.fromResponse(409, """{"error":{"code":"stream_busy","message":"busy","retry_after_ms":0}}""")
        refusals.report(target(), FileAction.STREAM, busy)
        assertEquals(ActionReason.StreamBusy, vm.uiState.value.plan[FileAction.STREAM].reason)
        // Another file is not affected.
        refusals.report(target(SampleData.file("Other.pdf", preview = PreviewKind.PDF)), FileAction.PREVIEW, busy)
        assertTrue(vm.uiState.value.plan[FileAction.PREVIEW].enabled)
    }

    @Test
    fun aRefusalReportedBeforeTheSheetOpenedIsStillThere() = runTest {
        val locked = GanderError.fromResponse(412, """{"error":{"code":"host_locked","message":"locked","retry_after_ms":0}}""")
        refusals.report(target(), FileAction.STREAM, locked)
        val vm = viewModel()
        assertEquals(ActionReason.HostLocked, vm.uiState.value.plan[FileAction.STREAM].reason)
    }

    @Test
    fun aChangeInTheFilesFolderRefreshesItsDetailsQuietly() = runTest {
        val vm = viewModel()
        backgroundScope.launch { vm.watchEvents() }
        runCurrent()
        assertEquals(1, api.statCalls.size)

        api.statHandler = { pdf.copy(size = 7 * mib) }
        connector.events.emit(ServerEvent.ShareChanged("shr_projects", "Clients"))
        runCurrent()
        assertEquals(2, api.statCalls.size)
        assertEquals(7 * mib, vm.uiState.value.entry.size)
        assertEquals(DetailsState.Loaded, vm.uiState.value.details)

        // Another folder or another share does not matter.
        connector.events.emit(ServerEvent.ShareChanged("shr_projects", "Other"))
        connector.events.emit(ServerEvent.ShareChanged("shr_other", "Clients"))
        runCurrent()
        assertEquals(2, api.statCalls.size)
    }

    @Test
    fun sharesUpdatedReloadsThePermissions() = runTest {
        val vm = viewModel()
        backgroundScope.launch { vm.watchEvents() }
        runCurrent()
        assertTrue(vm.uiState.value.plan[FileAction.FETCH].enabled)

        api.shares = listOf(SampleData.share(permissions = listOf(Permission.LIST, Permission.PREVIEW)))
        connector.events.emit(ServerEvent.SharesUpdated)
        runCurrent()
        assertEquals(ActionReason.NoPermission(Permission.FETCH), vm.uiState.value.plan[FileAction.FETCH].reason)
    }

    @Test
    fun aRevokedDeviceBlocksEveryAction() = runTest {
        val vm = viewModel()
        backgroundScope.launch { vm.watchEvents() }
        runCurrent()
        connector.events.emit(ServerEvent.DeviceRevoked)
        runCurrent()
        val state = vm.uiState.value
        assertEquals(UiErrorKind.REVOKED, (state.details as DetailsState.Failed).error.kind)
        assertTrue(state.plan.options.none { it.enabled })
    }

    // Probe and phone handler ---------------------------------------------------------------------------

    @Test
    fun aRelayedConnectionWarnsOnStream() = runTest {
        val prober = HostProber { HostStatus.Online(relay = RelayPath.DERP) }
        val vm = viewModel(prober = prober)
        val state = vm.uiState.value
        assertTrue(state.relayed)
        assertTrue(ActionWarning.RelayedConnection in state.plan[FileAction.STREAM].warnings)
    }

    @Test
    fun anUnreachableProbeChangesNothingByItself() = runTest {
        val prober = HostProber { HostStatus.Unavailable(UiError(UiErrorKind.OFFLINE)) }
        val state = viewModel(prober = prober).uiState.value
        assertFalse(state.relayed)
        assertEquals(DetailsState.Loaded, state.details)
    }

    @Test
    fun thePhoneHandlerLookupIsAskedWithTheHostsMimeAndExtension() = runTest {
        val asked = mutableListOf<Pair<String?, String?>>()
        val deck = SampleData.file("Deck.pptx", size = 4 * mib, preview = PreviewKind.OFFICE)
            .copy(mime = "application/vnd.openxmlformats-officedocument.presentationml.presentation")
        val vm = viewModel(deck, handlers = { mime, ext ->
            asked += mime to ext
            true
        })
        assertEquals(FileAction.FETCH, vm.uiState.value.plan.default)
        assertEquals(
            "application/vnd.openxmlformats-officedocument.presentationml.presentation" to "pptx",
            asked.first(),
        )
    }

    @Test
    fun linksAreNotActionable() = runTest {
        val link = SampleData.file("shortcut.pdf", preview = PreviewKind.PDF, flags = listOf(EntryFlag.LINK))
        val state = viewModel(link).uiState.value
        assertTrue(state.plan.options.all { it.reason == ActionReason.NotAFile })
    }
}
