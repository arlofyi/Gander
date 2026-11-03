package fyi.arlo.gander.ui.preview

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ZoomMathTest {
    private val w = 1000f
    private val h = 2000f
    private val identity = ZoomTransform()

    private fun gesture(
        from: ZoomTransform,
        zoom: Float,
        panX: Float = 0f,
        panY: Float = 0f,
        cx: Float = w / 2,
        cy: Float = h / 2,
    ) = ZoomMath.applyGesture(from, zoom, panX, panY, cx, cy, w, h)

    @Test
    fun pinchingAboutTheCentreScalesInPlace() {
        val t = gesture(identity, 2f)
        assertEquals(2f, t.scale, 1e-4f)
        assertEquals(0f, t.offsetX, 1e-3f)
        assertEquals(0f, t.offsetY, 1e-3f)
        assertTrue(t.isZoomed)
    }

    @Test
    fun theContentUnderTheFingersStaysUnderThem() {
        // pinch 2x about a point right of centre: that point of the content must not move on screen
        val cx = 800f
        val cy = 1500f
        val t = gesture(identity, 2f, cx = cx, cy = cy)
        // content point under the centroid before: q = c - centre; after: offset + scale * q must equal q (same place)
        val qx = cx - w / 2
        val qy = cy - h / 2
        assertEquals(qx, t.offsetX + t.scale * qx, 1e-3f)
        assertEquals(qy, t.offsetY + t.scale * qy, 1e-3f)
    }

    @Test
    fun scaleIsLimitedToTheRange() {
        assertEquals(ZoomMath.MAX_SCALE, gesture(identity, 100f).scale, 1e-4f)
        assertEquals(ZoomMath.MIN_SCALE, gesture(identity, 0.1f).scale, 1e-4f)
        val twice = gesture(gesture(identity, 3f), 3f)
        assertEquals(ZoomMath.MAX_SCALE, twice.scale, 1e-4f)
    }

    @Test
    fun zoomingBackToOneRecentresThePicture() {
        val zoomed = gesture(identity, 3f, panX = 200f, panY = -300f)
        assertTrue(zoomed.offsetX != 0f)
        val back = gesture(zoomed, 0.01f)
        assertEquals(1f, back.scale, 1e-4f)
        assertEquals(0f, back.offsetX, 0f)
        assertEquals(0f, back.offsetY, 0f)
        assertFalse(back.isZoomed)
    }

    @Test
    fun theZoomedPictureCannotBeDraggedPastItsEdges() {
        val zoomed = gesture(identity, 2f)
        // at 2x the picture is twice the viewport: half a viewport of slack on each side
        val dragged = gesture(zoomed, 1f, panX = 5000f, panY = -9000f)
        assertEquals(w / 2, dragged.offsetX, 1e-3f)
        assertEquals(-h / 2, dragged.offsetY, 1e-3f)
    }

    @Test
    fun atOneTimesThereIsNothingToPan() {
        val t = gesture(identity, 1f, panX = 300f, panY = 300f)
        assertEquals(identity, t)
    }

    @Test
    fun aSmallDriftAboveOneStillCountsAsNotZoomed() {
        assertFalse(ZoomTransform(scale = 1.005f).isZoomed)
        assertTrue(ZoomTransform(scale = 1.02f).isZoomed)
    }

    @Test
    fun doubleTapZoomsInAtThePointThenOutAgain() {
        val tap = ZoomMath.doubleTap(identity, tapX = 900f, tapY = 300f, width = w, height = h)
        assertEquals(ZoomMath.DOUBLE_TAP_SCALE, tap.scale, 1e-4f)
        // the tapped point stays where it was
        val qx = 900f - w / 2
        val qy = 300f - h / 2
        assertEquals(qx, tap.offsetX + tap.scale * qx, 1e-3f)
        assertEquals(qy, tap.offsetY + tap.scale * qy, 1e-3f)

        assertEquals(identity, ZoomMath.doubleTap(tap, 100f, 100f, w, h))
    }

    @Test
    fun clampKeepsAResizedViewportConsistent() {
        // rotation: a 3x picture offset for a tall viewport, now in a wide one
        val offsetForTall = ZoomTransform(3f, 1000f, 2000f)
        val clamped = ZoomMath.clamp(offsetForTall, width = 2000f, height = 1000f)
        assertEquals("still inside the wider bound", 1000f, clamped.offsetX, 1e-3f)
        assertEquals("cut to the shorter bound", 1000f, clamped.offsetY, 1e-3f)
        val smaller = ZoomMath.clamp(offsetForTall, width = 400f, height = 400f)
        assertEquals(400f, smaller.offsetX, 1e-3f)
        assertEquals(400f, smaller.offsetY, 1e-3f)
    }
}
