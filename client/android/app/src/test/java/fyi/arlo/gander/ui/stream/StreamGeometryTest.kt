package fyi.arlo.gander.ui.stream

import fyi.arlo.gander.core.net.protocol.Viewport
import fyi.arlo.gander.ui.preview.ZoomMath
import fyi.arlo.gander.ui.preview.ZoomTransform
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** The coordinate arithmetic of the stream viewer: letterboxing, the local zoom and normalised points. */
class StreamGeometryTest {
    private val eps = 1e-5

    /** The WP-44 phone: a 1080 x 2400 view showing a 1080 x 2280 picture, 60 px bars above and below. */
    private val letterboxed = StreamViewport(1080f, 2400f, frameWidth = 1080, frameHeight = 2280)

    private fun assertNorm(x: Double, y: Double, actual: Norm?) {
        assertNotNull("a point on the picture", actual)
        assertEquals("x", x, actual!!.x, eps)
        assertEquals("y", y, actual.y, eps)
    }

    @Test
    fun theLetterboxBarsAreNotPartOfThePicture() {
        val p = letterboxed.picture()
        assertEquals(0f, p.left, 0.001f)
        assertEquals(60f, p.top, 0.001f)
        assertEquals(1080f, p.width, 0.001f)
        assertEquals(2280f, p.height, 0.001f)
        assertEquals(2340f, p.bottom, 0.001f)
    }

    @Test
    fun aWidePictureInATallViewSitsInTheMiddle() {
        val p = StreamViewport.fit(1000f, 2000f, frameWidth = 1000, frameHeight = 500)
        assertEquals(0f, p.left, 0.001f)
        assertEquals(750f, p.top, 0.001f)
        assertEquals(1000f, p.width, 0.001f)
        assertEquals(500f, p.height, 0.001f)
    }

    @Test
    fun aTallPictureInAWideViewIsPillarboxed() {
        val p = StreamViewport.fit(2000f, 1000f, frameWidth = 1000, frameHeight = 1000)
        assertEquals(500f, p.left, 0.001f)
        assertEquals(0f, p.top, 0.001f)
        assertEquals(1000f, p.width, 0.001f)
    }

    @Test
    fun beforeTheFirstFrameThePictureFillsTheView() {
        val p = StreamViewport(1080f, 2400f).picture()
        assertEquals(1080f, p.width, 0.001f)
        assertEquals(2400f, p.height, 0.001f)
    }

    @Test
    fun aTouchOnThePictureIsNormalisedToIt() {
        assertNorm(0.5, 0.5, letterboxed.toNormalised(540f, 1200f))
        assertNorm(0.0, 0.0, letterboxed.toNormalised(0f, 60f))
        assertNorm(0.25, 0.25, letterboxed.toNormalised(270f, 60f + 570f))
    }

    @Test
    fun theFirstAndLastPixelAreInsideThePicture() {
        assertNorm(0.0, 0.0, letterboxed.toNormalised(0f, 60f))
        val last = letterboxed.toNormalised(1080f, 2340f)
        assertNotNull("the far corner is on the picture", last)
        assertTrue("never 1.0: the last pixel is size - 1", last!!.x < 1.0 && last.y < 1.0)
        assertEquals(Norm.MAX, last.x, eps)
    }

    @Test
    fun aTouchOnABarIsNotATouchOnTheWindow() {
        assertNull(letterboxed.toNormalised(540f, 30f))
        assertNull(letterboxed.toNormalised(540f, 2380f))
        // For the trackpad pointer a point on a bar lands on the nearest edge instead.
        val clamped = letterboxed.toNormalisedClamped(540f, 30f)
        assertEquals(0.0, clamped.y, eps)
        assertEquals(0.5, clamped.x, eps)
    }

    @Test
    fun zoomingKeepsThePointUnderTheViewCentreFixed() {
        val zoomed = letterboxed.copy(zoom = ZoomTransform(scale = 2f))
        assertNorm(0.5, 0.5, zoomed.toNormalised(540f, 1200f))
    }

