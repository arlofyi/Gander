package fyi.arlo.gander.core.data.actions

import fyi.arlo.gander.core.data.UiErrorKind
import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.protocol.Capabilities
import fyi.arlo.gander.core.net.protocol.Entry
import fyi.arlo.gander.core.net.protocol.EntryFlag
import fyi.arlo.gander.core.net.protocol.FetchCapabilities
import fyi.arlo.gander.core.net.protocol.HashAlgo
import fyi.arlo.gander.core.net.protocol.OfficePreview
import fyi.arlo.gander.core.net.protocol.Permission
import fyi.arlo.gander.core.net.protocol.PreviewKind
import fyi.arlo.gander.core.net.protocol.StreamDecision
import fyi.arlo.gander.testing.SampleData
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** The default-action rule table (client spec) as a pure function: no Android, no coroutines. */
class ActionRulesTest {
    private val rules = ActionRules()
    private val mib = 1024L * 1024L

    private val caps: Capabilities = SampleData.me().capabilities

    private fun file(
        name: String,
        size: Long = 1 * mib,
        preview: PreviewKind = PreviewKind.NONE,
        stream: StreamDecision = StreamDecision.ALLOWED,
        flags: List<EntryFlag> = emptyList(),
    ): Entry = SampleData.file(name, size = size, preview = preview, flags = flags).copy(stream = stream)

    private fun plan(
        entry: Entry,
        network: NetworkState = NetworkState.UNMETERED,
        handler: Boolean = false,
        capabilities: Capabilities? = caps,
        permissions: Set<Permission>? = setOf(Permission.LIST, Permission.PREVIEW, Permission.FETCH, Permission.STREAM),
        remembered: FileAction? = null,
        cached: Boolean = false,
        hostState: HostStateInfo? = null,
        relayed: Boolean = false,
        refusals: Map<FileAction, ActionReason> = emptyMap(),
        fileBlock: ActionReason? = null,
        rules: ActionRules = this.rules,
    ) = rules.plan(
        ActionInput(
            entry = entry,
            capabilities = capabilities,
            permissions = permissions,
            network = network,
            remembered = remembered,
            phoneHasHandler = handler,
            cachedLocally = cached,
            hostState = hostState,
            relayed = relayed,
            refusals = refusals,
            fileBlock = fileBlock,
        ),
    )

    private fun ActionPlan.reasonOf(action: FileAction) = this[action].reason

    // Rule 1: stream-only types ------------------------------------------------------------------

    @Test
    fun aStreamOnlyTypeDefaultsToStreamWhenTheHostStreams() {
        val p = plan(file("rig.blend", size = 300 * mib))
        assertEquals(FileAction.STREAM, p.default)
        assertEquals(DefaultSource.RULES, p.defaultSource)
        assertEquals(FileAction.STREAM, p.options.first().action)
        assertTrue(p[FileAction.STREAM].isDefault)
        assertEquals(ActionHint.Stream, p[FileAction.STREAM].hint)
        // "Preview: no renderer for .blend on studio-pc, try Stream"
        assertEquals(ActionReason.NoRenderer("blend"), p.reasonOf(FileAction.PREVIEW))
        assertEquals(FileAction.STREAM, p[FileAction.PREVIEW].suggestion)
    }

    @Test
    fun theWholeStreamOnlyListFromTheSpecIsShipped() {
        val listed = "blend psd ai indd dwg dxf skp step stp f3d kicad_pcb kicad_sch aep prproj drp als flp logicx " +
            "cpr sln ipynb accdb mpp vsdx max ma mb zpr sbs sbsar uasset unity mat slx"
        assertEquals(listed.split(' ').toSet(), StreamOnlyTypes.DEFAULT)
        for (ext in StreamOnlyTypes.DEFAULT) {
            assertEquals("$ext should default to Stream", FileAction.STREAM, plan(file("x.$ext")).default)
        }
    }

