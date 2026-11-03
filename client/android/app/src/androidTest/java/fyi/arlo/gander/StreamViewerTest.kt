package fyi.arlo.gander

import androidx.compose.ui.test.click
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsEnabled
import androidx.activity.ComponentActivity
import androidx.compose.ui.test.junit4.v2.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.test.performTextInput
import androidx.compose.ui.test.performTouchInput
import androidx.test.ext.junit.runners.AndroidJUnit4
import fyi.arlo.gander.core.data.stream.ControlMode
import fyi.arlo.gander.core.data.stream.EndCause
import fyi.arlo.gander.core.data.stream.FrameSize
import fyi.arlo.gander.core.data.stream.FrontStatus
import fyi.arlo.gander.core.data.stream.StreamPhase
import fyi.arlo.gander.core.data.stream.StreamUiState
import fyi.arlo.gander.core.net.protocol.Entry
import fyi.arlo.gander.core.net.protocol.InputMessage
import fyi.arlo.gander.core.net.protocol.KeyModifier
import fyi.arlo.gander.core.net.protocol.MouseButton
import fyi.arlo.gander.core.net.protocol.NetworkAdapter
import fyi.arlo.gander.core.net.protocol.RelayPath
import fyi.arlo.gander.core.net.protocol.StreamApp
import fyi.arlo.gander.core.net.protocol.StreamReason
import fyi.arlo.gander.testing.SampleData
import fyi.arlo.gander.ui.stream.EndSummary
import fyi.arlo.gander.ui.stream.GestureOutput
import fyi.arlo.gander.ui.stream.KeyCodes
import fyi.arlo.gander.ui.stream.StreamControls
import fyi.arlo.gander.ui.stream.StreamScreenContent
import fyi.arlo.gander.ui.stream.StreamScreenState
import fyi.arlo.gander.ui.stream.StreamTags
import fyi.arlo.gander.ui.theme.GanderTheme
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/**
 * The stream viewer's screen with a fixed state and no stream (WP-54): the banner, the keyboard
 * bar, the dialogs and, with a decoded-frame size, the gesture layer driving real touch events
 * into protocol messages. Compiled by `assembleDebugAndroidTest`; they run on the emulator in
 * WP-29/WP-57. The mapping itself is covered by JVM tests (`StreamGesturesTest`,
 * `StreamGeometryTest`); this covers the Compose wiring around it.
 */
@RunWith(AndroidJUnit4::class)
class StreamViewerTest {
    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    /** Records what the screen asks of its state holder. */
    private class Controls : StreamControls {
        val sizes = mutableListOf<Triple<Int, Int, Double>>()
        val modes = mutableListOf<ControlMode>()
        val sent = mutableListOf<InputMessage>()
        val zooms = mutableListOf<GestureOutput.ZoomPan>()
        val sticky = mutableListOf<KeyModifier>()
        val keys = mutableListOf<Pair<String, Set<KeyModifier>>>()
        val typed = mutableListOf<String>()
        val ended = mutableListOf<Boolean>()
        var resets = 0
        var relayAnswers = mutableListOf<Boolean>()
        var retries = 0
        var cancels = 0
        val picked = mutableListOf<Int>()
        var functionRow = 0

        override fun onViewSize(width: Int, height: Int, dpr: Double) {
            sizes += Triple(width, height, dpr)
        }

        override fun setMode(mode: ControlMode) {
            modes += mode
        }

        override fun zoomPan(step: GestureOutput.ZoomPan) {
            zooms += step
        }

        override fun resetZoom() {
            resets++
        }

        override fun send(message: InputMessage): Boolean {
            sent += message
            return true
        }

        override fun toggleSticky(modifier: KeyModifier) {
            sticky += modifier
        }

        override fun toggleFunctionRow() {
            functionRow++
        }

        override fun pressKey(code: String, extra: Set<KeyModifier>) {
            keys += code to extra
        }

        override fun typed(text: String) {
            typed += text
        }

        override fun end(closeApp: Boolean) {
            ended += closeApp
        }

        override fun cancelBeforeStart() {
            cancels++
        }

        override fun answerRelayWarning(continueStreaming: Boolean) {
            relayAnswers += continueStreaming
        }

        override fun retry() {
            retries++
        }

        override fun pickWindow(index: Int) {
            picked += index
        }

