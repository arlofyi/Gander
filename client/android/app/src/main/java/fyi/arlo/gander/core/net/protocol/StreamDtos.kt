package fyi.arlo.gander.core.net.protocol

import kotlinx.serialization.ExperimentalSerializationApi
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonClassDiscriminator

// Stream (Phase 2): POST /stream, WebSocket events and WebRTC data-channel input messages.

@Serializable
data class Viewport(val width: Int, val height: Int, val dpr: Double)

@Serializable
data class StreamPreference(val codec: VideoCodec, @SerialName("max_kbps") val maxKbps: Int)

/** `POST /v1/stream` body. */
@Serializable
data class StreamRequest(
    val share: String,
    val path: String,
    /** Explicit app id, or null to let the host's policy choose. Always sent. */
    val app: String?,
    val viewport: Viewport,
    val prefer: StreamPreference? = null,
    /** Session to resume within the grace period. */
    val resume: String? = null,
)

@Serializable
data class StreamApp(val id: String, val name: String)

/** `POST /v1/stream` 201. */
@Serializable
data class StreamCreated(
    val session: String,
    val state: StreamState,
    val app: StreamApp,
    @SerialName("needs_approval") val needsApproval: Boolean,
)

@Serializable
data class StreamWindow(val title: String, val width: Int, val height: Int, val dpi: Int)

/**
 * Host to client WebSocket message (`ServerEvent`). Decode with
 * [fyi.arlo.gander.core.net.GanderJson.decodeServerEvent], which maps a type this client does not
 * know to [Unknown] instead of failing (protocol compatibility rule).
 */
@OptIn(ExperimentalSerializationApi::class)
@Serializable
@JsonClassDiscriminator("type")
sealed interface ServerEvent {
    @Serializable
    @SerialName("hello")
    data class Hello(val protocol: Int, @SerialName("host_time") val hostTime: String) : ServerEvent

    @Serializable
    @SerialName("share.changed")
    data class ShareChanged(val share: String, val path: String) : ServerEvent

    @Serializable
    @SerialName("shares.updated")
    data object SharesUpdated : ServerEvent

    /** The device's token was revoked; the socket closes next. Forget the host after telling the user. */
    @Serializable
    @SerialName("device.revoked")
    data object DeviceRevoked : ServerEvent

    @Serializable
    @SerialName("host.state")
    data class HostState(
        val locked: Boolean,
        val attended: Boolean,
        @SerialName("on_battery") val onBattery: Boolean,
        @SerialName("stream_available") val streamAvailable: Boolean,
    ) : ServerEvent

    @Serializable
    @SerialName("stream.state")
    data class StreamStateChanged(
        val session: String,
        val state: StreamState,
        val reason: StreamReason?,
        val window: StreamWindow?,
        /** Window titles when [state] is `pick`. */
        val candidates: List<String>? = null,
    ) : ServerEvent

    @Serializable
    @SerialName("stream.offer")
    data class StreamOffer(val session: String, val sdp: String) : ServerEvent

    @Serializable
    @SerialName("stream.ice")
    data class StreamIce(val session: String, val candidate: String) : ServerEvent

    @Serializable
    @SerialName("pong")
    data object Pong : ServerEvent

    /**
     * A client message was refused (host crate `gander-host`, `events/mod.rs`; the protocol
     * spec's event list does not name it, and clients that do not know it ignore it). The socket
     * stays open. [session] is the stream session the refused message named; a lock that ends a
     * stream arrives as `error {host_locked}` just before `stream.state ended`.
     */
    @Serializable
    @SerialName("error")
    data class Error(
        val code: ErrorCode,
        val message: String,
        val session: String? = null,
    ) : ServerEvent

    /**
     * A type this client version does not know; carried only so callers can ignore it. Produced by
     * the decoder, never sent (the serial name is not a protocol type).
     */
    @Serializable
    @SerialName("gander.client.unknown")
    data class Unknown(@SerialName("raw_type") val rawType: String) : ServerEvent
}

/** Client to host WebSocket message (`ClientEvent`). */
@OptIn(ExperimentalSerializationApi::class)
@Serializable
@JsonClassDiscriminator("type")
sealed interface ClientEvent {
    @Serializable
    @SerialName("stream.answer")
    data class StreamAnswer(val session: String, val sdp: String) : ClientEvent

    @Serializable
    @SerialName("stream.ice")
    data class StreamIce(val session: String, val candidate: String) : ClientEvent

    @Serializable
    @SerialName("stream.resize")
    data class StreamResize(val session: String, val width: Int, val height: Int) : ClientEvent

    @Serializable
    @SerialName("stream.pause")
    data class StreamPause(val session: String) : ClientEvent

    @Serializable
    @SerialName("stream.resume")
    data class StreamResume(val session: String) : ClientEvent

    /**
     * The phone's choice after `stream.state pick`: [index] is zero-based into that event's
     * `candidates`. The host answers with `stream.state window_found`, or with `error`
     * (`app_unavailable` when the window is gone, `bad_request` for an index out of range) and
     * stays in `pick` until its 30 s run out.
     */
    @Serializable
    @SerialName("stream.pick")
    data class StreamPick(val session: String, val index: Int) : ClientEvent

    @Serializable
    @SerialName("ping")
    data object Ping : ClientEvent
}

/** WebRTC data-channel message (`InputMessage`); x and y are normalised to the window, 0..1. */
@OptIn(ExperimentalSerializationApi::class)
@Serializable
@JsonClassDiscriminator("t")
sealed interface InputMessage {
    @Serializable
    @SerialName("move")
    data class Move(val x: Double, val y: Double) : InputMessage

    @Serializable
    @SerialName("down")
    data class Down(val b: MouseButton, val x: Double, val y: Double) : InputMessage

    @Serializable
    @SerialName("up")
    data class Up(val b: MouseButton, val x: Double, val y: Double) : InputMessage

    /**
     * Wheel scroll in wheel-delta units (120 per notch), Windows wheel convention, which the host
     * passes to `SendInput` unchanged (protocol `InputMessage`, Rust `gander_core::input::mapping::wheel_data`):
     *
     * - [dy] positive = the wheel turned away from the user = the content scrolls up (the view
     *   moves toward the top); negative = toward the user = the content scrolls down.
     * - [dx] positive = a tilt to the right = the view moves right; negative = left.
     *
     * With natural scrolling (the content follows the fingers) the client sends the finger's own
     * dy (fingers up = negative) and the finger's dx negated (fingers left = positive); see
     * `StreamGestureRecognizer` and `StreamGesturesTest.scrollSignFollowsTheWindowsWheelConvention`.
     */
    @Serializable
    @SerialName("scroll")
    data class Scroll(val dx: Int, val dy: Int, val x: Double, val y: Double) : InputMessage

    /** [code] is a W3C UI Events `code` value such as `KeyA`. */
    @Serializable
    @SerialName("key")
    data class Key(val code: String, val down: Boolean, val mods: List<KeyModifier>) : InputMessage

    /** Typed locally, injected as Unicode on the host. */
    @Serializable
    @SerialName("text")
    data class Text(val s: String) : InputMessage

    @Serializable
    @SerialName("clipboard.get")
    data object ClipboardGet : InputMessage

    /** Host clipboard text (reply to [ClipboardGet]) or text to place on it. */
    @Serializable
    @SerialName("clipboard")
    data class Clipboard(val s: String) : InputMessage
}
