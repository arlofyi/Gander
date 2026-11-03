package fyi.arlo.gander.core.data.stream

import fyi.arlo.gander.core.data.actions.ActionRefusals
import fyi.arlo.gander.core.data.actions.FileAction
import fyi.arlo.gander.core.data.browse.EntryTarget
import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.protocol.ClientEvent
import fyi.arlo.gander.core.net.protocol.ClipboardMode
import fyi.arlo.gander.core.net.protocol.Entry
import fyi.arlo.gander.core.net.protocol.ErrorCode
import fyi.arlo.gander.core.net.protocol.InputMessage
import fyi.arlo.gander.core.net.protocol.MeResponse
import fyi.arlo.gander.core.net.protocol.RelayPath
import fyi.arlo.gander.core.net.protocol.ServerEvent
import fyi.arlo.gander.core.net.protocol.StreamApp
import fyi.arlo.gander.core.net.protocol.StreamCreated
import fyi.arlo.gander.core.net.protocol.StreamPreference
import fyi.arlo.gander.core.net.protocol.StreamRequest
import fyi.arlo.gander.core.net.protocol.StreamWindow
import fyi.arlo.gander.core.net.protocol.VideoCodec
import fyi.arlo.gander.core.net.protocol.Viewport
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Job
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.TimeoutCancellationException
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.withTimeoutOrNull
import java.io.IOException
import java.net.SocketTimeoutException

// The stream viewer's session logic (WP-54): the HTTP calls, the `/v1/events` signalling, the
// WebRTC transport and the state machine, tied together. It speaks only to the small interfaces
// below, so the whole session (offer, answer, trickled candidates, liveness pings, pause on
// background, reconnect, end) runs in JVM tests against fakes (`StreamSessionTest`); the
// libwebrtc and OkHttp implementations are `WebRtcStreamTransport` and `StreamHosts`.

/** The calls to the host that a stream needs. Every function throws [GanderError] and nothing else. */
interface StreamHostApi {
    suspend fun me(): MeResponse

    suspend fun startStream(request: StreamRequest): StreamCreated

    suspend fun endStream(session: String, closeApp: Boolean)

    /** `GET /shares/{share}/stat`, to learn whether the file changed during the session. */
    suspend fun stat(share: String, path: String): Entry
}

/** One `/v1/events` socket. */
interface StreamEventsConnection {
    /** Known events in order; completes when the socket closes and fails with a [GanderError] when it breaks. */
    val incoming: Flow<ServerEvent>

    fun send(event: ClientEvent): Boolean

    fun close()
}

fun interface StreamEventsSource {
    fun open(): StreamEventsConnection
}

/** What the peer connection reports. */
sealed interface TransportEvent {
    /** A local ICE candidate to trickle to the host (`candidate:...`). */
    data class LocalCandidate(val candidate: String) : TransportEvent

    /** Local gathering finished: no more candidates. */
    data object GatheringComplete : TransportEvent

    data class Link(val state: PeerLink) : TransportEvent

    /** A host-created data channel is open. */
    data class ChannelOpen(val channel: InputChannel) : TransportEvent

    /** A text message on the `control` channel. */
    data class ControlText(val text: String) : TransportEvent
}

/** One WebRTC peer connection answering the host's offer (receive-only video plus the host's data channels). */
interface StreamTransport {
    /** Events in order; nothing is lost between creation and the first collector. */
    val events: Flow<TransportEvent>

    /** Applies the host's [offerSdp] and returns the answer SDP (it may carry no candidates: they are trickled). */
    suspend fun acceptOffer(offerSdp: String): String

    /** A host candidate (`candidate:...`); held until the offer is applied. */
    fun addRemoteCandidate(candidate: String)

    /** Sends [text] on [channel]; false if the channel is not open. */
    fun send(channel: InputChannel, text: String): Boolean

    /** Plays (true) or silences the window's sound locally (WP-55); the host is told separately. */
    fun setAudioEnabled(enabled: Boolean) {}

