package fyi.arlo.gander.core.data.stream

import android.content.Context
import android.media.AudioAttributes
import android.media.MediaCodecInfo
import android.media.MediaCodecList
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.receiveAsFlow
import kotlinx.coroutines.suspendCancellableCoroutine
import org.webrtc.AudioTrack
import org.webrtc.DataChannel
import org.webrtc.DefaultVideoEncoderFactory
import org.webrtc.EglBase
import org.webrtc.HardwareVideoDecoderFactory
import org.webrtc.IceCandidate
import org.webrtc.MediaConstraints
import org.webrtc.MediaStream
import org.webrtc.PeerConnection
import org.webrtc.PeerConnectionFactory
import org.webrtc.Predicate
import org.webrtc.RtpTransceiver
import org.webrtc.SdpObserver
import org.webrtc.SessionDescription
import org.webrtc.VideoSink
import org.webrtc.VideoTrack
import org.webrtc.audio.JavaAudioDeviceModule
import java.nio.ByteBuffer
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException

// The phone's WebRTC side (WP-54), from the WP-44 spike (`spikes/stream/android`, findings in
// docs/specs/2026-11-01-stream-spike-findings.md): org.jitsi:webrtc 155, answering the host's
// offer receive-only, H.264 through MediaCodec only (this libwebrtc has no software H.264
// decoder), the host-created `input` and `control` data channels, and trickled candidates. What
// the spike measured is why the choices below are what they are:
//
// - Trickle, do not wait for gathering: `IceGatheringState.COMPLETE` fired before any candidate
//   existed in two emulator runs, so a non-trickle answer carried none. The answer goes out as
//   soon as the local description is set and every candidate follows as `stream.ice`.
// - The host dials the phone's first connectivity check back peer-reflexively, so an answer with
//   no candidates still connects; the trickled candidates are the better path when they work.
// - TCP candidates are off: the host serves UDP only.
// - Any MediaCodec H.264 decoder, hardware first (the emulator has only software ones).
// - Sound (WP-55): the host's Opus track (when it sends one) is answered recvonly and played by
//   libwebrtc's Java audio device module, set up for media playback (USAGE_MEDIA, stereo out)
//   rather than a call. Nothing records: the phone sends no audio track.

/** The size of the decoded picture; 0 x 0 until the first frame. */
data class FrameSize(val width: Int, val height: Int) {
    val known: Boolean get() = width > 0 && height > 0
}

/** Whether this phone can decode H.264 at all (MediaCodec, hardware or software). */
object H264Support {
    fun decoderAvailable(): Boolean = try {
        MediaCodecList(MediaCodecList.REGULAR_CODECS).codecInfos.any { info ->
            !info.isEncoder && info.supportedTypes.any { it.equals("video/avc", ignoreCase = true) }
        }
    } catch (_: RuntimeException) {
        false
    }
}

/**
 * Where decoded frames go: the EGL context the decoder and the renderer share, and a fan-out to
 * whatever views and probes are attached. It outlives a peer connection, so a reconnect (a new
 * connection for the same session) keeps showing in the same view.
 */
class StreamVideoOutput(
    /** Decode into GPU textures that the renderer draws without a pixel copy (the normal path on a phone). */
    val textureDecode: Boolean = true,
) {
    val eglBase: EglBase by lazy { EglBase.create() }

    private val sinks = CopyOnWriteArrayList<VideoSink>()
    private var track: VideoTrack? = null

    private val _frameSize = MutableStateFlow(FrameSize(0, 0))
    val frameSize: StateFlow<FrameSize> = _frameSize.asStateFlow()

    private val frames = AtomicLong()

    /** Frames delivered so far (the instrumented test and the diagnostics read it). */
    val framesReceived: Long get() = frames.get()

    private val fanOut = VideoSink { frame ->
        frames.incrementAndGet()
        val w = frame.rotatedWidth
        val h = frame.rotatedHeight
        val current = _frameSize.value
        if (current.width != w || current.height != h) _frameSize.value = FrameSize(w, h)
        for (sink in sinks) sink.onFrame(frame)
    }

    fun addSink(sink: VideoSink) {
        sinks.addIfAbsent(sink)
    }

    fun removeSink(sink: VideoSink) {
        sinks.remove(sink)
    }

    internal fun attach(newTrack: VideoTrack) {
        detach()
        track = newTrack
        newTrack.setEnabled(true)
        newTrack.addSink(fanOut)
    }

    internal fun detach() {
        track?.let { runCatching { it.removeSink(fanOut) } }
        track = null
    }

    /** Releases the EGL context. Views must have been released first. */
    fun release() {
        detach()
        sinks.clear()
        runCatching { eglBase.release() }
    }
}

