package fyi.arlo.gander

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import fyi.arlo.gander.core.data.UiError
import fyi.arlo.gander.core.data.UiErrorKind
import fyi.arlo.gander.testing.SampleData
import fyi.arlo.gander.ui.settings.HostDetailsContent
import fyi.arlo.gander.ui.settings.HostDetailsTags
import fyi.arlo.gander.ui.settings.HostDetailsUiState
import fyi.arlo.gander.ui.settings.RotateOutcome
import fyi.arlo.gander.ui.theme.GanderTheme
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/** Host details with fixed state: identity, token rotation and forgetting, each behind a confirmation (run by WP-29). */
@RunWith(AndroidJUnit4::class)
class HostDetailsScreenTest {
    @get:Rule
    val composeRule = createComposeRule()

    private fun str(id: Int, vararg args: Any) =
        InstrumentationRegistry.getInstrumentation().targetContext.getString(id, *args)

    private val host = SampleData.host(name = "studio-pc")
    private val calls = mutableListOf<String>()

    private fun show(rotate: RotateOutcome = RotateOutcome.None) {
        composeRule.setContent {
            GanderTheme(dynamicColor = false) {
                HostDetailsContent(
                    state = HostDetailsUiState(host = host, loaded = true, rotate = rotate),
                    onBack = { calls += "back" },
                    onRotate = { calls += "rotate" },
                    onDismissRotateMessage = { calls += "dismiss" },
                    onForget = { calls += "forget" },
                )
            }
        }
    }

    @Test
    fun theIdentityThePhoneTrustsIsShown() {
        show()
        composeRule.onNodeWithText("studio-pc").assertIsDisplayed()
        composeRule.onNodeWithTag(HostDetailsTags.FINGERPRINT).assertIsDisplayed()
        composeRule.onNodeWithText(host.pin.hostId).assertIsDisplayed()
        composeRule.onNodeWithText("Pixel 9").assertIsDisplayed()
        composeRule.onNodeWithTag(HostDetailsTags.ADDRESSES).assertIsDisplayed()
    }

    @Test
    fun rotatingNeedsAConfirmationAndThenRunsOnce() {
        show()
        composeRule.onNodeWithTag(HostDetailsTags.ROTATE).performClick()
        composeRule.onNodeWithText(str(R.string.host_rotate_confirm_title)).assertIsDisplayed()
        assertTrue("nothing happens before the answer", calls.isEmpty())
        composeRule.onNodeWithTag(HostDetailsTags.ROTATE_CONFIRM).performClick()
        assertEquals(listOf("rotate"), calls)
    }

    @Test
    fun theRotateButtonIsOffWhileARotationRuns() {
        show(RotateOutcome.Working)
        composeRule.onNodeWithTag(HostDetailsTags.ROTATE).assertIsNotEnabled()
        composeRule.onNodeWithTag(HostDetailsTags.ROTATE_STATUS).assertIsDisplayed()
    }

    @Test
    fun aSuccessfulRotationSaysTheOldTokenIsDead() {
        show(RotateOutcome.Done)
        composeRule.onNodeWithText(str(R.string.host_rotate_done)).assertIsDisplayed()
        composeRule.onNodeWithText(str(R.string.sheet_ok)).performClick()
        assertEquals(listOf("dismiss"), calls)
    }

    @Test
    fun aFailedRotationSaysWhy() {
        show(RotateOutcome.Failed(UiError(UiErrorKind.OFFLINE)))
        composeRule.onNodeWithTag(HostDetailsTags.ROTATE_STATUS).assertIsDisplayed()
        composeRule.onNodeWithText(str(R.string.host_rotate_failed, str(R.string.error_offline))).assertIsDisplayed()
    }

    @Test
    fun forgettingNeedsAConfirmationAndNamesTheComputer() {
        show()
        composeRule.onNodeWithTag(HostDetailsTags.FORGET).performClick()
        composeRule.onNodeWithText(str(R.string.host_forget_title, "studio-pc")).assertIsDisplayed()
        assertTrue(calls.isEmpty())
        composeRule.onNodeWithTag(HostDetailsTags.FORGET_CONFIRM).performClick()
        assertEquals(listOf("forget"), calls)
    }
}
