package fyi.arlo.gander.ui.stream

import fyi.arlo.gander.core.data.stream.ControlMode
import fyi.arlo.gander.core.net.protocol.InputMessage
import fyi.arlo.gander.core.net.protocol.MouseButton
import fyi.arlo.gander.ui.preview.ZoomTransform
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The gesture map of the client spec ("Stream viewer"), driven with plain pointer events:
 *
 * - Direct touch: tap = left click at the point; long press = right click; two-finger drag =
 *   scroll; double-tap-hold-drag = drag.
 * - Trackpad: one-finger move = pointer; tap = click; two-finger tap = right click; two-finger
 *   drag = scroll.
 * - Both: pinch and three-finger drag zoom and pan locally and send nothing.
 */
class StreamGesturesTest {
    /** A 1000 x 2000 view that the picture fills, so a point's normalised value is its pixel over the size. */
    private var viewport = StreamViewport(1000f, 2000f, frameWidth = 1000, frameHeight = 2000)

    private val config = GestureConfig(
        touchSlop = 10f,
        tapMaxMs = 300L,
        longPressMs = 500L,
        doubleTapMs = 300L,
        doubleTapSlop = 40f,
        scrollPxPerNotch = 50f,
    )

    private fun recogniser(mode: ControlMode) = StreamGestureRecognizer(config, { viewport }, mode)

    private fun List<GestureOutput>.messages(): List<InputMessage> =
        filterIsInstance<GestureOutput.Send>().map { it.message }

    private fun List<GestureOutput>.zooms(): List<GestureOutput.ZoomPan> = filterIsInstance<GestureOutput.ZoomPan>()

    private fun left(down: Boolean, x: Double, y: Double): InputMessage =
        if (down) InputMessage.Down(MouseButton.LEFT, x, y) else InputMessage.Up(MouseButton.LEFT, x, y)

    private fun right(down: Boolean, x: Double, y: Double): InputMessage =
        if (down) InputMessage.Down(MouseButton.RIGHT, x, y) else InputMessage.Up(MouseButton.RIGHT, x, y)

    /** Two fingers that moved in the same touch event. */
    private fun StreamGestureRecognizer.move2(id1: Long, x1: Float, y1: Float, id2: Long, x2: Float, y2: Float, t: Long) =
        onMoved(listOf(PointerMove(id1, x1, y1), PointerMove(id2, x2, y2)), t)

    // Direct touch ----------------------------------------------------------------------------------

    @Test
    fun aDirectTapIsALeftClickAtThePoint() {
        val g = recogniser(ControlMode.DIRECT)
        assertTrue(g.onDown(1, 250f, 500f, 0).isEmpty())
        val out = g.onUp(1, 250f, 500f, 80)
        assertEquals(listOf(left(true, 0.25, 0.25), left(false, 0.25, 0.25)), out.messages())
        assertEquals("the viewer shows where it clicked", Norm(0.25, 0.25), (out.last() as GestureOutput.Pointer).position)
    }

    @Test
    fun aDirectTapOnALetterboxBarClicksNothing() {
        viewport = StreamViewport(1000f, 2000f, frameWidth = 1000, frameHeight = 1000) // picture from y 500 to 1500
        val g = recogniser(ControlMode.DIRECT)
        g.onDown(1, 500f, 100f, 0)
        assertTrue(g.onUp(1, 500f, 100f, 60).isEmpty())
    }

    @Test
    fun aDirectTapIsMappedThroughTheZoom() {
        viewport = viewport.copy(zoom = ZoomTransform(scale = 2f))
        val g = recogniser(ControlMode.DIRECT)
        // 250 px right of the centre at 2x is 125 px right of the centre of the content.
        g.onDown(1, 750f, 1000f, 0)
        assertEquals(listOf(left(true, 0.625, 0.5), left(false, 0.625, 0.5)), g.onUp(1, 750f, 1000f, 50).messages())
    }