        var fronts = 0

        override fun bringToFront() {
            fronts++
        }
    }

    private val controls = Controls()
    private var left = 0
    private var fetched: Entry? = null

    private val app = StreamApp("powerpoint", "Microsoft PowerPoint")

    private fun state(
        phase: StreamPhase,
        mode: ControlMode = ControlMode.DIRECT,
        frame: FrameSize = FrameSize(0, 0),
        summary: EndSummary? = null,
        front: FrontStatus = FrontStatus.IDLE,
    ) = StreamScreenState(
        session = StreamUiState(phase = phase, app = app, session = "ses_1", front = front),
        title = "Deck.pptx",
        hostName = "studio-pc",
        mode = mode,
        frame = frame,
        summary = summary,
    )

    private fun show(state: StreamScreenState) {
        composeRule.setContent {
            GanderTheme {
                StreamScreenContent(
                    state = state,
                    controls = controls,
                    output = null,
                    onBack = { left++ },
                    onFetchSaved = { fetched = it },
                )
            }
        }
    }

    /** A key of the keyboard bar; the bar scrolls sideways and most keys start off a phone's screen. */
    private fun barKey(name: String) = composeRule.onNodeWithTag(StreamTags.key(name)).performScrollTo()

    @Test
    fun theFirstMeasuredSizeIsReportedSoTheStreamCanStart() {
        // The stream starts from Preparing: no session exists until the first size is reported.
        show(state(StreamPhase.Preparing))
        composeRule.waitForIdle()
        assertTrue("the view size reached the state holder", controls.sizes.isNotEmpty())
        assertTrue(controls.sizes.first().first > 0 && controls.sizes.first().second > 0)
    }

    @Test
    fun aSizeMeasuredDuringALiveStreamWaitsForTheLayoutToSettle() {
        // A live session (the screen recreated mid-stream, a rotation) reports its size only after
        // the layout settles (RESIZE_SETTLE_MS, 500 ms), so a rotation asks the host to fit once.
        show(state(StreamPhase.Streaming))
        composeRule.waitForIdle()
        assertTrue("not before the layout settled", controls.sizes.isEmpty())
        composeRule.mainClock.advanceTimeBy(600)
        composeRule.waitForIdle()
        assertTrue("the view size reached the state holder", controls.sizes.isNotEmpty())
        assertTrue(controls.sizes.first().first > 0 && controls.sizes.first().second > 0)
    }

    @Test
    fun theBannerNamesWhatTheHostIsDoing() {
        show(state(StreamPhase.Launching))
        composeRule.onNodeWithTag(StreamTags.BANNER).assertIsDisplayed()
    }

    @Test
    fun aFocusLostPauseOffersBringToFront() {
        show(state(StreamPhase.Paused(StreamReason.FOCUS_LOST)))
        composeRule.onNodeWithTag(StreamTags.BRING_TO_FRONT).assertIsDisplayed().performClick()
        assertEquals(1, controls.fronts)
        composeRule.onNodeWithTag(StreamTags.FRONT_RESULT).assertDoesNotExist()
    }

    @Test
    fun aRefusedBringToFrontSaysSoAndCanBeTriedAgain() {
        show(state(StreamPhase.Paused(StreamReason.FOCUS_LOST), front = FrontStatus.REFUSED))
        composeRule.onNodeWithTag(StreamTags.FRONT_RESULT).assertIsDisplayed()
        composeRule.onNodeWithTag(StreamTags.BRING_TO_FRONT).assertIsEnabled()
    }

    @Test
    fun otherPausesHaveNoBringToFront() {
        show(state(StreamPhase.Paused(StreamReason.LOCAL_USER_ACTIVE)))
        composeRule.onNodeWithTag(StreamTags.BANNER).assertIsDisplayed()
        composeRule.onNodeWithTag(StreamTags.BRING_TO_FRONT).assertDoesNotExist()
    }

    @Test
    fun theHostsCandidateWindowsAreTappableAndTheTapSendsThatIndex() {
        show(state(StreamPhase.PickWindow(listOf("Deck.pptx - PowerPoint", "Deck (2).pptx - PowerPoint"))))
        composeRule.onNodeWithTag(StreamTags.BANNER).assertIsDisplayed()
        composeRule.onNodeWithTag(StreamTags.pick(0)).assertIsDisplayed()
        composeRule.onNodeWithTag(StreamTags.pick(1)).assertIsDisplayed().performClick()
        assertEquals(listOf(1), controls.picked)
        composeRule.onNodeWithTag(StreamTags.PICK_GONE).assertDoesNotExist()
    }

