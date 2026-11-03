package fyi.arlo.gander.core.data.stream

import fyi.arlo.gander.core.net.GanderJson
import fyi.arlo.gander.core.net.protocol.ClientEvent
import fyi.arlo.gander.core.net.protocol.InputMessage
import fyi.arlo.gander.core.net.protocol.StreamReason
import fyi.arlo.gander.core.net.protocol.StreamState
import kotlinx.serialization.ExperimentalSerializationApi
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.SerializationException
import kotlinx.serialization.json.JsonClassDiscriminator

// How the stream viewer talks to the host (WP-54): the `/v1/events` signalling frames, the WebRTC
// data-channel messages and the ICE candidate strings. Plain Kotlin, no Android and no libwebrtc
// types, so every wire shape is pinned by JVM unit tests (`StreamWireTest`).

/**
 * Liveness and pause messages on the host-created `control` data channel (host crate
 * `gander-stream`, `control.rs`). JSON text tagged by `t`:
 *
 * | Direction | Message | Meaning |
 * | --- | --- | --- |
 * | phone to host | `ping {seq}` | still watching, every 1 s; the host answers [Pong] with the same `seq` |
 * | host to phone | `pong {seq}` | the answer |
 * | phone to host | `pause`, `resume` | the stream is not visible (backgrounded) or visible again |
 * | phone to host | `keyframe` | ask for an IDR (PLI is the normal path) |
 * | host to phone | `state {state, reason}` | `streaming` or `paused`, mirrored from `stream.state` |
 * | phone to host | `front` | "Bring to front": the host runs its foreground procedure for the window |
 * | host to phone | `front.result {acquired}` | the answer to `front`: whether the window is now in front |
 * | host to phone | `clipboard {s}`, `clipboard.ack {bytes}`, `clipboard.refused {op, reason}` | answers to the clipboard requests ([InputMessage.ClipboardGet], [InputMessage.Clipboard]; see `StreamClipboard.kt`) |
 * | phone to host | `mute {on}` | WP-55: stop (`true`) or restart the window's sound; the host stops capturing while muted |
 * | host to phone | `audio {on, reason}` | WP-55: whether the window's sound is sent now; `reason` `muted`, `paused`, `unavailable` (no sound this session) or `none` (this host sends no sound) |
 */
@OptIn(ExperimentalSerializationApi::class)
@Serializable
@JsonClassDiscriminator("t")
sealed interface ControlMessage {
    @Serializable
    @SerialName("ping")
    data class Ping(val seq: Long) : ControlMessage

    @Serializable
    @SerialName("pong")
    data class Pong(val seq: Long) : ControlMessage

    @Serializable
    @SerialName("pause")
    data object Pause : ControlMessage

    @Serializable
    @SerialName("resume")
    data object Resume : ControlMessage

    @Serializable
    @SerialName("keyframe")
    data object Keyframe : ControlMessage

    @Serializable
    @SerialName("state")
    data class State(val state: StreamState, val reason: StreamReason? = null) : ControlMessage

    /** "Bring to front" (input paused with `focus_lost`). */
    @Serializable
    @SerialName("front")
    data object Front : ControlMessage

    /** The host's answer to [Front]. */
    @Serializable
    @SerialName("front.result")
    data class FrontResult(val acquired: Boolean) : ControlMessage

    /** The computer's clipboard text, the answer to a `clipboard.get`. */
    @Serializable
    @SerialName("clipboard")
    data class ClipboardText(val s: String) : ControlMessage

    /** The text of a `clipboard` write is on the computer's clipboard; [bytes] is its UTF-8 size. */
    @Serializable
    @SerialName("clipboard.ack")
    data class ClipboardAck(val bytes: Long) : ControlMessage

    /** Mute ([on] true) or unmute the window's sound (WP-55). */
    @Serializable
    @SerialName("mute")
    data class Mute(val on: Boolean) : ControlMessage

    /** Whether the window's sound is being sent ([on]) and, if not, why ([reason], see [AudioReason]). */
    @Serializable
    @SerialName("audio")
    data class Audio(val on: Boolean, val reason: String? = null) : ControlMessage

