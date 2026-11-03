package fyi.arlo.gander.spike.stream

import android.content.Context
import android.media.MediaCodecInfo
import android.media.MediaCodecList
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import org.json.JSONObject
import org.webrtc.DataChannel
import org.webrtc.DefaultVideoEncoderFactory
import org.webrtc.EglBase
import org.webrtc.HardwareVideoDecoderFactory
import org.webrtc.IceCandidate
import org.webrtc.MediaConstraints
import org.webrtc.MediaStream
import org.webrtc.MediaStreamTrack
import org.webrtc.PeerConnection
import org.webrtc.PeerConnectionFactory
import org.webrtc.Predicate
import org.webrtc.RTCStats
import org.webrtc.RtpTransceiver
import org.webrtc.SdpObserver
import org.webrtc.SessionDescription
import org.webrtc.VideoSink
import org.webrtc.VideoTrack
import java.nio.ByteBuffer
import java.util.concurrent.atomic.AtomicInteger

/**
 * The phone side of the stream: WebSocket signalling to the host, one PeerConnection that answers
 * the host's offer (receive-only video, the host-created `input` data channel), the track going to
 * the renderer and the frame probe, taps going back as normalised coordinates.
 *
 * Signalling (docs/specs/2026-11-01-stream-spike-findings.md, "API and signalling shapes"): the
 * viewer speaks first (`hello`), the host offers (complete, no trickle), the viewer answers once
 * ICE gathering is complete, either side may say `bye`.
 */
