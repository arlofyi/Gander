package fyi.arlo.gander

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.assertIsOn
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import fyi.arlo.gander.core.data.UiError
import fyi.arlo.gander.core.data.UiErrorKind
import fyi.arlo.gander.core.data.actions.ActionInput
import fyi.arlo.gander.core.data.actions.ActionRules
import fyi.arlo.gander.core.data.actions.FileAction
import fyi.arlo.gander.core.data.actions.HostStateInfo
import fyi.arlo.gander.core.data.actions.NetworkState
import fyi.arlo.gander.core.data.actions.extension
import fyi.arlo.gander.core.data.browse.EntryTarget
import fyi.arlo.gander.core.net.protocol.Entry
import fyi.arlo.gander.core.net.protocol.EntryFlag
import fyi.arlo.gander.core.net.protocol.EntryMeta
import fyi.arlo.gander.core.net.protocol.PdfMeta
import fyi.arlo.gander.core.net.protocol.Permission
import fyi.arlo.gander.core.net.protocol.PreviewKind
import fyi.arlo.gander.core.net.protocol.StreamDecision
import fyi.arlo.gander.testing.SampleData
import fyi.arlo.gander.ui.filesheet.DetailsState
import fyi.arlo.gander.ui.filesheet.FileSheetActions
import fyi.arlo.gander.ui.filesheet.FileSheetContent
import fyi.arlo.gander.ui.filesheet.FileSheetTags
import fyi.arlo.gander.ui.filesheet.FileSheetUiState
import fyi.arlo.gander.ui.theme.GanderTheme
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/**
 * The file sheet with a fixed state (WP-25). Compiled by `assembleDebugAndroidTest`; they run on
 * the emulator in WP-29. The rule table itself is covered by JVM tests (`ActionRulesTest`).
 */
@RunWith(AndroidJUnit4::class)
class FileSheetTest {
    @get:Rule
    val composeRule = createComposeRule()

    private val host = SampleData.host()
    private val caps = SampleData.me().capabilities
    private val ran = mutableListOf<FileAction>()
    private val remembered = mutableListOf<Boolean>()
    private var resets = 0
    private var retries = 0

    private fun str(id: Int, vararg args: Any) =
        InstrumentationRegistry.getInstrumentation().targetContext.getString(id, *args)

    private fun state(
        entry: Entry,
        network: NetworkState = NetworkState.UNMETERED,
        permissions: Set<Permission>? = null,
        hostState: HostStateInfo? = null,
        details: DetailsState = DetailsState.Loaded,
        rememberedAction: FileAction? = null,
        rememberChoice: Boolean = false,
    ): FileSheetUiState {
        val plan = ActionRules().plan(
            ActionInput(
                entry = entry,
                capabilities = caps,
                permissions = permissions,
                network = network,
                remembered = rememberedAction,
                hostState = hostState,
            ),
        )
        return FileSheetUiState(
            target = EntryTarget(host.id, "shr_projects", "Clients/${entry.name}", entry),
            entry = entry,
            extension = entry.extension(),
            hostName = host.name,
            shareName = "Projects",
            details = details,
            network = network,
            plan = plan,
            remembered = rememberedAction,
            rememberChoice = rememberChoice,
        )
    }

    private fun show(state: FileSheetUiState) {
        composeRule.setContent {
            GanderTheme(dynamicColor = false) {
                FileSheetContent(
                    state = state,
                    actions = FileSheetActions(
                        onAction = { ran += it },
                        onRememberChange = { remembered += it },
                        onResetDefault = { resets++ },
                        onRetry = { retries++ },
                    ),
                )
            }
        }
    }

    private val pdf = SampleData.file("Report.pdf", size = 2 * 1024 * 1024, preview = PreviewKind.PDF)
        .copy(stream = StreamDecision.ALLOWED, mime = "application/pdf")

    @Test
    fun theSheetShowsWhatTheFileIsAndWhereItLives() {
        show(state(pdf.copy(meta = EntryMeta(pdf = PdfMeta(pages = 42)), flags = listOf(EntryFlag.CLOUD_ONLY, EntryFlag.READONLY))))
        composeRule.onNodeWithText("Report.pdf").assertIsDisplayed()
        // "studio-pc > Projects/..." (the action hints name the computer too, so match the location line)
        composeRule.onNodeWithText(str(R.string.sheet_where, "studio-pc", ""), substring = true).assertIsDisplayed()
        composeRule.onNodeWithText("2 MB").assertIsDisplayed() // the size row; the fetch hint says it too
        composeRule.onNodeWithText("application/pdf").assertIsDisplayed()
        composeRule.onNodeWithText("42").assertIsDisplayed()
        composeRule.onNodeWithText(str(R.string.sheet_chip_cloud_only)).assertIsDisplayed()
        composeRule.onNodeWithText(str(R.string.sheet_chip_readonly)).assertIsDisplayed()
    }