    fun close()
}

/** What the viewer shows. */
data class StreamUiState(
    val phase: StreamPhase = StreamPhase.Preparing,
    /** The app the host will use, once it said (`POST /stream`). */
    val app: StreamApp? = null,
    /** The captured window, once the host found it. Its size is the picture's size until a frame arrives. */
    val window: StreamWindow? = null,
    val session: String? = null,
    /** Where "Bring to front" stands (input paused with `focus_lost`). */
    val front: FrontStatus = FrontStatus.IDLE,
    /** Which way clipboard text may cross, as the host advertised it (`capabilities.stream.clipboard`). */
    val clipboard: ClipboardMode = ClipboardMode.OFF,
    /** The window's sound (WP-55). */
    val audio: StreamAudio = StreamAudio(),
)

/** The window's sound (WP-55): whether the session carries any, and the user's mute. */
data class StreamAudio(
    /** The host sends this window's sound (now, or once unmuted or resumed). */
    val available: Boolean = false,
    /** The user muted it: the host stops capturing (`{"t":"mute","on":true}`). */
    val muted: Boolean = false,
)

class StreamSessionController(
    private val target: EntryTarget,
    private val api: StreamHostApi,
    private val eventsSource: StreamEventsSource,
    private val transportFactory: () -> StreamTransport,
    /** False when the phone has no MediaCodec H.264 decoder (libwebrtc 155 has no software one). */
    private val decoderAvailable: () -> Boolean,
    private val refusals: ActionRefusals,
    private val scope: CoroutineScope,
    private val clock: () -> Long,
    private val maxKbps: () -> Int = { DEFAULT_MAX_KBPS },
    private val config: LivenessConfig = LivenessConfig(),
) {
    /** Guards the fields below and the machine. Never held while calling out (sockets, transport, HTTP). */
    private val lock = Any()

    /** Serialises the handling of host events: the offer, then the candidates that follow it, in order. */
    private val eventLock = Mutex()

    private var machine = StreamStateMachine(config)

    private val _state = MutableStateFlow(StreamUiState())
    val state: StateFlow<StreamUiState> = _state.asStateFlow()

    private val _clipboard = MutableSharedFlow<ClipboardOutcome>(extraBufferCapacity = CLIPBOARD_BUFFER)

    /** How each clipboard action ended ([pasteToComputer], [copyFromComputer]). */
    val clipboard: SharedFlow<ClipboardOutcome> = _clipboard.asSharedFlow()

    private var session: String? = null
    private var app: StreamApp? = null
    private var window: StreamWindow? = null
    private var request: StreamRequest? = null
    private var clipboardMode = ClipboardMode.OFF
    private var audioAvailable = false
    private var muted = false
    private val pendingClipboard = HashMap<ClipboardOp, Job>()
    private var connection: StreamEventsConnection? = null
    private var transport: StreamTransport? = null
    private var answerSent = false
    private var reconnecting = false
    private var terminalHandled = false
    private val queuedLocalCandidates = ArrayList<String>()
    private val queuedRemoteCandidates = ArrayList<String>()
    private val earlyEvents = ArrayList<ServerEvent>()

    private var runJob: Job? = null
    private var eventsJob: Job? = null
    private var transportJob: Job? = null
    private var tickJob: Job? = null
    private var relayAnswer: CompletableDeferred<Boolean>? = null

    /**
     * Begins: asks the host what it can do, warns about a relayed path, then requests the stream
     * with [viewport] (the video area in physical pixels). Does nothing if already begun.
     */
    fun start(viewport: Viewport) {
        synchronized(lock) {
            if (runJob != null) return
            runJob = scope.launch { prepareAndRequest(viewport) }
        }
    }

    /** The user's answer to the relay warning (`Continue` or `Cancel`). */
    fun answerRelayWarning(continueStreaming: Boolean) {
        relayAnswer?.complete(continueStreaming)
    }

    /** Tries again after an attempt that ended in failure (a new request, a new session). */
    fun retry(viewport: Viewport) {
        teardown()
        synchronized(lock) {
            machine = StreamStateMachine(config)
            session = null
            app = null
            window = null
            request = null
            answerSent = false
            reconnecting = false
            terminalHandled = false
            queuedLocalCandidates.clear()
            queuedRemoteCandidates.clear()
            earlyEvents.clear()
            runJob = null
            publish()
        }
        start(viewport)
    }

    // Input -------------------------------------------------------------------------------------

    /** Sends [message] to the host on its channel; false when nothing could be sent (not streaming, channel closed). */
    fun send(message: InputMessage): Boolean {
        val t = synchronized(lock) { if (machine.phase == StreamPhase.Streaming) transport else null } ?: return false
        return t.send(StreamWire.channelFor(message), StreamWire.encode(message))
    }

    /**
     * "Bring to front": asks the host to bring the window forward (`{"t":"front"}` on `control`)
     * while input is paused with `focus_lost`; the answer shows in [StreamUiState.front]. False
     * when nothing was sent (not paused for that, a request already waiting, channel closed).
     */
    fun bringToFront(): Boolean {
        var sent = false
        advance { machine.onFrontRequested(clock()).also { sent = it.isNotEmpty() } }
        return sent
    }

    // Sound (WP-55) ---------------------------------------------------------------------------------

    /**
     * Mutes or unmutes the window's sound: silences it here at once and tells the host
     * (`{"t":"mute","on":..}`), which stops capturing while muted. The choice holds for the rest
     * of this screen, across reconnects.
     */
    fun setMuted(mute: Boolean) {
        val t = synchronized(lock) {
            muted = mute
            publish()
            transport
        }
        t?.setAudioEnabled(!mute)
        sendControl(ControlMessage.Mute(mute))
    }

    /** The host's `audio` report; a host that starts unmuted after a reconnect is muted again. */
    private fun onAudioMessage(message: ControlMessage) {
        if (message !is ControlMessage.Audio) return
        val remute = synchronized(lock) {
            audioAvailable = AudioReason.hasSound(message)
            publish()
            muted && message.on
        }
        if (remute) sendControl(ControlMessage.Mute(true))
    }

    // Clipboard (WP-56) -----------------------------------------------------------------------------

    /**
     * "Paste to computer": puts [text] (the phone's clipboard) on the computer's clipboard. Only
     * when the host advertised [ClipboardMode.canPasteToComputer], and text that is empty or over 64
     * KiB is not sent. The outcome arrives on [clipboard]: [ClipboardOutcome.Pasted], a refusal, or
     * no answer within [CLIPBOARD_TIMEOUT_MS]. Never logs [text].
     */
    fun pasteToComputer(text: String) {
        val refusal = when {
            !clipboardMode.canPasteToComputer -> ClipboardRefusal.DISABLED
            else -> clipboardSendRefusal(text)
        }
        if (refusal != null) return reportClipboard(ClipboardOutcome.Refused(ClipboardOp.SET, refusal))
        requestClipboard(ClipboardOp.SET, InputMessage.Clipboard(text))
    }

    /**
     * "Copy from computer": asks for the computer's clipboard text. Only when the host advertised
     * [ClipboardMode.canCopyFromComputer]. The text arrives on [clipboard] as
     * [ClipboardOutcome.Copied]; the caller puts it on the phone's clipboard. Text the host sends
     * without being asked is dropped.
     */
    fun copyFromComputer() {
        if (!clipboardMode.canCopyFromComputer) {
            return reportClipboard(ClipboardOutcome.Refused(ClipboardOp.GET, ClipboardRefusal.DISABLED))
        }
        requestClipboard(ClipboardOp.GET, InputMessage.ClipboardGet)
    }

    private fun requestClipboard(op: ClipboardOp, message: InputMessage) {
        val timeout = scope.launch(start = CoroutineStart.LAZY) {
            delay(CLIPBOARD_TIMEOUT_MS)
            if (settleClipboard(op)) reportClipboard(ClipboardOutcome.NoAnswer(op))
        }
        // Registered before the request goes out, so even an instant answer finds its wait. One
        // request of each kind at a time: a second tap while the first is in flight does nothing.
        val registered = synchronized(lock) {
            (op !in pendingClipboard).also { if (it) pendingClipboard[op] = timeout }
        }
        if (!registered) {
            timeout.cancel()
            return
        }
        if (!send(message)) {
            settleClipboard(op)
            return reportClipboard(ClipboardOutcome.Refused(op, ClipboardRefusal.UNAVAILABLE))
        }
        timeout.start()
    }

    /** Ends the wait for [op]; false when nothing was waiting for it. */
    private fun settleClipboard(op: ClipboardOp): Boolean {
        val job = synchronized(lock) { pendingClipboard.remove(op) } ?: return false
        job.cancel()
        return true
    }

    private fun reportClipboard(outcome: ClipboardOutcome) {
        _clipboard.tryEmit(outcome)
    }

    /** The host's answers; one that nobody asked for is dropped (the host never pushes its clipboard). */
    private fun onClipboardMessage(message: ControlMessage) {
        when (message) {
            is ControlMessage.ClipboardText ->
                if (settleClipboard(ClipboardOp.GET)) reportClipboard(ClipboardOutcome.Copied(message.s))

            is ControlMessage.ClipboardAck ->
                if (settleClipboard(ClipboardOp.SET)) reportClipboard(ClipboardOutcome.Pasted(message.bytes))

            is ControlMessage.ClipboardRefused ->
                if (settleClipboard(message.op)) reportClipboard(ClipboardOutcome.Refused(message.op, message.reason))

            else -> Unit
        }
    }

    /** Fit-to-phone: the viewer's area changed (rotation). */
    fun resize(width: Int, height: Int) {
        val (socket, id) = synchronized(lock) { connection to session }
        if (socket != null && id != null) socket.send(ClientEvent.StreamResize(id, width, height))
    }

    /**
     * The user tapped candidate [index] of the window list (`stream.state pick`): sends
     * `stream.pick` on `/v1/events`. False when nothing was sent (the host is not waiting for a
     * pick, no such candidate, no socket). The host answers with `window_found`, or with an `error`
     * and a pick that starts over; it ends the session itself 30 s after it asked.
     */
    fun pickWindow(index: Int): Boolean {
        val id = synchronized(lock) { session } ?: return false
        var allowed = false
        change { allowed = machine.onPickChosen(index, clock()) }
        if (!allowed) return false
        val socket = synchronized(lock) { connection }
        if (socket?.send(ClientEvent.StreamPick(id, index)) == true) return true
        // Nothing went out: the user can try again.
        advance { machine.onPickRefused(clock()) }
        return false
    }

    // Lifecycle of the screen ---------------------------------------------------------------

    /** The app left the foreground: the host stops encoding. */
    fun onBackground() = advance { machine.onBackground(clock()) }

    /** The app is back: the host resumes; the events socket is reopened if the system closed it. */
    fun onForeground() {
        advance { machine.onForeground(clock()) }
        val reopen = synchronized(lock) { session != null && connection == null && !machine.isTerminal }
        if (reopen) scope.launch { reopenEvents() }
    }

    /**
     * Ends the stream: tells the host to stop ([closeApp]: close the app too, which the host
     * honours only if it launched the app for this session) and leaves.
     */
    suspend fun end(closeApp: Boolean) {
        // A session the host already ended needs no DELETE.
        val id = synchronized(lock) { if (machine.isTerminal) null else session }
        runJob?.cancel()
        if (id != null) {
            withContext(NonCancellable) {
                withTimeoutOrNull(END_TIMEOUT_MS) { runCatching { api.endStream(id, closeApp) } }
            }
        }
        change { machine.onUserEnd() }
    }

    /** The user left while no session existed yet (relay warning, request in flight). */
    fun cancelBeforeStart() {
        runJob?.cancel()
        change { machine.onUserEnd() }
    }

    /**
     * The screen is gone for good (ViewModel cleared). A stream still running is ended on the
     * host so the computer does not keep encoding into the void; [endScope] outlives the screen.
     */
    fun release(endScope: CoroutineScope) {
        val id = synchronized(lock) { if (machine.isTerminal) null else session }
        if (id != null) {
            endScope.launch { withTimeoutOrNull(END_TIMEOUT_MS) { runCatching { api.endStream(id, false) } } }
        }
        teardown()
    }

    /**
     * The file as it is on the computer now when it differs from what the sheet showed when the
     * stream started (a save during the session), else null. Powers "Fetch the saved file".
     */
    suspend fun changedFile(): Entry? {
        val before = target.entry
        val now = try {
            api.stat(target.shareId, target.path)
        } catch (_: GanderError) {
            return null
        }
        return now.takeIf { it.size != before.size || it.mtime != before.mtime }
    }

    // Start ---------------------------------------------------------------------------------------

    private suspend fun prepareAndRequest(viewport: Viewport) {
        change { machine.onPreparing() }
        val me = try {
            api.me()
        } catch (e: GanderError) {
            refusals.report(target, FileAction.STREAM, e)
            return change { machine.onFailed(e) }
        }
        val stream = me.capabilities.stream
        synchronized(lock) { clipboardMode = stream.clipboard }
        if (!stream.enabled) {
            val error = GanderError.Host.StreamUnavailable(ErrorCode.STREAM_UNAVAILABLE.status, "")
            refusals.report(target, FileAction.STREAM, error)
            return change { machine.onFailed(error) }
        }
        if (VideoCodec.H264 !in stream.codecs) {
            return change { machine.onUnsupported(StreamPhase.Unsupported.Why.HOST_DOES_NOT_STREAM_H264) }
        }
        if (!decoderAvailable()) {
            return change { machine.onUnsupported(StreamPhase.Unsupported.Why.PHONE_HAS_NO_H264_DECODER) }
        }
        val path = me.network.relayed
        if (path == RelayPath.DERP || path == RelayPath.PEER_RELAY) {
            val answer = CompletableDeferred<Boolean>().also { relayAnswer = it }
            change { machine.onConfirmRelay(me.network.adapter, path) }
            if (!answer.await()) return cancelBeforeStart()
        }
        requestStream(viewport)
    }

    private suspend fun requestStream(viewport: Viewport) {
        change { machine.onStarting() }
        val req = StreamRequest(
            share = target.shareId,
            path = target.path,
            app = null,
            viewport = viewport,
            prefer = StreamPreference(VideoCodec.H264, maxKbps()),
        )
        // Signalling first: the host sends its offer to the sockets that are open when the session starts.
        val socket = eventsSource.open()
        synchronized(lock) {
            request = req
            connection = socket
        }
        val hello = CompletableDeferred<Unit>()
        eventsJob = scope.launch { collectEvents(socket, hello) }
        try {
            withTimeout(HELLO_TIMEOUT_MS) { hello.await() }
        } catch (e: GanderError) {
            return change { machine.onFailed(e) }
        } catch (_: TimeoutCancellationException) {
            return change { machine.onFailed(GanderError.Timeout(SocketTimeoutException("no hello from the host"))) }
        }
        val created = try {
            api.startStream(req)
        } catch (e: GanderError) {
            refusals.report(target, FileAction.STREAM, e)
            return change { machine.onFailed(e) }
        }
        eventLock.withLock {
            synchronized(lock) {
                session = created.session
                app = created.app
            }
            advance { machine.onHostState(created.state, null, null, clock()) }
            startTicker()
            // Events that overtook the answer to `POST /stream`.
            val early = synchronized(lock) { ArrayList(earlyEvents).also { earlyEvents.clear() } }
            early.forEach { handle(it) }
        }
    }

    private suspend fun collectEvents(socket: StreamEventsConnection, hello: CompletableDeferred<Unit>) {
        try {
            socket.incoming.collect { event ->
                if (event is ServerEvent.Hello) {
                    hello.complete(Unit)
                } else {
                    eventLock.withLock { handle(event) }
                }
            }
            // The host closed the socket. Before the hello that is a failure; after it, the session goes on.
            val closed = GanderError.Network(IOException("events socket closed"))
            if (!hello.completeExceptionally(closed)) socketLost(socket, null)
        } catch (e: GanderError) {
            if (!hello.completeExceptionally(e)) socketLost(socket, e)
        }
    }

    private fun socketLost(socket: StreamEventsConnection, error: GanderError?) {
        val live = synchronized(lock) {
            if (connection === socket) connection = null
            session != null && !machine.isTerminal
        }
        if (!live) return
        if (error is GanderError.Host.TokenRevoked) {
            change { machine.onGiveUp(EndCause.REVOKED) }
            return
        }
        // The system may have closed it in the background: reopen after a moment (and on return).
        scope.launch {
            delay(REOPEN_DELAY_MS)
            reopenEvents()
        }
    }

    private suspend fun reopenEvents(attempt: Int = 1) {
        val id = synchronized(lock) { if (connection != null || machine.isTerminal) null else session } ?: return
        val socket = eventsSource.open()
        synchronized(lock) { connection = socket }
        val hello = CompletableDeferred<Unit>()
        eventsJob = scope.launch { collectEvents(socket, hello) }
        val ok = try {
            withTimeout(HELLO_TIMEOUT_MS) { hello.await() }
            true
        } catch (_: GanderError) {
            false
        } catch (_: TimeoutCancellationException) {
            false
        }
        if (ok) {
            // Say again what this phone is doing, so the host does not have to guess.
            if (synchronized(lock) { machine.phase } == StreamPhase.Backgrounded) socket.send(ClientEvent.StreamPause(id))
        } else {
            synchronized(lock) { if (connection === socket) connection = null }
            runCatching { socket.close() }
            if (attempt < MAX_REOPEN_ATTEMPTS) {
                delay(REOPEN_DELAY_MS * (attempt + 1))
                reopenEvents(attempt + 1)
            }
        }
    }

    // Events from the host ------------------------------------------------------------------------

    private suspend fun handle(event: ServerEvent) {
        val eventSession = when (event) {
            is ServerEvent.StreamStateChanged -> event.session
            is ServerEvent.StreamOffer -> event.session
            is ServerEvent.StreamIce -> event.session
            is ServerEvent.Error -> event.session
            else -> return
        }
        val id = synchronized(lock) { session }
        if (id == null) {
            // Events can overtake the answer to `POST /stream`: keep them until the session id is known.
            synchronized(lock) { if (earlyEvents.size < MAX_EARLY_EVENTS) earlyEvents += event }
            return
        }
        if (eventSession != null && eventSession != id) return
        when (event) {
            is ServerEvent.StreamStateChanged -> {
                if (event.window != null) synchronized(lock) { window = event.window }
                advance { machine.onHostState(event.state, event.reason, event.candidates, clock()) }
            }

            is ServerEvent.StreamOffer -> acceptOffer(id, event.sdp)

            is ServerEvent.StreamIce -> {
                val candidate = IceCandidates.fromWire(event.candidate) ?: return
                val t = synchronized(lock) { transport.also { if (it == null) queuedRemoteCandidates += candidate } }
                t?.addRemoteCandidate(candidate)
            }

            is ServerEvent.Error -> when (event.code) {
                ErrorCode.HOST_LOCKED -> synchronized(lock) { machine.onHostLocked() }
                // The picked window is gone (the pick starts over), or the host gave up waiting for a
                // window: `stream.state ended` follows and the end is "no window".
                ErrorCode.APP_UNAVAILABLE -> advance { machine.onHostAppUnavailable(clock()) }
                // A pick the host could not use (an index out of its list): choose again.
                ErrorCode.BAD_REQUEST -> advance { machine.onPickRefused(clock()) }
                // The host no longer knows the session.
                ErrorCode.NOT_FOUND -> change { machine.onGiveUp(EndCause.ERROR) }
                else -> Unit
            }
        }
    }

    private suspend fun acceptOffer(id: String, sdp: String) {
        val t: StreamTransport
        synchronized(lock) {
            if (machine.isTerminal) return
            val fresh = transport == null
            t = transport ?: transportFactory().also { transport = it }
            if (fresh) transportJob = scope.launch { collectTransport(t) }
            answerSent = false
        }
        if (synchronized(lock) { muted }) t.setAudioEnabled(false)
        advance { machine.onOffer(clock()) }
        val answer = try {
            t.acceptOffer(sdp)
        } catch (e: CancellationException) {
            throw e
        } catch (_: Exception) {
            return change { machine.onGiveUp(EndCause.NO_VIDEO_LINK) }
        }
        val socket: StreamEventsConnection?
        val local: List<String>
        val remote: List<String>
        synchronized(lock) {
            socket = connection
            local = ArrayList(queuedLocalCandidates)
            remote = ArrayList(queuedRemoteCandidates)
            queuedLocalCandidates.clear()
            queuedRemoteCandidates.clear()
            // From here candidates found by the transport are sent at once, after the answer.
            answerSent = true
        }
        socket?.send(ClientEvent.StreamAnswer(id, answer))
        local.forEach { socket?.send(ClientEvent.StreamIce(id, it)) }
        remote.forEach { t.addRemoteCandidate(it) }
    }

    private suspend fun collectTransport(t: StreamTransport) {
        t.events.collect { ev ->
            when (ev) {
                is TransportEvent.LocalCandidate -> sendLocalCandidate(IceCandidates.toWire(ev.candidate))
                TransportEvent.GatheringComplete -> sendLocalCandidate(IceCandidates.END_OF_CANDIDATES)
                is TransportEvent.Link -> advance { machine.onPeer(ev.state, clock()) }
                is TransportEvent.ChannelOpen ->
                    if (ev.channel == InputChannel.CONTROL) change { machine.onControlOpen(clock()) }

                is TransportEvent.ControlText -> {
                    val message = StreamWire.parseControl(ev.text)
                    if (message != null) {
                        advance { machine.onControlMessage(message, clock()) }
                        onClipboardMessage(message)
                        onAudioMessage(message)
                    }
                }
            }
        }
    }

    private fun sendLocalCandidate(candidate: String) {
        val socket: StreamEventsConnection?
        val id: String?
        val sendNow: Boolean
        synchronized(lock) {
            if (!answerSent) queuedLocalCandidates += candidate
            socket = connection
            id = session
            sendNow = answerSent
        }
        if (sendNow && id != null) socket?.send(ClientEvent.StreamIce(id, candidate))
    }

    // Machine plumbing ---------------------------------------------------------------------------------

    /** Runs a machine step under the lock, publishes the new state and performs the step's effects. */
    private fun advance(step: () -> List<StreamEffect>) {
        val effects: List<StreamEffect>
        val finished: Boolean
        synchronized(lock) {
            effects = step()
            publish()
            finished = machine.isTerminal && !terminalHandled
            if (finished) terminalHandled = true
        }
        effects.forEach(::perform)
        if (finished) onTerminal()
    }

    /** [advance] for the machine's steps that have no effects. */
    private fun change(step: () -> Unit) = advance {
        step()
        emptyList()
    }

    private fun publish() {
        _state.value = StreamUiState(
            phase = machine.phase,
            app = app,
            window = window,
            session = session,
            front = machine.front,
            clipboard = clipboardMode,
            audio = StreamAudio(available = audioAvailable, muted = muted),
        )
    }

    private fun perform(effect: StreamEffect) {
        when (effect) {
            is StreamEffect.SendPing -> sendControl(ControlMessage.Ping(effect.seq))
            StreamEffect.SendPause -> {
                sendControl(ControlMessage.Pause)
                signal { ClientEvent.StreamPause(it) }
            }

            StreamEffect.SendResume -> {
                sendControl(ControlMessage.Resume)
                signal { ClientEvent.StreamResume(it) }
            }

            StreamEffect.RequestKeyframe -> sendControl(ControlMessage.Keyframe)
            StreamEffect.SendFront -> sendControl(ControlMessage.Front)
            StreamEffect.Reconnect -> scope.launch { reconnect() }
        }
    }

    private fun sendControl(message: ControlMessage) {
        val t = synchronized(lock) { transport } ?: return
        t.send(InputChannel.CONTROL, StreamWire.encode(message))
    }

    private fun signal(event: (String) -> ClientEvent) {
        val (socket, id) = synchronized(lock) { connection to session }
        if (socket != null && id != null) socket.send(event(id))
    }

    private fun startTicker() {
        tickJob?.cancel()
        tickJob = scope.launch {
            while (isActive) {
                delay(TICK_MS)
                advance { machine.tick(clock()) }
            }
        }
    }

    /** A new peer connection for the same session after the old one failed (`POST /stream` with `resume`). */
    private suspend fun reconnect() {
        val req: StreamRequest
        synchronized(lock) {
            val base = request
            if (reconnecting || machine.isTerminal || base == null) return
            reconnecting = true
            req = base.copy(resume = session)
            val old = transport
            transport = null
            answerSent = false
            queuedLocalCandidates.clear()
            transportJob?.cancel()
            old?.let { runCatching { it.close() } }
        }
        change { machine.onTransportReplaced(clock()) }
        try {
            val created = api.startStream(req)
            synchronized(lock) { session = created.session }
        } catch (_: GanderError) {
            change { machine.onGiveUp(EndCause.CONNECTION_LOST) }
        } finally {
            synchronized(lock) { reconnecting = false }
        }
    }

    private fun onTerminal() {
        val cause: EndCause?
        val id: String?
        synchronized(lock) {
            cause = (machine.phase as? StreamPhase.Ended)?.cause
            id = session
        }
        // A host-side end needs no DELETE; a local one (link lost, no link) does, best effort.
        if (id != null && (cause == EndCause.CONNECTION_LOST || cause == EndCause.NO_VIDEO_LINK)) {
            scope.launch { withTimeoutOrNull(END_TIMEOUT_MS) { runCatching { api.endStream(id, false) } } }
        }
        teardown(cancelRun = false)
    }

    private fun teardown(cancelRun: Boolean = true) {
        val t: StreamTransport?
        val c: StreamEventsConnection?
        synchronized(lock) {
            t = transport
            c = connection
            transport = null
            connection = null
        }
        tickJob?.cancel()
        transportJob?.cancel()
        eventsJob?.cancel()
        if (cancelRun) runJob?.cancel()
        // Nobody answers a clipboard request on a connection that is gone.
        synchronized(lock) { pendingClipboard.values.toList().also { pendingClipboard.clear() } }.forEach { it.cancel() }
        runCatching { t?.close() }
        runCatching { c?.close() }
    }

    companion object {
        const val DEFAULT_MAX_KBPS = 8_000
        const val TICK_MS = 250L
        const val HELLO_TIMEOUT_MS = 10_000L
        const val END_TIMEOUT_MS = 5_000L
        const val REOPEN_DELAY_MS = 1_000L
        private const val MAX_REOPEN_ATTEMPTS = 5
        private const val MAX_EARLY_EVENTS = 64
        private const val CLIPBOARD_BUFFER = 8
    }
}