    @Test
    fun aLongPressIsARightClickThatFiresWhileTheFingerIsStillDown() {
        val g = recogniser(ControlMode.DIRECT)
        g.onDown(1, 400f, 800f, 1000)
        assertEquals("the timer asks to be woken when the press is long enough", 500L, g.msUntilTick(1000))
        assertEquals(300L, g.msUntilTick(1200))
        assertTrue("not yet", g.onTick(1400).isEmpty())
        val out = g.onTick(1500)
        assertEquals(listOf(right(true, 0.4, 0.4), right(false, 0.4, 0.4)), out.messages())
        assertNull("nothing left to time", g.msUntilTick(1600))
        assertTrue("lifting after a long press clicks nothing more", g.onUp(1, 400f, 800f, 1700).isEmpty())
    }

    @Test
    fun aSlowTapBelowTheLongPressIsStillALeftClick() {
        val g = recogniser(ControlMode.DIRECT)
        g.onDown(1, 400f, 800f, 0)
        val out = g.onUp(1, 400f, 800f, 450)
        assertEquals(listOf(left(true, 0.4, 0.4), left(false, 0.4, 0.4)), out.messages())
    }

    @Test
    fun aTouchThatMovedIsNeitherATapNorALongPress() {
        val g = recogniser(ControlMode.DIRECT)
        g.onDown(1, 400f, 800f, 0)
        g.onMove(1, 440f, 800f, 100)
        assertNull(g.msUntilTick(100))
        assertTrue(g.onTick(900).isEmpty())
        assertTrue(g.onUp(1, 440f, 800f, 1000).isEmpty())
    }

    @Test
    fun aDirectOneFingerDragMovesThePointerWithoutAButton() {
        val g = recogniser(ControlMode.DIRECT)
        g.onDown(1, 400f, 800f, 0)
        assertTrue("inside the slop nothing happens", g.onMove(1, 405f, 800f, 20).isEmpty())
        val out = g.onMove(1, 500f, 800f, 40)
        assertEquals(listOf<InputMessage>(InputMessage.Move(0.5, 0.4)), out.messages())
        assertEquals(listOf<InputMessage>(InputMessage.Move(0.6, 0.4)), g.onMove(1, 600f, 800f, 60).messages())
        assertTrue(g.onUp(1, 600f, 800f, 80).isEmpty())
    }

    @Test
    fun aTapThenATouchAndDragIsADragWithTheLeftButton() {
        val g = recogniser(ControlMode.DIRECT)
        g.onDown(1, 400f, 800f, 0)
        g.onUp(1, 400f, 800f, 50)
        // The second touch comes right after, at the same place.
        assertTrue(g.onDown(1, 405f, 800f, 200).isEmpty())
        val start = g.onMove(1, 460f, 800f, 230)
        assertEquals(
            listOf(left(true, 0.405, 0.4), InputMessage.Move(0.46, 0.4)),
            start.messages(),
        )
        assertEquals(listOf<InputMessage>(InputMessage.Move(0.6, 0.6)), g.onMove(1, 600f, 1200f, 260).messages())
        assertEquals(listOf(left(false, 0.6, 0.6)), g.onUp(1, 600f, 1200f, 300).messages())
    }

    @Test
    fun aTapThenAHeldTouchBecomesADragAfterTheTapTime() {
        val g = recogniser(ControlMode.DIRECT)
        g.onDown(1, 400f, 800f, 0)
        g.onUp(1, 400f, 800f, 50)
        g.onDown(1, 400f, 800f, 200)
        assertEquals("the hold is timed like the tap limit", 300L, g.msUntilTick(200))
        assertTrue(g.onTick(400).isEmpty())
        assertEquals(listOf(left(true, 0.4, 0.4)), g.onTick(500).messages())
        assertEquals(listOf(left(false, 0.4, 0.4)), g.onUp(1, 400f, 800f, 900).messages())
    }

    @Test
    fun aSecondQuickTapIsADoubleClick() {
        val g = recogniser(ControlMode.DIRECT)
        g.onDown(1, 400f, 800f, 0)
        g.onUp(1, 400f, 800f, 50)
        g.onDown(1, 402f, 801f, 150)
        val second = g.onUp(1, 402f, 801f, 200).messages()
        assertEquals(2, second.size)
        assertTrue(second[0] is InputMessage.Down && second[1] is InputMessage.Up)
    }

