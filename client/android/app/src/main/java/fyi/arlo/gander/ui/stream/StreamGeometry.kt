package fyi.arlo.gander.ui.stream

import fyi.arlo.gander.ui.preview.ZoomTransform
import fyi.arlo.gander.core.net.protocol.Viewport
import kotlin.math.abs
import kotlin.math.min
import kotlin.math.roundToLong

/** A point on the streamed picture: 0..1 from its top-left corner (protocol: "normalised to the captured window"). */
data class Norm(val x: Double, val y: Double) {
    companion object {
        /** Just under 1 at the 5-decimal precision: the last pixel of the picture is index `size - 1`, never `size`. */
        const val MAX = 0.99999

        /** [x] and [y] clamped to the picture and rounded to 5 decimals (sub-pixel up to 8K, short JSON). */
        fun of(x: Double, y: Double) = Norm(clean(x), clean(y))

        private fun clean(v: Double): Double {
            if (v.isNaN()) return 0.0
            val clamped = v.coerceIn(0.0, MAX)
            return minOf((clamped * SCALE).roundToLong() / SCALE, MAX)
        }

        private const val SCALE = 100_000.0
    }
}

/** The area the picture covers in the viewer, in view pixels, before any zoom. */
data class PictureRect(val left: Float, val top: Float, val width: Float, val height: Float) {
    val right: Float get() = left + width
    val bottom: Float get() = top + height
}

/**
 * Where things are on screen: the view, the streamed picture inside it and the local zoom. All
 * of the stream viewer's coordinate arithmetic lives here and is tested on the JVM.
 *
 * The picture is aspect-fit and centred in the view (letterboxed, "aim at the picture, not the
 * view": a touch on a bar is not a touch on the window). The zoom is the pinch-zoom of the viewer
 * ([ZoomTransform]: the content scaled about the view centre, then moved by the offset), applied
 * to the whole view content, bars included. Touch coordinates arrive in the view's own,
 * un-zoomed space.
 *
 * [frameWidth] and [frameHeight] are the decoded picture's size, 0 while no frame has arrived
 * (the picture then covers the whole view).
 */
data class StreamViewport(
    val viewWidth: Float,
    val viewHeight: Float,
    val frameWidth: Int = 0,
    val frameHeight: Int = 0,
    val zoom: ZoomTransform = ZoomTransform(),
) {
    /** The aspect-fit rectangle of the picture at 1x. */
    fun picture(): PictureRect = fit(viewWidth, viewHeight, frameWidth, frameHeight)

    /** [x], [y] (view space, as touched) as a point of the un-zoomed content. */
    fun contentX(x: Float): Float {
        val cx = viewWidth / 2f
        return cx + (x - cx - zoom.offsetX) / zoom.scale
    }

    fun contentY(y: Float): Float {
        val cy = viewHeight / 2f
        return cy + (y - cy - zoom.offsetY) / zoom.scale
    }

    /** The picture point under the touch at [x], [y], or null when the touch is outside the picture (on a bar). */
    fun toNormalised(x: Float, y: Float): Norm? {
        val p = picture()
        if (p.width <= 0f || p.height <= 0f) return null
        val cx = contentX(x)
        val cy = contentY(y)
        if (cx < p.left || cx > p.right || cy < p.top || cy > p.bottom) return null
        return Norm.of(((cx - p.left) / p.width).toDouble(), ((cy - p.top) / p.height).toDouble())
    }

    /** Like [toNormalised] but a touch outside the picture lands on its nearest edge. */
    fun toNormalisedClamped(x: Float, y: Float): Norm {
        val p = picture()
        if (p.width <= 0f || p.height <= 0f) return Norm(0.5, 0.5)
        return Norm.of(((contentX(x) - p.left) / p.width).toDouble(), ((contentY(y) - p.top) / p.height).toDouble())
    }

    /** A finger travel of ([dx], [dy]) view pixels as a change of the picture point (trackpad pointer motion). */
    fun deltaToNormalised(dx: Float, dy: Float): Pair<Double, Double> {
        val p = picture()
        if (p.width <= 0f || p.height <= 0f) return 0.0 to 0.0
        return (dx / (p.width * zoom.scale)).toDouble() to (dy / (p.height * zoom.scale)).toDouble()
    }

    /** Where [n] is drawn, in view space (the viewer's own pointer marker). */
    fun toView(n: Norm): Pair<Float, Float> {
        val p = picture()
        val contentX = p.left + n.x.toFloat() * p.width
        val contentY = p.top + n.y.toFloat() * p.height
        val cx = viewWidth / 2f
        val cy = viewHeight / 2f
        return (cx + (contentX - cx) * zoom.scale + zoom.offsetX) to (cy + (contentY - cy) * zoom.scale + zoom.offsetY)
    }

    companion object {
        /**
         * Aspect-fit of a [frameWidth] x [frameHeight] picture into a [viewWidth] x [viewHeight]
         * view, centred. An unknown frame (zero) fills the view.
         */
        fun fit(viewWidth: Float, viewHeight: Float, frameWidth: Int, frameHeight: Int): PictureRect {
            if (frameWidth <= 0 || frameHeight <= 0 || viewWidth <= 0f || viewHeight <= 0f) {
                return PictureRect(0f, 0f, viewWidth, viewHeight)
            }
            val scale = min(viewWidth / frameWidth, viewHeight / frameHeight)
            val w = frameWidth * scale
            val h = frameHeight * scale
            return PictureRect((viewWidth - w) / 2f, (viewHeight - h) / 2f, w, h)
        }
    }
}

/**
 * Whether [other] is a different enough viewport that the host should fit the window again: a
 * rotation, not the few pixels that a system bar or the keyboard bar changes.
 */
fun Viewport.differsEnough(other: Viewport, ratio: Double = 0.15): Boolean =
    abs(width - other.width) > other.width * ratio || abs(height - other.height) > other.height * ratio
