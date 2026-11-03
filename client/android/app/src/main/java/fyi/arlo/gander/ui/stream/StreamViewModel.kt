package fyi.arlo.gander.ui.stream

import android.os.SystemClock
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import fyi.arlo.gander.core.data.actions.ActionRefusals
import fyi.arlo.gander.core.data.browse.EntryTarget
import fyi.arlo.gander.core.data.hosts.HostRepository
import fyi.arlo.gander.core.data.stream.ClipboardOutcome
import fyi.arlo.gander.core.data.stream.ControlMode
import fyi.arlo.gander.core.data.stream.EndCause
import fyi.arlo.gander.core.data.stream.FrameSize
import fyi.arlo.gander.core.data.stream.GanderStreamEvents
import fyi.arlo.gander.core.data.stream.GanderStreamHostApi
import fyi.arlo.gander.core.data.stream.StreamPhase
import fyi.arlo.gander.core.data.stream.StreamPrefsRepository
import fyi.arlo.gander.core.data.stream.StreamSessionController
import fyi.arlo.gander.core.data.stream.StreamTransport
import fyi.arlo.gander.core.data.stream.StreamUiState
import fyi.arlo.gander.core.data.stream.StreamVideoOutput
import fyi.arlo.gander.core.net.GanderHttpClients
import fyi.arlo.gander.core.net.protocol.Entry
import fyi.arlo.gander.core.net.protocol.InputMessage
import fyi.arlo.gander.core.net.protocol.KeyModifier
import fyi.arlo.gander.core.net.protocol.Viewport
import fyi.arlo.gander.ui.preview.ZoomMath
import fyi.arlo.gander.ui.preview.ZoomTransform
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.filterNotNull
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/** What the end dialog needs once a stream is over. [changedFile] is the file as it is now if it changed during the session. */
data class EndSummary(val cause: EndCause, val changedFile: Entry?)

/** Everything the stream screen draws. */
data class StreamScreenState(
    val session: StreamUiState = StreamUiState(),
    /** The file's name. */
    val title: String = "",
    val hostName: String = "",
    /** The computer was forgotten while the screen was open (or never paired). */
    val hostMissing: Boolean = false,
    val mode: ControlMode = ControlMode.DEFAULT,
    /** The decoded picture's size, 0 x 0 before the first frame. */
    val frame: FrameSize = FrameSize(0, 0),
    val zoom: ZoomTransform = ZoomTransform(),
    val keyboard: KeyboardState = KeyboardState(),
    val summary: EndSummary? = null,
) {
    /** The picture's size for the geometry: the decoded frame, else the host's window until a frame arrives. */
    val pictureWidth: Int get() = if (frame.known) frame.width else session.window?.width ?: 0
    val pictureHeight: Int get() = if (frame.known) frame.height else session.window?.height ?: 0

    val isZoomed: Boolean get() = zoom.isZoomed
}

/**
 * State and actions of the stream viewer for one file. Survives rotation; owns the video output
 * (EGL context and frame fan-out) and the [StreamSessionController], and ends a stream that is
 * still running when it is cleared.
 */
