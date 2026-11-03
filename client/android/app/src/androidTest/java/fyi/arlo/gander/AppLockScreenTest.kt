package fyi.arlo.gander

import android.view.WindowManager
import androidx.activity.ComponentActivity
import androidx.compose.material3.Text
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.v2.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import fyi.arlo.gander.core.data.security.AppLock
import fyi.arlo.gander.core.data.security.LockState
import fyi.arlo.gander.core.data.settings.LockConfig
import fyi.arlo.gander.testing.FakeDeviceAuthenticator
import fyi.arlo.gander.ui.lock.AppLockGate
import fyi.arlo.gander.ui.lock.AuthAvailability
import fyi.arlo.gander.ui.lock.AuthResult
import fyi.arlo.gander.ui.lock.LockTags
import fyi.arlo.gander.ui.theme.GanderTheme
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/**
 * The lock screen and the gate around the app: nothing behind the lock until the first unlock,
 * FLAG_SECURE while locked, the prompt opens by itself, and the way out when the phone has no screen
 * lock any more. The system prompt is replaced by [FakeDeviceAuthenticator] (run by WP-29).
 */
@RunWith(AndroidJUnit4::class)
class AppLockScreenTest {
    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    private fun str(id: Int, vararg args: Any) =
        InstrumentationRegistry.getInstrumentation().targetContext.getString(id, *args)

    private var clock = 0L
    private val lock = AppLock { clock }
    private val authenticator = FakeDeviceAuthenticator()
    private var turnedOff = 0
    private val secureFlag get() = composeRule.activity.window.attributes.flags and WindowManager.LayoutParams.FLAG_SECURE

    private fun show() {
        composeRule.setContent {
            GanderTheme(dynamicColor = false) {
                AppLockGate(lock = lock, authenticator = authenticator, onTurnOffLock = { turnedOff++ }) {
                    Text("secret content", Modifier.testTag(CONTENT))
                }
            }
        }
    }

    @Test
    fun nothingIsShownBehindTheLockOnAColdStartUntilTheSettingsAreKnown() {
        show()
        composeRule.waitForIdle()
        composeRule.onNodeWithTag(CONTENT).assertDoesNotExist()
        composeRule.onNodeWithTag(LockTags.SCREEN).assertDoesNotExist()
    }

    @Test
    fun aLockedColdStartShowsTheLockScreenAndNotTheContent() {
        authenticator.autoAnswer = AuthResult.Cancelled
        show()
        composeRule.runOnIdle { lock.onConfig(LockConfig(enabled = true)) }
        composeRule.onNodeWithTag(LockTags.SCREEN).assertIsDisplayed()
        composeRule.onNodeWithText(str(R.string.lock_title)).assertIsDisplayed()
        composeRule.onNodeWithTag(CONTENT).assertDoesNotExist()
        assertTrue("the window is FLAG_SECURE while locked", secureFlag != 0)
    }

    @Test
    fun thePromptOpensByItselfAndAnUnlockRevealsTheContent() {
        authenticator.autoAnswer = AuthResult.Success
        show()
        composeRule.runOnIdle { lock.onConfig(LockConfig(enabled = true)) }
        composeRule.waitUntil(5_000) { lock.state.value == LockState.UNLOCKED }
        composeRule.onNodeWithTag(CONTENT).assertIsDisplayed()
        composeRule.onNodeWithTag(LockTags.SCREEN).assertDoesNotExist()
        assertTrue("the prompt came up without a tap", authenticator.prompts >= 1)
        composeRule.waitForIdle()
        assertEquals("FLAG_SECURE is cleared after unlocking", 0, secureFlag)
    }

    @Test
    fun aCancelledPromptLeavesTheLockAndTheUnlockButtonAsksAgain() {
        authenticator.autoAnswer = AuthResult.Cancelled
        show()
        composeRule.runOnIdle { lock.onConfig(LockConfig(enabled = true)) }
        composeRule.waitUntil(5_000) { authenticator.prompts >= 1 }
        composeRule.onNodeWithText(str(R.string.lock_declined)).assertIsDisplayed()
        val before = authenticator.prompts

        authenticator.autoAnswer = AuthResult.Success
        composeRule.onNodeWithTag(LockTags.UNLOCK).performClick()

        composeRule.waitUntil(5_000) { lock.state.value == LockState.UNLOCKED }
        assertTrue(authenticator.prompts > before)
    }

    @Test
    fun comingBackAfterTheTimeoutCoversTheContentAgainButKeepsItsState() {
        authenticator.autoAnswer = AuthResult.Success
        show()
        composeRule.runOnIdle { lock.onConfig(LockConfig(enabled = true, minutes = 1)) }
        composeRule.waitUntil(5_000) { lock.state.value == LockState.UNLOCKED }
        composeRule.onNodeWithTag(CONTENT).assertIsDisplayed()

        authenticator.autoAnswer = AuthResult.Cancelled
        composeRule.runOnIdle {
            lock.onBackground()
            clock += 2 * 60_000
            lock.onForeground()
        }

        composeRule.onNodeWithTag(LockTags.SCREEN).assertIsDisplayed()
        // hidden from accessibility services and not clickable: it is not in the semantics tree
        composeRule.onNodeWithTag(CONTENT).assertDoesNotExist()
        assertTrue(secureFlag != 0)
    }

    @Test
    fun withoutAScreenLockTheOnlyWayOutIsTurningTheAppLockOff() {
        authenticator.availability = AuthAvailability.NOT_SET_UP
        show()
        composeRule.runOnIdle { lock.onConfig(LockConfig(enabled = true)) }

        composeRule.onNodeWithText(str(R.string.lock_not_set_up)).assertIsDisplayed()
        composeRule.onNodeWithTag(LockTags.UNLOCK).assertDoesNotExist()
        assertEquals("no prompt can be shown", 0, authenticator.prompts)
        composeRule.onNodeWithTag(LockTags.TURN_OFF).performClick()
        assertEquals(1, turnedOff)
    }

    @Test
    fun aTemporarilyUnavailableSensorOffersTryAgain() {
        authenticator.availability = AuthAvailability.UNAVAILABLE
        show()
        composeRule.runOnIdle { lock.onConfig(LockConfig(enabled = true)) }

        composeRule.onNodeWithText(str(R.string.lock_unavailable)).assertIsDisplayed()
        authenticator.availability = AuthAvailability.AVAILABLE
        authenticator.autoAnswer = AuthResult.Success
        composeRule.onNodeWithTag(LockTags.UNLOCK).performClick()
        composeRule.waitUntil(5_000) { lock.state.value == LockState.UNLOCKED }
    }

    private companion object {
        const val CONTENT = "app-content"
    }
}
