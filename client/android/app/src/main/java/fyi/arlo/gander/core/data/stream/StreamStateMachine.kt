package fyi.arlo.gander.core.data.stream

import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.protocol.NetworkAdapter
import fyi.arlo.gander.core.net.protocol.RelayPath
import fyi.arlo.gander.core.net.protocol.StreamReason
import fyi.arlo.gander.core.net.protocol.StreamState

// What the stream viewer is doing and what it should say about it (client spec: "State banner
// from stream.state: launching, approving on host, paused (reason), reconnecting"), plus the
// liveness rule that WP-44 and the D12 acceptance require:
//
//   - while the stream is on screen the phone pings the host every second on the `control`
//     channel and the host answers `pong`;
//   - the host pauses a stream whose phone is silent for more than 5 s (reason `network`);
//   - the phone treats the host the same way: no `pong` or `state` for more than 5 s is
//     "reconnecting", and the next message from the host ends it;
//   - on `onStop` the phone says `pause` (both on the data channel and as `stream.pause`) and stops
//     pinging, on return `resume`; the host keeps the session for 120 s.
//
// And "Bring to front" (stream launch policy, "Input mapping"): while input is paused with
// `focus_lost` the user can ask the host to bring the window forward (`{"t":"front"}` on
// `control`, answered by `front.result`). The host tries once by itself when the phone connects;
// if the window is still not in front shortly after, the phone asks once on its own
// ([StreamStateMachine.AUTO_FRONT_WINDOW_MS]). Later `focus_lost` pauses (someone at the computer
// switched windows) are left to the user.
//
// Pure Kotlin with the clock passed in, so the whole state machine runs in JVM tests.

/** Where the viewer is, in the terms the UI explains. */
sealed interface StreamPhase {
    /** Asking the host what it can do (`GET /me`) before anything starts. */
    data object Preparing : StreamPhase

    /** The path to the host is relayed: the user is warned and decides before the stream starts. */
    data class ConfirmRelay(val adapter: NetworkAdapter, val path: RelayPath) : StreamPhase

    /** `POST /stream` is on its way. */
    data object Starting : StreamPhase

    /** The host has the request and has not started anything yet. */
    data object Requested : StreamPhase

    /** The owner of the computer is being asked. */
    data object Approving : StreamPhase

    data object Launching : StreamPhase

    /**
     * The host could not tell which window is the file's: [candidates] are their titles and the
     * user taps one (`stream.pick`). [chosen] is the index sent while the host checks it, [windowGone]
     * is true after the host said the chosen window is no longer open (the pick starts over).
     */
    data class PickWindow(
        val candidates: List<String>,
        val chosen: Int? = null,
        val windowGone: Boolean = false,
    ) : StreamPhase

    /** The window is found; the stream is being set up. */
    data object WindowFound : StreamPhase

    /** The host is streaming; the video link is being set up. */
    data object Connecting : StreamPhase

    data object Streaming : StreamPhase

    /** The host paused the stream: [reason] is `focus_lost`, `local_user_active`, `phone_background` or `network`. */
    data class Paused(val reason: StreamReason?) : StreamPhase

    /** This phone is not looking (app in the background): the host was told to stop encoding. */
    data object Backgrounded : StreamPhase

    /** The link to the host dropped or the host went quiet; it is being brought back. */
    data object Reconnecting : StreamPhase

    /** Over. [cause] says why; the end dialog explains it. */
    data class Ended(val cause: EndCause) : StreamPhase

    /** The stream could not be started: the host refused ([error]) or could not be reached. */
    data class Failed(val error: GanderError?) : StreamPhase

    /** Stream cannot work between this phone and this computer, whatever the user does. */
    data class Unsupported(val why: Why) : StreamPhase {
        enum class Why {
            /** No MediaCodec H.264 decoder: libwebrtc 155 has no software H.264 decoder. */
            PHONE_HAS_NO_H264_DECODER,

            /** `capabilities.stream.codecs` does not list H.264. */
            HOST_DOES_NOT_STREAM_H264,
        }
    }
}

/** Why a session ended, as the end dialog tells it. */
enum class EndCause {
    /** The user left. */
    YOU_LEFT,

    /** The owner stopped it at the computer (kill switch). */
    HOST_KILL_SWITCH,

    /** The app's window was closed on the computer. */
    WINDOW_CLOSED,

    /** The owner removed this phone. */
    REVOKED,

    /** The owner did not answer, or the host waited too long. */
    TIMEOUT,