    @Test
    fun theThreeActionsShowHintsAndTheDefaultIsMarked() {
        show(state(pdf))
        composeRule.onNodeWithTag(FileSheetTags.action(FileAction.PREVIEW)).assertIsEnabled()
        composeRule.onNodeWithTag(FileSheetTags.action(FileAction.FETCH)).assertIsEnabled()
        composeRule.onNodeWithTag(FileSheetTags.action(FileAction.STREAM)).assertIsEnabled()
        composeRule.onNodeWithText(str(R.string.sheet_default_tag)).assertIsDisplayed()
        composeRule.onNodeWithText(str(R.string.hint_preview_pages, "studio-pc")).assertIsDisplayed()
        composeRule.onNodeWithText(str(R.string.hint_fetch_unmetered, "2 MB")).assertIsDisplayed()
        composeRule.onNodeWithText(str(R.string.hint_stream, "studio-pc")).assertIsDisplayed()
    }

    @Test
    fun onAMeteredNetworkTheFetchHintSaysMobileData() {
        show(state(pdf.copy(size = 80L * 1024 * 1024), network = NetworkState.METERED))
        composeRule.onNodeWithText(str(R.string.hint_fetch_metered, "80 MB")).assertIsDisplayed()
        composeRule.onNodeWithText(str(R.string.warn_cellular_large)).assertIsDisplayed()
        composeRule.onNodeWithText(str(R.string.sheet_chip_metered)).assertIsDisplayed()
    }

    @Test
    fun anActionThatIsOffSaysWhyAndSuggestsAnotherInPlaceOfAGenericFailure() {
        show(state(SampleData.file("rig.blend", size = 5 * 1024 * 1024).copy(stream = StreamDecision.ALLOWED)))
        composeRule.onNodeWithTag(FileSheetTags.action(FileAction.PREVIEW)).assertIsNotEnabled()
        composeRule.onNodeWithText(str(R.string.reason_no_renderer_ext, ".blend", "studio-pc")).assertIsDisplayed()
        composeRule.onNodeWithText(str(R.string.sheet_try_instead, str(R.string.sheet_action_stream))).assertIsDisplayed()
    }

    @Test
    fun aLockedComputerIsNamedOnTheStreamAction() {
        show(state(pdf, hostState = HostStateInfo(locked = true, attended = true, streamAvailable = true)))
        composeRule.onNodeWithTag(FileSheetTags.action(FileAction.STREAM)).assertIsNotEnabled()
        composeRule.onNodeWithText(str(R.string.reason_host_locked, "studio-pc")).assertIsDisplayed()
    }

    @Test
    fun aMissingSharePermissionIsNamedOnTheAction() {
        show(state(pdf, permissions = setOf(Permission.LIST, Permission.PREVIEW)))
        composeRule.onNodeWithTag(FileSheetTags.action(FileAction.FETCH)).assertIsNotEnabled()
        composeRule.onNodeWithText(str(R.string.reason_no_permission_fetch)).assertIsDisplayed()
    }

    @Test
    fun tappingAnEnabledActionRunsItAndADisabledOneDoesNothing() {
        show(state(pdf, permissions = setOf(Permission.LIST, Permission.PREVIEW)))
        composeRule.onNodeWithTag(FileSheetTags.action(FileAction.PREVIEW)).performClick()
        composeRule.onNodeWithTag(FileSheetTags.action(FileAction.FETCH)).performClick()
        assertEquals(listOf(FileAction.PREVIEW), ran)
    }

    @Test
    fun theRememberCheckboxReportsItsChange() {
        show(state(pdf))
        composeRule.onNodeWithTag(FileSheetTags.REMEMBER).performScrollTo().performClick()
        assertEquals(listOf(true), remembered)
    }

    @Test
    fun aTickedRememberCheckboxIsShownOn() {
        show(state(pdf, rememberChoice = true))
        composeRule.onNodeWithTag(FileSheetTags.REMEMBER).performScrollTo().assertIsOn()
    }

    @Test
    fun aRememberedChoiceIsShownWithAWayToResetIt() {
        show(state(pdf, rememberedAction = FileAction.FETCH))
        composeRule.onNodeWithText(str(R.string.sheet_remembered, ".pdf", str(R.string.sheet_action_fetch)))
            .performScrollTo().assertIsDisplayed()
        composeRule.onNodeWithTag(FileSheetTags.RESET).performScrollTo().performClick()
        assertEquals(1, resets)
    }

    @Test
    fun aFileWithoutAnExtensionOffersNoRemembering() {
        show(state(SampleData.file("README", preview = PreviewKind.TEXT).copy(ext = null)))
        composeRule.onNodeWithTag(FileSheetTags.REMEMBER).assertDoesNotExist()
    }

    @Test
    fun aFailedStatShowsTheReasonAndOffersRetry() {
        show(state(pdf, details = DetailsState.Failed(UiError(UiErrorKind.OFFLINE))))
        composeRule.onNodeWithTag(FileSheetTags.DETAILS_FAILED).assertIsDisplayed()
        composeRule.onNodeWithText(str(R.string.sheet_details_failed)).assertIsDisplayed()
        composeRule.onNodeWithTag(FileSheetTags.RETRY).performClick()
        assertEquals(1, retries)
    }

    @Test
    fun whileTheHostIsBeingAskedAProgressLineShows() {
        show(state(pdf, details = DetailsState.Loading))
        composeRule.onNodeWithTag(FileSheetTags.DETAILS_LOADING).assertIsDisplayed()
        // the listing's own data is already usable
        composeRule.onNodeWithTag(FileSheetTags.action(FileAction.PREVIEW)).assertIsEnabled()
        assertTrue(composeRule.onAllNodes(hasText("Report.pdf")).fetchSemanticsNodes().isNotEmpty())
    }
}