    @Test
    fun aPickedWindowThatVanishedIsSaidSoAboveTheList() {
        show(state(StreamPhase.PickWindow(listOf("Deck.pptx - PowerPoint"), chosen = null, windowGone = true)))
        composeRule.onNodeWithTag(StreamTags.PICK_GONE).assertIsDisplayed()
        composeRule.onNodeWithTag(StreamTags.pick(0)).performClick()
        assertEquals(listOf(0), controls.picked)
    }

    @Test
    fun aStreamThatEndedWithNoWindowSaysWhyAndOffersTryAgain() {
        show(state(StreamPhase.Ended(EndCause.APP_UNAVAILABLE), summary = EndSummary(EndCause.APP_UNAVAILABLE, null)))
        composeRule.onNodeWithTag(StreamTags.ENDED_DIALOG).assertIsDisplayed()
    }

    @Test
    fun noBannerWhileStreaming() {
        show(state(StreamPhase.Streaming))
        composeRule.onNodeWithTag(StreamTags.BANNER).assertDoesNotExist()
    }

    @Test
    fun aPausedStreamSaysWhy() {
        show(state(StreamPhase.Paused(StreamReason.FOCUS_LOST)))
        composeRule.onNodeWithTag(StreamTags.BANNER).assertIsDisplayed()
    }

    @Test
    fun theModeIsLabelledOnScreenAndTogglesFromTheTopBar() {
        show(state(StreamPhase.Streaming, mode = ControlMode.DIRECT))
        composeRule.onNodeWithTag(StreamTags.MODE_TOGGLE).assertIsDisplayed().performClick()
        assertEquals(listOf(ControlMode.TRACKPAD), controls.modes)
    }

    @Test
    fun theKeyboardBarOpensFromTheTopBarAndSendsKeys() {
        show(state(StreamPhase.Streaming))
        composeRule.onNodeWithTag(StreamTags.KEYBOARD_BAR).assertDoesNotExist()
        composeRule.onNodeWithTag(StreamTags.KEYBOARD_TOGGLE).performClick()
        composeRule.onNodeWithTag(StreamTags.KEYBOARD_BAR).assertIsDisplayed()
        barKey("esc").performClick()
        barKey("undo").performClick()
        barKey(KeyModifier.CTRL.wire).performClick()
        assertEquals(
            listOf(KeyCodes.ESCAPE to emptySet<KeyModifier>(), KeyCodes.KEY_Z to setOf(KeyModifier.CTRL)),
            controls.keys,
        )
        assertEquals(listOf(KeyModifier.CTRL), controls.sticky)
    }

    @Test
    fun theTypeButtonOpensTheLocalTextBoxThatSendsTextMessages() {
        show(state(StreamPhase.Streaming))
        composeRule.onNodeWithTag(StreamTags.KEYBOARD_TOGGLE).performClick()
        barKey("type").performClick()
        composeRule.onNodeWithTag(StreamTags.TEXT_BOX).assertIsDisplayed()
        composeRule.onNodeWithTag(StreamTags.TEXT_FIELD).performTextInput("hello")
        composeRule.onNodeWithTag(StreamTags.TEXT_SEND).performClick()
        assertEquals(listOf("hello"), controls.typed)
    }

