package fyi.arlo.gander

import android.os.SystemClock
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import fyi.arlo.gander.core.data.actions.InMemoryActionRefusals
import fyi.arlo.gander.core.data.browse.EntryTarget
import fyi.arlo.gander.core.data.browse.PinnedHostConnector
import fyi.arlo.gander.core.data.hosts.InMemoryHostRepository
import fyi.arlo.gander.core.data.hosts.PairedHost
import fyi.arlo.gander.core.data.stream.ControlMode
import fyi.arlo.gander.core.data.stream.GanderStreamEvents
import fyi.arlo.gander.core.data.stream.GanderStreamHostApi
import fyi.arlo.gander.core.data.stream.H264Support
import fyi.arlo.gander.core.data.stream.StreamPhase
import fyi.arlo.gander.core.data.stream.StreamSessionController
import fyi.arlo.gander.core.data.stream.StreamVideoOutput
import fyi.arlo.gander.core.data.stream.WebRtcStreamTransport
import fyi.arlo.gander.core.net.BearerToken
import fyi.arlo.gander.core.net.SpkiPin
import fyi.arlo.gander.core.net.protocol.Viewport
import fyi.arlo.gander.testing.SampleData
import fyi.arlo.gander.ui.stream.GestureConfig
import fyi.arlo.gander.ui.stream.GestureOutput
import fyi.arlo.gander.ui.stream.StreamGestureRecognizer
import fyi.arlo.gander.ui.stream.StreamViewport
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeoutOrNull
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Assume.assumeTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.webrtc.VideoFrame
import org.webrtc.VideoSink
import kotlin.math.abs

/**
 * A real stream from the host's `gander-testwin` window (WP-54 Verify: "stream from the host's
 * testwin renders and a tap changes the box colour"). It needs the stream lifecycle of WP-53
 * (`POST /v1/stream` and the session that follows) and the emulator, so it is written to compile
 * and to skip (assumption) until the integration packages run it: WP-53 and WP-57
 * (`cargo xtask e2e-stream`, `e2e-android`).
 *
 * Instrumentation arguments (never committed, never logged); the first four are the convention of
 * `HostSessionFixtureTest`:
 *
 * - `gander.host.id`, `gander.host.addresses`, `gander.host.port`: where the host is (from the
 *   emulator, `10.0.2.2` reaches a host bound to the development machine's loopback)
 * - `gander.host.token`: bearer token of a paired device whose fixture share allows `stream`
 * - `gander.stream.share`, `gander.stream.path`: the file whose launch opens the test window
 * - `gander.stream.box_x`, `gander.stream.box_y`: where the test window's box is, normalised 0..1 on
 *   the picture. The harness freezes the box first (`gander-testwin` `freeze`), so that it is
 *   where it was said to be; without them only the rendering test runs
 * - `gander.stream.timeout_s` (default 90) and `gander.stream.min_frames` (default 30)
 *
 * The tap goes through the real gesture recogniser (Direct touch, a tap on the box), the real
 * wire encoding and the real `control` data channel, and the check reads the colour from the
 * decoded frames on the phone: no reading of the host's own state is needed.
 *
 * Manual run, no xtask: start the host with the stream lifecycle and a `gander-testwin` fixture,
 * pair a device, install the test APK and `adb shell am instrument -w -e gander.host.id ... -e
 * class fyi.arlo.gander.StreamViewerFixtureTest fyi.arlo.gander.test/androidx.test.runner.AndroidJUnitRunner`.
 * (The WP-44 harness `spike-input serve` speaks the spike's own signalling, not `/v1/events`, so
 * it cannot be used with this viewer.)
 */
@RunWith(AndroidJUnit4::class)
class StreamViewerFixtureTest {
    private val args = InstrumentationRegistry.getArguments()

    private fun arg(name: String): String? = args.getString(name)?.takeIf { it.isNotBlank() }

