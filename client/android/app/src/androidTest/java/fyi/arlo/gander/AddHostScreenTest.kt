package fyi.arlo.gander

import android.view.WindowManager
import androidx.activity.ComponentActivity
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.assertCountEquals
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.junit4.v2.createAndroidComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import fyi.arlo.gander.core.data.pairing.PairingFailure
import fyi.arlo.gander.core.data.pairing.PairingMethod
import fyi.arlo.gander.testing.SampleData
import fyi.arlo.gander.ui.pairing.AddHostActions
import fyi.arlo.gander.ui.pairing.AddHostContent
import fyi.arlo.gander.ui.pairing.AddHostTags
import fyi.arlo.gander.ui.pairing.AddHostUiState
import fyi.arlo.gander.ui.pairing.SecureWindowEffect
import fyi.arlo.gander.ui.theme.GanderTheme
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/** Add host screens with fixed state, and FLAG_SECURE while the QR code or the SAS is visible (run by WP-29). */
@RunWith(AndroidJUnit4::class)
class AddHostScreenTest {
    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    private fun str(id: Int, vararg args: Any) =
        InstrumentationRegistry.getInstrumentation().targetContext.getString(id, *args)

    private val calls = mutableListOf<String>()
    private val actions = AddHostActions(
        onScan = { calls += "scan" },
        onManual = { calls += "manual" },
        onQrScanned = { calls += "qr" },
        onAddressChanged = { calls += "address:$it" },
        onManualSubmit = { calls += "submit" },
        onRetry = { calls += "retry" },
        onCancel = { calls += "cancel" },
        onBack = { calls += "back" },
        onOpenHost = { calls += "open:$it" },
        onClose = { calls += "close" },
    )

    // setContent may run once per test; later calls only swap the state the content reads.
    private var shown by mutableStateOf<AddHostUiState?>(null)

    private fun show(state: AddHostUiState) {
        val first = shown == null
        shown = state
        if (first) {
            composeRule.setContent { GanderTheme(dynamicColor = false) { shown?.let { AddHostContent(it, actions) } } }
        } else {
            composeRule.waitForIdle()
        }
    }

    private val secureFlag get() = composeRule.activity.window.attributes.flags and WindowManager.LayoutParams.FLAG_SECURE

    @Test
    fun theFirstScreenOffersBothFlowsAndTheStopIfSomeoneToldYouWarning() {
        show(AddHostUiState.Choose)
        composeRule.onNodeWithTag(AddHostTags.WARNING).assertIsDisplayed()
        composeRule.onNodeWithText(str(R.string.pairing_warning_title)).assertIsDisplayed()
        composeRule.onNodeWithTag(AddHostTags.SCAN).performClick()
        composeRule.onNodeWithTag(AddHostTags.MANUAL).performClick()
        assertEquals(listOf("scan", "manual"), calls)
    }

    @Test
    fun manualEntryTakesTheAddressAndSubmits() {
        show(AddHostUiState.Manual(address = ""))
        composeRule.onNodeWithTag(AddHostTags.CONTINUE).assertIsNotEnabled()
        composeRule.onNodeWithTag(AddHostTags.ADDRESS).performTextInput("10.0.2.2")
        assertEquals("address:10.0.2.2", calls.single())

        show(AddHostUiState.Manual(address = "10.0.2.2"))
        composeRule.onNodeWithTag(AddHostTags.CONTINUE).assertIsEnabled().performClick()
        assertTrue("submit" in calls)
    }

    @Test
    fun anInvalidAddressSaysSo() {
        show(AddHostUiState.Manual(address = "nope nope", invalid = true))
        composeRule.onNodeWithText(str(R.string.pairing_manual_invalid)).assertIsDisplayed()
    }

    @Test
    fun theWaitingScreenShowsTheCodeAndTheWarning() {
        show(AddHostUiState.Waiting(hostLabel = "studio-pc", sas = "482 913", method = PairingMethod.QR))
        composeRule.onNodeWithTag(AddHostTags.SAS).assertIsDisplayed()
        composeRule.onNodeWithText("482 913").assertIsDisplayed()
        composeRule.onNodeWithText(str(R.string.pairing_waiting_title, "studio-pc")).assertIsDisplayed()
        composeRule.onNodeWithText(str(R.string.pairing_waiting_warning)).assertIsDisplayed()
        composeRule.onNodeWithTag(AddHostTags.CANCEL).performClick()
        assertEquals(listOf("cancel"), calls)
    }

