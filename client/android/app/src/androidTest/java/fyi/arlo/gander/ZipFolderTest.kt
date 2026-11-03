package fyi.arlo.gander

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import fyi.arlo.gander.core.data.UiError
import fyi.arlo.gander.core.data.UiErrorKind
import fyi.arlo.gander.core.data.browse.EntryTarget
import fyi.arlo.gander.core.net.protocol.Permission
import fyi.arlo.gander.core.net.protocol.ZipCapability
import fyi.arlo.gander.core.net.protocol.ZipPlan
import fyi.arlo.gander.testing.SampleData
import fyi.arlo.gander.testing.SampleData.dir
import fyi.arlo.gander.testing.SampleData.file
import fyi.arlo.gander.ui.browser.BrowserActions
import fyi.arlo.gander.ui.browser.BrowserContent
import fyi.arlo.gander.ui.browser.BrowserTags
import fyi.arlo.gander.ui.browser.BrowserUiState
import fyi.arlo.gander.ui.browser.ListingState
import fyi.arlo.gander.ui.browser.formatFileSize
import fyi.arlo.gander.ui.fetch.ZipDialog
import fyi.arlo.gander.ui.fetch.ZipPrompt
import fyi.arlo.gander.ui.fetch.ZipTags
import fyi.arlo.gander.ui.theme.GanderTheme
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/**
 * "Download folder as zip" (WP-68): the folder's overflow menu in the browser (only when the host
 * advertises it and the share allows fetching) and the confirmation dialog with the count and the
 * size (Measuring, Ready, TooLarge, Failed). The download itself is covered by `FolderZipTest`
 * on the JVM and by the host's `zip.*` end-to-end cases.
 */
@RunWith(AndroidJUnit4::class)
class ZipFolderTest {
    @get:Rule
    val composeRule = createComposeRule()

    private fun str(id: Int, vararg args: Any) =
        InstrumentationRegistry.getInstrumentation().targetContext.getString(id, *args)

    private val folder = EntryTarget("host", "shr_projects", "Clients/Acme", dir("Acme"))
    private val limits = ZipCapability(maxBytes = 4L shl 30, maxEntries = 20_000)

    private fun browser(state: BrowserUiState, actions: BrowserActions) {
        composeRule.setContent {
            GanderTheme(dynamicColor = false) { BrowserContent(state, actions) }
        }
    }

    private fun readyState(zip: ZipCapability?, permissions: List<Permission> = listOf(Permission.LIST, Permission.FETCH)) =
        BrowserUiState(
            shareName = "Projects",
            share = SampleData.share(permissions = permissions),
            ready = true,
            listing = ListingState(entries = listOf(file("a.txt")), loaded = true),
            zip = zip,
        )

    @Test
    fun theFolderMenuOffersTheZipAndHandsOverTheAction() {
        var asked = 0
        browser(readyState(limits), BrowserActions(onDownloadZip = { asked++ }))
        composeRule.onNodeWithTag(BrowserTags.MORE).performClick()
        composeRule.onNodeWithText(str(R.string.zip_menu)).assertIsDisplayed()
        composeRule.onNodeWithTag(BrowserTags.ZIP).performClick()
        assertEquals(1, asked)
    }

    @Test
    fun noMenuWithoutHostSupportPermissionOrAHandler() {
        browser(readyState(zip = null), BrowserActions(onDownloadZip = {}))
        composeRule.onNodeWithTag(BrowserTags.MORE).assertDoesNotExist()
    }

    @Test
    fun noMenuWithoutTheFetchPermission() {
        browser(readyState(limits, permissions = listOf(Permission.LIST)), BrowserActions(onDownloadZip = {}))
        composeRule.onNodeWithTag(BrowserTags.MORE).assertDoesNotExist()
    }

    @Test
    fun noMenuWhileNothingConsumesTheAction() {
        browser(readyState(limits), BrowserActions())
        composeRule.onNodeWithTag(BrowserTags.MORE).assertDoesNotExist()
    }

    private fun dialog(prompt: ZipPrompt, onConfirm: () -> Unit = {}, onDismiss: () -> Unit = {}, onRetry: () -> Unit = {}) {
        composeRule.setContent {
            GanderTheme(dynamicColor = false) { ZipDialog(prompt, onConfirm, onDismiss, onRetry) }
        }
    }

    @Test
    fun measuringShowsTheFolderAndOffersNoDownloadYet() {
        dialog(ZipPrompt.Measuring(folder))
        composeRule.onNodeWithText(str(R.string.zip_title)).assertIsDisplayed()
        composeRule.onNodeWithText(str(R.string.zip_measuring, "Acme")).assertIsDisplayed()
        composeRule.onNodeWithTag(ZipTags.CONFIRM).assertDoesNotExist()
    }

    @Test
    fun readyShowsCountAndSizeAndOnlyThenDownloads() {
        var confirmed = 0
        val plan = ZipPlan(entries = 1204, bytes = 83_886_080, skipped = 2, cloudOnly = 3)
        dialog(ZipPrompt.Ready(folder, plan), onConfirm = { confirmed++ })
        composeRule.onNodeWithText("“Acme” has 1204 items, 80 MB in total.", substring = true).assertIsDisplayed()
        // The warnings that change what the user should expect.
        composeRule.onNodeWithText("3 files are online-only", substring = true).assertIsDisplayed()
        composeRule.onNodeWithText("2 items are left out", substring = true).assertIsDisplayed()
        assertEquals(0, confirmed)
        composeRule.onNodeWithTag(ZipTags.CONFIRM).performClick()
        assertEquals(1, confirmed)
    }

    @Test
    fun aFolderOverTheLimitSaysSoInTheHostsWordsAndCannotBeDownloaded() {
        var dismissed = 0
        val message = "This folder is over the limit for a zip download (4 GiB and 20,000 items at most)."
        dialog(ZipPrompt.TooLarge(folder, message), onDismiss = { dismissed++ })
        composeRule.onNodeWithText(message).assertIsDisplayed()
        composeRule.onNodeWithTag(ZipTags.CONFIRM).assertDoesNotExist()
        composeRule.onNodeWithText(str(R.string.zip_close)).performClick()
        assertEquals(1, dismissed)
    }

    @Test
    fun aFailedMeasurementOffersRetry() {
        var retried = 0
        dialog(ZipPrompt.Failed(folder, UiError(UiErrorKind.OFFLINE)), onRetry = { retried++ })
        composeRule.onNodeWithText(str(R.string.error_offline)).assertIsDisplayed()
        composeRule.onNodeWithTag(ZipTags.RETRY).performClick()
        assertEquals(1, retried)
    }
}