    /**
     * No window to stream: the app showed none, or the phone did not choose one within
     * [StreamStateMachine.PICK_TIMEOUT_SECONDS] s. The host ends with `error app_unavailable`
     * followed by `stream.state ended` with reason `timeout`.
     */
    APP_UNAVAILABLE,

    /** The computer was locked while streaming (the host sends `error host_locked`). */
    HOST_LOCKED,

    /** The host reported an error. */
    ERROR,

    /** The link was lost and did not come back. */
    CONNECTION_LOST,

    /** The video link could not be set up at all (a firewall blocking UDP is the usual reason). */
    NO_VIDEO_LINK,
    ;

    companion object {
        fun fromReason(reason: StreamReason?): EndCause = when (reason) {
            StreamReason.CLIENT_CLOSED -> YOU_LEFT
            StreamReason.HOST_KILL_SWITCH -> HOST_KILL_SWITCH
            StreamReason.WINDOW_CLOSED -> WINDOW_CLOSED
            StreamReason.REVOKED -> REVOKED
            StreamReason.TIMEOUT -> TIMEOUT
            StreamReason.NETWORK -> CONNECTION_LOST
            else -> ERROR
        }
    }
}

/** The state of the WebRTC connection as the machine needs it. */
enum class PeerLink {
    /** Connected: media and data channels can flow. */
    UP,

    /** ICE disconnected; libwebrtc may bring it back on its own. */
    DOWN,

    /** ICE failed or the connection closed: only a new negotiation helps. */
    FAILED,
}

/** Where the phone's "Bring to front" stands. */
enum class FrontStatus {
    /** Nothing asked (or the answer no longer matters). */
    IDLE,

    /** `front` sent, no `front.result` yet. */
    SENDING,

    /** The host says the window is in front; the pause ends with the next `stream.state`. */
    ACQUIRED,

    /** The host could not bring the window forward (Windows refused, or the window is gone). */
    REFUSED,
}

/** What the controller must do because of a change. */
sealed interface StreamEffect {
    /** Send `{"t":"front"}` on the `control` channel. */
    data object SendFront : StreamEffect

    /** Send `{"t":"ping","seq":seq}` on the `control` channel. */
    data class SendPing(val seq: Long) : StreamEffect

    /** Tell the host this phone stopped looking: `{"t":"pause"}` and `stream.pause`. */
    data object SendPause : StreamEffect

    /** Tell the host this phone looks again: `{"t":"resume"}` and `stream.resume`. */
    data object SendResume : StreamEffect

    /** Ask for an IDR (`{"t":"keyframe"}`): the picture may be broken after a gap. */
    data object RequestKeyframe : StreamEffect

    /** Negotiate a new peer connection for the same session (`POST /stream` with `resume`). */
    data object Reconnect : StreamEffect
}

data class LivenessConfig(
    /** The phone pings this often while the stream is on screen. */
    val pingIntervalMs: Long = 1_000L,
    /** Silence longer than this pauses the stream (host rule) and shows "reconnecting" (phone rule). */
    val silenceMs: Long = 5_000L,
    /** While reconnecting, a fresh negotiation is tried this often. */
    val reconnectAfterMs: Long = 30_000L,
    /** The host keeps a session this long after the link dropped (stream launch policy: 120 s). */
    val giveUpAfterMs: Long = 120_000L,
    /** The first video link must come up within this long after the offer. */
    val connectTimeoutMs: Long = 20_000L,
)

class StreamStateMachine(private val config: LivenessConfig = LivenessConfig()) {
    companion object {
        /** How long the host waits for a pick before it ends the session (`PICK_TIMEOUT` in the host's lifecycle). */
        const val PICK_TIMEOUT_SECONDS = 30

        /**
         * A `focus_lost` pause within this long of the `control` channel opening means the host's
         * own attempt at connect did not work: the phone asks once by itself. Later pauses are the
         * local user's doing and only the user's tap asks.
         */
        const val AUTO_FRONT_WINDOW_MS = 10_000L

        /** A `front` with no `front.result` within this long is given up (the button comes back). */
        const val FRONT_TIMEOUT_MS = 5_000L
    }

    /** Where the phone's "Bring to front" stands. */
    var front: FrontStatus = FrontStatus.IDLE
        private set

    private var frontSentAt: Long? = null
    private var autoFrontDone = false
    private var controlOpenedAt: Long? = null

    /** What the viewer is doing now. */
    var phase: StreamPhase = StreamPhase.Preparing
        private set