    @Test
    fun aTapOnThePictureIsALeftClickAtThatPointOfTheWindow() {
        // A 1000 x 2000 picture in whatever the screen is: the centre of the gesture layer is the centre of the picture.
        show(state(StreamPhase.Streaming, frame = FrameSize(1000, 2000)))
        composeRule.onNodeWithTag(StreamTags.GESTURE_LAYER).performTouchInput { click(center) }
        composeRule.waitForIdle()
        val down = controls.sent.filterIsInstance<InputMessage.Down>().single()
        val up = controls.sent.filterIsInstance<InputMessage.Up>().single()
        assertEquals(MouseButton.LEFT, down.b)
        assertEquals(0.5, down.x, 0.02)
        assertEquals(0.5, down.y, 0.02)
        assertEquals(MouseButton.LEFT, up.b)
    }
    @Test
    fun leavingALiveStreamAsksWhetherToLeaveTheAppOpenOrCloseIt() {
        show(state(StreamPhase.Streaming))
        composeRule.onNodeWithTag(StreamTags.TOP_BAR).assertIsDisplayed()
        composeRule.runOnUiThread { composeRule.activity.onBackPressedDispatcher.onBackPressed() }
        composeRule.onNodeWithTag(StreamTags.END_DIALOG).assertIsDisplayed()
        composeRule.onNodeWithTag(StreamTags.END_CLOSE_APP).performClick()
        assertEquals(listOf(true), controls.ended)
    }

    @Test
    fun theEndDialogsChoicesDoNotOverlapAndKeepStreamingEndsNothing() {
        // Regression: AlertDialog's dismiss slot was laid over the right half of "Close", so a tap
        // there kept streaming instead of closing the app.
        show(state(StreamPhase.Streaming))
        composeRule.runOnUiThread { composeRule.activity.onBackPressedDispatcher.onBackPressed() }
        val tags = listOf(StreamTags.END_LEAVE_OPEN, StreamTags.END_CLOSE_APP, StreamTags.END_KEEP)
        val bounds = tags.map { composeRule.onNodeWithTag(it).assertIsDisplayed().fetchSemanticsNode().boundsInWindow }
        for (i in bounds.indices) {
            for (j in i + 1 until bounds.size) {
                assertTrue("${tags[i]} ${bounds[i]} overlaps ${tags[j]} ${bounds[j]}", !bounds[i].overlaps(bounds[j]))
            }
        }
        composeRule.onNodeWithTag(StreamTags.END_KEEP).performClick()
        composeRule.onNodeWithTag(StreamTags.END_DIALOG).assertDoesNotExist()
        assertTrue("keep streaming ends nothing", controls.ended.isEmpty())
    }

    @Test
    fun theRelayWarningComesBeforeTheStream() {
        show(state(StreamPhase.ConfirmRelay(NetworkAdapter.TAILSCALE, RelayPath.DERP)))
        composeRule.onNodeWithTag(StreamTags.RELAY_DIALOG).assertIsDisplayed()
        composeRule.onNodeWithTag(StreamTags.RELAY_CONTINUE).performClick()
        assertEquals(listOf(true), controls.relayAnswers)
    }

    @Test
    fun aStreamThatCannotStartExplainsWhyAndOffersTryAgain() {
        show(state(StreamPhase.Unsupported(StreamPhase.Unsupported.Why.PHONE_HAS_NO_H264_DECODER)))
        composeRule.onNodeWithTag(StreamTags.FAILURE).assertIsDisplayed()
    }

    @Test
    fun aFileSavedDuringTheSessionOffersTheFetchShortcut() {
        val saved = SampleData.file("Deck.pptx", size = 9999)
        show(state(StreamPhase.Ended(EndCause.WINDOW_CLOSED), summary = EndSummary(EndCause.WINDOW_CLOSED, saved)))
        composeRule.onNodeWithTag(StreamTags.ENDED_DIALOG).assertIsDisplayed()
        composeRule.onNodeWithTag(StreamTags.FETCH_SAVED).assertIsDisplayed().performClick()
        assertEquals(saved, fetched)
    }

    @Test
    fun anEndWithNoSavedFileHasNoFetchShortcut() {
        show(state(StreamPhase.Ended(EndCause.HOST_KILL_SWITCH), summary = EndSummary(EndCause.HOST_KILL_SWITCH, null)))
        composeRule.onNodeWithTag(StreamTags.ENDED_DIALOG).assertIsDisplayed()
        composeRule.onNodeWithTag(StreamTags.FETCH_SAVED).assertDoesNotExist()
    }

    @Test
    fun leavingOnPurposeWithNothingSavedClosesTheScreenWithoutADialog() {
        show(state(StreamPhase.Ended(EndCause.YOU_LEFT), summary = EndSummary(EndCause.YOU_LEFT, null)))
        composeRule.waitForIdle()
        assertEquals(1, left)
        composeRule.onNodeWithTag(StreamTags.ENDED_DIALOG).assertDoesNotExist()
    }
}
