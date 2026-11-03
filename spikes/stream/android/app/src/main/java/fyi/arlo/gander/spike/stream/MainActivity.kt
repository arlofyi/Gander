package fyi.arlo.gander.spike.stream

import android.app.Activity
import android.graphics.Color
import android.graphics.Typeface
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.view.MotionEvent
import android.view.WindowManager
import android.widget.FrameLayout
import android.widget.TextView
import org.webrtc.EglBase
import org.webrtc.RendererCommon
import org.webrtc.SurfaceViewRenderer
import kotlin.math.abs
import kotlin.math.min

/**
 * Full-screen viewer of the host's window stream, with a frame counter overlay.
 *
 * Launch (all extras optional):
 * `am start -n fyi.arlo.gander.spike.stream/.MainActivity --es signal ws://10.0.2.2:47440/
 *  --ez tex false --es client emulator`
 *
 * A tap anywhere on the picture is sent to the host as normalised coordinates (0..1 over the
 * picture, origin top-left; the letterbox bars are not part of the picture). The overlay shows the
 * connection state, the frames that reached the renderer, decode and jitter-buffer cost, the data
 * channel echo round trip and the last tap -> changed-pixels time.
 */
class MainActivity : Activity() {
    private lateinit var renderer: SurfaceViewRenderer
    private lateinit var overlay: TextView
    private lateinit var eglBase: EglBase
    private var session: StreamSession? = null
    private val handler = Handler(Looper.getMainLooper())

    @Volatile private var state = "starting"

    @Volatile private var videoW = 0

    @Volatile private var videoH = 0

    @Volatile private var lastRtt = Double.NaN

    @Volatile private var lastTapNs = 0L

    @Volatile private var lastTapSeq = 0

    @Volatile private var lastEffectMs = Double.NaN

    private var taps = 0
    private var downX = 0f
    private var downY = 0f
    private var downNs = 0L

