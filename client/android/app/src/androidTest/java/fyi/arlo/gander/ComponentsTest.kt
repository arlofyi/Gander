package fyi.arlo.gander

import androidx.compose.foundation.layout.Column
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.SemanticsMatcher
import androidx.compose.ui.test.assert
import androidx.compose.ui.test.assertHasNoClickAction
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.assertIsOff
import androidx.compose.ui.test.assertIsOn
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.ext.junit.runners.AndroidJUnit4
import fyi.arlo.gander.core.data.actions.FileAction
import fyi.arlo.gander.ui.components.ActionNote
import fyi.arlo.gander.ui.components.ActionPanel
import fyi.arlo.gander.ui.components.ActionPanelState
import fyi.arlo.gander.ui.components.Banner
import fyi.arlo.gander.ui.components.BannerTone
import fyi.arlo.gander.ui.components.KeyCap
import fyi.arlo.gander.ui.components.SasCode
import fyi.arlo.gander.ui.components.StatusKind
import fyi.arlo.gander.ui.components.StatusLine
import fyi.arlo.gander.ui.components.Tag
import fyi.arlo.gander.ui.components.WorkRail
import fyi.arlo.gander.ui.components.icon
import fyi.arlo.gander.ui.theme.GanderIcons
import fyi.arlo.gander.ui.theme.GanderTheme
import fyi.arlo.gander.ui.theme.LensMark
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/** The shared design-system components (docs/design/2026-11-02-ui-redesign.md section 8) and their accessibility rules. */
@RunWith(AndroidJUnit4::class)
class ComponentsTest {
    @get:Rule
    val composeRule = createComposeRule()

    @Test
    fun theSasIsFoundAsWrittenAndReadDigitByDigit() {
        composeRule.setContent { GanderTheme { SasCode("482 913", Modifier.testTag("sas")) } }
        composeRule.onNodeWithText("482 913").assertIsDisplayed()
        composeRule.onNodeWithContentDescription("4 8 2, 9 1 3").assertIsDisplayed()
    }

    @Test
    fun anUnavailableActionKeepsItsReasonReadableAndIsNotClickable() {
        var clicks = 0
        composeRule.setContent {
            GanderTheme {
                ActionPanel(
                    title = "Stream",
                    icon = FileAction.STREAM.icon(),
                    state = ActionPanelState.UNAVAILABLE,
                    hint = "Opens it in Blender on studio-pc and shows that window here.",
                    notes = listOf(ActionNote("studio-pc is locked.", GanderIcons.Lock)),
                    mode = FileAction.STREAM,
                    railCaption = "Runs on studio-pc",
                    modifier = Modifier.testTag("panel"),
                    onClick = { clicks++ },
                )
            }
        }
        composeRule.onNodeWithText("studio-pc is locked.").assertIsDisplayed()
        composeRule.onNodeWithTag("panel").assertIsNotEnabled()
        composeRule.onNodeWithTag("panel").performClick()
        assertEquals(0, clicks)
    }

    @Test
    fun theDefaultActionShowsItsTagAndRuns() {
        var clicks = 0
        composeRule.setContent {
            GanderTheme {
                ActionPanel(
                    title = "Preview",
                    icon = FileAction.PREVIEW.icon(),
                    state = ActionPanelState.DEFAULT,
                    hint = "studio-pc renders the pages for you.",
                    mode = FileAction.PREVIEW,
                    railCaption = "Work on studio-pc",
                    defaultTag = "Default",
                    modifier = Modifier.testTag("panel"),
                    onClick = { clicks++ },
                )
            }
        }
        composeRule.onNodeWithText("Default").assertIsDisplayed()
        composeRule.onNodeWithContentDescription("Work on studio-pc").assertIsDisplayed()
        composeRule.onNodeWithTag("panel").assertIsEnabled().performClick()
        assertEquals(1, clicks)
    }

    @Test
    fun aStickyKeyIsAToggleThatSaysItIsOn() {
        composeRule.setContent {
            GanderTheme {
                var on by remember { mutableStateOf(false) }
                KeyCap(
                    label = "Ctrl",
                    onClick = { on = !on },
                    latched = on,
                    latchedDescription = if (on) "sticky, on" else "sticky, off",
                    modifier = Modifier.testTag("ctrl"),
                )
            }
        }
        composeRule.onNodeWithTag("ctrl").assertIsOff().performClick()
        composeRule.onNodeWithTag("ctrl").assertIsOn()
            .assert(SemanticsMatcher.expectValue(SemanticsProperties.StateDescription, "sticky, on"))
    }

    @Test
    fun statusBannersTagsAndTheMarkRender() {
        composeRule.setContent {
            GanderTheme {
                Column {
                    StatusLine(StatusKind.ONLINE, "Online")
                    StatusLine(StatusKind.OFFLINE, "Offline")
                    Banner(BannerTone.WARN, "Relayed through Tailscale.", title = "Relayed")
                    Banner(BannerTone.BAD, "Nothing was shared")
                    Tag("Default", Modifier.testTag("tag"))
                    WorkRail(FileAction.FETCH, "Work on this phone")
                    LensMark(modifier = Modifier.testTag("lens"))
                }
            }
        }
        composeRule.onNodeWithText("Online").assertIsDisplayed()
        composeRule.onNodeWithText("Offline").assertIsDisplayed()
        composeRule.onNodeWithText("Relayed").assertIsDisplayed()
        composeRule.onNodeWithText("Nothing was shared").assertIsDisplayed()
        composeRule.onNodeWithTag("tag").assertHasNoClickAction()
        composeRule.onNodeWithContentDescription("Work on this phone").assertIsDisplayed()
        composeRule.onNodeWithTag("lens").assertIsDisplayed()
    }
}