    /** True once the session is over ([StreamPhase.Ended], [StreamPhase.Failed], [StreamPhase.Unsupported]); nothing changes after that. */
    val isTerminal: Boolean get() = terminal != null

    private var hostState: StreamState? = null
    private var hostReason: StreamReason? = null
    private var candidates: List<String> = emptyList()
    private var terminal: StreamPhase? = null

    private var link: PeerLink? = null
    private var everUp = false
    private var controlOpen = false
    private var background = false
    private var silent = false
    private var lastHeardAt = 0L
    private var lastPingAt: Long? = null
    private var pingSeq = 0L
    private var offerAt: Long? = null
    private var reconnectingSince: Long? = null
    private var lastReconnectAt: Long? = null

    // Before and during the request ------------------------------------------------------------

    fun onPreparing() = set(StreamPhase.Preparing)

    fun onConfirmRelay(adapter: NetworkAdapter, path: RelayPath) = set(StreamPhase.ConfirmRelay(adapter, path))

    fun onStarting() = set(StreamPhase.Starting)

    /** The request was refused or could not be sent. */
    fun onFailed(error: GanderError?) = finish(StreamPhase.Failed(error))

    /** Stream mode cannot work here (no H.264 decoder on the phone, no H.264 on the computer). */
    fun onUnsupported(why: StreamPhase.Unsupported.Why) = finish(StreamPhase.Unsupported(why))

    private fun finish(next: StreamPhase) {
        terminal = next
        phase = next
    }

    // The host ----------------------------------------------------------------------------------

    /** `stream.state` over `/v1/events` (or the same state mirrored on the `control` channel). */
    fun onHostState(state: StreamState, reason: StreamReason?, candidates: List<String>?, now: Long): List<StreamEffect> {
        if (terminal != null) return emptyList()
        if (state == StreamState.ENDED) {
            end(EndCause.fromReason(reason))
            return emptyList()
        }
        if (state != StreamState.PICK) {
            pickChosen = null
            pickWindowGone = false
        }
        if (state != StreamState.PICK && state != StreamState.LAUNCHING) appUnavailable = false
        hostState = state
        hostReason = reason
        if (candidates != null) this.candidates = candidates
        return recompute(now)
    }

    /** The host's `error host_locked`: the end that follows is the lock, not a generic error. */
    fun onHostLocked() {
        if (terminal == null) lockedBeforeEnd = true
    }

    private var lockedBeforeEnd = false

    /**
     * The host's `error app_unavailable`. Either the picked window is gone (the session stays in
     * `pick` and the user chooses again) or the host gave up on the app or the pick, in which case
     * `stream.state ended` (reason `timeout`) follows at once and the end is [EndCause.APP_UNAVAILABLE].
     * Cleared when the session leaves `pick` and `launching`, so a later timeout stays a timeout.
     */
    fun onHostAppUnavailable(now: Long): List<StreamEffect> {
        if (terminal != null) return emptyList()
        appUnavailable = true
        // A pick that failed is over: the user chooses again.
        pickChosen = null
        pickWindowGone = hostState == StreamState.PICK
        return recompute(now)
    }

    /** The host refused the phone's pick for another reason (`bad_request`): the user chooses again. */
    fun onPickRefused(now: Long): List<StreamEffect> {
        if (terminal != null) return emptyList()
        pickChosen = null
        return recompute(now)
    }

    /**
     * The user tapped candidate [index]. True when the choice may be sent: the host is waiting for
     * a pick and the index names a candidate. The choice stays marked as sent until the host moves
     * on or refuses it; another tap meanwhile is allowed (the host ignores a pick once it has a
     * window), so a pick lost with the events socket never strands the user.
     */
    fun onPickChosen(index: Int, now: Long): Boolean {
        if (terminal != null || background || hostState != StreamState.PICK) return false
        if (index !in candidates.indices) return false
        pickChosen = index
        pickWindowGone = false
        recompute(now)
        return true
    }

    private var appUnavailable = false
    private var pickChosen: Int? = null
    private var pickWindowGone = false

    /** The host's offer arrived: the video link must come up soon. */
    fun onOffer(now: Long): List<StreamEffect> {
        if (offerAt == null) offerAt = now
        return recompute(now)
    }

    // The peer connection -----------------------------------------------------------------------

    fun onPeer(state: PeerLink, now: Long): List<StreamEffect> {
        if (terminal != null) return emptyList()
        link = state
        when (state) {
            PeerLink.UP -> {
                everUp = true
                silent = false
                // Quiet is expected until the first pong: start the clock now.
                lastHeardAt = now
            }

            PeerLink.FAILED -> if (!everUp) {
                end(EndCause.NO_VIDEO_LINK)
                return emptyList()
            }

            PeerLink.DOWN -> Unit
        }
        return recompute(now)
    }