    private val timeoutMs = (arg("gander.stream.timeout_s")?.toLongOrNull() ?: 90L) * 1000L
    private val minFrames = arg("gander.stream.min_frames")?.toLongOrNull() ?: 30L

    private class Stream(
        val controller: StreamSessionController,
        val output: StreamVideoOutput,
        val probe: ColourProbe,
        val scope: CoroutineScope,
    )

    /** Connects to the fixture host and runs [block] with a started stream; always ends it and frees everything. */
    private fun withStream(block: suspend (Stream) -> Unit) {
        val hostId = arg("gander.host.id")
        val addresses = arg("gander.host.addresses")?.split(',')?.map { it.trim() }
        val token = arg("gander.host.token")
        val share = arg("gander.stream.share")
        val path = arg("gander.stream.path")
        assumeTrue(
            "needs a host with the stream lifecycle (WP-53), a stream share and file and a token (cargo xtask e2e-stream)",
            hostId != null && addresses != null && token != null && share != null && path != null,
        )
        val pin = SpkiPin.fromHostId(hostId!!)
        val host = PairedHost(
            id = pin.hostId,
            name = "fixture",
            pin = pin,
            addresses = addresses!!,
            port = arg("gander.host.port")?.toInt() ?: 42637,
        )
        val hosts = InMemoryHostRepository(listOf(host))
        runBlocking { hosts.seedToken(host.id, BearerToken(token!!)) }
        val clients = PinnedHostConnector(hosts, "androidTest").clients(host)

        val context = InstrumentationRegistry.getInstrumentation().targetContext
        // CPU buffers, so the probe can read pixels from the decoded frames.
        val output = StreamVideoOutput(textureDecode = false)
        val probe = ColourProbe().also { output.addSink(it) }
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        val controller = StreamSessionController(
            target = EntryTarget(host.id, share!!, path!!, SampleData.file(path.substringAfterLast('/'))),
            api = GanderStreamHostApi(clients),
            eventsSource = GanderStreamEvents(clients),
            transportFactory = { WebRtcStreamTransport(context, output) },
            decoderAvailable = H264Support::decoderAvailable,
            refusals = InMemoryActionRefusals(),
            scope = scope,
            clock = SystemClock::elapsedRealtime,
        )
        try {
            runBlocking {
                controller.start(Viewport(1080, 2280, 2.625))
                val reached = withTimeoutOrNull(timeoutMs) {
                    controller.state.first { it.phase == StreamPhase.Streaming || it.phase.isOver() }
                }
                assertEquals("the stream reached Streaming: ${reached?.phase}", StreamPhase.Streaming, reached?.phase)
                block(Stream(controller, output, probe, scope))
            }
        } finally {
            runBlocking { controller.end(closeApp = false) }
            output.removeSink(probe)
            output.release()
            scope.cancel()
        }
    }

    private fun StreamPhase.isOver() =
        this is StreamPhase.Ended || this is StreamPhase.Failed || this is StreamPhase.Unsupported

    @Test
    fun theHostsTestWindowRendersOnThePhone() {
        withStream { stream ->
            val frames = withTimeoutOrNull(timeoutMs) {
                while (stream.output.framesReceived < minFrames) delay(100)
                stream.output.framesReceived
            }
            assertNotNull("at least $minFrames frames arrived (got ${stream.output.framesReceived})", frames)
            val size = stream.output.frameSize.value
            assertTrue("the picture has a size: $size", size.known)
            stream.probe.watch(0.5, 0.5)
            delay(500)
            val centre = stream.probe.latest
            assertNotNull("the probe saw a decoded frame", centre)
            // A test window is never black: black means nothing was decoded or the capture was empty.
            assertTrue("the picture is not black: $centre", centre!!.luma() > 8)
        }
    }

