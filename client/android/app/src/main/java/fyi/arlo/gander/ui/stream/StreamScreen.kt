package fyi.arlo.gander.ui.stream

import android.widget.Toast
import androidx.activity.compose.BackHandler
import androidx.activity.compose.LocalActivity
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.key
import androidx.compose.ui.input.key.onPreviewKeyEvent
import androidx.compose.ui.input.key.type
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.platform.LocalViewConfiguration
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import fyi.arlo.gander.R
import fyi.arlo.gander.ui.components.ButtonKind
import fyi.arlo.gander.ui.components.GanderButton
import fyi.arlo.gander.ui.components.GanderProgress
import fyi.arlo.gander.ui.shell.isExpandedWidth
import fyi.arlo.gander.ui.theme.GanderIcons
import fyi.arlo.gander.ui.theme.GanderTheme
import fyi.arlo.gander.ui.theme.Immersive
import fyi.arlo.gander.core.data.actions.ActionReason
import fyi.arlo.gander.core.data.actions.FileAction
import fyi.arlo.gander.core.data.browse.EntryTarget
import fyi.arlo.gander.core.data.stream.ControlMode
import fyi.arlo.gander.core.data.stream.EndCause
import fyi.arlo.gander.core.data.stream.StreamPhase
import fyi.arlo.gander.core.data.stream.StreamVideoOutput
import fyi.arlo.gander.core.data.stream.banner
import fyi.arlo.gander.core.net.protocol.Entry
import fyi.arlo.gander.core.net.protocol.InputMessage
import fyi.arlo.gander.core.net.protocol.KeyModifier
import fyi.arlo.gander.core.net.protocol.MouseButton
import fyi.arlo.gander.ui.filesheet.text
import kotlinx.coroutines.delay

/** What the stream screen asks of its state holder. The ViewModel implements it; Compose tests use a fake. */
interface StreamControls {
    /** The video area is this many physical pixels; the first call starts the stream. */
    fun onViewSize(width: Int, height: Int, dpr: Double)

    fun setMode(mode: ControlMode)

    fun zoomPan(step: GestureOutput.ZoomPan)

    fun resetZoom()

    /** Sends a gesture's message to the host; false when the stream cannot take input now. */
    fun send(message: InputMessage): Boolean

    fun toggleSticky(modifier: KeyModifier)

    fun toggleFunctionRow()

    fun pressKey(code: String, extra: Set<KeyModifier>)

    fun typed(text: String)

    /** Stops the stream; [closeApp] asks the computer to close the app too. */
    fun end(closeApp: Boolean)

    /** Leaves before a session exists. */
    fun cancelBeforeStart()

    fun answerRelayWarning(continueStreaming: Boolean)

    /** Mutes or unmutes the window's sound (WP-55). */
    fun setMuted(muted: Boolean) {}

    fun retry()

    /** The user tapped window [index] of the pick list: sends `stream.pick`. */
    fun pickWindow(index: Int)

    /** "Bring to front" on a `focus_lost` pause: sends `{"t":"front"}`. */
    fun bringToFront()

    /** The keyboard bar's "Paste to computer": the phone's clipboard text goes to the computer's clipboard. */
    fun pasteToComputer() {}

    /** The keyboard bar's "Copy from computer": the computer's clipboard text goes to the phone's clipboard. */
    fun copyFromComputer() {}
}

/** Whether a stream session exists on the host and is not over: Back asks before ending it. */
fun StreamPhase.isLive(): Boolean = when (this) {
    StreamPhase.Requested, StreamPhase.Approving, StreamPhase.Launching, is StreamPhase.PickWindow,
    StreamPhase.WindowFound, StreamPhase.Connecting, StreamPhase.Streaming, is StreamPhase.Paused,
    StreamPhase.Backgrounded, StreamPhase.Reconnecting,
    -> true

    StreamPhase.Preparing, is StreamPhase.ConfirmRelay, StreamPhase.Starting, is StreamPhase.Ended,
    is StreamPhase.Failed, is StreamPhase.Unsupported,
    -> false
}