    @Test
    fun streamOnlyTypesWinOverAPhoneHandlerAndAPreview() {
        // ipynb is also text: the table puts Stream first.
        val p = plan(file("lab.ipynb", preview = PreviewKind.TEXT), handler = true)
        assertEquals(FileAction.STREAM, p.default)
    }

    @Test
    fun theExtensionIsMatchedCaseInsensitively() {
        assertEquals(FileAction.STREAM, plan(file("SCENE.BLEND")).default)
    }

    @Test
    fun anUnknownTypeTheHostCannotPreviewAndThePhoneCannotOpenGoesToStream() {
        assertEquals(FileAction.STREAM, plan(file("weird.qzx9")).default)
        assertEquals(FileAction.STREAM, plan(file("README_NO_EXT")).default)
    }

    @Test
    fun anUnknownTypeThePhoneCanOpenIsFetched() {
        assertEquals(FileAction.FETCH, plan(file("weird.qzx9"), handler = true).default)
    }

    @Test
    fun streamOnlyFallsBackWhenTheHostDoesNotStream() {
        val noStream = caps.copy(stream = caps.stream.copy(enabled = false))
        val p = plan(file("rig.blend"), capabilities = noStream)
        assertEquals(FileAction.FETCH, p.default)
        assertEquals(ActionReason.StreamNotEnabled, p.reasonOf(FileAction.STREAM))
        assertEquals(FileAction.FETCH, p[FileAction.STREAM].suggestion)
    }

    @Test
    fun streamOnlyFallsBackWhenPolicyRefusesTheFile() {
        val p = plan(file("rig.blend", stream = StreamDecision.REFUSED))
        assertEquals(FileAction.FETCH, p.default)
        assertEquals(ActionReason.StreamRefusedByPolicy, p.reasonOf(FileAction.STREAM))
    }

    @Test
    fun withoutKnownCapabilitiesStreamIsNotChosenByTheTableButIsNotBlocked() {
        val p = plan(file("rig.blend"), capabilities = null)
        assertEquals(FileAction.FETCH, p.default)
        assertTrue(p[FileAction.STREAM].enabled)
    }

    @Test
    fun theStreamOnlyListIsUserEditable() {
        val custom = ActionRules(RuleConfig(streamOnlyTypes = setOf("txt")))
        assertEquals(FileAction.STREAM, plan(file("notes.txt", preview = PreviewKind.TEXT), rules = custom).default)
        // blend is no longer stream-only once the user edits the list: the phone handler wins.
        assertEquals(FileAction.FETCH, plan(file("rig.blend", preview = PreviewKind.NONE), handler = true, rules = custom).default)
    }

    // Rule 2: preview types ------------------------------------------------------------------------

    @Test
    fun pdfImagesTextAndVideoPreviewEvenWhenThePhoneCouldOpenThem() {
        assertEquals(FileAction.PREVIEW, plan(file("a.pdf", preview = PreviewKind.PDF), handler = true).default)
        assertEquals(FileAction.PREVIEW, plan(file("a.jpg", preview = PreviewKind.IMAGE), handler = true).default)
        assertEquals(FileAction.PREVIEW, plan(file("a.md", preview = PreviewKind.TEXT), handler = true).default)
        assertEquals(FileAction.PREVIEW, plan(file("a.mp4", preview = PreviewKind.VIDEO_POSTER), handler = true).default)
    }

    @Test
    fun theHostsPreviewKindCountsEvenForAnExtensionTheTableDoesNotList() {
        assertEquals(FileAction.PREVIEW, plan(file("a.weirdimg", preview = PreviewKind.IMAGE), handler = true).default)
    }

    @Test
    fun audioWithoutAPreviewFallsToTheNextRule() {
        // The protocol has no audio preview kind, so Preview is off for an mp3 and the table moves on.
        val p = plan(file("song.mp3", size = 5 * mib), handler = true)
        assertEquals(FileAction.FETCH, p.default)
        assertEquals(ActionReason.NoRenderer("mp3"), p.reasonOf(FileAction.PREVIEW))
    }