    /** The computer did not read ([op] get) or write ([op] set) its clipboard, for [reason]. */
    @Serializable
    @SerialName("clipboard.refused")
    data class ClipboardRefused(val op: ClipboardOp, val reason: ClipboardRefusal) : ControlMessage
}

/** The `reason` values of [ControlMessage.Audio] (WP-55); unknown ones count as "no sound". */
object AudioReason {
    const val MUTED = "muted"
    const val PAUSED = "paused"
    const val UNAVAILABLE = "unavailable"
    const val NONE = "none"

    /** The session carries sound (now or after unmute or resume), so the mute button makes sense. */
    fun hasSound(message: ControlMessage.Audio): Boolean = message.on || message.reason == MUTED || message.reason == PAUSED
}

/** The two data channels the host creates before its offer. */
enum class InputChannel(val label: String) {
    /** Unordered, `maxRetransmits` 0: pointer moves and scroll, where a lost message costs nothing. */
    INPUT("input"),

    /** Ordered and reliable: button presses and releases, keys, text and the liveness messages. */
    CONTROL("control"),
}

object StreamWire {
    private val json = GanderJson.json

    /** One `/v1/events` text frame from the phone (`stream.answer`, `stream.ice`, `stream.pause`, ...). */
    fun encode(event: ClientEvent): String = json.encodeToString(ClientEvent.serializer(), event)

    /** A data-channel message from the phone. */
    fun encode(message: ControlMessage): String = json.encodeToString(ControlMessage.serializer(), message)

    /** An input message (`move`, `down`, `up`, `scroll`, `key`, `text`) as compact JSON. */
    fun encode(message: InputMessage): String = json.encodeToString(InputMessage.serializer(), message)

    /**
     * Parses one `control` channel message from the host. Null for anything that is not a known
     * control message (a newer host's type, garbage): clients ignore what they do not know.
     */
    fun parseControl(text: String): ControlMessage? = try {
        json.decodeFromString(ControlMessage.serializer(), text)
    } catch (_: SerializationException) {
        null
    } catch (_: IllegalArgumentException) {
        null
    }

    /**
     * Which channel carries [message]. A button press or release must not be lost or reordered
     * (a lost `up` is a stuck button), so everything discrete goes on the reliable `control`
     * channel; pointer moves and scroll deltas go on the lossy `input` channel, where the next
     * message supersedes a lost one and a congested link must not delay a click behind them.
     */
    fun channelFor(message: InputMessage): InputChannel = when (message) {
        is InputMessage.Move, is InputMessage.Scroll -> InputChannel.INPUT
        is InputMessage.Down, is InputMessage.Up, is InputMessage.Key, is InputMessage.Text,
        InputMessage.ClipboardGet, is InputMessage.Clipboard,
        -> InputChannel.CONTROL
    }
}

/** ICE candidate strings on `stream.ice` (the message has no `sdpMid`: the host assumes the first m-section). */
object IceCandidates {
    /** `stream.ice` candidate that tells the host there are no more candidates. */
    const val END_OF_CANDIDATES = ""

    /** The wire form of libwebrtc's `IceCandidate.sdp` (`candidate:...`, no `a=` prefix). */
    fun toWire(candidateSdp: String): String = candidateSdp.trim().removePrefix("a=")

    /** The candidate attribute of a host `stream.ice`, or null for end-of-candidates and blanks. */
    fun fromWire(candidate: String): String? {
        val body = candidate.trim().removePrefix("a=")
        if (body.isEmpty()) return null
        return if (body.startsWith("candidate:")) body else "candidate:$body"
    }

    /** The `a=mid` of the first media section: the mid trickled candidates belong to. Defaults to `0`. */
    fun firstMid(sdp: String): String =
        sdp.lineSequence().map { it.trim() }.firstOrNull { it.startsWith("a=mid:") }
            ?.removePrefix("a=mid:")?.trim()?.takeIf { it.isNotEmpty() } ?: DEFAULT_MID

    private const val DEFAULT_MID = "0"
}