/** The stream destination: the state holder is the ViewModel and the picture is drawn into `output`. */
@Composable
fun StreamScreen(
    viewModel: StreamViewModel,
    onBack: () -> Unit,
    onFetchSaved: (EntryTarget) -> Unit,
    modifier: Modifier = Modifier,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()

    // Client spec: on onStop send stream.pause, on return stream.resume. A rotation stops and
    // restarts the activity but the user never left.
    val lifecycleOwner = LocalLifecycleOwner.current
    val activity = LocalActivity.current
    DisposableEffect(lifecycleOwner, viewModel) {
        val observer = LifecycleEventObserver { _, event ->
            when (event) {
                Lifecycle.Event.ON_STOP -> if (activity?.isChangingConfigurations != true) viewModel.onBackground()
                Lifecycle.Event.ON_START -> viewModel.onForeground()
                else -> Unit
            }
        }
        lifecycleOwner.lifecycle.addObserver(observer)
        onDispose { lifecycleOwner.lifecycle.removeObserver(observer) }
    }

    // What a clipboard action did, in one short message (never the text).
    val context = LocalContext.current
    LaunchedEffect(viewModel) {
        viewModel.clipboardNotices.collect { Toast.makeText(context, it.message, Toast.LENGTH_SHORT).show() }
    }

    // The picture is the point of the screen: keep it lit.
    val view = LocalView.current
    DisposableEffect(view) {
        view.keepScreenOn = true
        onDispose { view.keepScreenOn = false }
    }

    StreamScreenContent(
        state = state,
        controls = viewModel,
        output = viewModel.output,
        onBack = onBack,
        onFetchSaved = { entry -> onFetchSaved(viewModel.target.copy(entry = entry)) },
        modifier = modifier,
    )
}

/**
 * The screen without its ViewModel: [state] in, [controls] out. [output] is where decoded frames
 * go; null draws no picture (the Compose tests of the chrome run without libwebrtc's natives).
 */
