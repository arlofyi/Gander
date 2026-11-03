package fyi.arlo.gander.ui.stream

import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Keyboard
import androidx.compose.material.icons.filled.ZoomOutMap
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.AssistChip
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import fyi.arlo.gander.R
import androidx.compose.foundation.BorderStroke
import androidx.compose.material3.HorizontalDivider
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.sp
import fyi.arlo.gander.ui.components.Banner
import fyi.arlo.gander.ui.components.BannerTone
import fyi.arlo.gander.ui.components.ButtonKind
import fyi.arlo.gander.ui.components.GanderButton
import fyi.arlo.gander.ui.components.GanderIconButton
import fyi.arlo.gander.ui.components.GanderProgress
import fyi.arlo.gander.ui.components.KeyCap
import fyi.arlo.gander.ui.components.LiveIndicator
import fyi.arlo.gander.ui.components.SegmentOption
import fyi.arlo.gander.ui.components.Segmented
import fyi.arlo.gander.ui.theme.GanderIcons
import fyi.arlo.gander.ui.theme.GanderShape
import fyi.arlo.gander.ui.theme.GanderTheme
import fyi.arlo.gander.core.data.stream.ControlMode
import fyi.arlo.gander.core.data.stream.EndCause
import fyi.arlo.gander.core.data.stream.FrontStatus
import fyi.arlo.gander.core.data.stream.StreamAudio
import fyi.arlo.gander.core.data.stream.StreamBanner
import fyi.arlo.gander.core.data.stream.StreamStateMachine
import fyi.arlo.gander.core.net.protocol.ClipboardMode
import fyi.arlo.gander.core.net.protocol.NetworkAdapter
import fyi.arlo.gander.core.net.protocol.KeyModifier
import fyi.arlo.gander.core.net.protocol.StreamReason

// The stream viewer's chrome: everything around the picture. Stateless composables (state in,
// callbacks out) so the instrumented Compose tests can drive them without a stream.

object StreamTags {
    const val SURFACE = "stream-surface"
    const val GESTURE_LAYER = "stream-gesture-layer"
    const val TOP_BAR = "stream-top-bar"
    const val BANNER = "stream-banner"
    const val PICK_GONE = "stream-pick-gone"
    const val BRING_TO_FRONT = "stream-bring-to-front"
    const val FRONT_RESULT = "stream-front-result"
    const val MODE_TOGGLE = "stream-mode-toggle"
    const val ZOOM_RESET = "stream-zoom-reset"
    const val KEYBOARD_TOGGLE = "stream-keyboard-toggle"
    const val MUTE_TOGGLE = "stream-mute-toggle"
    const val KEYBOARD_BAR = "stream-keyboard-bar"
    const val FUNCTION_ROW = "stream-function-row"
    const val TEXT_BOX = "stream-text-box"
    const val TEXT_FIELD = "stream-text-field"
    const val TEXT_SEND = "stream-text-send"
    const val END_DIALOG = "stream-end-dialog"
    const val END_LEAVE_OPEN = "stream-end-leave-open"
    const val END_CLOSE_APP = "stream-end-close-app"
    const val END_KEEP = "stream-end-keep"
    const val ENDED_DIALOG = "stream-ended-dialog"
    const val FETCH_SAVED = "stream-fetch-saved"
    const val RELAY_DIALOG = "stream-relay-dialog"
    const val RELAY_CONTINUE = "stream-relay-continue"
    const val FAILURE = "stream-failure"

    fun key(name: String) = "stream-key-$name"

    /** The window list's row for candidate [index] (`stream.state pick`). */
    fun pick(index: Int) = "stream-pick-$index"
}

// Top bar -------------------------------------------------------------------------------------------

/**
 * The session bar (mockup 30): leave, LIVE with what is streamed from where, the Direct /
 * Trackpad switch, zoom reset (while zoomed), mute (while the window's sound is sent, WP-55) and
 * the keyboard bar toggle. Always visible.
 */