    @Test
    fun aTouchTooLateOrTooFarFromTheLastTapIsNotADragCandidate() {
        val late = recogniser(ControlMode.DIRECT)
        late.onDown(1, 400f, 800f, 0)
        late.onUp(1, 400f, 800f, 50)
        late.onDown(1, 400f, 800f, 600)
        assertEquals("a fresh long-press timer, not the drag timer", 500L, late.msUntilTick(600))

        val far = recogniser(ControlMode.DIRECT)
        far.onDown(1, 400f, 800f, 0)
        far.onUp(1, 400f, 800f, 50)
        far.onDown(1, 800f, 800f, 150)
        assertEquals(500L, far.msUntilTick(150))
    }

    @Test
    fun twoFingersDraggingUpScrollDownByTheWheelUnitsOfTheirTravel() {
        val g = recogniser(ControlMode.DIRECT)
        g.onDown(1, 400f, 1000f, 0)
        g.onDown(2, 600f, 1000f, 5)
        // 15 px up: past the slop, so the gesture locks to scroll and counts the travel from the start.
        val out = g.move2(1, 400f, 985f, 2, 600f, 985f, 20)
        // 15 px at 50 px per notch is 36 wheel units; fingers moving up scroll the content down (negative).
        assertEquals(listOf<InputMessage>(InputMessage.Scroll(0, -36, 0.5, 0.4925)), out.messages())
        assertTrue("a scroll never zooms", out.zooms().isEmpty())
        val more = g.move2(1, 400f, 935f, 2, 600f, 935f, 40)
        assertEquals("50 px more is one more notch: 120 units", listOf<InputMessage>(InputMessage.Scroll(0, -120, 0.5, 0.4675)), more.messages())
    }

    /**
     * The sign table of the protocol's `scroll` message, Windows wheel convention: positive `dy` =
     * the wheel turned away from the user (the view moves up), positive `dx` = a tilt to the right
     * (the view moves right). The host forwards both unchanged as `MOUSEEVENTF_WHEEL` and
     * `MOUSEEVENTF_HWHEEL` (`gander_core::input::mapping::wheel_data`, pinned there too). With
     * natural scrolling the content follows the fingers: up = negative dy, down = positive dy,
     * left = positive dx, right = negative dx. If this fails, change the host and this together.
     */
    @Test
    fun scrollSignFollowsTheWindowsWheelConvention() {
        fun scrollFor(dxPx: Float, dyPx: Float): InputMessage.Scroll {
            val g = recogniser(ControlMode.DIRECT)
            g.onDown(1, 400f, 1000f, 0)
            g.onDown(2, 600f, 1000f, 5)
            // 50 px at 50 px per notch is exactly one notch: 120 wheel units.
            return g.move2(1, 400f + dxPx, 1000f + dyPx, 2, 600f + dxPx, 1000f + dyPx, 20)
                .messages().filterIsInstance<InputMessage.Scroll>().single()
        }

        val up = scrollFor(0f, -50f)
        assertEquals("fingers up: the view moves down, the wheel turns toward the user", -120, up.dy)
        assertEquals(0, up.dx)
        val down = scrollFor(0f, 50f)
        assertEquals("fingers down: the view moves up, the wheel turns away from the user", 120, down.dy)
        assertEquals(0, down.dx)
        val left = scrollFor(-50f, 0f)
        assertEquals("fingers left: the view moves right, a tilt to the right", 120, left.dx)
        assertEquals(0, left.dy)
        val right = scrollFor(50f, 0f)
        assertEquals("fingers right: the view moves left, a tilt to the left", -120, right.dx)
        assertEquals(0, right.dy)
        // On the wire the units are the protocol's, 120 per notch, signs as above.
        assertEquals(
            """{"t":"scroll","dx":0,"dy":120,"x":0.5,"y":0.5}""",
            fyi.arlo.gander.core.data.stream.StreamWire.encode(down.copy(x = 0.5, y = 0.5)),
        )
    }

