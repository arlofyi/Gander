package fyi.arlo.gander.ui.stream

import android.content.ClipData
import android.content.ClipDescription
import android.content.ClipboardManager
import android.content.Context
import android.os.Build
import android.os.PersistableBundle
import androidx.annotation.StringRes
import fyi.arlo.gander.R
import fyi.arlo.gander.core.data.stream.ClipboardOp
import fyi.arlo.gander.core.data.stream.ClipboardOutcome
import fyi.arlo.gander.core.data.stream.ClipboardRefusal

// The phone's own clipboard, for the keyboard bar's "Paste to computer" and "Copy from computer"
// (WP-56). Text only. The user taps a button, so Android's clipboard access rules (the app in the
// foreground, an explicit action) are met; nothing here reads or writes it on its own.

/** The phone's clipboard text. Behind an interface so the ViewModel is tested without Android. */
interface PhoneClipboard {
    /** The text on the phone's clipboard, null when there is none (empty, or an image or file). */
    fun readText(): String?

    /** Replaces the phone's clipboard with [text]. */
    fun writeText(text: String)
}

/** The system clipboard. */
class AndroidPhoneClipboard(private val context: Context) : PhoneClipboard {
    private val manager: ClipboardManager? get() = context.getSystemService(ClipboardManager::class.java)

    override fun readText(): String? {
        val clipboard = manager ?: return null
        val description = clipboard.primaryClipDescription ?: return null
        val isText = description.hasMimeType(ClipDescription.MIMETYPE_TEXT_PLAIN) ||
            description.hasMimeType(ClipDescription.MIMETYPE_TEXT_HTML)
        if (!isText) return null
        val clip = clipboard.primaryClip ?: return null
        if (clip.itemCount == 0) return null
        return clip.getItemAt(0).coerceToText(context)?.toString()?.takeIf { it.isNotEmpty() }
    }

    override fun writeText(text: String) {
        val clipboard = manager ?: return
        val clip = ClipData.newPlainText(CLIP_LABEL, text)
        // The text came from another computer: keep it out of the clipboard preview and history.
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            clip.description.extras = PersistableBundle().apply {
                putBoolean(ClipDescription.EXTRA_IS_SENSITIVE, true)
            }
        }
        clipboard.setPrimaryClip(clip)
    }

    private companion object {
        const val CLIP_LABEL = "Gander"
    }
}

/** What the viewer tells the user after a clipboard action: one short message, never the text itself. */
enum class ClipboardNotice(@StringRes val message: Int) {
    COPIED(R.string.stream_clipboard_copied),
    PASTED(R.string.stream_clipboard_pasted),
    PHONE_CLIPBOARD_EMPTY(R.string.stream_clipboard_phone_empty),
    TOO_LARGE(R.string.stream_clipboard_too_large),
    DISABLED(R.string.stream_clipboard_disabled),
    LOCKED(R.string.stream_clipboard_locked),
    NOT_ACTIVE(R.string.stream_clipboard_not_active),
    COMPUTER_HAS_NO_TEXT(R.string.stream_clipboard_computer_empty),
    BUSY(R.string.stream_clipboard_busy),
    FAILED(R.string.stream_clipboard_failed),
    NO_ANSWER(R.string.stream_clipboard_no_answer),
    ;

    companion object {
        /** The message for how an action ended. */
        fun of(outcome: ClipboardOutcome): ClipboardNotice = when (outcome) {
            is ClipboardOutcome.Copied -> COPIED
            is ClipboardOutcome.Pasted -> PASTED
            is ClipboardOutcome.NoAnswer -> NO_ANSWER
            is ClipboardOutcome.Refused -> when (outcome.reason) {
                ClipboardRefusal.DISABLED -> DISABLED
                ClipboardRefusal.HOST_LOCKED -> LOCKED
                ClipboardRefusal.PHONE_BACKGROUND, ClipboardRefusal.FOCUS_LOST, ClipboardRefusal.LOCAL_USER_ACTIVE -> NOT_ACTIVE
                ClipboardRefusal.TOO_LARGE -> TOO_LARGE
                // Nothing to send is the phone's side; nothing to read is the computer's.
                ClipboardRefusal.NO_TEXT, ClipboardRefusal.INVALID_TEXT ->
                    if (outcome.op == ClipboardOp.SET) PHONE_CLIPBOARD_EMPTY else COMPUTER_HAS_NO_TEXT

                ClipboardRefusal.BUSY -> BUSY
                ClipboardRefusal.UNAVAILABLE, ClipboardRefusal.UNKNOWN -> FAILED
            }
        }
    }
}