    /** The data channel `control` opened: the pings start with the next [tick]. */
    fun onControlOpen(now: Long) {
        controlOpen = true
        controlOpenedAt = now
        lastHeardAt = now
        lastPingAt = null
    }

    /**
     * A new peer connection replaces a failed one (same session): the old link and channels are
     * gone, the viewer keeps saying "reconnecting" until the new link is up.
     */
    fun onTransportReplaced(now: Long) {
        link = null
        controlOpen = false
        controlOpenedAt = null
        // A new connection: the host tries once more by itself, and so may the phone.
        autoFrontDone = false
        front = FrontStatus.IDLE
        frontSentAt = null
        silent = false
        offerAt = null
        lastReconnectAt = now
    }

    /** Anything from the host on the `control` channel proves it is there. */
    fun onControlMessage(message: ControlMessage, now: Long): List<StreamEffect> {
        if (terminal != null) return emptyList()
        lastHeardAt = now
        // The host is there: whatever quiet came before is over.
        silent = false
        val effects = ArrayList<StreamEffect>(2)
        if (message is ControlMessage.State) {
            // Mirror of `stream.state`; only the two states the host mirrors.
            if (message.state == StreamState.STREAMING || message.state == StreamState.PAUSED) {
                hostState = message.state
                hostReason = message.reason
            }
        }
        if (message is ControlMessage.FrontResult && front == FrontStatus.SENDING) {
            front = if (message.acquired) FrontStatus.ACQUIRED else FrontStatus.REFUSED
            frontSentAt = null
        }
        effects += recompute(now)
        return effects
    }

    /** True while input is paused because another window is in front on the computer. */
    val isFocusLost: Boolean get() = phase == StreamPhase.Paused(StreamReason.FOCUS_LOST)

    /**
     * The user tapped "Bring to front": [StreamEffect.SendFront] when it may go out (input paused
     * for `focus_lost`, `control` open, no request waiting for its answer), else nothing.
     */
    fun onFrontRequested(now: Long): List<StreamEffect> {
        if (terminal != null || !isFocusLost || !controlOpen || front == FrontStatus.SENDING) return emptyList()
        front = FrontStatus.SENDING
        frontSentAt = now
        return listOf(StreamEffect.SendFront)
    }

    // This phone ---------------------------------------------------------------------------------

    /** The app went to the background (`onStop`): the host stops encoding. */
    fun onBackground(now: Long): List<StreamEffect> {
        if (terminal != null || background) return emptyList()
        background = true
        return listOf(StreamEffect.SendPause) + recompute(now)
    }

    /** The app is back: the host resumes with an IDR; the clock for "quiet host" starts again. */
    fun onForeground(now: Long): List<StreamEffect> {
        if (terminal != null || !background) return emptyList()
        background = false
        silent = false
        lastHeardAt = now
        lastPingAt = null
        return listOf(StreamEffect.SendResume) + recompute(now)
    }

    /** The user left. */
    fun onUserEnd() {
        if (terminal == null) end(EndCause.YOU_LEFT)
    }

    /** The viewer is giving up for a reason of its own (the controller could not reconnect). */
    fun onGiveUp(cause: EndCause) {
        if (terminal == null) end(cause)
    }

    // Time ----------------------------------------------------------------------------------------

    /** Call about every 250 ms while the session is alive. */
    fun tick(now: Long): List<StreamEffect> {
        if (terminal != null) return emptyList()
        val effects = ArrayList<StreamEffect>(2)
        val sent = frontSentAt
        if (front == FrontStatus.SENDING && sent != null && now - sent >= FRONT_TIMEOUT_MS) {
            front = FrontStatus.IDLE
            frontSentAt = null
        }
        val opened = controlOpenedAt
        if (!autoFrontDone && isFocusLost && opened != null) {
            // Once per connection, and only right after it came up.
            autoFrontDone = true
            if (now - opened <= AUTO_FRONT_WINDOW_MS) effects += onFrontRequested(now)
        }
        if (controlOpen && !background) {
            val sentAt = lastPingAt
            if (sentAt == null || now - sentAt >= config.pingIntervalMs) {
                pingSeq += 1
                lastPingAt = now
                effects += StreamEffect.SendPing(pingSeq)
            }
            if (!silent && now - lastHeardAt > config.silenceMs) {
                silent = true
                effects += recompute(now)
            }
        }
        val firstOffer = offerAt
        if (!everUp && firstOffer != null && now - firstOffer > config.connectTimeoutMs) {
            end(EndCause.NO_VIDEO_LINK)
            return effects
        }
        val since = reconnectingSince
        if (phase == StreamPhase.Reconnecting && since != null && !background) {
            if (now - since >= config.giveUpAfterMs) {
                end(EndCause.CONNECTION_LOST)
                return effects
            }
            val last = maxOf(since, lastReconnectAt ?: since)
            if (now - last >= config.reconnectAfterMs) {
                lastReconnectAt = now
                effects += StreamEffect.Reconnect
            }
        }
        return effects
    }