    @Test
    fun aPreviewTypeWhosePreviewIsOffIsNeverTheDefault() {
        val noPdf = caps.copy(previewKinds = caps.previewKinds.copy(pdf = false))
        val p = plan(file("a.pdf", preview = PreviewKind.PDF), capabilities = noPdf, handler = true)
        assertEquals(FileAction.FETCH, p.default)
        assertEquals(ActionReason.HostCannotRender(PreviewKind.PDF), p.reasonOf(FileAction.PREVIEW))
    }

    @Test
    fun aComputerWithoutItsRenderWorkerSaysPreviewsAreUnavailableThere() {
        // Found on a real host started with `cargo run -p gander-host` (no gander-render.exe).
        val off = caps.copy(
            previewKinds = caps.previewKinds.copy(image = false, pdf = false, text = false),
            previewsUnavailable = "render_worker_missing",
        )
        val p = plan(file("a.png", preview = PreviewKind.NONE), capabilities = off, handler = true)
        assertEquals(ActionReason.PreviewsOffOnHost(renderWorkerMissing = true), p.reasonOf(FileAction.PREVIEW))
        assertEquals(FileAction.FETCH, p.default)
        val other = plan(file("a.png", preview = PreviewKind.IMAGE), capabilities = off.copy(previewsUnavailable = "cache_unavailable"))
        assertEquals(ActionReason.PreviewsOffOnHost(renderWorkerMissing = false), other.reasonOf(FileAction.PREVIEW))
    }

    // Rule 3: phone handler and the metered threshold ---------------------------------------------

    @Test
    fun aFileThePhoneCanOpenIsFetchedOnAnUnmeteredNetworkWhateverItsSize() {
        val p = plan(file("deck.pptx", size = 900 * mib, preview = PreviewKind.OFFICE), handler = true)
        assertEquals(FileAction.FETCH, p.default)
        assertEquals(ActionHint.Fetch(900 * mib, metered = false), p[FileAction.FETCH].hint)
    }

    @Test
    fun onAMeteredNetworkASmallFileThePhoneCanOpenIsStillFetched() {
        val p = plan(file("deck.pptx", size = 25 * mib - 1, preview = PreviewKind.OFFICE), NetworkState.METERED, handler = true)
        assertEquals(FileAction.FETCH, p.default)
    }

    @Test
    fun onAMeteredNetworkTheThresholdIsExclusive() {
        val p = plan(file("deck.pptx", size = 25 * mib, preview = PreviewKind.OFFICE), NetworkState.METERED, handler = true)
        assertEquals(FileAction.PREVIEW, p.default)
    }

    @Test
    fun aLargeFileOnAMeteredNetworkIsPreviewedWhenAPreviewExists() {
        val p = plan(file("deck.pptx", size = 400 * mib, preview = PreviewKind.OFFICE), NetworkState.METERED, handler = true)
        assertEquals(FileAction.PREVIEW, p.default)
    }

    @Test
    fun theMeteredThresholdIsConfigurable() {
        val strict = ActionRules(RuleConfig(meteredHandlerFetchBytes = 1 * mib))
        val p = plan(file("deck.pptx", size = 2 * mib, preview = PreviewKind.OFFICE), NetworkState.METERED, handler = true, rules = strict)
        assertEquals(FileAction.PREVIEW, p.default)
    }

    // Rules 4 and 5: preview if there is one, else Fetch ---------------------------------------------

    @Test
    fun withoutAPhoneHandlerAnOfficeFilePreviews() {
        assertEquals(FileAction.PREVIEW, plan(file("deck.pptx", preview = PreviewKind.OFFICE)).default)
    }

    @Test
    fun theLastResortIsFetchWithTheCellularWarningOnMetered() {
        val zip = file("backup.zip", size = 200 * mib, stream = StreamDecision.REFUSED)
        val metered = plan(zip, NetworkState.METERED)
        assertEquals(FileAction.FETCH, metered.default)
        assertEquals(listOf(ActionWarning.CellularLarge(200 * mib)), metered[FileAction.FETCH].warnings)
        assertEquals(ActionHint.Fetch(200 * mib, metered = true), metered[FileAction.FETCH].hint)

        val unmetered = plan(zip, NetworkState.UNMETERED)
        assertEquals(FileAction.FETCH, unmetered.default)
        assertTrue(unmetered[FileAction.FETCH].warnings.isEmpty())
    }

