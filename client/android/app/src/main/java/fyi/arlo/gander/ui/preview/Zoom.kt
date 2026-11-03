package fyi.arlo.gander.ui.preview

import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.calculateCentroid
import androidx.compose.foundation.gestures.calculatePan
import androidx.compose.foundation.gestures.calculateZoom
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.input.pointer.changedToUp
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.input.pointer.positionChanged
import androidx.compose.ui.layout.onSizeChanged
import kotlin.math.abs
import kotlin.math.max

/** Scale and translation of zoomed content: scaled about the viewport centre, then moved by the offset. */
data class ZoomTransform(val scale: Float = 1f, val offsetX: Float = 0f, val offsetY: Float = 0f) {
    val isZoomed: Boolean get() = scale > ZoomMath.ZOOMED_ABOVE
}

/**
 * The arithmetic of pinch zoom, apart from Compose so it is tested on the JVM. A point of the
 * content stays under the fingers while they pinch ([applyGesture]); the picture can never be
 * dragged past the viewport's edges ([clamp]); at scale 1 it sits centred.
 */
object ZoomMath {
    const val MIN_SCALE = 1f
    const val MAX_SCALE = 5f

    /** Scale reached by a double tap. */
    const val DOUBLE_TAP_SCALE = 2.5f

    /** Anything above this counts as zoomed (floating point drift after zooming back out). */
    const val ZOOMED_ABOVE = 1.01f

    /**
     * Applies one gesture step: scale by [zoomChange] and pan by ([panX], [panY]) about the
     * centroid ([centroidX], [centroidY]), both in viewport coordinates with the origin at the top
     * left of a [width] x [height] viewport.
     */
    fun applyGesture(
        current: ZoomTransform,
        zoomChange: Float,
        panX: Float,
        panY: Float,
        centroidX: Float,
        centroidY: Float,
        width: Float,
        height: Float,
        maxScale: Float = MAX_SCALE,
    ): ZoomTransform {
        val scale = (current.scale * zoomChange).coerceIn(MIN_SCALE, maxScale)
        val ratio = scale / current.scale
        // Centroid relative to the viewport centre: the content is scaled about the centre.
        val cx = centroidX - width / 2f
        val cy = centroidY - height / 2f
        val x = (current.offsetX - cx) * ratio + cx + panX
        val y = (current.offsetY - cy) * ratio + cy + panY
        return clamp(ZoomTransform(scale, x, y), width, height)
    }

    /** Keeps the zoomed content covering the viewport: at most half of the extra size either way. */
    fun clamp(transform: ZoomTransform, width: Float, height: Float): ZoomTransform {
        if (!transform.isZoomed) return ZoomTransform()
        val maxX = max(0f, width * (transform.scale - 1f) / 2f)
        val maxY = max(0f, height * (transform.scale - 1f) / 2f)
        return transform.copy(
            offsetX = transform.offsetX.coerceIn(-maxX, maxX),
            offsetY = transform.offsetY.coerceIn(-maxY, maxY),
        )
    }

    /** Double tap: back to 1x when zoomed, else [DOUBLE_TAP_SCALE] with the tapped point staying put. */
    fun doubleTap(
        current: ZoomTransform,
        tapX: Float,
        tapY: Float,
        width: Float,
        height: Float,
    ): ZoomTransform =
        if (current.isZoomed) {
            ZoomTransform()
        } else {
            applyGesture(current, DOUBLE_TAP_SCALE, 0f, 0f, tapX, tapY, width, height)
        }
}

/** The zoom of one picture on screen. Create one per picture with `remember`. */
@Stable
class ZoomState {
    var scale by mutableFloatStateOf(1f)
        private set
    var offsetX by mutableFloatStateOf(0f)
        private set
    var offsetY by mutableFloatStateOf(0f)
        private set

    /** How far a pinch may zoom; [TileOverlay] raises it when deep-zoom tiles can show more (WP-67). */
    var maxScale: Float = ZoomMath.MAX_SCALE

    private var width = 0f
    private var height = 0f

    val isZoomed: Boolean get() = scale > ZoomMath.ZOOMED_ABOVE

    internal fun setViewport(w: Float, h: Float) {
        if (w == width && h == height) return
        width = w
        height = h
        set(ZoomMath.clamp(current(), w, h))
    }

    internal fun gesture(zoomChange: Float, pan: Offset, centroid: Offset) {
        if (width <= 0f || height <= 0f) return
        set(
            ZoomMath.applyGesture(current(), zoomChange, pan.x, pan.y, centroid.x, centroid.y, width, height, maxScale),
        )
    }

    internal fun doubleTap(at: Offset) {
        if (width <= 0f || height <= 0f) return
        set(ZoomMath.doubleTap(current(), at.x, at.y, width, height))
    }

    fun reset() = set(ZoomTransform())

    private fun current() = ZoomTransform(scale, offsetX, offsetY)

    private fun set(t: ZoomTransform) {
        scale = t.scale
        offsetX = t.offsetX
        offsetY = t.offsetY
    }
}

/**
 * Pinch to zoom, drag to pan while zoomed, double tap to toggle. A one-finger drag at 1x is left
 * alone (not consumed), so a pager around the picture still swipes; while zoomed (or pinching) the
 * gesture is consumed and the pager stays put. The picture is drawn through a `graphicsLayer`
 * placed after the pointer input, so touch coordinates stay in the viewport's own space.
 */
fun Modifier.zoomable(state: ZoomState): Modifier = this
    .onSizeChanged { state.setViewport(it.width.toFloat(), it.height.toFloat()) }
    .pointerInput(state) {
        detectTapGestures(onDoubleTap = { state.doubleTap(it) })
    }
    .pointerInput(state) {
        awaitEachGesture {
            awaitFirstDown(requireUnconsumed = false)
            do {
                val event = awaitPointerEvent()
                val pressed = event.changes.count { it.pressed }
                if (pressed > 1 || state.isZoomed) {
                    val zoom = event.calculateZoom()
                    val pan = event.calculatePan()
                    if (abs(zoom - 1f) > EPSILON || pan != Offset.Zero) {
                        state.gesture(zoom, pan, event.calculateCentroid(useCurrent = false))
                    }
                    event.changes.forEach { if (it.positionChanged()) it.consume() }
                }
            } while (event.changes.any { it.pressed && !it.changedToUp() })
        }
    }
    .graphicsLayer {
        scaleX = state.scale
        scaleY = state.scale
        translationX = state.offsetX
        translationY = state.offsetY
    }

private const val EPSILON = 0.0005f
