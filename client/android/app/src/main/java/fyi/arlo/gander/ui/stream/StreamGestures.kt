package fyi.arlo.gander.ui.stream

import fyi.arlo.gander.core.data.stream.ControlMode
import fyi.arlo.gander.core.net.protocol.InputMessage
import fyi.arlo.gander.core.net.protocol.MouseButton
import kotlin.math.abs
import kotlin.math.hypot
import kotlin.math.roundToInt

// The stream viewer's gesture map (client spec, "Stream viewer"), as a recogniser that is fed
// plain pointer events and answers with protocol messages and local zoom steps. No Compose or
// Android types, so every gesture of the map is covered by JVM tests (`StreamGesturesTest`); the
// Compose side (`Modifier.streamGestures`) only forwards `PointerInputChange`s.
//
//   Direct touch                               Trackpad
//   tap               left click at the point  tap              left click at the pointer
//   long press        right click              two-finger tap   right click at the pointer
//   two-finger drag   scroll                   one-finger drag  moves the pointer
//   tap, then touch-hold-drag  drag            two-finger drag  scroll
//                                              tap, then touch-and-drag  drag (the usual trackpad tap-drag)
//
// In both modes two fingers that spread or pinch zoom the viewer locally (the pinch's centre
// also pans), three fingers dragging pan it, and a one-finger drag in Direct mode just moves the
// pointer (no button). Zoom and pan never reach the host.

/** What the recogniser asks for. */
sealed interface GestureOutput {
    /** An input message for the host, in the order it must be sent. */
    data class Send(val message: InputMessage) : GestureOutput

    /** A local zoom step for `ZoomMath.applyGesture`: scale by [zoomChange] and pan by ([panX], [panY]) about ([centroidX], [centroidY]). */
    data class ZoomPan(
        val zoomChange: Float,
        val panX: Float,
        val panY: Float,
        val centroidX: Float,
        val centroidY: Float,
    ) : GestureOutput

    /** The viewer's own pointer is now at [position] (the host captures without a cursor). */
    data class Pointer(val position: Norm) : GestureOutput
}

/** Timing and distance thresholds, in view pixels and milliseconds. */
data class GestureConfig(
    /** Finger travel that turns a touch into a drag (Android's touch slop). */
    val touchSlop: Float,
    /** A touch that ends within this time (Trackpad) is a tap. */
    val tapMaxMs: Long = 300L,
    /** A Direct touch held still this long is a right click; also its tap limit. */
    val longPressMs: Long = 500L,
    /** A touch within this time and [doubleTapSlop] of the last tap's end arms tap-and-drag. */
    val doubleTapMs: Long = 300L,
    val doubleTapSlop: Float = touchSlop * 4f,
    /** Finger travel that scrolls one wheel notch (120 units). */
    val scrollPxPerNotch: Float = touchSlop * 5f,
    /** Pointer travel per finger travel in Trackpad mode. */
    val trackpadGain: Float = 1f,
)

/** One finger's new position in a batch of moves (a touch event reports every finger that moved). */
data class PointerMove(val id: Long, val x: Float, val y: Float)