    @Test
    fun theCellularWarningStartsAtFiftyMiB() {
        fun warns(size: Long) = plan(file("a.zip", size = size, stream = StreamDecision.REFUSED), NetworkState.METERED)[FileAction.FETCH]
            .warnings.any { it is ActionWarning.CellularLarge }
        assertFalse(warns(50 * mib - 1))
        assertTrue(warns(50 * mib))
    }

    // Remembered default -----------------------------------------------------------------------------

    @Test
    fun aRememberedChoiceBeatsTheTable() {
        val p = plan(file("a.pdf", preview = PreviewKind.PDF), remembered = FileAction.FETCH)
        assertEquals(FileAction.FETCH, p.default)
        assertEquals(DefaultSource.REMEMBERED, p.defaultSource)
        assertNull(p.rememberedUnavailable)
        assertEquals(FileAction.FETCH, p.options.first().action)
    }

    @Test
    fun aRememberedChoiceThatIsOffIsSetAsideAndReported() {
        val p = plan(
            file("a.pdf", preview = PreviewKind.PDF),
            remembered = FileAction.STREAM,
            hostState = HostStateInfo(locked = true, attended = true, streamAvailable = true),
        )
        assertEquals(FileAction.PREVIEW, p.default)
        assertEquals(DefaultSource.RULES, p.defaultSource)
        assertEquals(FileAction.STREAM, p.rememberedUnavailable)
        assertEquals(ActionReason.HostLocked, p.reasonOf(FileAction.STREAM))
    }

    // Order ------------------------------------------------------------------------------------------

    @Test
    fun theDefaultComesFirstThenPreviewFetchStreamEnabledBeforeDisabled() {
        val p = plan(file("a.pdf", preview = PreviewKind.PDF), remembered = FileAction.STREAM)
        assertEquals(listOf(FileAction.STREAM, FileAction.PREVIEW, FileAction.FETCH), p.options.map { it.action })

        val noStream = caps.copy(stream = caps.stream.copy(enabled = false))
        val q = plan(file("a.pdf", preview = PreviewKind.PDF), capabilities = noStream)
        assertEquals(listOf(FileAction.PREVIEW, FileAction.FETCH, FileAction.STREAM), q.options.map { it.action })
        assertTrue(q.options.take(2).all { it.enabled })
        assertFalse(q.options.last().enabled)
    }

    @Test
    fun aDisabledActionSortsAfterTheEnabledOnes() {
        val p = plan(file("rig.blend"), permissions = setOf(Permission.LIST, Permission.PREVIEW, Permission.STREAM))
        // Stream is the default; Preview is off (no renderer) and Fetch is off (permission): both after it.
        assertEquals(FileAction.STREAM, p.options.first().action)
        assertTrue(p.options.drop(1).none { it.enabled })
    }

    @Test
    fun onAMeteredNetworkALargeFileListsStreamBeforeFetch() {
        val order = plan(file("big.pptx", size = 80 * mib, preview = PreviewKind.OFFICE), NetworkState.METERED, remembered = FileAction.PREVIEW)
            .options.map { it.action }
        assertEquals(listOf(FileAction.PREVIEW, FileAction.STREAM, FileAction.FETCH), order)

        val unmetered = plan(file("big.pptx", size = 80 * mib, preview = PreviewKind.OFFICE), NetworkState.UNMETERED, remembered = FileAction.PREVIEW)
            .options.map { it.action }
        assertEquals(listOf(FileAction.PREVIEW, FileAction.FETCH, FileAction.STREAM), unmetered)
    }

    // Permissions ---------------------------------------------------------------------------------------