    @Test
    fun smallTwoFingerMovementsBelowTheSlopDoNothingYet() {
        val g = recogniser(ControlMode.DIRECT)
        g.onDown(1, 400f, 1000f, 0)
        g.onDown(2, 600f, 1000f, 5)
        assertTrue(g.move2(1, 400f, 996f, 2, 600f, 996f, 20).isEmpty())
    }

    @Test
    fun twoFingersDraggingDownScrollUp() {
        val g = recogniser(ControlMode.DIRECT)
        g.onDown(1, 400f, 1000f, 0)
        g.onDown(2, 600f, 1000f, 5)
        val scroll = g.move2(1, 400f, 1050f, 2, 600f, 1050f, 20).messages().filterIsInstance<InputMessage.Scroll>().single()
        assertTrue("content follows the fingers down, the wheel turns away from the user", scroll.dy > 0)
        assertEquals(120, scroll.dy)
    }

    @Test
    fun twoFingersDraggingLeftScrollRight() {
        val g = recogniser(ControlMode.DIRECT)
        g.onDown(1, 400f, 1000f, 0)
        g.onDown(2, 600f, 1000f, 5)
        // Moving together sideways is a scroll, not a pinch, however far it goes in one event.
        val out = g.move2(1, 350f, 1000f, 2, 550f, 1000f, 20)
        assertTrue("no zoom", out.zooms().isEmpty())
        val scroll = out.messages().filterIsInstance<InputMessage.Scroll>().single()
        assertEquals("fingers moving left reveal what is to the right", 120, scroll.dx)
        assertEquals(0, scroll.dy)
    }

    @Test
    fun scrollCarriesTheFractionsOfAWheelUnitToTheNextStep() {
        val g = recogniser(ControlMode.DIRECT)
        g.onDown(1, 400f, 1000f, 0)
        g.onDown(2, 600f, 1000f, 5)
        var total = g.move2(1, 400f, 985f, 2, 600f, 985f, 20).messages().filterIsInstance<InputMessage.Scroll>().sumOf { it.dy }
        assertEquals(-36, total)
        // 1 px steps are 2.4 units each: whole units go out, the rest waits for the next step.
        for (i in 1..10) {
            val step = g.move2(1, 400f, 985f - i, 2, 600f, 985f - i, 30L + i)
            total += step.messages().filterIsInstance<InputMessage.Scroll>().sumOf { it.dy }
        }
        assertEquals("10 px of travel is exactly 24 units more", -36 - 24, total)
    }

    @Test
    fun twoFingersSpreadingZoomLocallyAndSendNothing() {
        val g = recogniser(ControlMode.DIRECT)
        g.onDown(1, 400f, 1000f, 0)
        g.onDown(2, 600f, 1000f, 5)
        val first = g.onMoved(listOf(PointerMove(1, 350f, 1000f)), 20)
        assertTrue("a pinch is never sent to the host", first.messages().isEmpty())
        val step = first.zooms().single()
        assertEquals("200 px apart became 250", 1.25f, step.zoomChange, 0.0001f)
        assertEquals("the pinch centre moved 25 px left with it", -25f, step.panX, 0.0001f)
        assertEquals(475f, step.centroidX, 0.0001f)
        val second = g.onMoved(listOf(PointerMove(2, 650f, 1000f)), 30)
        assertTrue(second.messages().isEmpty())
        assertEquals(1.2f, second.zooms().single().zoomChange, 0.0001f)
    }

    @Test
    fun aGestureThatStartedAsAPinchStaysAPinch() {
        val g = recogniser(ControlMode.DIRECT)
        g.onDown(1, 400f, 1000f, 0)
        g.onDown(2, 600f, 1000f, 5)
        g.onMoved(listOf(PointerMove(1, 350f, 1000f)), 20)
        // Now both fingers travel up with a constant spread: still zoom and pan, not a scroll.
        val out = g.move2(1, 350f, 900f, 2, 600f, 900f, 30)
        assertTrue(out.messages().isEmpty())
        val step = out.zooms().single()
        assertEquals(1f, step.zoomChange, 0.0001f)
        assertEquals(-100f, step.panY, 0.0001f)
    }

