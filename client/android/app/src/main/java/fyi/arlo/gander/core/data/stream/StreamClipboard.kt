package fyi.arlo.gander.core.data.stream

import fyi.arlo.gander.core.net.protocol.WireEnum
import fyi.arlo.gander.core.net.protocol.WireEnumSerializer
import kotlinx.serialization.Serializable

// The clipboard text bridge (WP-56), the data layer: what the host says back, the caps, and how
// one action ends. Plain Kotlin, no Android types, so every rule is pinned by JVM unit tests
// (`StreamClipboardTest`, `StreamSessionTest`). The Android clipboard itself is
// `ui/stream/PhoneClipboard.kt`.
//
// Wire (host crate `gander-core`, `clipboard.rs`; JSON on the `control` data channel only):
//
// | Direction | Message | Meaning |
// | --- | --- | --- |
// | phone to host | `clipboard.get` | read the computer's clipboard text ("Copy from computer") |
// | host to phone | `clipboard {s}` | the answer |
// | phone to host | `clipboard {s}` | put this text on the computer's clipboard ("Paste to computer") |
// | host to phone | `clipboard.ack {bytes}` | it is there |
// | host to phone | `clipboard.refused {op, reason}` | nothing was read or written |
//
// The host allows it only when the owner turned it on (`capabilities.stream.clipboard`), text only,
// at most 64 KiB of UTF-8, and only while the stream is active.

/** The most text one transfer may carry, in UTF-8 bytes. The host refuses more, so the phone does not send it. */
const val CLIPBOARD_MAX_TEXT_BYTES = 64 * 1024

/** How long the phone waits for the host's answer to a clipboard request. */
const val CLIPBOARD_TIMEOUT_MS = 10_000L

/** `op` of `clipboard.refused`: which request was refused. */
@Serializable(with = ClipboardOp.Serializer::class)
enum class ClipboardOp(override val wire: String) : WireEnum {
    /** A `clipboard.get`. */
    GET("get"),

    /** A `clipboard` write. */
    SET("set"),

    UNKNOWN("");

    object Serializer : WireEnumSerializer<ClipboardOp>("ClipboardOp", ClipboardOp.entries, ClipboardOp.UNKNOWN)
}

/** `reason` of `clipboard.refused`; the phone also uses it for what it refuses to send itself. */
@Serializable(with = ClipboardRefusal.Serializer::class)
enum class ClipboardRefusal(override val wire: String) : WireEnum {
    /** The owner's setting does not allow this direction. */
    DISABLED("disabled"),
    HOST_LOCKED("host_locked"),
    PHONE_BACKGROUND("phone_background"),

    /** Another window is in front on the computer. */
    FOCUS_LOST("focus_lost"),

    /** Someone is using the computer's own keyboard or mouse. */
    LOCAL_USER_ACTIVE("local_user_active"),

    /** More than 64 KiB (either side), refused whole. */
    TOO_LARGE("too_large"),

    /** Empty, or the clipboard holds no text. */
    NO_TEXT("no_text"),

    /** The text has a NUL character. */
    INVALID_TEXT("invalid_text"),

    /** Another application kept the computer's clipboard open. */
    BUSY("busy"),

    /** The computer's clipboard failed, or (phone side) the stream cannot take it now. */
    UNAVAILABLE("unavailable"),

    UNKNOWN("");

    object Serializer : WireEnumSerializer<ClipboardRefusal>("ClipboardRefusal", ClipboardRefusal.entries, ClipboardRefusal.UNKNOWN)
}

/** How one clipboard action ended. */
sealed interface ClipboardOutcome {
    /** "Copy from computer" worked: [text] is the computer's clipboard text, for the phone's clipboard. */
    data class Copied(val text: String) : ClipboardOutcome

    /** "Paste to computer" worked: [bytes] of text are on the computer's clipboard. */
    data class Pasted(val bytes: Long) : ClipboardOutcome

    /** The host (or the phone, before sending) refused: nothing was copied. */
    data class Refused(val op: ClipboardOp, val reason: ClipboardRefusal) : ClipboardOutcome

    /** The host did not answer within [CLIPBOARD_TIMEOUT_MS]. */
    data class NoAnswer(val op: ClipboardOp) : ClipboardOutcome
}

/** What the phone checks before it sends [text] to the computer; null when it may be sent. */
fun clipboardSendRefusal(text: String): ClipboardRefusal? = when {
    text.isEmpty() -> ClipboardRefusal.NO_TEXT
    text.toByteArray(Charsets.UTF_8).size > CLIPBOARD_MAX_TEXT_BYTES -> ClipboardRefusal.TOO_LARGE
    else -> null
}