class WebRtcStreamTransport(
    context: Context,
    private val output: StreamVideoOutput,
) : StreamTransport {

    private val appContext: Context = context.applicationContext
    private val channel = Channel<TransportEvent>(Channel.UNLIMITED)

    override val events: Flow<TransportEvent> = channel.receiveAsFlow()

    private val lock = Any()
    private var factory: PeerConnectionFactory? = null
    private var audioModule: JavaAudioDeviceModule? = null
    private var audioTrack: AudioTrack? = null

    @Volatile
    private var audioEnabled = true
    private var pc: PeerConnection? = null
    private var control: DataChannel? = null
    private var input: DataChannel? = null
    private var mid = "0"
    private var remoteSet = false
    private val pendingRemote = ArrayList<String>()

    @Volatile
    private var closed = false

    private fun setUp() {
        ensureInitialised(appContext)
        // Any MediaCodec H.264 decoder, hardware first (MediaCodecList order): the emulator has no
        // hardware one, and the default predicate would hide its software decoder.
        val decoders = HardwareVideoDecoderFactory(
            if (output.textureDecode) output.eglBase.eglBaseContext else null,
            Predicate<MediaCodecInfo> { true },
        )
        val encoders = DefaultVideoEncoderFactory(output.eglBase.eglBaseContext, true, true)
        // The window's sound is media, not a call: media volume and routing, stereo.
        val audio = JavaAudioDeviceModule.builder(appContext)
            .setAudioAttributes(
                AudioAttributes.Builder()
                    .setUsage(AudioAttributes.USAGE_MEDIA)
                    .setContentType(AudioAttributes.CONTENT_TYPE_MOVIE)
                    .build(),
            )
            .setUseStereoOutput(true)
            .createAudioDeviceModule()
        audioModule = audio
        val f = PeerConnectionFactory.builder()
            .setVideoDecoderFactory(decoders)
            .setVideoEncoderFactory(encoders)
            .setAudioDeviceModule(audio)
            .createPeerConnectionFactory()
        val rtc = PeerConnection.RTCConfiguration(emptyList()).apply {
            sdpSemantics = PeerConnection.SdpSemantics.UNIFIED_PLAN
            // Candidates keep coming after the answer: they are trickled (see the note above).
            continualGatheringPolicy = PeerConnection.ContinualGatheringPolicy.GATHER_CONTINUALLY
            tcpCandidatePolicy = PeerConnection.TcpCandidatePolicy.DISABLED
            bundlePolicy = PeerConnection.BundlePolicy.MAXBUNDLE
            rtcpMuxPolicy = PeerConnection.RtcpMuxPolicy.REQUIRE
        }
        factory = f
        pc = f.createPeerConnection(rtc, observer)
        if (pc == null) channel.trySend(TransportEvent.Link(PeerLink.FAILED))
    }

    override suspend fun acceptOffer(offerSdp: String): String = suspendCancellableCoroutine { cont ->
        val p = pc
        if (p == null || closed) {
            cont.resumeWithException(IllegalStateException("peer connection is closed"))
            return@suspendCancellableCoroutine
        }
        mid = IceCandidates.firstMid(offerSdp)
        p.setRemoteDescription(
            object : Sdp("setRemoteDescription") {
                override fun onSetSuccess() {
                    flushRemote(p)
                    p.createAnswer(
                        object : Sdp("createAnswer") {
                            override fun onCreateSuccess(desc: SessionDescription) {
                                p.setLocalDescription(
                                    object : Sdp("setLocalDescription") {
                                        override fun onSetSuccess() {
                                            val answer = p.localDescription?.description
                                            if (answer == null) fail("no local description") else cont.resume(answer)
                                        }

                                        override fun fail(message: String) = cont.resumeWithException(IllegalStateException(message))
                                    },
                                    desc,
                                )
                            }

                            override fun fail(message: String) = cont.resumeWithException(IllegalStateException(message))
                        },
                        MediaConstraints(),
                    )
                }

                override fun fail(message: String) = cont.resumeWithException(IllegalStateException(message))
            },
            SessionDescription(SessionDescription.Type.OFFER, offerSdp),
        )
    }

    override fun addRemoteCandidate(candidate: String) {
        val p = pc ?: return
        val ready = synchronized(lock) {
            if (!remoteSet) pendingRemote += candidate
            remoteSet
        }
        if (ready) p.addIceCandidate(IceCandidate(mid, 0, candidate))
    }

    private fun flushRemote(p: PeerConnection) {
        val queued = synchronized(lock) {
            remoteSet = true
            ArrayList(pendingRemote).also { pendingRemote.clear() }
        }
        queued.forEach { p.addIceCandidate(IceCandidate(mid, 0, it)) }
    }

    override fun send(channel: InputChannel, text: String): Boolean {
        val dc = when (channel) {
            InputChannel.INPUT -> input
            InputChannel.CONTROL -> control
        } ?: return false
        if (closed || dc.state() != DataChannel.State.OPEN) return false
        return dc.send(DataChannel.Buffer(ByteBuffer.wrap(text.toByteArray(Charsets.UTF_8)), false))
    }

    override fun setAudioEnabled(enabled: Boolean) {
        audioEnabled = enabled
        audioTrack?.let { runCatching { it.setEnabled(enabled) } }
    }

    override fun close() {
        if (closed) return
        closed = true
        output.detach()
        audioTrack = null
        runCatching { control?.unregisterObserver() }
        runCatching { input?.unregisterObserver() }
        runCatching { control?.dispose() }
        runCatching { input?.dispose() }
        runCatching { pc?.close() }
        runCatching { pc?.dispose() }
        runCatching { factory?.dispose() }
        runCatching { audioModule?.release() }
        audioModule = null
        control = null
        input = null
        pc = null
        factory = null
        channel.close()
    }

    private val observer = object : PeerConnection.Observer {
        override fun onSignalingChange(state: PeerConnection.SignalingState) {}

        override fun onIceConnectionChange(state: PeerConnection.IceConnectionState) {}

        override fun onConnectionChange(state: PeerConnection.PeerConnectionState) {
            if (closed) return
            when (state) {
                PeerConnection.PeerConnectionState.CONNECTED -> channel.trySend(TransportEvent.Link(PeerLink.UP))
                PeerConnection.PeerConnectionState.DISCONNECTED -> channel.trySend(TransportEvent.Link(PeerLink.DOWN))
                PeerConnection.PeerConnectionState.FAILED,
                PeerConnection.PeerConnectionState.CLOSED,
                -> channel.trySend(TransportEvent.Link(PeerLink.FAILED))

                else -> Unit
            }
        }

        override fun onIceConnectionReceivingChange(receiving: Boolean) {}

        override fun onIceGatheringChange(state: PeerConnection.IceGatheringState) {
            if (state == PeerConnection.IceGatheringState.COMPLETE) channel.trySend(TransportEvent.GatheringComplete)
        }

        override fun onIceCandidate(candidate: IceCandidate) {
            channel.trySend(TransportEvent.LocalCandidate(candidate.sdp))
        }

        override fun onIceCandidatesRemoved(candidates: Array<out IceCandidate>) {}

        override fun onAddStream(stream: MediaStream) {}

        override fun onRemoveStream(stream: MediaStream) {}

        override fun onDataChannel(dc: DataChannel) {
            val kind = InputChannel.entries.firstOrNull { it.label == dc.label() } ?: return
            when (kind) {
                InputChannel.INPUT -> input = dc
                InputChannel.CONTROL -> control = dc
            }
            dc.registerObserver(
                object : DataChannel.Observer {
                    override fun onBufferedAmountChange(previousAmount: Long) {}

                    override fun onStateChange() {
                        if (dc.state() == DataChannel.State.OPEN) channel.trySend(TransportEvent.ChannelOpen(kind))
                    }

                    override fun onMessage(buffer: DataChannel.Buffer) {
                        if (kind != InputChannel.CONTROL || buffer.binary) return
                        val bytes = ByteArray(buffer.data.remaining())
                        buffer.data.get(bytes)
                        channel.trySend(TransportEvent.ControlText(String(bytes, Charsets.UTF_8)))
                    }
                },
            )
            if (dc.state() == DataChannel.State.OPEN) channel.trySend(TransportEvent.ChannelOpen(kind))
        }

        override fun onRenegotiationNeeded() {}

        override fun onTrack(transceiver: RtpTransceiver) {
            when (val track = transceiver.receiver.track()) {
                is VideoTrack -> output.attach(track)
                is AudioTrack -> {
                    audioTrack = track
                    track.setEnabled(audioEnabled)
                }
                else -> Unit
            }
        }
    }

    // After `observer`: the peer connection is created with it.
    init {
        setUp()
    }

    /** An [SdpObserver] that turns both failure callbacks into [fail]. */
    private abstract inner class Sdp(private val what: String) : SdpObserver {
        override fun onCreateSuccess(desc: SessionDescription) {}

        override fun onSetSuccess() {}

        override fun onCreateFailure(error: String) = fail("$what: $error")

        override fun onSetFailure(error: String) = fail("$what: $error")

        abstract fun fail(message: String)
    }

    private companion object {
        private val initialised = AtomicBoolean(false)

        fun ensureInitialised(context: Context) {
            if (initialised.compareAndSet(false, true)) {
                PeerConnectionFactory.initialize(
                    PeerConnectionFactory.InitializationOptions.builder(context.applicationContext)
                        .createInitializationOptions(),
                )
            }
        }
    }
}