    @Test
    fun threeFingersPanTheViewWithoutZoomingOrSending() {
        val g = recogniser(ControlMode.TRACKPAD)
        g.onDown(1, 300f, 1000f, 0)
        g.onDown(2, 500f, 1000f, 2)
        g.onDown(3, 700f, 1000f, 4)
        val out = g.onMoved(listOf(PointerMove(1, 330f, 1010f), PointerMove(2, 530f, 1010f), PointerMove(3, 730f, 1010f)), 20)
        assertTrue(out.messages().isEmpty())
        val step = out.zooms().single()
        assertEquals(1f, step.zoomChange, 0f)
        assertEquals(30f, step.panX, 0.001f)
        assertEquals(10f, step.panY, 0.001f)
    }

    @Test
    fun aTwoFingerTapInDirectModeDoesNothing() {
        val g = recogniser(ControlMode.DIRECT)
        g.onDown(1, 400f, 1000f, 0)
        g.onDown(2, 600f, 1000f, 20)
        assertTrue(g.onUp(1, 400f, 1000f, 80).isEmpty())
        assertTrue(g.onUp(2, 600f, 1000f, 90).isEmpty())
    }

    @Test
    fun aSecondFingerCancelsTheLongPressOfTheFirst() {
        val g = recogniser(ControlMode.DIRECT)
        g.onDown(1, 400f, 1000f, 0)
        g.onDown(2, 600f, 1000f, 100)
        assertNull(g.msUntilTick(100))
        assertTrue(g.onTick(900).isEmpty())
    }

    // Trackpad ----------------------------------------------------------------------------------------

    @Test
    fun aTrackpadDragMovesThePointerByTheFingersTravel() {
        val g = recogniser(ControlMode.TRACKPAD)
        assertEquals(Norm(0.5, 0.5), g.pointer)
        g.onDown(1, 100f, 100f, 0)
        // 100 px right and 200 px down on a 1000 x 2000 picture is a tenth each way.
        val out = g.onMove(1, 200f, 300f, 30)
        assertEquals(listOf<InputMessage>(InputMessage.Move(0.6, 0.6)), out.messages())
        assertEquals(Norm(0.6, 0.6), g.pointer)
        // Further motion is relative to the last event.
        assertEquals(listOf<InputMessage>(InputMessage.Move(0.65, 0.65)), g.onMove(1, 250f, 400f, 60).messages())
        assertTrue("moving never clicks", g.onUp(1, 250f, 400f, 90).isEmpty())
    }

    @Test
    fun aTrackpadTapClicksWhereThePointerIsNotWhereTheFingerLanded() {
        val g = recogniser(ControlMode.TRACKPAD)
        g.onDown(1, 900f, 1800f, 0)
        val out = g.onUp(1, 900f, 1800f, 60)
        assertEquals(listOf(left(true, 0.5, 0.5), left(false, 0.5, 0.5)), out.messages())
    }

    @Test
    fun aTrackpadTouchHeldPastTheTapTimeIsNotAClick() {
        val g = recogniser(ControlMode.TRACKPAD)
        g.onDown(1, 900f, 1800f, 0)
        assertNull("no long-press action on the trackpad", g.msUntilTick(0))
        assertTrue(g.onUp(1, 900f, 1800f, 400).isEmpty())
    }

    @Test
    fun aTwoFingerTapOnTheTrackpadIsARightClickAtThePointer() {
        val g = recogniser(ControlMode.TRACKPAD)
        g.onDown(1, 400f, 1000f, 0)
        g.onDown(2, 600f, 1000f, 20)
        val out = g.onUp(1, 400f, 1000f, 90)
        assertEquals(listOf(right(true, 0.5, 0.5), right(false, 0.5, 0.5)), out.messages())
        assertTrue("the second finger lifting adds nothing", g.onUp(2, 600f, 1000f, 100).isEmpty())
    }