    private val probe = FrameProbe { name, firstFrameNs ->
        val tap = lastTapNs
        val ms = (firstFrameNs - tap) / 1e6
        if (tap != 0L && ms in 0.0..3000.0) {
            lastEffectMs = ms
            Evt.log("tap_effect", "seq" to lastTapSeq, "ms" to ms, "colour" to name)
            lastTapNs = 0L // one effect per tap
        } else {
            Evt.log("colour_change", "colour" to name)
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        eglBase = EglBase.create()

        val root = FrameLayout(this)
        root.setBackgroundColor(Color.BLACK)
        renderer = SurfaceViewRenderer(this)
        renderer.init(
            eglBase.eglBaseContext,
            object : RendererCommon.RendererEvents {
                override fun onFirstFrameRendered() {
                    Evt.log("first_frame_rendered")
                }

                override fun onFrameResolutionChanged(w: Int, h: Int, rotation: Int) {
                    videoW = w
                    videoH = h
                    logLayout()
                }
            },
        )
        renderer.setScalingType(RendererCommon.ScalingType.SCALE_ASPECT_FIT)
        renderer.setEnableHardwareScaler(false)
        renderer.setOnTouchListener { _, e -> onTouch(e) }
        root.addView(renderer, FrameLayout.LayoutParams(-1, -1))

        overlay = TextView(this).apply {
            typeface = Typeface.MONOSPACE
            textSize = 11f
            setTextColor(Color.WHITE)
            setBackgroundColor(0xAA000000.toInt())
            setPadding(8, 4, 8, 4)
            // The overlay must not eat touches: taps on it still count as taps on the picture.
            isClickable = false
            isFocusable = false
        }
        root.addView(overlay, FrameLayout.LayoutParams(-2, -2))
        setContentView(root)
        root.addOnLayoutChangeListener { _, _, _, _, _, _, _, _, _ -> logLayout() }

        val signal = intent.getStringExtra("signal") ?: "ws://10.0.2.2:47440/"
        val client = intent.getStringExtra("client") ?: "spike-android"
        val tex = intent.getBooleanExtra("tex", false)
        Evt.log("start", "signal" to signal, "client" to client, "texture_decode" to tex, "sdk" to android.os.Build.VERSION.SDK_INT)
        session = StreamSession(
            applicationContext,
            signal,
            client,
            eglBase,
            listOf(renderer, probe),
            tex,
            { s ->
                state = s
                Evt.log("state", "state" to s)
            },
            { rtt -> lastRtt = rtt },
        )
        session?.start()
        handler.post(tick)
    }

    private val tick = object : Runnable {
        override fun run() {
            val s = session
            val text = buildString {
                append("GANDER SPIKE  ").append(state).append('\n')
                append("frames ").append(probe.frames.get())
                append("  fps ").append("%.1f".format(probe.fps()))
                append("  ").append(probe.width).append('x').append(probe.height).append('\n')
                if (s != null) {
                    append("decoded ").append(s.framesDecoded).append(" drop ").append(s.framesDropped)
                    append(" lost ").append(s.packetsLost).append('\n')
                    append("dec %.1f ms  jitbuf %.0f ms\n".format(s.decodeMsPerFrame, s.jitterBufferMs))
                    append(s.decoderImpl).append('\n')
                    append("ice ").append(s.iceState).append("  dc ").append(if (s.dcOpen) "open" else "closed").append('\n')
                }
                append("echo rtt ").append(if (lastRtt.isNaN()) "-" else "%.1f ms".format(lastRtt))
                append("  taps ").append(taps).append('\n')
                append("tap->pixels ").append(if (lastEffectMs.isNaN()) "-" else "%.0f ms".format(lastEffectMs))
                append("  box ").append(if (probe.colourReadable) probe.colour else "n/a")
            }
            overlay.text = text
            handler.postDelayed(this, 250)
        }
    }

    /** Rectangle of the picture inside the view (aspect fit, centred). */
    private fun pictureRect(): FloatArray? {
        val vw = renderer.width
        val vh = renderer.height
        if (videoW <= 0 || videoH <= 0 || vw <= 0 || vh <= 0) return null
        val scale = min(vw.toFloat() / videoW, vh.toFloat() / videoH)
        val pw = videoW * scale
        val ph = videoH * scale
        return floatArrayOf((vw - pw) / 2f, (vh - ph) / 2f, pw, ph)
    }

    private var lastLayout = ""

    private fun logLayout() {
        val loc = IntArray(2)
        renderer.getLocationOnScreen(loc)
        val r = pictureRect()
        val key = "${renderer.width}x${renderer.height}@${loc[0]},${loc[1]} ${videoW}x$videoH ${r?.joinToString()}"
        if (key == lastLayout) return // the overlay text relayouts the root several times a second
        lastLayout = key
        Evt.log(
            "layout",
            "view_w" to renderer.width,
            "view_h" to renderer.height,
            "view_x" to loc[0],
            "view_y" to loc[1],
            "video_w" to videoW,
            "video_h" to videoH,
            "pic_l" to r?.get(0)?.toDouble(),
            "pic_t" to r?.get(1)?.toDouble(),
            "pic_w" to r?.get(2)?.toDouble(),
            "pic_h" to r?.get(3)?.toDouble(),
        )
    }

    /** A tap is a down and an up close together in space and time; it fires on the up. */
    private fun onTouch(e: MotionEvent): Boolean {
        when (e.actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                downX = e.x
                downY = e.y
                downNs = System.nanoTime()
            }
            MotionEvent.ACTION_UP -> {
                val slop = 24f * resources.displayMetrics.density
                val isTap = abs(e.x - downX) < slop && abs(e.y - downY) < slop
                val r = pictureRect()
                if (isTap && r != null) {
                    val nx = ((e.x - r[0]) / r[2]).toDouble()
                    val ny = ((e.y - r[1]) / r[3]).toDouble()
                    if (nx in 0.0..1.0 && ny in 0.0..1.0) {
                        taps++
                        val now = System.nanoTime()
                        val seq = session?.sendTap(nx, ny) ?: 0
                        lastTapSeq = seq
                        lastTapNs = now
                        Evt.log("touch", "seq" to seq, "view_x" to e.x.toDouble(), "view_y" to e.y.toDouble(), "press_ms" to (now - downNs) / 1e6)
                    } else {
                        Evt.log("tap_outside_picture", "view_x" to e.x.toDouble(), "view_y" to e.y.toDouble())
                    }
                }
            }
        }
        return true
    }

    override fun onPause() {
        Evt.log("lifecycle", "state" to "onPause")
        super.onPause()
    }

    override fun onStop() {
        Evt.log("lifecycle", "state" to "onStop")
        super.onStop()
    }

    override fun onStart() {
        super.onStart()
        Evt.log("lifecycle", "state" to "onStart")
    }

    override fun onResume() {
        super.onResume()
        Evt.log("lifecycle", "state" to "onResume")
    }

    override fun onDestroy() {
        handler.removeCallbacks(tick)
        session?.close()
        session = null
        renderer.release()
        eglBase.release()
        Evt.log("destroyed")
        super.onDestroy()
    }
}