@Composable
fun StreamScreenContent(
    state: StreamScreenState,
    controls: StreamControls,
    output: StreamVideoOutput?,
    onBack: () -> Unit,
    onFetchSaved: (Entry) -> Unit,
    modifier: Modifier = Modifier,
) {
    val phase = state.session.phase
    val density = LocalDensity.current
    var viewSize by remember { mutableStateOf(IntSize.Zero) }
    var keyboardOpen by rememberSaveable { mutableStateOf(false) }
    var textBoxOpen by rememberSaveable { mutableStateOf(false) }
    var systemKeyboard by rememberSaveable { mutableStateOf(false) }
    var askEnd by rememberSaveable { mutableStateOf(false) }

    // The first measured size starts the stream; later sizes (rotation) wait for the layout to settle.
    LaunchedEffect(viewSize) {
        if (viewSize == IntSize.Zero) return@LaunchedEffect
        if (phase.isLive()) delay(RESIZE_SETTLE_MS)
        controls.onViewSize(viewSize.width, viewSize.height, density.density.toDouble())
    }

    // Leaving can be asked for twice (the end of the stream and the Back that caused it): once only.
    var leaving by remember { mutableStateOf(false) }
    val leave = {
        if (!leaving) {
            leaving = true
            onBack()
        }
    }
    // Before a session exists nothing needs ending on the host; the ViewModel cancels what is in flight.
    val leaveNow = {
        controls.cancelBeforeStart()
        leave()
    }

    val summary = state.summary
    LaunchedEffect(summary) {
        // The user's own leaving, with nothing saved meanwhile, needs no dialog.
        if (summary != null && summary.cause == EndCause.YOU_LEFT && summary.changedFile == null) leave()
    }

    // Back: ask before ending a live stream; otherwise just leave.
    BackHandler {
        if (phase.isLive()) askEnd = true else leaveNow()
    }

    Column(
        modifier = modifier.fillMaxSize().background(Immersive.StreamBackdrop).statusBarsPadding().navigationBarsPadding(),
    ) {
        StreamTopBar(
            title = state.title,
            subtitle = stringResource(R.string.stream_title_on_host, state.hostName),
            mode = state.mode,
            zoomed = state.isZoomed,
            keyboardOpen = keyboardOpen,
            onLeave = { if (phase.isLive()) askEnd = true else leaveNow() },
            onToggleMode = {
                controls.setMode(if (state.mode == ControlMode.DIRECT) ControlMode.TRACKPAD else ControlMode.DIRECT)
            },
            onResetZoom = controls::resetZoom,
            live = phase == StreamPhase.Streaming,
            audio = state.session.audio,
            onToggleMute = { controls.setMuted(!state.session.audio.muted) },
            onToggleKeyboard = {
                keyboardOpen = !keyboardOpen
                if (!keyboardOpen) {
                    textBoxOpen = false
                    systemKeyboard = false
                }
            },
        )
        Box(
            modifier = Modifier
                .weight(1f)
                .fillMaxWidth()
                .clipToBounds()
                .onSizeChanged { viewSize = it },
        ) {
            when (phase) {
                is StreamPhase.Failed, is StreamPhase.Unsupported -> FailurePanel(state, controls, leave)
                StreamPhase.Preparing -> Preparing(state.hostName)
                else -> StreamBody(state, controls, output, viewSize)
            }
            if (phase.isLive() || phase == StreamPhase.Starting) {
                phase.banner()?.let { banner ->
                    StateBanner(
                        banner = banner,
                        host = state.hostName,
                        app = state.session.app?.name,
                        modifier = Modifier.align(Alignment.TopCenter).padding(12.dp),
                        onPick = controls::pickWindow,
                        front = state.session.front,
                        onBringToFront = controls::bringToFront,
                    )
                }
            }
            Column(modifier = Modifier.align(Alignment.BottomCenter)) {
                if (textBoxOpen) {
                    LocalTextBox(onSend = controls::typed, onClose = { textBoxOpen = false })
                }
                AnimatedVisibility(visible = keyboardOpen) {
                    KeyboardBarView(
                        state = state.keyboard,
                        onToggleSticky = controls::toggleSticky,
                        onKey = controls::pressKey,
                        onToggleFunctionRow = controls::toggleFunctionRow,
                        onType = { textBoxOpen = !textBoxOpen; systemKeyboard = false },
                        onSystemKeyboard = { systemKeyboard = !systemKeyboard; textBoxOpen = false },
                        oneRow = isExpandedWidth(),
                        clipboard = state.session.clipboard,
                        onPasteToComputer = controls::pasteToComputer,
                        onCopyFromComputer = controls::copyFromComputer,
                    )
                }
            }
            if (systemKeyboard) LiveTypingField(
                onText = controls::typed,
                onKey = { code -> controls.pressKey(code, emptySet()) },
            )
        }
    }

    (phase as? StreamPhase.ConfirmRelay)?.let { relay ->
        RelayWarningDialog(
            host = state.hostName,
            adapter = relay.adapter,
            onContinue = { controls.answerRelayWarning(true) },
            onCancel = {
                controls.answerRelayWarning(false)
                leave()
            },
        )
    }
    if (askEnd && phase.isLive()) {
        EndStreamDialog(
            app = state.session.app?.name,
            host = state.hostName,
            onLeaveOpen = { askEnd = false; controls.end(closeApp = false) },
            onCloseApp = { askEnd = false; controls.end(closeApp = true) },
            onKeepStreaming = { askEnd = false },
        )
    }
    if (summary != null && !(summary.cause == EndCause.YOU_LEFT && summary.changedFile == null)) {
        StreamEndedDialog(
            cause = summary.cause,
            host = state.hostName,
            fileChanged = summary.changedFile != null,
            onFetchSaved = { summary.changedFile?.let(onFetchSaved) },
            onTryAgain = if (summary.cause.canRetry()) controls::retry else null,
            onDone = { leave() },
        )
    }
}