    @Test
    fun aTapOnTheBoxChangesItsColour() {
        val boxX = arg("gander.stream.box_x")?.toDoubleOrNull()
        val boxY = arg("gander.stream.box_y")?.toDoubleOrNull()
        assumeTrue("needs gander.stream.box_x and gander.stream.box_y (the frozen box)", boxX != null && boxY != null)
        withStream { stream ->
            withTimeoutOrNull(timeoutMs) {
                while (stream.output.framesReceived < minFrames) delay(100)
            }
            val size = stream.output.frameSize.value
            assertTrue("the picture has a size: $size", size.known)
            stream.probe.watch(boxX!!, boxY!!)
            delay(700)
            val before = stream.probe.latest
            assertNotNull("the probe saw the box", before)

            // The touch as the viewer sees it: a view the size of the picture, a Direct tap on the box.
            val viewport = StreamViewport(size.width.toFloat(), size.height.toFloat(), size.width, size.height)
            val recogniser = StreamGestureRecognizer(GestureConfig(touchSlop = 8f), { viewport }, ControlMode.DIRECT)
            val x = (boxX * size.width).toFloat()
            val y = (boxY * size.height).toFloat()
            val t = SystemClock.uptimeMillis()
            fun forward(outputs: List<GestureOutput>) {
                outputs.filterIsInstance<GestureOutput.Send>().forEach { stream.controller.send(it.message) }
            }
            forward(recogniser.onDown(1, x, y, t))
            forward(recogniser.onUp(1, x, y, t + 60))

            // The test window repaints at once; the new colour has to come back through encode, network and decode.
            val changed = withTimeoutOrNull(10_000L) {
                while (before!!.distanceTo(stream.probe.latest ?: before) < COLOUR_CHANGE) delay(50)
                true
            }
            assertTrue("the box changed colour within 10 s of the tap (was $before, is ${stream.probe.latest})", changed == true)
        }
    }

    private data class Rgb(val r: Int, val g: Int, val b: Int) {
        fun luma(): Int = (299 * r + 587 * g + 114 * b) / 1000

        fun distanceTo(other: Rgb): Int = abs(r - other.r) + abs(g - other.g) + abs(b - other.b)
    }

    /** Reads the colour at one point of the picture from every decoded frame (I420, BT.601 limited range). */
    private class ColourProbe : VideoSink {
        @Volatile
        private var point: Pair<Double, Double>? = null

        @Volatile
        var latest: Rgb? = null
            private set

        fun watch(x: Double, y: Double) {
            latest = null
            point = x to y
        }

        override fun onFrame(frame: VideoFrame) {
            val (nx, ny) = point ?: return
            val i420 = frame.buffer.toI420() ?: return
            try {
                val cx = (nx * i420.width).toInt().coerceIn(0, i420.width - 1)
                val cy = (ny * i420.height).toInt().coerceIn(0, i420.height - 1)
                var r = 0
                var g = 0
                var b = 0
                var n = 0
                for (dy in -RADIUS..RADIUS) {
                    for (dx in -RADIUS..RADIUS) {
                        val px = (cx + dx).coerceIn(0, i420.width - 1)
                        val py = (cy + dy).coerceIn(0, i420.height - 1)
                        val yy = i420.dataY.get(py * i420.strideY + px).toInt() and 0xFF
                        val uu = i420.dataU.get((py / 2) * i420.strideU + px / 2).toInt() and 0xFF
                        val vv = i420.dataV.get((py / 2) * i420.strideV + px / 2).toInt() and 0xFF
                        val c = yy - 16
                        val d = uu - 128
                        val e = vv - 128
                        r += ((298 * c + 409 * e + 128) shr 8).coerceIn(0, 255)
                        g += ((298 * c - 100 * d - 208 * e + 128) shr 8).coerceIn(0, 255)
                        b += ((298 * c + 516 * d + 128) shr 8).coerceIn(0, 255)
                        n++
                    }
                }
                latest = Rgb(r / n, g / n, b / n)
            } finally {
                i420.release()
            }
        }

        private companion object {
            const val RADIUS = 3
        }
    }

    private companion object {
        /** Sum of the channel differences that counts as "a different colour" (the box cycles through saturated colours). */
        const val COLOUR_CHANGE = 120
    }
}