    @Test
    fun aTwoFingerTouchThatTookTooLongIsNotATap() {
        val g = recogniser(ControlMode.TRACKPAD)
        g.onDown(1, 400f, 1000f, 0)
        g.onDown(2, 600f, 1000f, 20)
        assertTrue(g.onUp(1, 400f, 1000f, 500).isEmpty())
    }

    @Test
    fun twoFingersDraggingOnTheTrackpadScrollAtThePointer() {
        val g = recogniser(ControlMode.TRACKPAD)
        // Move the pointer first.
        g.onDown(1, 100f, 100f, 0)
        g.onMove(1, 300f, 300f, 20)
        g.onUp(1, 300f, 300f, 40)
        val pointer = g.pointer
        g.onDown(1, 400f, 1000f, 1000)
        g.onDown(2, 600f, 1000f, 1005)
        val out = g.move2(1, 400f, 950f, 2, 600f, 950f, 1020).messages().filterIsInstance<InputMessage.Scroll>()
        assertTrue(out.isNotEmpty())
        assertTrue("scrolls where the pointer is", out.all { it.x == pointer.x && it.y == pointer.y })
        assertTrue(out.sumOf { it.dy } < 0)
    }

    @Test
    fun aTrackpadTapThenADragHoldsTheButtonAtThePointer() {
        val g = recogniser(ControlMode.TRACKPAD)
        g.onDown(1, 100f, 100f, 0)
        g.onUp(1, 100f, 100f, 50)
        g.onDown(1, 100f, 100f, 200)
        val out = g.onMove(1, 200f, 100f, 230).messages()
        assertEquals(listOf(left(true, 0.5, 0.5), InputMessage.Move(0.6, 0.5)), out)
        assertEquals(listOf(left(false, 0.6, 0.5)), g.onUp(1, 200f, 100f, 260).messages())
    }

    @Test
    fun theTrackpadPointerStopsAtTheEdgesOfThePicture() {
        val g = recogniser(ControlMode.TRACKPAD)
        g.onDown(1, 100f, 100f, 0)
        val out = g.onMove(1, 5000f, 5000f, 30).messages().single() as InputMessage.Move
        assertEquals(Norm.MAX, out.x, 1e-9)
        assertEquals(Norm.MAX, out.y, 1e-9)
        val back = g.onMove(1, -5000f, -5000f, 60).messages().single() as InputMessage.Move
        assertEquals(0.0, back.x, 1e-9)
        assertEquals(0.0, back.y, 1e-9)
    }

    @Test
    fun zoomedInTheTrackpadPointerMovesLessPerFingerTravel() {
        viewport = viewport.copy(zoom = ZoomTransform(scale = 2f))
        val g = recogniser(ControlMode.TRACKPAD)
        g.onDown(1, 100f, 100f, 0)
        val move = g.onMove(1, 300f, 100f, 30).messages().single() as InputMessage.Move
        assertEquals(0.6, move.x, 1e-9)
    }

    // Both modes ---------------------------------------------------------------------------------------

    @Test
    fun switchingModeMidDragReleasesTheButton() {
        val g = recogniser(ControlMode.DIRECT)
        g.onDown(1, 400f, 800f, 0)
        g.onUp(1, 400f, 800f, 50)
        g.onDown(1, 400f, 800f, 150)
        g.onMove(1, 500f, 800f, 180) // the drag has begun: the left button is down
        val released = g.switchMode(ControlMode.TRACKPAD)
        assertEquals(listOf<InputMessage>(left(false, 0.5, 0.4)), released.messages())
        assertEquals(ControlMode.TRACKPAD, g.mode)
        assertTrue("the rest of that touch is ignored", g.onMove(1, 600f, 800f, 200).isEmpty())
        assertTrue(g.onUp(1, 600f, 800f, 220).isEmpty())
    }

    @Test
    fun switchingToTheSameModeChangesNothing() {
        val g = recogniser(ControlMode.DIRECT)
        assertTrue(g.switchMode(ControlMode.DIRECT).isEmpty())
    }

