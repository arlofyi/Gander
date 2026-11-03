package fyi.arlo.gander.ui.stream

import fyi.arlo.gander.core.net.protocol.InputMessage
import fyi.arlo.gander.core.net.protocol.KeyModifier

/** W3C UI Events `code` values of the keys the keyboard bar sends. */
object KeyCodes {
    const val ESCAPE = "Escape"
    const val TAB = "Tab"
    const val ENTER = "Enter"
    const val BACKSPACE = "Backspace"
    const val DELETE = "Delete"
    const val ARROW_LEFT = "ArrowLeft"
    const val ARROW_RIGHT = "ArrowRight"
    const val ARROW_UP = "ArrowUp"
    const val ARROW_DOWN = "ArrowDown"
    const val KEY_Z = "KeyZ"
    const val KEY_Y = "KeyY"
    const val KEY_S = "KeyS"
    const val SPACE = "Space"

    /** `F1`..`F12`. */
    fun function(n: Int): String {
        require(n in 1..12) { "F1..F12" }
        return "F$n"
    }

    /** The `code` of the key that types [c] on a US layout, or null for a character with no single key. */
    fun forChar(c: Char): String? = when (c) {
        in 'a'..'z' -> "Key${c.uppercaseChar()}"
        in 'A'..'Z' -> "Key$c"
        in '0'..'9' -> "Digit$c"
        ' ' -> SPACE
        else -> null
    }
}

/** What the keyboard bar remembers: the sticky modifiers and whether the Fn row is open. */
data class KeyboardState(
    val sticky: Set<KeyModifier> = emptySet(),
    val functionRow: Boolean = false,
)

/** The messages one tap on the bar sends and the bar's state afterwards. */
data class KeyPress(val state: KeyboardState, val messages: List<InputMessage>)

/**
 * The keyboard bar's logic (client spec: "sticky Ctrl/Alt/Shift, Esc, Tab, arrows, Fn row toggle,
 * Ctrl+Z/Y, Ctrl+S"). A modifier tapped on the bar is sticky for the next key only, the way an
 * on-screen modifier has to be: the phone cannot hold a key down while the other hand types.
 */
object KeyboardBar {
    /** The modifiers the bar offers, in the order they are listed in `mods`. */
    val MODIFIERS = listOf(KeyModifier.CTRL, KeyModifier.SHIFT, KeyModifier.ALT)

    fun toggleSticky(state: KeyboardState, modifier: KeyModifier): KeyboardState =
        state.copy(sticky = if (modifier in state.sticky) state.sticky - modifier else state.sticky + modifier)

    fun toggleFunctionRow(state: KeyboardState): KeyboardState = state.copy(functionRow = !state.functionRow)

    /**
     * A key tap: down then up, both carrying the modifiers (sticky ones plus [extra], such as the
     * Ctrl of a Ctrl+Z button). The sticky modifiers are spent.
     */
    fun press(state: KeyboardState, code: String, extra: Set<KeyModifier> = emptySet()): KeyPress {
        val held = canonical(state.sticky + extra)
        return KeyPress(
            state = state.copy(sticky = emptySet()),
            messages = listOf(InputMessage.Key(code, true, held), InputMessage.Key(code, false, held)),
        )
    }

    /**
     * Text from the phone's keyboard. With no modifier held it is a `text` message (injected as
     * Unicode, so any layout and any IME works). With a sticky modifier a character that has a
     * key becomes that key with the modifier (Ctrl then `c` is Ctrl+C); the modifier is spent.
     */
    fun typed(state: KeyboardState, text: String): KeyPress {
        if (text.isEmpty()) return KeyPress(state, emptyList())
        if (state.sticky.isEmpty()) return KeyPress(state, listOf(InputMessage.Text(text)))
        val first = text.first()
        val code = KeyCodes.forChar(first) ?: return KeyPress(state.copy(sticky = emptySet()), listOf(InputMessage.Text(text)))
        val chord = press(state, code)
        val rest = text.drop(1)
        return KeyPress(chord.state, chord.messages + if (rest.isEmpty()) emptyList() else listOf(InputMessage.Text(rest)))
    }

    private fun canonical(mods: Set<KeyModifier>): List<KeyModifier> =
        listOf(KeyModifier.CTRL, KeyModifier.SHIFT, KeyModifier.ALT, KeyModifier.META).filter { it in mods }
}