    @Test
    fun theManualWaitingScreenTellsTheOwnerToCompareTheCodes() {
        show(AddHostUiState.Waiting(hostLabel = "100.101.102.103", sas = "079 515", method = PairingMethod.MANUAL))
        composeRule.onNodeWithText(str(R.string.pairing_waiting_manual, "100.101.102.103")).assertIsDisplayed()
    }

    @Test
    fun aMismatchOffersNoRetry() {
        show(AddHostUiState.Failed(PairingFailure.Kind.SAS_MISMATCH, null, PairingMethod.MANUAL, retrySame = false))
        composeRule.onNodeWithText(str(R.string.pairing_error_sas_mismatch)).assertIsDisplayed()
        composeRule.onNodeWithTag(AddHostTags.START_OVER).assertIsDisplayed()
        composeRule.onAllNodesWithTag(AddHostTags.RETRY).assertCountEquals(0)
    }

    @Test
    fun aFailureExplainsAndCanBeRetried() {
        show(AddHostUiState.Failed(PairingFailure.Kind.CLOSED, null, PairingMethod.QR, retrySame = false))
        composeRule.onNodeWithText(str(R.string.pairing_error_closed)).assertIsDisplayed()
        composeRule.onNodeWithTag(AddHostTags.RETRY).performClick()
        assertEquals(listOf("retry"), calls)
    }

    @Test
    fun doneOpensTheNewHost() {
        show(AddHostUiState.Done(SampleData.host(name = "studio-pc")))
        composeRule.onNodeWithText(str(R.string.pairing_done_title, "studio-pc")).assertIsDisplayed()
        composeRule.onNodeWithTag(AddHostTags.DONE_OPEN).performClick()
        assertEquals(listOf("open:${SampleData.HOST_ID}"), calls)
    }

    // FLAG_SECURE -------------------------------------------------------------------------------

    @Test
    fun secureEffectSetsAndClearsTheWindowFlag() {
        var secure by mutableStateOf(true)
        composeRule.setContent { SecureWindowEffect(enabled = secure) }
        composeRule.waitForIdle()
        assertTrue("FLAG_SECURE while enabled", secureFlag != 0)
        secure = false
        composeRule.waitForIdle()
        assertEquals("flag removed when disabled", 0, secureFlag)
    }

    @Test
    fun theSasAndTheScannerAreSecureButTheChooserIsNot() {
        var state by mutableStateOf<AddHostUiState>(AddHostUiState.Choose)
        composeRule.setContent { GanderTheme(dynamicColor = false) { AddHostContent(state, actions) } }
        composeRule.waitForIdle()
        assertEquals(0, secureFlag)

        state = AddHostUiState.Waiting("studio-pc", "482 913", PairingMethod.QR)
        composeRule.waitForIdle()
        assertTrue("SAS is secure", secureFlag != 0)

        state = AddHostUiState.Failed(PairingFailure.Kind.DENIED, null, PairingMethod.QR, retrySame = false)
        composeRule.waitForIdle()
        assertEquals("secure only while secrets are shown", 0, secureFlag)

        state = AddHostUiState.Scan()
        composeRule.waitForIdle()
        assertTrue("the QR code in the viewfinder is secure", secureFlag != 0)
    }

    @Test
    fun theFlagIsRemovedWhenTheScreenLeaves() {
        var visible by mutableStateOf(true)
        composeRule.setContent {
            if (visible) GanderTheme(dynamicColor = false) { AddHostContent(AddHostUiState.Waiting("pc", "123 456", PairingMethod.QR), actions) }
        }
        composeRule.waitForIdle()
        assertTrue(secureFlag != 0)
        visible = false
        composeRule.waitForIdle()
        assertFalse("FLAG_SECURE must not outlive the pairing screen", secureFlag != 0)
    }

}