    @Test
    fun aSharesMissingPermissionDisablesTheActionWithThatPermissionAsTheReason() {
        val p = plan(file("a.pdf", preview = PreviewKind.PDF), permissions = setOf(Permission.LIST, Permission.PREVIEW))
        assertEquals(ActionReason.NoPermission(Permission.FETCH), p.reasonOf(FileAction.FETCH))
        assertEquals(ActionReason.NoPermission(Permission.STREAM), p.reasonOf(FileAction.STREAM))
        assertTrue(p[FileAction.PREVIEW].enabled)
        assertEquals(FileAction.PREVIEW, p[FileAction.FETCH].suggestion)
    }

    @Test
    fun withNoPermissionsAtAllNothingIsPossibleAndThereIsNoDefault() {
        val p = plan(file("a.pdf", preview = PreviewKind.PDF), permissions = setOf(Permission.LIST))
        assertNull(p.default)
        assertNull(p.defaultSource)
        assertTrue(p.options.none { it.enabled })
        assertTrue(p.options.none { it.isDefault })
        assertTrue(p.options.all { it.suggestion == null })
    }

    @Test
    fun unknownSharePermissionsBlockNothing() {
        val p = plan(file("a.pdf", preview = PreviewKind.PDF), permissions = null)
        assertTrue(p.options.all { it.enabled })
    }

    // Missing capability ------------------------------------------------------------------------------------

    @Test
    fun aHostWithoutAnOfficeRendererOffersNoPreviewForOfficeFiles() {
        val none = caps.copy(previewKinds = caps.previewKinds.copy(office = OfficePreview.NONE))
        val p = plan(file("deck.pptx", preview = PreviewKind.OFFICE), capabilities = none)
        assertEquals(ActionReason.HostCannotRender(PreviewKind.OFFICE), p.reasonOf(FileAction.PREVIEW))
        assertEquals(FileAction.FETCH, p.default)
    }

    @Test
    fun everyPreviewKindChecksItsOwnCapability() {
        val off = caps.previewKinds.copy(image = false, pdf = false, text = false, videoPoster = false)
        val none = caps.copy(previewKinds = off)
        for (kind in listOf(PreviewKind.IMAGE, PreviewKind.PDF, PreviewKind.TEXT, PreviewKind.VIDEO_POSTER)) {
            val p = plan(file("x.dat", preview = kind), capabilities = none)
            assertEquals(ActionReason.HostCannotRender(kind), p.reasonOf(FileAction.PREVIEW))
        }
    }

    @Test
    fun aCapabilitySetThatIsNotKnownYetBlocksNoPreview() {
        val p = plan(file("a.pdf", preview = PreviewKind.PDF), capabilities = null)
        assertTrue(p[FileAction.PREVIEW].enabled)
    }

    @Test
    fun aPreviewKindFromANewerHostIsNotRenderable() {
        val p = plan(file("a.holo", preview = PreviewKind.UNKNOWN))
        assertEquals(ActionReason.IncompatibleHost, p.reasonOf(FileAction.PREVIEW))
    }

    @Test
    fun aStreamOutcomeFromANewerHostIsNotUsable() {
        val p = plan(file("a.blend", stream = StreamDecision.UNKNOWN))
        assertEquals(ActionReason.IncompatibleHost, p.reasonOf(FileAction.STREAM))
        assertEquals(FileAction.FETCH, p.default)
    }

    // Host state and error codes ---------------------------------------------------------------------------------

    @Test
    fun aLockedComputerDisablesStreamAndTheTableMovesOn() {
        val p = plan(file("rig.blend"), hostState = HostStateInfo(locked = true, attended = true, streamAvailable = true))
        assertEquals(ActionReason.HostLocked, p.reasonOf(FileAction.STREAM))
        assertEquals(FileAction.FETCH, p.default)
        // A passing reason has no "try something else" suggestion.
        assertNull(p[FileAction.STREAM].suggestion)
    }