/** The picture with its gestures, the viewer's pointer and (as the host reports it) nothing else on top. */
@Composable
private fun StreamBody(
    state: StreamScreenState,
    controls: StreamControls,
    output: StreamVideoOutput?,
    viewSize: IntSize,
) {
    val viewport = StreamViewport(
        viewWidth = viewSize.width.toFloat(),
        viewHeight = viewSize.height.toFloat(),
        frameWidth = state.pictureWidth,
        frameHeight = state.pictureHeight,
        zoom = state.zoom,
    )
    val currentViewport by rememberUpdatedState(viewport)
    val slop = LocalViewConfiguration.current.touchSlop
    val config = remember(slop) { GestureConfig(touchSlop = slop) }
    val recognizer = remember(config) { StreamGestureRecognizer(config, { currentViewport }, state.mode) }
    var pointer by remember(recognizer) { mutableStateOf(recognizer.pointer) }
    val haptics = LocalHapticFeedback.current
    val currentControls by rememberUpdatedState(controls)

    val dispatch = rememberUpdatedState<(List<GestureOutput>) -> Unit> { outputs ->
        for (out in outputs) {
            when (out) {
                is GestureOutput.Send -> {
                    val message = out.message
                    // A long press fires on its own: let the finger feel it.
                    if (message is InputMessage.Down && message.b == MouseButton.RIGHT) {
                        haptics.performHapticFeedback(HapticFeedbackType.LongPress)
                    }
                    currentControls.send(message)
                }

                is GestureOutput.ZoomPan -> currentControls.zoomPan(out)
                is GestureOutput.Pointer -> pointer = out.position
            }
        }
    }

    // A change of mode in the middle of a touch must not leave a button down.
    LaunchedEffect(recognizer, state.mode) {
        dispatch.value(recognizer.switchMode(state.mode))
        pointer = recognizer.pointer
    }
    // Paused, backgrounded or ended: whatever touch is in progress is over.
    LaunchedEffect(recognizer, state.session.phase) {
        if (state.session.phase != StreamPhase.Streaming) dispatch.value(recognizer.onCancel())
    }

    Box(modifier = Modifier.fillMaxSize()) {
        if (output != null) StreamPicture(output, viewport)
        if (state.mode == ControlMode.TRACKPAD && state.session.phase == StreamPhase.Streaming) {
            PointerMarker(viewport, pointer)
        }
        Box(
            modifier = Modifier
                .fillMaxSize()
                .testTag(StreamTags.GESTURE_LAYER)
                .streamGestures(recognizer) { outputs -> dispatch.value(outputs) },
        )
    }
}

@Composable
private fun Preparing(host: String) {
    Column(
        modifier = Modifier.fillMaxSize(),
        verticalArrangement = Arrangement.spacedBy(12.dp, Alignment.CenterVertically),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        GanderProgress(fraction = null, modifier = Modifier.padding(horizontal = 64.dp))
        Text(stringResource(R.string.stream_preparing, host), color = Immersive.Content)
    }
}