    @Test
    fun aZoomedTouchHitsTheContentThatIsUnderIt() {
        val zoomed = letterboxed.copy(zoom = ZoomTransform(scale = 2f))
        // 270 px right of the centre at 2x is 135 px right of the centre of the content.
        assertNorm((540.0 + 135.0) / 1080.0, 0.5, zoomed.toNormalised(540f + 270f, 1200f))
    }

    @Test
    fun aPannedZoomMovesWhatIsUnderTheFinger() {
        // Zoomed 2x and dragged 200 px left: the content right of the centre moved under the centre.
        val panned = letterboxed.copy(zoom = ZoomTransform(scale = 2f, offsetX = -200f, offsetY = 0f))
        assertNorm((540.0 + 100.0) / 1080.0, 0.5, panned.toNormalised(540f, 1200f))
    }

    @Test
    fun aZoomedTouchOnAZoomedBarIsStillOutside() {
        // Zoomed 2x and panned as far down as the zoom allows: the top bar (content y 0..60) is at the
        // very top of the view, and the picture starts a little below it.
        val zoomed = letterboxed.copy(zoom = ZoomTransform(scale = 2f, offsetX = 0f, offsetY = 1200f))
        assertNull(zoomed.toNormalised(540f, 100f))
        assertNotNull(zoomed.toNormalised(540f, 200f))
    }

    @Test
    fun normalisedPointsRoundTripThroughTheView() {
        val zoomed = letterboxed.copy(zoom = ZoomTransform(scale = 3f, offsetX = -120f, offsetY = 80f))
        for ((x, y) in listOf(300f to 900f, 700f to 1500f, 540f to 1200f)) {
            val n = zoomed.toNormalised(x, y)
            assertNotNull(n)
            val (vx, vy) = zoomed.toView(n!!)
            assertEquals(x.toDouble(), vx.toDouble(), 0.1)
            assertEquals(y.toDouble(), vy.toDouble(), 0.1)
        }
    }

    @Test
    fun theViewersPointerIsDrawnWhereItClicks() {
        val (x, y) = letterboxed.toView(Norm(0.5, 0.5))
        assertEquals(540f, x, 0.01f)
        assertEquals(1200f, y, 0.01f)
    }

    @Test
    fun aFingerTravelMovesThePointerByThePicturesOwnFraction() {
        val (dx, dy) = letterboxed.deltaToNormalised(108f, 228f)
        assertEquals(0.1, dx, eps)
        assertEquals(0.1, dy, eps)
        // Zoomed in, the same finger travel covers less of the picture.
        val (zx, zy) = letterboxed.copy(zoom = ZoomTransform(scale = 2f)).deltaToNormalised(108f, 228f)
        assertEquals(0.05, zx, eps)
        assertEquals(0.05, zy, eps)
    }

    @Test
    fun normalisedPointsAreClampedAndShort() {
        val n = Norm.of(1.4, -0.2)
        assertEquals(Norm.MAX, n.x, eps)
        assertEquals(0.0, n.y, eps)
        assertEquals(0.12346, Norm.of(0.123456789, 0.0).x, eps)
        assertEquals("a NaN becomes the origin", 0.0, Norm.of(Double.NaN, 0.0).x, eps)
        assertTrue(Norm.of(1.0, 1.0).x < 1.0)
    }

    @Test
    fun theViewersZoomStaysInsideWhatTheMathAllows() {
        // The ViewModel applies pinch steps with ZoomMath and its own ceiling.
        var t = ZoomTransform()
        repeat(20) { t = ZoomMath.applyGesture(t, 1.5f, 0f, 0f, 540f, 1200f, 1080f, 2400f, 6f) }
        assertEquals(6f, t.scale, 0.001f)
        val back = ZoomMath.applyGesture(t, 0.01f, 0f, 0f, 540f, 1200f, 1080f, 2400f, 6f)
        assertFalse("zooming all the way out resets", back.isZoomed)
    }

    @Test
    fun aRotationNeedsTheHostToFitAgainButASystemBarDoesNot() {
        val portrait = Viewport(1080, 2400, 2.6)
        assertTrue(Viewport(2400, 1080, 2.6).differsEnough(portrait))
        assertFalse("a keyboard bar or a status bar", Viewport(1080, 2300, 2.6).differsEnough(portrait))
        assertTrue(Viewport(1080, 1800, 2.6).differsEnough(portrait))
    }
}