    // Internals -----------------------------------------------------------------------------------

    private fun end(cause: EndCause) {
        val resolved = when {
            cause == EndCause.ERROR && lockedBeforeEnd -> EndCause.HOST_LOCKED
            cause == EndCause.TIMEOUT && appUnavailable -> EndCause.APP_UNAVAILABLE
            else -> cause
        }
        finish(StreamPhase.Ended(resolved))
    }

    private fun set(next: StreamPhase) {
        if (terminal == null) phase = next
    }

    private fun recompute(now: Long): List<StreamEffect> {
        if (terminal != null) return emptyList()
        val before = phase
        val next = when {
            background -> StreamPhase.Backgrounded
            everUp && (link != PeerLink.UP || silent) -> StreamPhase.Reconnecting
            hostState == StreamState.PAUSED -> StreamPhase.Paused(hostReason)
            hostState == StreamState.STREAMING -> if (link == PeerLink.UP) StreamPhase.Streaming else StreamPhase.Connecting
            hostState == StreamState.PICK -> StreamPhase.PickWindow(candidates, pickChosen, pickWindowGone)
            hostState == StreamState.WINDOW_FOUND -> if (offerAt != null) StreamPhase.Connecting else StreamPhase.WindowFound
            hostState == StreamState.LAUNCHING -> StreamPhase.Launching
            hostState == StreamState.APPROVING -> StreamPhase.Approving
            hostState == StreamState.REQUESTED -> StreamPhase.Requested
            else -> before
        }
        phase = next
        // The answer to a "Bring to front" only matters while the pause lasts.
        if (!isFocusLost && front != FrontStatus.SENDING) front = FrontStatus.IDLE
        val effects = ArrayList<StreamEffect>(2)
        if (next == StreamPhase.Reconnecting && before != StreamPhase.Reconnecting) {
            reconnectingSince = now
            // A failed connection cannot recover by itself: negotiate again at once.
            if (link == PeerLink.FAILED) {
                lastReconnectAt = now
                effects += StreamEffect.Reconnect
            }
        } else if (next != StreamPhase.Reconnecting && before == StreamPhase.Reconnecting) {
            reconnectingSince = null
            effects += StreamEffect.RequestKeyframe
        }
        return effects
    }
}

/** What the banner over the picture says; the UI turns it into a sentence. */
sealed interface StreamBanner {
    data object Requested : StreamBanner

    data object Approving : StreamBanner

    data object Launching : StreamBanner

    data class PickWindow(val candidates: List<String>, val chosen: Int? = null, val windowGone: Boolean = false) : StreamBanner

    data object SettingUp : StreamBanner

    data class Paused(val reason: StreamReason?) : StreamBanner

    data object Backgrounded : StreamBanner

    data object Reconnecting : StreamBanner
}

/** The banner of a phase, or null while the picture speaks for itself (and for the phases that have their own screen). */
fun StreamPhase.banner(): StreamBanner? = when (this) {
    StreamPhase.Requested, StreamPhase.Starting -> StreamBanner.Requested
    StreamPhase.Approving -> StreamBanner.Approving
    StreamPhase.Launching -> StreamBanner.Launching
    is StreamPhase.PickWindow -> StreamBanner.PickWindow(candidates, chosen, windowGone)
    StreamPhase.WindowFound, StreamPhase.Connecting -> StreamBanner.SettingUp
    is StreamPhase.Paused -> StreamBanner.Paused(reason)
    StreamPhase.Backgrounded -> StreamBanner.Backgrounded
    StreamPhase.Reconnecting -> StreamBanner.Reconnecting
    StreamPhase.Streaming, StreamPhase.Preparing, is StreamPhase.ConfirmRelay,
    is StreamPhase.Ended, is StreamPhase.Failed, is StreamPhase.Unsupported,
    -> null
}