class StreamSession(
    appContext: Context,
    private val signalUrl: String,
    private val clientName: String,
    private val eglBase: EglBase,
    private val sinks: List<VideoSink>,
    private val textureDecode: Boolean,
    private val onState: (String) -> Unit,
    private val onEchoRtt: (Double) -> Unit,
) {
    private val http = OkHttpClient()
    private var ws: WebSocket? = null
    private var factory: PeerConnectionFactory? = null
    private var pc: PeerConnection? = null
    private var input: DataChannel? = null
    private var statsThread: Thread? = null

    @Volatile private var closed = false
    private val seq = AtomicInteger()
    private var answerSent = false
    private var wsOpened = false
    private var connectAttempts = 0

    @Volatile var dcOpen = false

    @Volatile var iceState = "new"

    @Volatile var decoderImpl = "?"

    @Volatile var decodeMsPerFrame = 0.0

    @Volatile var jitterBufferMs = 0.0

    @Volatile var framesDecoded = 0L

    @Volatile var framesDropped = 0L

    @Volatile var packetsLost = 0L

    @Volatile var pairInfo = ""

    init {
        PeerConnectionFactory.initialize(
            PeerConnectionFactory.InitializationOptions.builder(appContext).createInitializationOptions(),
        )
        logCodecs()
        // Any MediaCodec H.264 decoder, hardware or not: the emulator has no hardware one, and the
        // default predicate would hide its software decoder. A real device uses its hardware
        // decoder first (MediaCodecList order).
        val decoders = HardwareVideoDecoderFactory(
            if (textureDecode) eglBase.eglBaseContext else null,
            Predicate<MediaCodecInfo> { true },
        )
        val encoders = DefaultVideoEncoderFactory(eglBase.eglBaseContext, true, true)
        factory = PeerConnectionFactory.builder()
            .setVideoDecoderFactory(decoders)
            .setVideoEncoderFactory(encoders)
            .createPeerConnectionFactory()
        val caps = factory!!.getRtpReceiverCapabilities(MediaStreamTrack.MediaType.MEDIA_TYPE_VIDEO)
        Evt.log(
            "receiver_codecs",
            "codecs" to caps.codecs.joinToString(" | ") { "${it.mimeType} pt=${it.preferredPayloadType} ${it.parameters}" },
        )
    }

    private fun logCodecs() {
        try {
            val list = MediaCodecList(MediaCodecList.ALL_CODECS).codecInfos
            val avc = list.filter { !it.isEncoder && it.supportedTypes.any { t -> t.equals("video/avc", true) } }
            Evt.log(
                "mediacodec_avc_decoders",
                "decoders" to avc.joinToString(" | ") {
                    "${it.name} hw=${it.isHardwareAccelerated} sw=${it.isSoftwareOnly} vendor=${it.isVendor}"
                },
            )
        } catch (e: Exception) {
            Evt.log("mediacodec_list_error", "error" to e.toString())
        }
    }

    fun start() {
        onState("connecting")
        val rtc = PeerConnection.RTCConfiguration(emptyList()).apply {
            sdpSemantics = PeerConnection.SdpSemantics.UNIFIED_PLAN
            continualGatheringPolicy = PeerConnection.ContinualGatheringPolicy.GATHER_ONCE
            tcpCandidatePolicy = PeerConnection.TcpCandidatePolicy.DISABLED
            bundlePolicy = PeerConnection.BundlePolicy.MAXBUNDLE
            rtcpMuxPolicy = PeerConnection.RtcpMuxPolicy.REQUIRE
        }
        pc = factory!!.createPeerConnection(rtc, observer)
        if (pc == null) {
            onState("error: no peer connection")
            return
        }
        connect()
    }

    private fun connect() {
        connectAttempts++
        ws = http.newWebSocket(Request.Builder().url(signalUrl).build(), signalling)
    }

    private val signalling = object : WebSocketListener() {
        override fun onOpen(webSocket: WebSocket, response: Response) {
            wsOpened = true
            Evt.log("ws_open", "url" to signalUrl, "attempt" to connectAttempts)
            webSocket.send(JSONObject().put("type", "hello").put("client", clientName).put("version", 1).toString())
        }

        override fun onMessage(webSocket: WebSocket, text: String) {
            val m = JSONObject(text)
            when (val type = m.optString("type")) {
                "offer" -> {
                    val sdp = m.getString("sdp")
                    Evt.log("offer", "bytes" to sdp.length, "candidates" to candidateLines(sdp))
                    pc?.setRemoteDescription(
                        object : Sdp("setRemote") {
                            override fun onSetSuccess() = createAnswer()
                        },
                        SessionDescription(SessionDescription.Type.OFFER, sdp),
                    )
                }
                "bye" -> {
                    Evt.log("bye")
                    onState("host said bye")
                }
                "candidate" -> Evt.log("candidate_ignored")
                else -> Evt.log("ws_unknown", "type" to type)
            }
        }

        override fun onClosed(webSocket: WebSocket, code: Int, reason: String) {
            Evt.log("ws_closed", "code" to code, "reason" to reason)
        }

        override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
            Evt.log("ws_failure", "error" to t.toString(), "attempt" to connectAttempts)
            // Right after boot the emulator's network can still be coming up (the first connect
            // to 10.0.2.2 failed in the first run): retry the connection, not the session.
            if (!wsOpened && !closed && connectAttempts < 15) {
                onState("signalling retry $connectAttempts")
                Thread {
                    try {
                        Thread.sleep(1500)
                        if (!closed) connect()
                    } catch (_: InterruptedException) {
                        // Closing.
                    }
                }.start()
            } else {
                onState("signalling failed: ${t.message}")
            }
        }
    }

    private fun candidateLines(sdp: String): String =
        sdp.lines().filter { it.startsWith("a=candidate:") }.joinToString(" | ")

    private fun createAnswer() {
        pc?.createAnswer(
            object : Sdp("createAnswer") {
                override fun onCreateSuccess(desc: SessionDescription) {
                    pc?.setLocalDescription(
                        object : Sdp("setLocal") {
                            override fun onSetSuccess() = maybeSendAnswer()
                        },
                        desc,
                    )
                }
            },
            MediaConstraints(),
        )
    }

    /** Sends the answer once ICE gathering is complete (non-trickle). */
    @Synchronized
    private fun maybeSendAnswer() {
        val p = pc ?: return
        if (answerSent || p.iceGatheringState() != PeerConnection.IceGatheringState.COMPLETE) return
        val sdp = p.localDescription?.description ?: return
        answerSent = true
        Evt.log(
            "answer",
            "bytes" to sdp.length,
            "candidates" to candidateLines(sdp),
            "negotiation" to sdp.lines().filter {
                it.startsWith("m=") || it.startsWith("a=rtcp-fb") || it.startsWith("a=fmtp") ||
                    it.startsWith("a=extmap") || it.startsWith("a=recvonly") || it.startsWith("a=sendonly") ||
                    it.startsWith("a=sendrecv") || it.startsWith("a=inactive") || it.startsWith("a=setup")
            }.joinToString(" | "),
        )
        ws?.send(JSONObject().put("type", "answer").put("sdp", sdp).toString())
        onState("answered")
    }

    private val observer = object : PeerConnection.Observer {
        override fun onSignalingChange(s: PeerConnection.SignalingState) = Evt.log("signaling", "state" to s.name)

        override fun onIceConnectionChange(s: PeerConnection.IceConnectionState) {
            iceState = s.name
            Evt.log("ice_connection", "state" to s.name)
        }

        override fun onConnectionChange(s: PeerConnection.PeerConnectionState) {
            Evt.log("connection", "state" to s.name)
            if (s == PeerConnection.PeerConnectionState.CONNECTED) {
                onState("connected")
                startStats()
            }
            if (s == PeerConnection.PeerConnectionState.FAILED) onState("connection failed")
        }

        override fun onIceConnectionReceivingChange(b: Boolean) {}

        override fun onIceGatheringChange(s: PeerConnection.IceGatheringState) {
            Evt.log("ice_gathering", "state" to s.name)
            if (s == PeerConnection.IceGatheringState.COMPLETE) maybeSendAnswer()
        }

        override fun onIceCandidate(c: IceCandidate) = Evt.log("local_candidate", "sdp" to c.sdp)

        override fun onIceCandidatesRemoved(c: Array<out IceCandidate>) {}

        override fun onAddStream(s: MediaStream) {}

        override fun onRemoveStream(s: MediaStream) {}

        override fun onDataChannel(dc: DataChannel) {
            Evt.log("data_channel", "label" to dc.label(), "state" to dc.state().name)
            if (dc.label() != "input") return
            input = dc
            dc.registerObserver(object : DataChannel.Observer {
                override fun onBufferedAmountChange(previous: Long) {}

                override fun onStateChange() {
                    dcOpen = dc.state() == DataChannel.State.OPEN
                    Evt.log("data_channel_state", "state" to dc.state().name)
                }

                override fun onMessage(buffer: DataChannel.Buffer) {
                    val now = System.nanoTime()
                    val bytes = ByteArray(buffer.data.remaining())
                    buffer.data.get(bytes)
                    try {
                        val v = JSONObject(String(bytes, Charsets.UTF_8))
                        if (v.optString("t") == "echo") {
                            val ts = v.optLong("ts", 0L)
                            val rttMs = (now / 100 - ts) / 10_000.0
                            onEchoRtt(rttMs)
                            Evt.log("echo", "seq" to v.optInt("seq"), "rtt_ms" to rttMs, "host_rx" to v.optLong("host_rx"))
                        }
                    } catch (e: Exception) {
                        Evt.log("dc_bad_message", "error" to e.toString())
                    }
                }
            })
            dcOpen = dc.state() == DataChannel.State.OPEN
        }

        override fun onRenegotiationNeeded() {}

        override fun onTrack(t: RtpTransceiver) {
            val track = t.receiver.track()
            Evt.log("track", "kind" to track?.kind(), "mid" to t.mid, "direction" to t.currentDirection?.name)
            if (track is VideoTrack) {
                track.setEnabled(true)
                for (s in sinks) track.addSink(s)
            }
        }
    }

    /** Sends one tap, normalised to the picture (0..1, origin top-left). Returns its sequence. */
    fun sendTap(x: Double, y: Double): Int {
        val dc = input
        val n = seq.incrementAndGet()
        if (dc == null || dc.state() != DataChannel.State.OPEN) {
            Evt.log("tap_dropped", "seq" to n, "reason" to "data channel not open")
            return n
        }
        val ts = System.nanoTime() / 100
        val msg = JSONObject().put("t", "tap").put("seq", n).put("x", x).put("y", y).put("ts", ts).toString()
        dc.send(DataChannel.Buffer(ByteBuffer.wrap(msg.toByteArray(Charsets.UTF_8)), false))
        Evt.log("tap_sent", "seq" to n, "x" to x, "y" to y)
        return n
    }

    private fun startStats() {
        if (statsThread != null) return
        statsThread = Thread {
            while (!closed) {
                try {
                    Thread.sleep(1000)
                    pc?.getStats { report ->
                        val all = report.statsMap
                        for (s in all.values) {
                            if (s.type == "inbound-rtp" && s.members["kind"] == "video") inbound(s)
                        }
                        val pair = all.values.firstOrNull {
                            it.type == "candidate-pair" && it.members["state"] == "succeeded" && it.members["nominated"] == true
                        }
                        if (pair != null) {
                            val local = all[pair.members["localCandidateId"]]
                            val remote = all[pair.members["remoteCandidateId"]]
                            pairInfo = "local ${cand(local)} -> remote ${cand(remote)}"
                            Evt.log(
                                "candidate_pair",
                                "local" to cand(local),
                                "remote" to cand(remote),
                                "rtt_s" to num(pair.members["currentRoundTripTime"]),
                                "bytes_received" to num(pair.members["bytesReceived"]),
                            )
                        }
                    }
                } catch (e: InterruptedException) {
                    return@Thread
                } catch (e: Exception) {
                    Evt.log("stats_error", "error" to e.toString())
                }
            }
        }.also {
            it.isDaemon = true
            it.start()
        }
    }

    private fun cand(s: RTCStats?): String {
        if (s == null) return "?"
        val m = s.members
        return "${m["candidateType"]} ${m["address"] ?: m["ip"]}:${m["port"]} ${m["protocol"]}"
    }

    private fun num(v: Any?): Double? = (v as? Number)?.toDouble()

    private fun inbound(s: RTCStats) {
        val m = s.members
        val decoded = num(m["framesDecoded"])?.toLong() ?: 0L
        val totalDecode = num(m["totalDecodeTime"]) ?: 0.0
        val jbDelay = num(m["jitterBufferDelay"]) ?: 0.0
        val jbCount = num(m["jitterBufferEmittedCount"]) ?: 0.0
        framesDecoded = decoded
        framesDropped = num(m["framesDropped"])?.toLong() ?: 0L
        packetsLost = num(m["packetsLost"])?.toLong() ?: 0L
        decoderImpl = (m["decoderImplementation"] as? String) ?: "?"
        decodeMsPerFrame = if (decoded > 0) totalDecode * 1000.0 / decoded else 0.0
        jitterBufferMs = if (jbCount > 0) jbDelay * 1000.0 / jbCount else 0.0
        Evt.log(
            "inbound_video",
            "frames_received" to num(m["framesReceived"]),
            "frames_decoded" to decoded,
            "frames_dropped" to framesDropped,
            "key_frames_decoded" to num(m["keyFramesDecoded"]),
            "frames_per_second" to num(m["framesPerSecond"]),
            "frame_width" to num(m["frameWidth"]),
            "frame_height" to num(m["frameHeight"]),
            "decode_ms_per_frame" to decodeMsPerFrame,
            "jitter_buffer_ms" to jitterBufferMs,
            "packets_received" to num(m["packetsReceived"]),
            "packets_lost" to packetsLost,
            "bytes_received" to num(m["bytesReceived"]),
            "pli_count" to num(m["pliCount"]),
            "nack_count" to num(m["nackCount"]),
            "freeze_count" to num(m["freezeCount"]),
            "decoder" to decoderImpl,
        )
    }

    fun close() {
        if (closed) return
        closed = true
        try {
            ws?.send(JSONObject().put("type", "bye").toString())
            ws?.close(1000, "bye")
        } catch (_: Exception) {
            // Closing anyway.
        }
        statsThread?.interrupt()
        try {
            input?.unregisterObserver()
            input?.dispose()
            pc?.close()
            pc?.dispose()
            factory?.dispose()
        } catch (e: Exception) {
            Evt.log("close_error", "error" to e.toString())
        }
        http.dispatcher.executorService.shutdown()
    }

    /** SdpObserver that logs failures; subclasses override what they need. */
    private open inner class Sdp(private val what: String) : SdpObserver {
        override fun onCreateSuccess(desc: SessionDescription) {}

        override fun onSetSuccess() {}

        override fun onCreateFailure(error: String) {
            Evt.log("sdp_failure", "what" to what, "error" to error)
            onState("$what failed: $error")
        }

        override fun onSetFailure(error: String) {
            Evt.log("sdp_failure", "what" to what, "error" to error)
            onState("$what failed: $error")
        }
    }
}