    @Test
    fun anUnattendedOrUnavailableComputerExplainsItself() {
        val unattended = plan(file("rig.blend"), hostState = HostStateInfo(locked = false, attended = false, streamAvailable = true))
        assertEquals(ActionReason.HostUnattended, unattended.reasonOf(FileAction.STREAM))
        val unavailable = plan(file("rig.blend"), hostState = HostStateInfo(locked = false, attended = true, streamAvailable = false))
        assertEquals(ActionReason.StreamUnavailableNow, unavailable.reasonOf(FileAction.STREAM))
    }

    @Test
    fun aRefusalLearnedFromRunningAnActionIsShownOnThatAction() {
        val refusal = ActionReason.fromError(FileAction.STREAM, error(412, "host_locked", "The computer is locked."))
        val p = plan(file("rig.blend"), refusals = mapOf(FileAction.STREAM to refusal))
        assertEquals(ActionReason.HostLocked, p.reasonOf(FileAction.STREAM))
        assertEquals(FileAction.FETCH, p.default)
        assertTrue(p[FileAction.FETCH].enabled)
    }

    @Test
    fun errorCodesMapToTheReasonsTheSheetShows() {
        fun reason(action: FileAction, e: GanderError, ext: String? = null) = ActionReason.fromError(action, e, ext)

        assertEquals(ActionReason.HostLocked, reason(FileAction.STREAM, error(412, "host_locked")))
        assertEquals(ActionReason.HostUnattended, reason(FileAction.STREAM, error(412, "host_unattended")))
        assertEquals(ActionReason.AppUnavailable, reason(FileAction.STREAM, error(412, "app_unavailable")))
        assertEquals(ActionReason.StreamBusy, reason(FileAction.STREAM, error(409, "stream_busy")))
        assertEquals(ActionReason.StreamNotEnabled, reason(FileAction.STREAM, error(503, "stream_unavailable")))
        assertEquals(
            ActionReason.StreamRefused("Scripts are not opened."),
            reason(FileAction.STREAM, error(422, "stream_refused", "Scripts are not opened.")),
        )
        assertEquals(
            ActionReason.NoPermission(Permission.FETCH, "missing permission: fetch"),
            reason(FileAction.FETCH, error(403, "permission_denied", "missing permission: fetch")),
        )
        assertEquals(ActionReason.NoRenderer("blend"), reason(FileAction.PREVIEW, error(415, "unsupported_type"), "blend"))
        assertEquals(ActionReason.PreviewTooLarge, reason(FileAction.PREVIEW, error(413, "too_large")))
        assertEquals(ActionReason.FileGone, reason(FileAction.FETCH, error(404, "not_found")))
        assertEquals(ActionReason.HostUnreachable, reason(FileAction.PREVIEW, GanderError.Unreachable(listOf("100.64.0.1"), null)))
        val revoked = reason(FileAction.PREVIEW, error(401, "token_revoked"))
        assertEquals(UiErrorKind.REVOKED, (revoked as ActionReason.HostError).error.kind)
    }

    // Offline, missing file, saved copy ---------------------------------------------------------------------------

    @Test
    fun withoutANetworkEverythingThatNeedsTheComputerIsOff() {
        val p = plan(file("a.pdf", preview = PreviewKind.PDF), NetworkState.OFFLINE)
        assertTrue(p.options.all { it.reason == ActionReason.PhoneOffline })
        assertNull(p.default)
        assertTrue(p.options.all { it.suggestion == null })
    }

    @Test
    fun aSavedCopyStillOpensWithoutANetwork() {
        val p = plan(file("a.pdf", preview = PreviewKind.PDF), NetworkState.OFFLINE, cached = true)
        assertEquals(FileAction.FETCH, p.default)
        assertEquals(ActionHint.SavedCopy, p[FileAction.FETCH].hint)
        assertEquals(ActionReason.PhoneOffline, p.reasonOf(FileAction.PREVIEW))
        // Opening a copy costs no data: no cellular warning.
        assertTrue(p[FileAction.FETCH].warnings.isEmpty())
    }