class StreamViewModel(
    val target: EntryTarget,
    private val hosts: HostRepository,
    private val clientsFor: suspend (hostId: String) -> GanderHttpClients,
    private val prefs: StreamPrefsRepository,
    private val refusals: ActionRefusals,
    val output: StreamVideoOutput,
    private val transportFactory: (StreamVideoOutput) -> StreamTransport,
    private val decoderAvailable: () -> Boolean,
    /** Outlives this screen: the DELETE of a stream that is left is sent from here. */
    private val endScope: CoroutineScope,
    /** The phone's clipboard, for "Paste to computer" and "Copy from computer" (WP-56). */
    private val phoneClipboard: PhoneClipboard,
) : ViewModel(), StreamControls {

    private val _state = MutableStateFlow(StreamScreenState(title = target.entry.name))
    val state: StateFlow<StreamScreenState> = _state.asStateFlow()

    /** The session's own coroutines (signalling, pings, transport); cancelled with the screen. */
    private val sessionScope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

    private val controller = MutableStateFlow<StreamSessionController?>(null)

    private val _clipboardNotices = MutableSharedFlow<ClipboardNotice>(extraBufferCapacity = 4)

    /** One short message per clipboard action ("Copied from the computer"), for the screen to show. */
    val clipboardNotices: SharedFlow<ClipboardNotice> = _clipboardNotices.asSharedFlow()
    private var requestedViewport: Viewport? = null
    private var viewWidth = 0f
    private var viewHeight = 0f

    init {
        viewModelScope.launch {
            val host = hosts.get(target.hostId)
            _state.update { it.copy(hostName = host?.name.orEmpty(), hostMissing = host == null) }
            if (host == null) return@launch
            val clients = clientsFor(host.id)
            val c = StreamSessionController(
                target = target,
                api = GanderStreamHostApi(clients),
                eventsSource = GanderStreamEvents(clients),
                transportFactory = { transportFactory(output) },
                decoderAvailable = decoderAvailable,
                refusals = refusals,
                scope = sessionScope,
                clock = SystemClock::elapsedRealtime,
            )
            controller.value = c
            launch {
                c.clipboard.collect { outcome ->
                    // The computer's text goes to the phone's clipboard only because the user asked for it.
                    if (outcome is ClipboardOutcome.Copied) phoneClipboard.writeText(outcome.text)
                    _clipboardNotices.tryEmit(ClipboardNotice.of(outcome))
                }
            }
            c.state.collect { session ->
                _state.update { it.copy(session = session) }
                val phase = session.phase
                if (phase is StreamPhase.Ended && _state.value.summary == null) summarise(c, phase.cause, session.session != null)
            }
        }
        viewModelScope.launch { prefs.observeMode(target.hostId).collect { mode -> _state.update { it.copy(mode = mode) } } }
        viewModelScope.launch { output.frameSize.collect { size -> _state.update { it.copy(frame = size) } } }
    }

    private suspend fun summarise(c: StreamSessionController, cause: EndCause, hadSession: Boolean) {
        val changed = if (hadSession) c.changedFile() else null
        _state.update { it.copy(summary = EndSummary(cause, changed)) }
    }

    // Start and end -----------------------------------------------------------------------------

    /** True once the stream was requested (the first measured size starts it). */
    val hasStarted: Boolean get() = requestedViewport != null

    /**
     * The video area is [width] x [height] physical pixels. The first size starts the stream (it
     * is the viewport the host fits the window to); a later size that differs a lot (rotation)
     * asks the host to fit again.
     */
    override fun onViewSize(width: Int, height: Int, dpr: Double) {
        if (width <= 0 || height <= 0) return
        viewWidth = width.toFloat()
        viewHeight = height.toFloat()
        _state.update { it.copy(zoom = ZoomMath.clamp(it.zoom, viewWidth, viewHeight)) }
        val next = Viewport(width, height, dpr)
        val before = requestedViewport
        requestedViewport = next
        if (before == null) {
            viewModelScope.launch { controller.filterNotNull().first().start(next) }
        } else if (next.differsEnough(before)) {
            controller.value?.resize(width, height)
        }
    }

    /** "Try again" after a failure. */
    override fun retry() {
        val viewport = requestedViewport ?: return
        _state.update { it.copy(summary = null, zoom = ZoomTransform()) }
        viewModelScope.launch { controller.filterNotNull().first().retry(viewport) }
    }

    override fun answerRelayWarning(continueStreaming: Boolean) {
        controller.value?.answerRelayWarning(continueStreaming)
    }

    /** Ends the stream; [closeApp] asks the host to close the app too (it honours that only for an app it launched). */
    override fun end(closeApp: Boolean) {
        val c = controller.value ?: return
        endScope.launch { c.end(closeApp) }
    }

    /** The user chose window [index] of the pick list; the host answers with the window or a refusal. */
    override fun pickWindow(index: Int) {
        controller.value?.pickWindow(index)
    }

    /** "Bring to front": asks the host to bring the window forward; the answer shows in the banner. */
    override fun bringToFront() {
        controller.value?.bringToFront()
    }

    /** The session bar's mute button (WP-55). */
    override fun setMuted(muted: Boolean) {
        controller.value?.setMuted(muted)
    }

        /** Leaves before a stream exists (relay warning, request in flight, failure). */
    override fun cancelBeforeStart() {
        controller.value?.cancelBeforeStart()
    }

    fun onBackground() {
        controller.value?.onBackground()
    }

    fun onForeground() {
        controller.value?.onForeground()
    }

    // Mode, zoom, keys ---------------------------------------------------------------------------

    override fun setMode(mode: ControlMode) {
        viewModelScope.launch { prefs.setMode(target.hostId, mode) }
    }

    /** One local pinch or pan step; never reaches the host. */
    override fun zoomPan(step: GestureOutput.ZoomPan) {
        if (viewWidth <= 0f || viewHeight <= 0f) return
        _state.update {
            it.copy(
                zoom = ZoomMath.applyGesture(
                    it.zoom, step.zoomChange, step.panX, step.panY, step.centroidX, step.centroidY,
                    viewWidth, viewHeight, MAX_ZOOM,
                ),
            )
        }
    }

    override fun resetZoom() {
        _state.update { it.copy(zoom = ZoomTransform()) }
    }

    /** Sends a message from a gesture; false when the stream cannot take input now. */
    override fun send(message: InputMessage): Boolean = controller.value?.send(message) ?: false

    override fun toggleSticky(modifier: KeyModifier) {
        _state.update { it.copy(keyboard = KeyboardBar.toggleSticky(it.keyboard, modifier)) }
    }

    override fun toggleFunctionRow() {
        _state.update { it.copy(keyboard = KeyboardBar.toggleFunctionRow(it.keyboard)) }
    }

    /** A key of the keyboard bar; the sticky modifiers are spent. */
    override fun pressKey(code: String, extra: Set<KeyModifier>) {
        val press = KeyboardBar.press(_state.value.keyboard, code, extra)
        _state.update { it.copy(keyboard = press.state) }
        press.messages.forEach(::send)
    }

    /** The keyboard bar's "Paste to computer": the phone's clipboard text goes to the computer's clipboard. */
    override fun pasteToComputer() {
        val text = phoneClipboard.readText()
        if (text == null) {
            _clipboardNotices.tryEmit(ClipboardNotice.PHONE_CLIPBOARD_EMPTY)
            return
        }
        controller.value?.pasteToComputer(text)
    }

    /** The keyboard bar's "Copy from computer": the computer's clipboard text goes to the phone's clipboard. */
    override fun copyFromComputer() {
        controller.value?.copyFromComputer()
    }

    /** Text from the local text box or the phone keyboard. */
    override fun typed(text: String) {
        val press = KeyboardBar.typed(_state.value.keyboard, text)
        _state.update { it.copy(keyboard = press.state) }
        press.messages.forEach(::send)
    }

    override fun onCleared() {
        controller.value?.release(endScope)
        sessionScope.cancel()
        output.release()
    }

    companion object {
        /** The pinch zoom ceiling; a phone showing a desktop window needs more than a photo viewer's 5x. */
        const val MAX_ZOOM = 6f
    }
}