@Composable
fun StreamTopBar(
    title: String,
    subtitle: String,
    mode: ControlMode,
    zoomed: Boolean,
    keyboardOpen: Boolean,
    onLeave: () -> Unit,
    onToggleMode: () -> Unit,
    onResetZoom: () -> Unit,
    onToggleKeyboard: () -> Unit,
    modifier: Modifier = Modifier,
    live: Boolean = false,
    audio: StreamAudio = StreamAudio(),
    onToggleMute: () -> Unit = {},
) {
    val c = GanderTheme.colors
    Surface(color = c.surface, contentColor = c.text, modifier = modifier.testTag(StreamTags.TOP_BAR)) {
        Column {
            Row(
                modifier = Modifier.fillMaxWidth().padding(start = 4.dp, end = 6.dp, top = 4.dp, bottom = 6.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                GanderIconButton(GanderIcons.Close, stringResource(R.string.stream_leave), onLeave)
                Column(modifier = Modifier.weight(1f)) {
                    if (live) {
                        LiveIndicator(stringResource(R.string.stream_live))
                    } else {
                        Text(title, style = MaterialTheme.typography.titleSmall, maxLines = 1, overflow = TextOverflow.Ellipsis)
                    }
                    Text(
                        subtitle,
                        style = MaterialTheme.typography.bodySmall.copy(fontSize = 12.5.sp),
                        color = c.text2,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
                val switchLabel = stringResource(R.string.stream_mode_switch, mode.label())
                val direct = mode == ControlMode.DIRECT
                // The segment that is not selected carries the toggle's tag: tapping it switches.
                val toggle = Modifier
                    .testTag(StreamTags.MODE_TOGGLE)
                    .semantics { contentDescription = switchLabel }
                Segmented(
                    options = listOf(
                        SegmentOption(
                            ControlMode.DIRECT.label(),
                            GanderIcons.Touch,
                            if (direct) Modifier else toggle,
                        ),
                        SegmentOption(
                            ControlMode.TRACKPAD.label(),
                            GanderIcons.Pointer,
                            if (direct) toggle else Modifier,
                        ),
                    ),
                    selectedIndex = if (direct) 0 else 1,
                    onSelect = { index -> if ((index == 0) != direct) onToggleMode() },
                )
                if (zoomed) {
                    GanderIconButton(
                        GanderIcons.Zoom,
                        stringResource(R.string.stream_zoom_reset),
                        onResetZoom,
                        modifier = Modifier.testTag(StreamTags.ZOOM_RESET),
                    )
                }
                if (audio.available) {
                    GanderIconButton(
                        icon = if (audio.muted) GanderIcons.SoundOff else GanderIcons.Sound,
                        contentDescription = stringResource(
                            if (audio.muted) R.string.stream_sound_unmute else R.string.stream_sound_mute,
                        ),
                        onClick = onToggleMute,
                        modifier = Modifier.testTag(StreamTags.MUTE_TOGGLE),
                    )
                }
                GanderIconButton(
                    icon = if (keyboardOpen) GanderIcons.KeyboardHide else GanderIcons.Keyboard,
                    contentDescription = stringResource(
                        if (keyboardOpen) R.string.stream_keyboard_hide else R.string.stream_keyboard_show,
                    ),
                    onClick = onToggleKeyboard,
                    modifier = Modifier.testTag(StreamTags.KEYBOARD_TOGGLE),
                )
            }
            HorizontalDivider(color = c.line)
        }
    }
}

@Composable
fun ControlMode.label(): String = stringResource(
    when (this) {
        ControlMode.DIRECT -> R.string.stream_mode_direct
        ControlMode.TRACKPAD -> R.string.stream_mode_trackpad
    },
)

// State banner ---------------------------------------------------------------------------------------

/** The sentence of a banner (client spec: launching, approving on the computer, paused with its reason, reconnecting). */
@Composable
fun StreamBanner.text(host: String, app: String?): String = when (this) {
    StreamBanner.Requested -> stringResource(R.string.stream_banner_requested, host)
    StreamBanner.Approving -> stringResource(R.string.stream_banner_approving, host)
    StreamBanner.Launching ->
        if (app == null) {
            stringResource(R.string.stream_banner_launching_app, host)
        } else {
            stringResource(R.string.stream_banner_launching, app, host)
        }
    is StreamBanner.PickWindow -> stringResource(
        if (candidates.isEmpty()) R.string.stream_banner_pick_none else R.string.stream_banner_pick,
        host,
        pickTimeout(),
    )
    StreamBanner.SettingUp -> stringResource(R.string.stream_banner_connecting, host)
    is StreamBanner.Paused -> when (reason) {
        StreamReason.FOCUS_LOST -> stringResource(R.string.stream_banner_paused_focus, host)
        StreamReason.LOCAL_USER_ACTIVE -> stringResource(R.string.stream_banner_paused_user, host)
        StreamReason.NETWORK -> stringResource(R.string.stream_banner_paused_network, host)
        StreamReason.PHONE_BACKGROUND -> stringResource(R.string.stream_banner_paused_background)
        else -> stringResource(R.string.stream_banner_paused)
    }
    StreamBanner.Backgrounded -> stringResource(R.string.stream_banner_backgrounded)
    StreamBanner.Reconnecting -> stringResource(R.string.stream_banner_reconnecting, host)
}

/** "30 seconds": how long the host waits for a pick. */
@Composable
private fun pickTimeout(): String =
    pluralStringResource(R.plurals.stream_seconds, StreamStateMachine.PICK_TIMEOUT_SECONDS, StreamStateMachine.PICK_TIMEOUT_SECONDS)

/** True for the banners that mean "working on it" rather than "waiting for somebody". */
fun StreamBanner.isBusy(): Boolean = when (this) {
    StreamBanner.Requested, StreamBanner.Launching, StreamBanner.SettingUp, StreamBanner.Reconnecting -> true
    StreamBanner.Approving, is StreamBanner.PickWindow, is StreamBanner.Paused, StreamBanner.Backgrounded -> false
}

/**
 * The banner over the picture. For `stream.state pick` it is also the window picker: one tappable
 * row per candidate title, and [onPick] gets the tapped row's index (the phone then sends
 * `stream.pick`). The host ends the session if nothing is chosen within 30 s. Paused with
 * `focus_lost`, it offers "Bring to front" ([onBringToFront]) and shows the host's answer ([front]).
 */
@Composable
fun StateBanner(
    banner: StreamBanner,
    host: String,
    app: String?,
    modifier: Modifier = Modifier,
    onPick: (Int) -> Unit = {},
    front: FrontStatus = FrontStatus.IDLE,
    onBringToFront: () -> Unit = {},
) {
    val fillWidth = if (banner is StreamBanner.PickWindow) Modifier.fillMaxWidth() else Modifier
    val tone = when (banner) {
        is StreamBanner.Paused, StreamBanner.Backgrounded -> BannerTone.WARN
        else -> BannerTone.INFO
    }
    val icon = when (banner) {
        is StreamBanner.Paused, StreamBanner.Backgrounded -> GanderIcons.Pause
        is StreamBanner.PickWindow -> GanderIcons.Window
        StreamBanner.Approving -> GanderIcons.User
        else -> GanderIcons.Stream
    }
    Surface(
        shape = GanderShape.Banner,
        color = GanderTheme.colors.surface,
        shadowElevation = 4.dp,
        modifier = modifier.then(fillWidth).testTag(StreamTags.BANNER),
    ) {
        Banner(tone = tone, icon = icon) {
            Text(banner.text(host, app), style = MaterialTheme.typography.bodyMedium)
            if (banner.isBusy()) GanderProgress(fraction = null, modifier = Modifier.padding(top = 8.dp))
            if (banner is StreamBanner.PickWindow) PickWindowList(banner, onPick)
            if (banner == StreamBanner.Paused(StreamReason.FOCUS_LOST)) BringToFront(host, front, onBringToFront)
        }
    }
}

/** "Bring to front" under a `focus_lost` pause, with the host's answer once it came. */
@Composable
private fun BringToFront(host: String, front: FrontStatus, onBringToFront: () -> Unit) {
    val c = GanderTheme.colors
    if (front == FrontStatus.REFUSED || front == FrontStatus.ACQUIRED) {
        val refused = front == FrontStatus.REFUSED
        Text(
            stringResource(if (refused) R.string.stream_front_refused else R.string.stream_front_acquired, host),
            style = MaterialTheme.typography.bodyMedium,
            color = if (refused) c.bad else c.text2,
            modifier = Modifier.padding(top = 6.dp).testTag(StreamTags.FRONT_RESULT),
        )
    }
    val sending = front == FrontStatus.SENDING
    GanderButton(
        text = stringResource(if (sending) R.string.stream_front_sending else R.string.stream_front_bring),
        onClick = onBringToFront,
        kind = ButtonKind.TONAL,
        icon = GanderIcons.Window,
        enabled = !sending,
        small = true,
        modifier = Modifier.padding(top = 8.dp).testTag(StreamTags.BRING_TO_FRONT),
    )
}

/** The candidate windows of a pick as tappable rows; the one just sent shows progress, and a window that vanished says so. */
@Composable
private fun PickWindowList(pick: StreamBanner.PickWindow, onPick: (Int) -> Unit) {
    val c = GanderTheme.colors
    if (pick.windowGone) {
        Text(
            stringResource(R.string.stream_pick_gone),
            style = MaterialTheme.typography.bodyMedium,
            color = c.bad,
            modifier = Modifier.padding(top = 6.dp).testTag(StreamTags.PICK_GONE),
        )
    }
    // Many windows must not push the banner off a landscape screen.
    Column(
        modifier = Modifier.padding(top = 8.dp).heightIn(max = 240.dp).verticalScroll(rememberScrollState()),
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        pick.candidates.forEachIndexed { index, title ->
            val chosen = pick.chosen == index
            Surface(
                onClick = { onPick(index) },
                shape = GanderShape.Panel,
                color = c.surface2,
                border = if (chosen) BorderStroke(2.dp, c.accent) else null,
                modifier = Modifier.fillMaxWidth().testTag(StreamTags.pick(index)),
            ) {
                Row(
                    modifier = Modifier.padding(horizontal = 12.dp, vertical = 10.dp),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(10.dp),
                ) {
                    Icon(GanderIcons.Window, contentDescription = null, tint = c.text2, modifier = Modifier.size(20.dp))
                    Text(
                        title,
                        style = MaterialTheme.typography.bodyMedium.copy(fontWeight = FontWeight.SemiBold),
                        maxLines = 2,
                        overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.weight(1f),
                    )
                }
            }
        }
    }
}

// Keyboard bar and local text box ------------------------------------------------------------------

/**
 * Sticky Ctrl/Alt/Shift, Esc, Tab, arrows, the Fn row, Ctrl+Z/Y, Ctrl+S, the "type" button (local
 * text box) and the on-screen keyboard button (client spec: a button, not a hidden multi-finger gesture).
 */
@Composable
fun KeyboardBarView(
    state: KeyboardState,
    onToggleSticky: (KeyModifier) -> Unit,
    onKey: (code: String, extraModifiers: Set<KeyModifier>) -> Unit,
    onToggleFunctionRow: () -> Unit,
    onType: () -> Unit,
    onSystemKeyboard: () -> Unit,
    modifier: Modifier = Modifier,
    oneRow: Boolean = false,
    /** Which clipboard actions the computer allows (`capabilities.stream.clipboard`); none by default. */
    clipboard: ClipboardMode = ClipboardMode.OFF,
    onPasteToComputer: () -> Unit = {},
    onCopyFromComputer: () -> Unit = {},
) {
    val c = GanderTheme.colors
    val on = stringResource(R.string.stream_key_sticky_on)
    val off = stringResource(R.string.stream_key_sticky_off)
    @Composable
    fun sticky(mod: KeyModifier, keyModifier: Modifier) = KeyCap(
        label = mod.label(),
        onClick = { onToggleSticky(mod) },
        latched = mod in state.sticky,
        latchedDescription = if (mod in state.sticky) on else off,
        modifier = keyModifier.testTag(StreamTags.key(mod.wire)),
    )
    @Composable
    fun key(label: String, tag: String, keyModifier: Modifier, description: String? = null, icon: ImageVector? = null, onClick: () -> Unit) =
        KeyCap(label = label, icon = icon, contentDescription = description, onClick = onClick, modifier = keyModifier.testTag(StreamTags.key(tag)))
    @Composable
    fun fn(keyModifier: Modifier) = KeyCap(
        label = stringResource(R.string.stream_key_fn),
        onClick = onToggleFunctionRow,
        latched = state.functionRow,
        modifier = keyModifier.testTag(StreamTags.key("fn")),
    )

    Surface(color = c.surface2, modifier = modifier.testTag(StreamTags.KEYBOARD_BAR)) {
        Column(Modifier.fillMaxWidth()) {
            HorizontalDivider(color = c.line)
            if (state.functionRow) {
                Row(
                    modifier = Modifier
                        .testTag(StreamTags.FUNCTION_ROW)
                        .horizontalScroll(rememberScrollState())
                        .padding(horizontal = 6.dp, vertical = 4.dp),
                    horizontalArrangement = Arrangement.spacedBy(5.dp),
                ) {
                    (1..12).forEach { n ->
                        key("F$n", "f$n", Modifier.width(48.dp)) { onKey(KeyCodes.function(n), emptySet()) }
                    }
                }
            }
            if (oneRow) {
                // Tablet (mockup 36): every key on one scrolling row.
                Row(
                    modifier = Modifier
                        .horizontalScroll(rememberScrollState())
                        .padding(horizontal = 8.dp, vertical = 6.dp),
                    horizontalArrangement = Arrangement.spacedBy(5.dp),
                ) {
                    val w = Modifier.width(64.dp)
                    KeyboardBar.MODIFIERS.forEach { sticky(it, w) }
                    AllOtherKeys(w, ::key, { fn(it) }, onKey, onType, onSystemKeyboard)
                    ClipboardKeys(Modifier.width(CLIPBOARD_KEY_WIDTH), clipboard, ::key, onPasteToComputer, onCopyFromComputer)
                }
            } else {
                // Phone: three rows of six to seven keys; the column scrolls when the screen is short.
                Column(
                    modifier = Modifier
                        .heightIn(max = 160.dp)
                        .verticalScroll(rememberScrollState())
                        .padding(start = 6.dp, end = 6.dp, top = 6.dp, bottom = 4.dp),
                    verticalArrangement = Arrangement.spacedBy(5.dp),
                ) {
                    Row(horizontalArrangement = Arrangement.spacedBy(5.dp)) {
                        KeyboardBar.MODIFIERS.forEach { sticky(it, Modifier.weight(1f)) }
                        key(stringResource(R.string.stream_key_esc), "esc", Modifier.weight(1f)) { onKey(KeyCodes.ESCAPE, emptySet()) }
                        key(stringResource(R.string.stream_key_tab), "tab", Modifier.weight(1f)) { onKey(KeyCodes.TAB, emptySet()) }
                        fn(Modifier.weight(1f))
                    }
                    Row(horizontalArrangement = Arrangement.spacedBy(5.dp)) {
                        ArrowKeys(Modifier.weight(1f), ::key, onKey)
                        key(stringResource(R.string.stream_key_undo), "undo", Modifier.weight(1.3f)) { onKey(KeyCodes.KEY_Z, setOf(KeyModifier.CTRL)) }
                        key(stringResource(R.string.stream_key_redo), "redo", Modifier.weight(1.3f)) { onKey(KeyCodes.KEY_Y, setOf(KeyModifier.CTRL)) }
                        key(stringResource(R.string.stream_key_save), "save", Modifier.weight(1.3f)) { onKey(KeyCodes.KEY_S, setOf(KeyModifier.CTRL)) }
                    }
                    Row(horizontalArrangement = Arrangement.spacedBy(5.dp)) {
                        key(stringResource(R.string.stream_key_type), "type", Modifier.weight(1.6f), icon = GanderIcons.Keyboard, onClick = onType)
                        key(
                            stringResource(R.string.stream_key_system_keyboard),
                            "keyboard",
                            Modifier.weight(1.6f),
                            icon = GanderIcons.Keyboard,
                            onClick = onSystemKeyboard,
                        )
                        key(stringResource(R.string.stream_key_backspace), "backspace", Modifier.weight(1f)) { onKey(KeyCodes.BACKSPACE, emptySet()) }
                        key(stringResource(R.string.stream_key_delete), "delete", Modifier.weight(1f)) { onKey(KeyCodes.DELETE, emptySet()) }
                        key(stringResource(R.string.stream_key_enter), "enter", Modifier.weight(1f)) { onKey(KeyCodes.ENTER, emptySet()) }
                    }
                    // Only when the computer's owner allowed a direction: a fourth row (the column scrolls).
                    if (clipboard.canPasteToComputer || clipboard.canCopyFromComputer) {
                        Row(horizontalArrangement = Arrangement.spacedBy(5.dp)) {
                            ClipboardKeys(Modifier.weight(1f), clipboard, ::key, onPasteToComputer, onCopyFromComputer)
                        }
                    }
                }
            }
        }
    }
}

private typealias KeyFactory = @Composable (
    label: String,
    tag: String,
    keyModifier: Modifier,
    description: String?,
    icon: ImageVector?,
    onClick: () -> Unit,
) -> Unit

@Composable
private fun ArrowKeys(modifier: Modifier, key: KeyFactory, onKey: (String, Set<KeyModifier>) -> Unit) {
    key(LEFT_ARROW, "left", modifier, stringResource(R.string.stream_key_left), null) { onKey(KeyCodes.ARROW_LEFT, emptySet()) }
    key(UP_ARROW, "up", modifier, stringResource(R.string.stream_key_up), null) { onKey(KeyCodes.ARROW_UP, emptySet()) }
    key(DOWN_ARROW, "down", modifier, stringResource(R.string.stream_key_down), null) { onKey(KeyCodes.ARROW_DOWN, emptySet()) }
    key(RIGHT_ARROW, "right", modifier, stringResource(R.string.stream_key_right), null) { onKey(KeyCodes.ARROW_RIGHT, emptySet()) }
}

@Composable
private fun AllOtherKeys(
    modifier: Modifier,
    key: KeyFactory,
    fn: @Composable (Modifier) -> Unit,
    onKey: (String, Set<KeyModifier>) -> Unit,
    onType: () -> Unit,
    onSystemKeyboard: () -> Unit,
) {
    key(stringResource(R.string.stream_key_esc), "esc", modifier, null, null) { onKey(KeyCodes.ESCAPE, emptySet()) }
    key(stringResource(R.string.stream_key_tab), "tab", modifier, null, null) { onKey(KeyCodes.TAB, emptySet()) }
    fn(modifier)
    ArrowKeys(modifier, key, onKey)
    key(stringResource(R.string.stream_key_undo), "undo", modifier, null, null) { onKey(KeyCodes.KEY_Z, setOf(KeyModifier.CTRL)) }
    key(stringResource(R.string.stream_key_redo), "redo", modifier, null, null) { onKey(KeyCodes.KEY_Y, setOf(KeyModifier.CTRL)) }
    key(stringResource(R.string.stream_key_save), "save", modifier, null, null) { onKey(KeyCodes.KEY_S, setOf(KeyModifier.CTRL)) }
    key(stringResource(R.string.stream_key_backspace), "backspace", modifier, null, null) { onKey(KeyCodes.BACKSPACE, emptySet()) }
    key(stringResource(R.string.stream_key_delete), "delete", modifier, null, null) { onKey(KeyCodes.DELETE, emptySet()) }
    key(stringResource(R.string.stream_key_enter), "enter", modifier, null, null) { onKey(KeyCodes.ENTER, emptySet()) }
    key(stringResource(R.string.stream_key_type), "type", Modifier.width(120.dp), null, GanderIcons.Keyboard, onType)
    key(stringResource(R.string.stream_key_system_keyboard), "keyboard", Modifier.width(120.dp), null, GanderIcons.Keyboard, onSystemKeyboard)
}

/** "Paste to computer" and "Copy from computer", each only when the host's clipboard setting allows that direction. */
@Composable
private fun ClipboardKeys(
    modifier: Modifier,
    clipboard: ClipboardMode,
    key: KeyFactory,
    onPasteToComputer: () -> Unit,
    onCopyFromComputer: () -> Unit,
) {
    if (clipboard.canPasteToComputer) {
        key(stringResource(R.string.stream_key_paste_to_computer), "paste", modifier, null, null, onPasteToComputer)
    }
    if (clipboard.canCopyFromComputer) {
        key(stringResource(R.string.stream_key_copy_from_computer), "copy", modifier, null, null, onCopyFromComputer)
    }
}

private val CLIPBOARD_KEY_WIDTH = 150.dp

@Composable
private fun KeyModifier.label(): String = stringResource(
    when (this) {
        KeyModifier.CTRL -> R.string.stream_key_ctrl
        KeyModifier.ALT -> R.string.stream_key_alt
        KeyModifier.SHIFT -> R.string.stream_key_shift
        KeyModifier.META, KeyModifier.UNKNOWN -> R.string.stream_key_ctrl
    },
)

private const val LEFT_ARROW = "←"
private const val UP_ARROW = "↑"
private const val DOWN_ARROW = "↓"
private const val RIGHT_ARROW = "→"

/**
 * The local text box (client spec: a "type" button opening a local text box that sends `text`
 * messages, IME-friendly): the phone's keyboard and autocorrect work as usual and [onSend] gets the
 * finished string, which the host injects as Unicode.
 */
@Composable
fun LocalTextBox(onSend: (String) -> Unit, onClose: () -> Unit, modifier: Modifier = Modifier) {
    var text by rememberSaveable { mutableStateOf("") }
    val focus = remember { FocusRequester() }
    LaunchedEffect(Unit) { focus.requestFocus() }
    val send = {
        if (text.isNotEmpty()) {
            onSend(text)
            text = ""
        }
    }
    Surface(color = GanderTheme.colors.surface2, modifier = modifier.testTag(StreamTags.TEXT_BOX)) {
        Row(
            modifier = Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 6.dp),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            OutlinedTextField(
                value = text,
                onValueChange = { text = it },
                placeholder = { Text(stringResource(R.string.stream_text_hint)) },
                keyboardOptions = androidx.compose.foundation.text.KeyboardOptions(imeAction = ImeAction.Send),
                keyboardActions = androidx.compose.foundation.text.KeyboardActions(onSend = { send() }),
                modifier = Modifier.weight(1f).focusRequester(focus).testTag(StreamTags.TEXT_FIELD),
            )
            TextButton(onClick = { send() }, modifier = Modifier.testTag(StreamTags.TEXT_SEND)) {
                Text(stringResource(R.string.stream_text_send))
            }
            TextButton(onClick = onClose) { Text(stringResource(R.string.stream_done)) }
        }
    }
}

// Dialogs ----------------------------------------------------------------------------------------

/** Before the stream starts, when the path to the computer is relayed (client spec, "Relay warning"). */
@Composable
fun RelayWarningDialog(host: String, adapter: NetworkAdapter, onContinue: () -> Unit, onCancel: () -> Unit) {
    AlertDialog(
        modifier = Modifier.testTag(StreamTags.RELAY_DIALOG),
        onDismissRequest = onCancel,
        containerColor = GanderTheme.colors.surface2,
        icon = { Icon(GanderIcons.Relay, contentDescription = null, tint = GanderTheme.colors.warn) },
        title = { Text(stringResource(R.string.stream_relay_title)) },
        text = {
            Text(
                stringResource(
                    if (adapter == NetworkAdapter.TAILSCALE) {
                        R.string.stream_relay_body_tailscale
                    } else {
                        R.string.stream_relay_body_generic
                    },
                    host,
                ),
            )
        },
        confirmButton = {
            TextButton(onClick = onContinue, modifier = Modifier.testTag(StreamTags.RELAY_CONTINUE)) {
                Text(stringResource(R.string.stream_relay_continue))
            }
        },
        dismissButton = { TextButton(onClick = onCancel) { Text(stringResource(R.string.stream_relay_cancel)) } },
    )
}

/**
 * "Leave PowerPoint open on studio-pc / Close it" (client spec, "End"). The three choices are one
 * column in the confirm slot: AlertDialog's button row lays a separate dismiss button over the
 * second button of a column (measured on the emulator: "Keep streaming" covered the right half of
 * "Close PowerPoint", so a tap there kept streaming).
 */
@Composable
fun EndStreamDialog(
    app: String?,
    host: String,
    onLeaveOpen: () -> Unit,
    onCloseApp: () -> Unit,
    onKeepStreaming: () -> Unit,
) {
    AlertDialog(
        modifier = Modifier.testTag(StreamTags.END_DIALOG),
        onDismissRequest = onKeepStreaming,
        containerColor = GanderTheme.colors.surface2,
        icon = { Icon(GanderIcons.Stream, contentDescription = null) },
        title = { Text(stringResource(R.string.stream_end_title)) },
        text = {
            Text(
                if (app == null) {
                    stringResource(R.string.stream_end_body_unknown_app, host)
                } else {
                    stringResource(R.string.stream_end_body, app, host)
                },
            )
        },
        confirmButton = {
            Column(horizontalAlignment = Alignment.End) {
                TextButton(onClick = onLeaveOpen, modifier = Modifier.testTag(StreamTags.END_LEAVE_OPEN)) {
                    Text(
                        if (app == null) {
                            stringResource(R.string.stream_end_leave_open_unknown_app, host)
                        } else {
                            stringResource(R.string.stream_end_leave_open, app, host)
                        },
                    )
                }
                TextButton(onClick = onCloseApp, modifier = Modifier.testTag(StreamTags.END_CLOSE_APP)) {
                    Text(
                        if (app == null) {
                            stringResource(R.string.stream_end_close_app_unknown_app)
                        } else {
                            stringResource(R.string.stream_end_close_app, app)
                        },
                    )
                }
                TextButton(onClick = onKeepStreaming, modifier = Modifier.testTag(StreamTags.END_KEEP)) {
                    Text(stringResource(R.string.stream_end_keep))
                }
            }
        },
    )
}

@Composable
fun EndCause.text(host: String): String = when (this) {
    EndCause.YOU_LEFT -> stringResource(R.string.stream_ended_you_left)
    EndCause.HOST_KILL_SWITCH -> stringResource(R.string.stream_ended_kill_switch, host)
    EndCause.WINDOW_CLOSED -> stringResource(R.string.stream_ended_window_closed, host)
    EndCause.REVOKED -> stringResource(R.string.stream_ended_revoked, host)
    EndCause.TIMEOUT -> stringResource(R.string.stream_ended_timeout)
    EndCause.APP_UNAVAILABLE ->
        stringResource(R.string.stream_ended_app_unavailable, host, pickTimeout())
    EndCause.HOST_LOCKED -> stringResource(R.string.stream_ended_locked, host)
    EndCause.ERROR -> stringResource(R.string.stream_ended_error, host)
    EndCause.CONNECTION_LOST -> stringResource(R.string.stream_ended_connection_lost, host)
    EndCause.NO_VIDEO_LINK -> stringResource(R.string.stream_ended_no_video, host)
}

/** Whether the end screen offers "Try again": the user did not leave on purpose and the host did not revoke the phone. */
fun EndCause.canRetry(): Boolean = when (this) {
    EndCause.CONNECTION_LOST, EndCause.NO_VIDEO_LINK, EndCause.ERROR, EndCause.TIMEOUT, EndCause.HOST_LOCKED,
    EndCause.APP_UNAVAILABLE,
    -> true
    EndCause.YOU_LEFT, EndCause.HOST_KILL_SWITCH, EndCause.WINDOW_CLOSED, EndCause.REVOKED -> false
}

/**
 * Shown when a stream is over for any reason but the user leaving without a change to the file:
 * why it ended and, when the file changed while streaming, "Fetch the saved file".
 */
@Composable
fun StreamEndedDialog(
    cause: EndCause,
    host: String,
    fileChanged: Boolean,
    onFetchSaved: () -> Unit,
    onTryAgain: (() -> Unit)?,
    onDone: () -> Unit,
) {
    AlertDialog(
        modifier = Modifier.testTag(StreamTags.ENDED_DIALOG),
        onDismissRequest = onDone,
        containerColor = GanderTheme.colors.surface2,
        icon = { Icon(GanderIcons.Stream, contentDescription = null) },
        title = { Text(stringResource(R.string.stream_ended_title)) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(cause.text(host))
                if (fileChanged) Text(stringResource(R.string.stream_ended_fetch_saved, host))
            }
        },
        confirmButton = {
            Column(horizontalAlignment = Alignment.End) {
                if (fileChanged) {
                    TextButton(onClick = onFetchSaved, modifier = Modifier.testTag(StreamTags.FETCH_SAVED)) {
                        Text(stringResource(R.string.stream_fetch_saved))
                    }
                }
                if (onTryAgain != null) TextButton(onClick = onTryAgain) { Text(stringResource(R.string.stream_try_again)) }
                TextButton(onClick = onDone) { Text(stringResource(R.string.stream_done)) }
            }
        },
    )
}