    @Test
    fun anUnreachableComputerDisablesTheActionsButNotWhatIsKnownToBePermanent() {
        val p = plan(
            file("rig.blend"),
            fileBlock = ActionReason.HostUnreachable,
            permissions = setOf(Permission.LIST, Permission.PREVIEW, Permission.STREAM),
        )
        // Permanent causes are reported ahead of the passing outage.
        assertEquals(ActionReason.NoRenderer("blend"), p.reasonOf(FileAction.PREVIEW))
        assertEquals(ActionReason.NoPermission(Permission.FETCH), p.reasonOf(FileAction.FETCH))
        assertEquals(ActionReason.HostUnreachable, p.reasonOf(FileAction.STREAM))
        assertNull(p.default)
    }

    @Test
    fun aFileThatIsGoneHasNoWayOut() {
        val p = plan(file("a.pdf", preview = PreviewKind.PDF), fileBlock = ActionReason.FileGone)
        assertTrue(p.options.all { it.reason == ActionReason.FileGone })
        assertTrue(p.options.all { it.suggestion == null })
        assertNull(p.default)
    }

    @Test
    fun foldersAndLinksAreNotActionable() {
        val dir = plan(SampleData.dir("Clients"))
        assertTrue(dir.options.all { it.reason == ActionReason.NotAFile })
        val link = plan(file("shortcut.pdf", preview = PreviewKind.PDF, flags = listOf(EntryFlag.LINK)))
        assertTrue(link.options.all { it.reason == ActionReason.NotAFile })
        assertNull(link.default)
    }

    // Warnings ------------------------------------------------------------------------------------------------

    @Test
    fun fetchWarnsAboutOnlineOnlyFilesAndHostsThatCannotResume() {
        val noRange = caps.copy(fetch = FetchCapabilities(range = false, hash = listOf(HashAlgo.SHA256)))
        val p = plan(file("a.zip", flags = listOf(EntryFlag.CLOUD_ONLY)), capabilities = noRange)
        assertEquals(listOf(ActionWarning.CloudOnly, ActionWarning.NoResume), p[FileAction.FETCH].warnings)
    }

    @Test
    fun streamWarnsAboutOwnerApprovalAndRelayedConnections() {
        val p = plan(file("rig.blend", stream = StreamDecision.ASK), relayed = true)
        assertEquals(listOf(ActionWarning.OwnerApproval, ActionWarning.RelayedConnection), p[FileAction.STREAM].warnings)
        assertTrue(plan(file("rig.blend", stream = StreamDecision.ALLOWED))[FileAction.STREAM].warnings.isEmpty())
    }

    @Test
    fun anEmbeddedThumbnailPreviewOfOfficeFilesIsFlaggedLowFidelity() {
        val thumb = caps.copy(previewKinds = caps.previewKinds.copy(office = OfficePreview.THUMBNAIL))
        val p = plan(file("deck.pptx", preview = PreviewKind.OFFICE), capabilities = thumb)
        assertEquals(listOf(ActionWarning.LowFidelityPreview), p[FileAction.PREVIEW].warnings)
        assertTrue(plan(file("deck.pptx", preview = PreviewKind.OFFICE))[FileAction.PREVIEW].warnings.isEmpty())
    }

    // Extension helper --------------------------------------------------------------------------------------------

    @Test
    fun theExtensionComesFromTheHostThenTheName() {
        assertEquals("pdf", file("Report.PDF").copy(ext = "PDF").extension())
        assertEquals("pptx", file("Deck.pptx").copy(ext = null).extension())
        assertEquals("gz", file("a.tar.gz").copy(ext = null).extension())
        assertNull(file(".gitignore").copy(ext = null).extension())
        assertNull(file("Makefile").copy(ext = null).extension())
        assertNull(file("trailing.").copy(ext = null).extension())
        assertEquals("pdf", file("x").copy(ext = ".pdf").extension())
    }

    private fun error(status: Int, code: String, message: String = "refused"): GanderError.Host =
        GanderError.fromResponse(status, """{"error":{"code":"$code","message":"$message","retry_after_ms":0}}""")
}