class StreamGestureRecognizer(
    private val config: GestureConfig,
    /** The view, picture and zoom as they are right now; read on every event. */
    private val viewport: () -> StreamViewport,
    mode: ControlMode = ControlMode.DEFAULT,
) {
    private class Finger(val id: Long, var x: Float, var y: Float, val downX: Float, val downY: Float, val downAt: Long)

    private enum class Lock { NONE, SCROLL, PINCH }

    private class Two(
        val startAt: Long,
        val startCx: Float,
        val startCy: Float,
        val startDist: Float,
        /** False when the second finger came after a move or a held press: that is not the start of a tap. */
        val tapOk: Boolean,
    ) {
        var lock = Lock.NONE
        var lastCx = startCx
        var lastCy = startCy
        var lastDist = startDist

        /** Wheel units not yet sent (finger travel is fractional). */
        var accX = 0f
        var accY = 0f
    }

    private sealed interface Phase {
        data object Idle : Phase

        /** One finger down, nothing decided. [armed]: it came right after a tap, so it may become a drag. */
        data class Pending(val armed: Boolean) : Phase

        /** One finger moving the pointer, no button. */
        data object Moving : Phase

        /** The long press fired (right click sent); wait for the finger to lift. */
        data object Held : Phase

        /** The left button is down; the finger moves it. */
        data object Dragging : Phase

        data class TwoFingers(val state: Two) : Phase

        /** Three fingers pan the view. */
        data class ThreeFingers(var lastCx: Float, var lastCy: Float) : Phase

        /** Decided or cancelled; ignore the rest of the gesture until every finger is up. */
        data object Done : Phase
    }

    private class Tap(val at: Long, val x: Float, val y: Float)

    var mode: ControlMode = mode
        private set

    /**
     * Changes the mode. A change in the middle of a gesture must not leave a button down: the
     * returned outputs release it, and the rest of the gesture is ignored.
     */
    fun switchMode(newMode: ControlMode): List<GestureOutput> {
        if (newMode == mode) return emptyList()
        val out = ArrayList<GestureOutput>(1)
        cancelIntoDone(out)
        mode = newMode
        return out
    }

    /** The viewer's pointer: where Trackpad mode clicks and scrolls, and the last point Direct mode touched. */
    var pointer: Norm = Norm(0.5, 0.5)
        private set

    private val fingers = LinkedHashMap<Long, Finger>()
    private var phase: Phase = Phase.Idle
    private var lastTap: Tap? = null

    /** True while a touch is in progress (the Compose side keeps its tick running). */
    val isActive: Boolean get() = fingers.isNotEmpty()

    fun onDown(id: Long, x: Float, y: Float, t: Long): List<GestureOutput> {
        val out = ArrayList<GestureOutput>(2)
        fingers[id] = Finger(id, x, y, x, y, t)
        when (fingers.size) {
            1 -> {
                val armed = lastTap?.let { tap ->
                    t - tap.at <= config.doubleTapMs && hypot(x - tap.x, y - tap.y) <= config.doubleTapSlop
                } == true
                phase = Phase.Pending(armed)
            }

            2 -> {
                val wasTouch = phase is Phase.Pending
                releaseButton(out)
                val (cx, cy) = centroid()
                phase = Phase.TwoFingers(Two(fingers.values.first().downAt, cx, cy, spread(), tapOk = wasTouch))
                lastTap = null
            }

            3 -> {
                val (cx, cy) = centroid()
                phase = Phase.ThreeFingers(cx, cy)
            }

            else -> phase = Phase.Done
        }
        return out
    }


    fun onMove(id: Long, x: Float, y: Float, t: Long): List<GestureOutput> = onMoved(listOf(PointerMove(id, x, y)), t)

    /**
     * Fingers moved. All of them are applied before anything is decided, so two fingers that move
     * together are judged by where they are now and not by the in-between state of one having
     * moved and the other not (which would read a sideways two-finger drag as a pinch).
     */
    fun onMoved(moves: List<PointerMove>, t: Long): List<GestureOutput> {
        var finger: Finger? = null
        var dx = 0f
        var dy = 0f
        for (m in moves) {
            val f = fingers[m.id] ?: continue
            dx = m.x - f.x
            dy = m.y - f.y
            f.x = m.x
            f.y = m.y
            finger = f
        }
        val moved = finger ?: return emptyList()
        val out = ArrayList<GestureOutput>(2)
        when (val p = phase) {
            is Phase.Pending -> {
                if (hypot(moved.x - moved.downX, moved.y - moved.downY) > config.touchSlop) beginSingleMove(moved, p.armed, out)
            }

            Phase.Moving, Phase.Dragging -> moveSingle(moved, dx, dy, out)
            is Phase.TwoFingers -> moveTwo(p.state, out)
            is Phase.ThreeFingers -> {
                val (cx, cy) = centroid()
                val panX = cx - p.lastCx
                val panY = cy - p.lastCy
                p.lastCx = cx
                p.lastCy = cy
                if (panX != 0f || panY != 0f) out += GestureOutput.ZoomPan(1f, panX, panY, cx, cy)
            }

            Phase.Idle, Phase.Held, Phase.Done -> Unit
        }
        return out
    }


    fun onUp(id: Long, x: Float, y: Float, t: Long): List<GestureOutput> {
        val finger = fingers[id] ?: return emptyList()
        finger.x = x
        finger.y = y
        val out = ArrayList<GestureOutput>(3)
        when (val p = phase) {
            is Phase.Pending -> {
                val limit = if (mode == ControlMode.DIRECT) config.longPressMs else config.tapMaxMs
                if (t - finger.downAt <= limit) tap(finger, t, out)
            }

            Phase.Dragging -> {
                releaseButton(out)
                lastTap = null
            }

            is Phase.TwoFingers -> {
                val s = p.state
                if (s.tapOk && s.lock == Lock.NONE && t - s.startAt <= config.tapMaxMs && mode == ControlMode.TRACKPAD) {
                    click(MouseButton.RIGHT, pointer, out)
                }
            }

            Phase.Idle, Phase.Moving, Phase.Held, is Phase.ThreeFingers, Phase.Done -> Unit
        }
        fingers.remove(id)
        phase = if (fingers.isEmpty()) Phase.Idle else Phase.Done
        return out
    }

    /** The system took the touch away (a parent intercepted it, the window lost focus). */
    fun onCancel(): List<GestureOutput> {
        val out = ArrayList<GestureOutput>(1)
        cancelIntoDone(out)
        fingers.clear()
        phase = Phase.Idle
        return out
    }

    /** Milliseconds from [now] until [onTick] has something to decide, or null when no timer runs. */
    fun msUntilTick(now: Long): Long? {
        val p = phase as? Phase.Pending ?: return null
        val finger = fingers.values.firstOrNull() ?: return null
        val at = when {
            p.armed -> finger.downAt + config.tapMaxMs
            mode == ControlMode.DIRECT -> finger.downAt + config.longPressMs
            else -> return null
        }
        return (at - now).coerceAtLeast(0L)
    }

    /** Timers: a held Direct touch becomes a right click; a held touch after a tap becomes a drag. */
    fun onTick(now: Long): List<GestureOutput> {
        val p = phase as? Phase.Pending ?: return emptyList()
        val finger = fingers.values.firstOrNull() ?: return emptyList()
        val out = ArrayList<GestureOutput>(3)
        if (p.armed) {
            if (now - finger.downAt >= config.tapMaxMs) beginDrag(finger, out)
        } else if (mode == ControlMode.DIRECT && now - finger.downAt >= config.longPressMs) {
            val at = viewport().toNormalised(finger.downX, finger.downY)
            if (at != null) click(MouseButton.RIGHT, at, out)
            phase = Phase.Held
        }
        return out
    }

    // Single finger ---------------------------------------------------------------------------

    private fun tap(finger: Finger, t: Long, out: MutableList<GestureOutput>) {
        val at = if (mode == ControlMode.DIRECT) viewport().toNormalised(finger.downX, finger.downY) else pointer
        if (at != null) click(MouseButton.LEFT, at, out)
        lastTap = Tap(t, finger.x, finger.y)
    }

    private fun beginSingleMove(finger: Finger, armed: Boolean, out: MutableList<GestureOutput>) {
        if (armed) {
            beginDrag(finger, out)
            return
        }
        phase = Phase.Moving
        // The whole travel since the touch, so the pointer does not lag by the slop.
        moveSingle(finger, finger.x - finger.downX, finger.y - finger.downY, out)
    }

    private fun beginDrag(finger: Finger, out: MutableList<GestureOutput>) {
        phase = Phase.Dragging
        lastTap = null
        if (mode == ControlMode.DIRECT) {
            val start = viewport().toNormalised(finger.downX, finger.downY)
            if (start != null) {
                pointer = start
                out += GestureOutput.Send(InputMessage.Down(MouseButton.LEFT, start.x, start.y))
                out += GestureOutput.Pointer(start)
                buttonDown = true
            }
        } else {
            out += GestureOutput.Send(InputMessage.Down(MouseButton.LEFT, pointer.x, pointer.y))
            buttonDown = true
        }
        // The finger may already have moved: carry the motion on.
        if (finger.x != finger.downX || finger.y != finger.downY) {
            moveSingle(finger, finger.x - finger.downX, finger.y - finger.downY, out)
        }
    }

    private fun moveSingle(finger: Finger, dx: Float, dy: Float, out: MutableList<GestureOutput>) {
        val to = if (mode == ControlMode.DIRECT) {
            viewport().toNormalised(finger.x, finger.y) ?: return
        } else {
            val (nx, ny) = viewport().deltaToNormalised(dx * config.trackpadGain, dy * config.trackpadGain)
            if (nx == 0.0 && ny == 0.0) return
            Norm.of(pointer.x + nx, pointer.y + ny)
        }
        pointer = to
        out += GestureOutput.Send(InputMessage.Move(to.x, to.y))
        out += GestureOutput.Pointer(to)
    }

    // Two and three fingers -------------------------------------------------------------------

    private fun moveTwo(s: Two, out: MutableList<GestureOutput>) {
        val (cx, cy) = centroid()
        val dist = spread()
        if (s.lock == Lock.NONE) {
            val spreadChange = abs(dist - s.startDist)
            val shift = hypot(cx - s.startCx, cy - s.startCy)
            if (maxOf(spreadChange, shift) <= config.touchSlop) return
            s.lock = if (spreadChange > shift) Lock.PINCH else Lock.SCROLL
            // The travel so far counts: it is processed as the first step of the locked gesture.
            s.lastCx = s.startCx
            s.lastCy = s.startCy
            s.lastDist = s.startDist
        }
        val panX = cx - s.lastCx
        val panY = cy - s.lastCy
        when (s.lock) {
            Lock.PINCH -> {
                val zoom = if (s.lastDist > 0f) dist / s.lastDist else 1f
                if (zoom != 1f || panX != 0f || panY != 0f) out += GestureOutput.ZoomPan(zoom, panX, panY, cx, cy)
            }

            Lock.SCROLL -> scroll(s, panX, panY, cx, cy, out)
            Lock.NONE -> Unit
        }
        s.lastCx = cx
        s.lastCy = cy
        s.lastDist = dist
    }

    /**
     * Finger travel as wheel units in the protocol's Windows wheel convention (`InputMessage.Scroll`;
     * the host passes both axes to `SendInput` unchanged, `gander_core::input::mapping::wheel_data`):
     * `dy` positive = the wheel turned away from the user = the view moves up, `dx` positive = a tilt
     * to the right = the view moves right. The direction is "natural": the content follows the
     * fingers, so
     *
     * - fingers moving up (screen y decreasing) scroll the view down: `dy` negative; the finger's
     *   own dy, because screen y grows downward;
     * - fingers moving down scroll the view up: `dy` positive;
     * - fingers moving left scroll the view right: `dx` positive, the finger's dx negated;
     * - fingers moving right scroll the view left: `dx` negative.
     *
     * Pinned by `StreamGesturesTest.scrollSignFollowsTheWindowsWheelConvention` and, for the host's
     * side of the same table, `wheel_data`'s test in the host crate. The sub-unit remainder is
     * carried to the next step.
     */
    private fun scroll(s: Two, panX: Float, panY: Float, cx: Float, cy: Float, out: MutableList<GestureOutput>) {
        val perPx = WHEEL_DELTA / config.scrollPxPerNotch
        s.accX += -panX * perPx
        s.accY += panY * perPx
        val ux = s.accX.roundToInt()
        val uy = s.accY.roundToInt()
        if (ux == 0 && uy == 0) return
        s.accX -= ux
        s.accY -= uy
        val at = if (mode == ControlMode.TRACKPAD) pointer else viewport().toNormalisedClamped(cx, cy)
        out += GestureOutput.Send(InputMessage.Scroll(ux, uy, at.x, at.y))
    }

    // Helpers -----------------------------------------------------------------------------------

    /** Whether the host believes the left button is down (so it must get an `up`). */
    private var buttonDown = false

    private fun releaseButton(out: MutableList<GestureOutput>) {
        if (!buttonDown) return
        buttonDown = false
        out += GestureOutput.Send(InputMessage.Up(MouseButton.LEFT, pointer.x, pointer.y))
    }

    private fun cancelIntoDone(out: MutableList<GestureOutput>) {
        releaseButton(out)
        lastTap = null
        if (fingers.isNotEmpty()) phase = Phase.Done
    }

    private fun click(button: MouseButton, at: Norm, out: MutableList<GestureOutput>) {
        pointer = at
        out += GestureOutput.Send(InputMessage.Down(button, at.x, at.y))
        out += GestureOutput.Send(InputMessage.Up(button, at.x, at.y))
        out += GestureOutput.Pointer(at)
    }

    private fun centroid(): Pair<Float, Float> {
        val all = fingers.values
        return (all.sumOf { it.x.toDouble() } / all.size).toFloat() to (all.sumOf { it.y.toDouble() } / all.size).toFloat()
    }

    /** Distance between the first two fingers. */
    private fun spread(): Float {
        val it = fingers.values.iterator()
        val a = it.next()
        val b = it.next()
        return hypot(a.x - b.x, a.y - b.y)
    }

    private companion object {
        const val WHEEL_DELTA = 120f
    }
}

/**
 * Times [recognizer]'s timers (long press, tap-then-hold) on the clock of the pointer events
 * themselves, never on another clock: "now" is the latest event's time, moved on by how long the
 * caller then waited without an event. Events carry their own time base (`PointerInputChange.uptimeMillis`;
 * Compose's test input starts it at 0), and comparing it with `SystemClock.uptimeMillis()` read a
 * 16 ms tap as held for the device's whole uptime, so it fired the long press (a right click).
 */
class GestureTimer(private val recognizer: StreamGestureRecognizer) {
    private var now: Long? = null

    /** An event at [t] (event clock) is about to be fed to the recogniser. */
    fun onEvent(t: Long) {
        now = now?.let { maxOf(it, t) } ?: t
    }

    /** How long to wait for the next event before calling [onTimeout]; null to wait without limit. */
    fun waitMs(): Long? {
        val at = now ?: return null
        return recognizer.msUntilTick(at)?.coerceAtLeast(1L)
    }

    /** [waited] ms went by with no event (the wait from [waitMs]): runs the timers at the new time. */
    fun onTimeout(waited: Long): List<GestureOutput> {
        val at = (now ?: return emptyList()) + waited
        now = at
        return recognizer.onTick(at)
    }
}
