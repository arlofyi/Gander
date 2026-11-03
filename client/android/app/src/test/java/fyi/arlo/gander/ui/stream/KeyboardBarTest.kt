package fyi.arlo.gander.ui.stream

import fyi.arlo.gander.core.net.protocol.InputMessage
import fyi.arlo.gander.core.net.protocol.KeyModifier
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

/** The keyboard bar (client spec): sticky Ctrl/Alt/Shift, Esc, Tab, arrows, Fn row, Ctrl+Z/Y, Ctrl+S, typed text. */
class KeyboardBarTest {
    private fun key(code: String, down: Boolean, vararg mods: KeyModifier) =
        InputMessage.Key(code, down, mods.toList())

    @Test
    fun aKeyIsAPressAndARelease() {
        val press = KeyboardBar.press(KeyboardState(), KeyCodes.ESCAPE)
        assertEquals(listOf<InputMessage>(key("Escape", true), key("Escape", false)), press.messages)
        assertEquals(KeyboardState(), press.state)
    }

    @Test
    fun aStickyModifierIsHeldForTheNextKeyOnlyAndThenSpent() {
        var state = KeyboardBar.toggleSticky(KeyboardState(), KeyModifier.CTRL)
        assertEquals(setOf(KeyModifier.CTRL), state.sticky)
        val press = KeyboardBar.press(state, KeyCodes.function(5))
        assertEquals(listOf<InputMessage>(key("F5", true, KeyModifier.CTRL), key("F5", false, KeyModifier.CTRL)), press.messages)
        assertTrue("spent", press.state.sticky.isEmpty())
        state = press.state
        assertEquals("the next key is plain", listOf<InputMessage>(key("Tab", true), key("Tab", false)), KeyboardBar.press(state, KeyCodes.TAB).messages)
    }

    @Test
    fun aStickyModifierCanBeTappedOffAgain() {
        val on = KeyboardBar.toggleSticky(KeyboardState(), KeyModifier.ALT)
        assertEquals(emptySet<KeyModifier>(), KeyboardBar.toggleSticky(on, KeyModifier.ALT).sticky)
    }

    @Test
    fun severalModifiersAreListedInAFixedOrder() {
        var state = KeyboardState()
        for (m in listOf(KeyModifier.ALT, KeyModifier.SHIFT, KeyModifier.CTRL)) state = KeyboardBar.toggleSticky(state, m)
        val press = KeyboardBar.press(state, KeyCodes.ARROW_LEFT)
        assertEquals(
            listOf(KeyModifier.CTRL, KeyModifier.SHIFT, KeyModifier.ALT),
            (press.messages.first() as InputMessage.Key).mods,
        )
    }

    @Test
    fun theShortcutButtonsAreCtrlChords() {
        for ((code, label) in listOf(KeyCodes.KEY_Z to "KeyZ", KeyCodes.KEY_Y to "KeyY", KeyCodes.KEY_S to "KeyS")) {
            val press = KeyboardBar.press(KeyboardState(), code, setOf(KeyModifier.CTRL))
            assertEquals(listOf<InputMessage>(key(label, true, KeyModifier.CTRL), key(label, false, KeyModifier.CTRL)), press.messages)
        }
    }

    @Test
    fun aShortcutAddsToTheStickyModifiers() {
        val shift = KeyboardBar.toggleSticky(KeyboardState(), KeyModifier.SHIFT)
        val press = KeyboardBar.press(shift, KeyCodes.KEY_Z, setOf(KeyModifier.CTRL))
        assertEquals(listOf(KeyModifier.CTRL, KeyModifier.SHIFT), (press.messages.first() as InputMessage.Key).mods)
        assertTrue(press.state.sticky.isEmpty())
    }

    @Test
    fun theFunctionRowToggles() {
        val open = KeyboardBar.toggleFunctionRow(KeyboardState())
        assertTrue(open.functionRow)
        assertFalse(KeyboardBar.toggleFunctionRow(open).functionRow)
        assertEquals("F1", KeyCodes.function(1))
        assertEquals("F12", KeyCodes.function(12))
        assertThrows(IllegalArgumentException::class.java) { KeyCodes.function(13) }
    }

    @Test
    fun typedTextIsATextMessageWithoutModifiers() {
        val press = KeyboardBar.typed(KeyboardState(), "héllo wörld")
        assertEquals(listOf<InputMessage>(InputMessage.Text("héllo wörld")), press.messages)
        assertTrue(KeyboardBar.typed(KeyboardState(), "").messages.isEmpty())
    }

    @Test
    fun aStickyModifierTurnsTheNextTypedCharacterIntoAChord() {
        val ctrl = KeyboardBar.toggleSticky(KeyboardState(), KeyModifier.CTRL)
        val press = KeyboardBar.typed(ctrl, "c")
        assertEquals(listOf<InputMessage>(key("KeyC", true, KeyModifier.CTRL), key("KeyC", false, KeyModifier.CTRL)), press.messages)
        assertTrue(press.state.sticky.isEmpty())
    }

    @Test
    fun onlyTheFirstCharacterTakesTheModifier() {
        val ctrl = KeyboardBar.toggleSticky(KeyboardState(), KeyModifier.CTRL)
        val press = KeyboardBar.typed(ctrl, "cat")
        assertEquals(
            listOf<InputMessage>(key("KeyC", true, KeyModifier.CTRL), key("KeyC", false, KeyModifier.CTRL), InputMessage.Text("at")),
            press.messages,
        )
    }

    @Test
    fun aCharacterWithNoKeyIsStillTyped() {
        val ctrl = KeyboardBar.toggleSticky(KeyboardState(), KeyModifier.CTRL)
        val press = KeyboardBar.typed(ctrl, "é")
        assertEquals(listOf<InputMessage>(InputMessage.Text("é")), press.messages)
        assertTrue("the modifier is spent either way", press.state.sticky.isEmpty())
    }

    @Test
    fun charactersMapToUsKeyCodes() {
        assertEquals("KeyA", KeyCodes.forChar('a'))
        assertEquals("KeyA", KeyCodes.forChar('A'))
        assertEquals("Digit5", KeyCodes.forChar('5'))
        assertEquals("Space", KeyCodes.forChar(' '))
        assertNull(KeyCodes.forChar('!'))
        assertNull(KeyCodes.forChar('é'))
    }

    @Test
    fun theBarOffersCtrlShiftAndAlt() {
        assertEquals(listOf(KeyModifier.CTRL, KeyModifier.SHIFT, KeyModifier.ALT), KeyboardBar.MODIFIERS)
    }
}