    @Test
    fun aCancelledTouchNeverLeavesAButtonDown() {
        val g = recogniser(ControlMode.DIRECT)
        g.onDown(1, 400f, 800f, 0)
        g.onUp(1, 400f, 800f, 50)
        g.onDown(1, 400f, 800f, 150)
        g.onMove(1, 500f, 800f, 180)
        val out = g.onCancel()
        assertEquals(listOf<InputMessage>(left(false, 0.5, 0.4)), out.messages())
        assertTrue(!g.isActive)
        // The next touch starts clean.
        g.onDown(1, 100f, 100f, 1000)
        assertEquals(2, g.onUp(1, 100f, 100f, 1050).messages().size)
    }

    @Test
    fun aSecondFingerDuringADragReleasesTheButtonFirst() {
        val g = recogniser(ControlMode.DIRECT)
        g.onDown(1, 400f, 800f, 0)
        g.onUp(1, 400f, 800f, 50)
        g.onDown(1, 400f, 800f, 150)
        g.onMove(1, 500f, 800f, 180)
        val out = g.onDown(2, 700f, 800f, 200)
        assertEquals(listOf<InputMessage>(left(false, 0.5, 0.4)), out.messages())
    }

    @Test
    fun fourFingersAreIgnored() {
        val g = recogniser(ControlMode.DIRECT)
        for (i in 1..4) g.onDown(i.toLong(), 100f * i, 1000f, i.toLong())
        assertTrue(g.onMove(1, 150f, 1100f, 20).isEmpty())
        for (i in 1..4) assertTrue(g.onUp(i.toLong(), 100f * i, 1000f, 30L + i).isEmpty())
        assertTrue(!g.isActive)
    }

    // The timer on the events' own clock (GestureTimer) ------------------------------------------

    @Test
    fun aQuickTapWhoseEventsStartAtZeroIsALeftClickNotALongPress() {
        // Compose's test input stamps a tap's down at 0 and its up at 16 ms, whatever the device's
        // uptime is: the wait before the up is measured on that clock, so it is the whole long press.
        val g = recogniser(ControlMode.DIRECT)
        val timer = GestureTimer(g)
        timer.onEvent(0)
        g.onDown(1, 250f, 500f, 0)
        assertEquals(500L, timer.waitMs())
        timer.onEvent(16)
        val out = g.onUp(1, 250f, 500f, 16)
        assertEquals(listOf(left(true, 0.25, 0.25), left(false, 0.25, 0.25)), out.messages())
        assertNull("nothing left to time after the finger is up", timer.waitMs())
    }

    @Test
    fun aTouchHeldPastTheLongPressOnTheEventClockIsARightClick() {
        val g = recogniser(ControlMode.DIRECT)
        val timer = GestureTimer(g)
        timer.onEvent(1_000_000)
        g.onDown(1, 250f, 500f, 1_000_000)
        val wait = timer.waitMs()!!
        assertEquals(500L, wait)
        assertEquals(listOf(right(true, 0.25, 0.25), right(false, 0.25, 0.25)), timer.onTimeout(wait).messages())
        timer.onEvent(1_000_700)
        assertTrue("the up after a long press sends nothing more", g.onUp(1, 250f, 500f, 1_000_700).messages().isEmpty())
    }

    @Test
    fun aTimeoutShorterThanTheLongPressFiresNothing() {
        val g = recogniser(ControlMode.DIRECT)
        val timer = GestureTimer(g)
        timer.onEvent(0)
        g.onDown(1, 250f, 500f, 0)
        assertTrue(timer.onTimeout(200).isEmpty())
        assertEquals("the rest of the long press", 300L, timer.waitMs())
        timer.onEvent(250)
        assertEquals(listOf(left(true, 0.25, 0.25), left(false, 0.25, 0.25)), g.onUp(1, 250f, 500f, 250).messages())
    }

    @Test
    fun noEventYetMeansNoTimer() {
        val timer = GestureTimer(recogniser(ControlMode.DIRECT))
        assertNull(timer.waitMs())
        assertTrue(timer.onTimeout(500).isEmpty())
    }
}
