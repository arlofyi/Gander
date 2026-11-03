package fyi.arlo.gander

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextClearance
import androidx.compose.ui.test.performTextInput
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import fyi.arlo.gander.core.data.UiError
import fyi.arlo.gander.core.data.UiErrorKind
import fyi.arlo.gander.core.data.hosts.HostStatus
import fyi.arlo.gander.core.net.protocol.RelayPath
import fyi.arlo.gander.testing.SampleData
import fyi.arlo.gander.ui.hosts.HostCard
import fyi.arlo.gander.ui.hosts.HostsContent
import fyi.arlo.gander.ui.hosts.HostsTags
import fyi.arlo.gander.ui.hosts.HostsUiState
import fyi.arlo.gander.ui.theme.GanderTheme
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/** Hosts screen rendering with fixed state (emulator or device, run by WP-29). */
@RunWith(AndroidJUnit4::class)
class HostsScreenTest {
    @get:Rule
    val composeRule = createComposeRule()

    private fun str(id: Int, vararg args: Any) =
        InstrumentationRegistry.getInstrumentation().targetContext.getString(id, *args)

    private val renamed = mutableListOf<Pair<String, String>>()
    private val forgotten = mutableListOf<String>()
    private val opened = mutableListOf<String>()
    private var added = 0
    private var refreshed = 0

    private fun show(state: HostsUiState) {
        composeRule.setContent {
            GanderTheme(dynamicColor = false) {
                HostsContent(
                    state = state,
                    onOpenHost = { opened += it },
                    onAddHost = { added++ },
                    onRefresh = { refreshed++ },
                    onRename = { id, name -> renamed += id to name },
                    onForget = { forgotten += it },
                )
            }
        }
    }

    @Test
    fun targetContextHasAgreedPackageName() {
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        assertEquals("fyi.arlo.gander", context.packageName)
    }

    @Test
    fun emptyStateIsShownWithAnAddButton() {
        show(HostsUiState(loaded = true))
        composeRule.onNodeWithText("No computers yet").assertIsDisplayed()
        composeRule.onNodeWithTag(HostsTags.ADD).performClick()
        assertEquals(1, added)
    }

    @Test
    fun anOnlineHostShowsNameStatusAndPairedAs() {
        show(
            HostsUiState(
                loaded = true,
                privateNetworkUp = true,
                cards = listOf(HostCard(SampleData.host(), HostStatus.Online())),
            ),
        )
        composeRule.onNodeWithText("studio-pc").assertIsDisplayed()
        composeRule.onNodeWithText(str(R.string.host_status_online)).assertIsDisplayed()
        composeRule.onNodeWithText(str(R.string.host_paired_as, "Pixel 9")).assertIsDisplayed()
        composeRule.onNodeWithText(str(R.string.hosts_private_network_up)).assertIsDisplayed()
        composeRule.onNodeWithTag(HostsTags.card(SampleData.HOST_ID)).performClick()
        assertEquals(listOf(SampleData.HOST_ID), opened)
    }

    @Test
    fun aRelayedHostShowsTheRelayChipAndAWarning() {
        show(
            HostsUiState(
                loaded = true,
                cards = listOf(HostCard(SampleData.host(), HostStatus.Online(relay = RelayPath.DERP))),
            ),
        )
        composeRule.onNodeWithText(str(R.string.host_relay_chip)).assertIsDisplayed()
        composeRule.onNodeWithText(str(R.string.host_relay_derp)).assertIsDisplayed()
    }

    @Test
    fun aDirectHostHasNoRelayChip() {
        show(
            HostsUiState(
                loaded = true,
                cards = listOf(HostCard(SampleData.host(), HostStatus.Online(relay = RelayPath.DIRECT))),
            ),
        )
        composeRule.onNodeWithText(str(R.string.host_relay_chip)).assertDoesNotExist()
    }

    @Test
    fun anOfflineHostShowsLastSeenWithoutAnAlarmingSentence() {
        show(
            HostsUiState(
                loaded = true,
                cards = listOf(
                    HostCard(
                        SampleData.host(lastSeenAtMillis = System.currentTimeMillis() - 3 * 3_600_000L),
                        HostStatus.Unavailable(UiError(UiErrorKind.OFFLINE)),
                    ),
                ),
            ),
        )
        composeRule.onNodeWithText(str(R.string.host_status_offline), substring = true).assertIsDisplayed()
        composeRule.onNodeWithText("Last seen", substring = true).assertIsDisplayed()
    }

    @Test
    fun aWrongCertificateIsExplainedOnTheCard() {
        show(
            HostsUiState(
                loaded = true,
                cards = listOf(HostCard(SampleData.host(), HostStatus.Unavailable(UiError(UiErrorKind.WRONG_COMPUTER)))),
            ),
        )
        composeRule.onNodeWithText(str(R.string.error_wrong_computer)).assertIsDisplayed()
    }

    /** Found on a real tablet: a revoked phone showed a red error; now it is a state with its two ways out. */
    @Test
    fun aPhoneRemovedOnTheComputerOffersPairAgainAndForget() {
        show(
            HostsUiState(
                loaded = true,
                cards = listOf(HostCard(SampleData.host(), HostStatus.Unavailable(UiError(UiErrorKind.REVOKED)))),
            ),
        )
        composeRule.onNodeWithTag(HostsTags.removed(SampleData.HOST_ID)).assertIsDisplayed()
        composeRule.onNodeWithText(str(R.string.host_status_removed)).assertIsDisplayed()
        composeRule.onNodeWithText(str(R.string.error_revoked)).assertDoesNotExist()

        composeRule.onNodeWithTag(HostsTags.pairAgain(SampleData.HOST_ID)).performClick()
        assertEquals(1, added)

        composeRule.onNodeWithTag(HostsTags.forgetNow(SampleData.HOST_ID)).performClick()
        composeRule.onNodeWithTag(HostsTags.FORGET_CONFIRM).performClick()
        assertEquals(listOf(SampleData.HOST_ID), forgotten)
    }

    @Test
    fun renameFromTheKebabMenu() {
        show(HostsUiState(loaded = true, cards = listOf(HostCard(SampleData.host(), HostStatus.Checking))))
        composeRule.onNodeWithContentDescription(str(R.string.action_more)).performClick()
        composeRule.onNodeWithText(str(R.string.host_menu_rename)).performClick()
        composeRule.onNodeWithText(str(R.string.host_rename_label)).performTextClearance()
        composeRule.onNodeWithText(str(R.string.host_rename_label)).performTextInput("Office PC")
        composeRule.onNodeWithText(str(R.string.action_save)).performClick()
        assertEquals(listOf(SampleData.HOST_ID to "Office PC"), renamed)
    }

    @Test
    fun forgetAsksForConfirmationFirst() {
        show(HostsUiState(loaded = true, cards = listOf(HostCard(SampleData.host(), HostStatus.Checking))))
        composeRule.onNodeWithContentDescription(str(R.string.action_more)).performClick()
        composeRule.onNodeWithText(str(R.string.host_menu_forget)).performClick()
        composeRule.onNodeWithText(str(R.string.host_forget_title, "studio-pc")).assertIsDisplayed()
        assertEquals(emptyList<String>(), forgotten) // nothing happens until it is confirmed
        composeRule.onNodeWithTag(HostsTags.FORGET_CONFIRM).performClick()
        assertEquals(listOf(SampleData.HOST_ID), forgotten)
    }
}