/** Why the stream cannot start, in the file sheet's own words for the refusal, with Try again when it can help. */
@Composable
private fun FailurePanel(state: StreamScreenState, controls: StreamControls, onBack: () -> Unit) {
    val host = state.hostName
    Column(
        modifier = Modifier.fillMaxSize().background(GanderTheme.colors.bg).testTag(StreamTags.FAILURE),
        verticalArrangement = Arrangement.spacedBy(12.dp, Alignment.CenterVertically),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text(stringResource(R.string.stream_failed_title), style = MaterialTheme.typography.titleMedium)
        val message = when (val phase = state.session.phase) {
            is StreamPhase.Unsupported -> stringResource(
                when (phase.why) {
                    StreamPhase.Unsupported.Why.PHONE_HAS_NO_H264_DECODER -> R.string.stream_unsupported_phone
                    StreamPhase.Unsupported.Why.HOST_DOES_NOT_STREAM_H264 -> R.string.stream_unsupported_host
                },
                host,
            )

            is StreamPhase.Failed -> phase.error?.let { error ->
                ActionReason
                    .fromError(FileAction.STREAM, error)
                    .text(host)
            } ?: stringResource(R.string.error_other)

            else -> ""
        }
        if (state.hostMissing) {
            Text(
                stringResource(R.string.stream_host_missing),
                textAlign = TextAlign.Center,
                modifier = Modifier.padding(horizontal = 24.dp),
            )
        } else {
            Text(message, textAlign = TextAlign.Center, modifier = Modifier.padding(horizontal = 24.dp))
        }
        if (state.session.phase is StreamPhase.Failed && !state.hostMissing) {
            GanderButton(stringResource(R.string.stream_try_again), onClick = controls::retry, icon = GanderIcons.Refresh)
        }
        GanderButton(stringResource(R.string.stream_done), onClick = onBack, kind = ButtonKind.TEXT)
    }
}

/**
 * The on-screen keyboard button (client spec: a button, not a hidden multi-finger gesture): an
 * invisible one-character-at-a-time text field that takes the phone keyboard's focus. Typed
 * characters go out as `text` right away (with a sticky modifier, as the key chord), Enter and
 * Backspace as keys. `Password` turns off the suggestion strip so that characters are
 * committed as typed instead of being held back as composing text.
 */
@Composable
private fun LiveTypingField(onText: (String) -> Unit, onKey: (String) -> Unit) {
    var value by remember { mutableStateOf(TextFieldValue("")) }
    val focus = remember { FocusRequester() }
    val keyboard = LocalSoftwareKeyboardController.current
    LaunchedEffect(Unit) {
        focus.requestFocus()
        keyboard?.show()
    }
    // Out of the tree with the button: the phone keyboard goes with it.
    DisposableEffect(Unit) { onDispose { keyboard?.hide() } }
    BasicTextField(
        value = value,
        onValueChange = { next ->
            if (next.composition != null) {
                value = next
            } else {
                val typed = next.text
                if (typed.isNotEmpty()) {
                    val chunk = StringBuilder()
                    for (c in typed) {
                        if (c == '\n') {
                            if (chunk.isNotEmpty()) onText(chunk.toString())
                            chunk.clear()
                            onKey(KeyCodes.ENTER)
                        } else {
                            chunk.append(c)
                        }
                    }
                    if (chunk.isNotEmpty()) onText(chunk.toString())
                }
                value = TextFieldValue("")
            }
        },
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password, autoCorrectEnabled = false),
        modifier = Modifier
            .size(1.dp)
            .alpha(0f)
            .focusRequester(focus)
            .onPreviewKeyEvent { event ->
                if (event.type != KeyEventType.KeyDown) return@onPreviewKeyEvent false
                val code = when (event.key) {
                    Key.Backspace -> KeyCodes.BACKSPACE
                    Key.Delete -> KeyCodes.DELETE
                    Key.Enter -> KeyCodes.ENTER
                    Key.DirectionLeft -> KeyCodes.ARROW_LEFT
                    Key.DirectionRight -> KeyCodes.ARROW_RIGHT
                    Key.DirectionUp -> KeyCodes.ARROW_UP
                    Key.DirectionDown -> KeyCodes.ARROW_DOWN
                    Key.Tab -> KeyCodes.TAB
                    Key.Escape -> KeyCodes.ESCAPE
                    else -> return@onPreviewKeyEvent false
                }
                onKey(code)
                true
            },
    )
}

private const val RESIZE_SETTLE_MS = 500L
