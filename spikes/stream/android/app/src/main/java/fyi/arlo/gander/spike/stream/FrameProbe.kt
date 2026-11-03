package fyi.arlo.gander.spike.stream

import org.webrtc.VideoFrame
import org.webrtc.VideoSink
import java.util.concurrent.atomic.AtomicLong

/**
 * Counts the frames that reach the renderer (the overlay's frame counter and fps) and, when the
 * decoder delivers CPU I420 buffers, classifies the colour of the testwin's moving box so the
 * phone can time "tap -> changed pixels in the decoded stream" on its own clock.
 *
 * The box palette (host/gander-testwin) is orange, blue, yellow, red, advancing on every click on
 * the box; the arena is dark grey, the patches magenta and green, the counter white. A sample is
 * classified by hue, so the exact decoded values (colour matrix, coding noise) do not matter.
 */
class FrameProbe(private val onColour: (name: String, firstFrameNs: Long) -> Unit) : VideoSink {
    val frames = AtomicLong()

    @Volatile var width = 0
    @Volatile var height = 0
    @Volatile var lastFrameNs = 0L
    @Volatile var colour = "?"
    @Volatile var colourReadable = false

    private val window = ArrayDeque<Long>()
    private var candidate = "?"
    private var candidateSince = 0L
    private var candidateCount = 0

    /** Frames per second over the last second (renderer input). */
    @Synchronized
    fun fps(): Double {
        val now = System.nanoTime()
        while (window.isNotEmpty() && now - window.first() > 1_000_000_000L) window.removeFirst()
        return window.size.toDouble()
    }

    override fun onFrame(frame: VideoFrame) {
        val now = System.nanoTime()
        frames.incrementAndGet()
        width = frame.rotatedWidth
        height = frame.rotatedHeight
        lastFrameNs = now
        synchronized(this) { window.addLast(now) }
        val buf = frame.buffer
        if (buf is VideoFrame.I420Buffer) {
            colourReadable = true
            val name = classify(buf)
            if (name != null) track(name, now)
        }
    }

    private fun track(name: String, now: Long) {
        if (name == colour) {
            candidate = name
            candidateCount = 0
            return
        }
        if (name != candidate) {
            candidate = name
            candidateSince = now
            candidateCount = 1
        } else {
            candidateCount++
        }
        // Two consecutive frames with the new colour: a change (one frame of noise is ignored).
        if (candidateCount >= 2) {
            val previous = colour
            colour = name
            if (previous != "?") onColour(name, candidateSince)
            candidateCount = 0
        }
    }

    /** The dominant palette colour among a coarse grid of samples, or null when none is present. */
    private fun classify(b: VideoFrame.I420Buffer): String? {
        val w = b.width
        val h = b.height
        val y = b.dataY
        val u = b.dataU
        val v = b.dataV
        val sy = b.strideY
        val su = b.strideU
        val sv = b.strideV
        val step = maxOf(4, w / 160)
        val counts = IntArray(4)
        var py = 0
        while (py < h) {
            var px = 0
            while (px < w) {
                val yy = (y.get(py * sy + px).toInt() and 0xFF) - 16
                val uu = (u.get((py shr 1) * su + (px shr 1)).toInt() and 0xFF) - 128
                val vv = (v.get((py shr 1) * sv + (px shr 1)).toInt() and 0xFF) - 128
                // BT.709 limited range (what the host's video processor writes).
                val r = 1.164f * yy + 1.793f * vv
                val g = 1.164f * yy - 0.213f * uu - 0.533f * vv
                val bl = 1.164f * yy + 2.112f * uu
                val idx = paletteIndex(r, g, bl)
                if (idx >= 0) counts[idx]++
                px += step
            }
            py += step
        }
        var best = 0
        for (i in 1..3) if (counts[i] > counts[best]) best = i
        // A 64 px logical box is at least ~40 grid samples at the grid step above.
        return if (counts[best] >= 25) NAMES[best] else null
    }

    private fun paletteIndex(r: Float, g: Float, b: Float): Int {
        val max = maxOf(r, g, b)
        val min = minOf(r, g, b)
        if (max < 128f || max - min < 0.5f * max) return -1 // dark or washed out
        val d = max - min
        var hue = when (max) {
            r -> 60f * (((g - b) / d) % 6f)
            g -> 60f * ((b - r) / d + 2f)
            else -> 60f * ((r - g) / d + 4f)
        }
        if (hue < 0f) hue += 360f
        return when {
            hue < 12f || hue >= 348f -> 3 // red
            hue < 45f -> 0 // orange
            hue < 75f -> 2 // yellow
            hue in 190f..250f -> 1 // blue
            else -> -1 // green, magenta, ...
        }
    }

    companion object {
        val NAMES = arrayOf("orange", "blue", "yellow", "red")
    }
}
