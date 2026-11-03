package fyi.arlo.gander.core.data.stream

import fyi.arlo.gander.core.net.GanderJson
import fyi.arlo.gander.core.net.protocol.ClipboardMode
import fyi.arlo.gander.core.net.protocol.StreamCapabilities
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** The clipboard bridge's phone-side rules (WP-56): what the host advertises, and what is sent. */
class StreamClipboardTest {
    private fun capabilities(extra: String): StreamCapabilities = GanderJson.json.decodeFromString(
        StreamCapabilities.serializer(),
        """{"enabled":true,"codecs":["h264"],"max_sessions":1,"audio":false,"input":true,"fit_to_phone":true$extra}""",
    )

    @Test
    fun aHostThatSaysNothingOffersNoClipboard() {
        val caps = capabilities("")
        assertEquals(ClipboardMode.OFF, caps.clipboard)
        assertFalse(caps.clipboard.canCopyFromComputer)
        assertFalse(caps.clipboard.canPasteToComputer)
    }

    @Test
    fun eachAdvertisedModeAllowsItsDirections() {
        fun mode(wire: String) = capabilities(""","clipboard":"$wire"""").clipboard
        assertEquals(ClipboardMode.TO_PHONE, mode("to_phone"))
        assertTrue(mode("to_phone").canCopyFromComputer)
        assertFalse("to_phone does not let the phone write", mode("to_phone").canPasteToComputer)

        assertTrue(mode("from_phone").canPasteToComputer)
        assertFalse("from_phone does not let the phone read", mode("from_phone").canCopyFromComputer)

        assertTrue(mode("both").canCopyFromComputer && mode("both").canPasteToComputer)
        assertFalse(mode("off").canCopyFromComputer || mode("off").canPasteToComputer)
    }

    @Test
    fun aModeFromANewerHostAllowsNothing() {
        val unknown = capabilities(""","clipboard":"images_too"""").clipboard
        assertEquals(ClipboardMode.UNKNOWN, unknown)
        assertFalse(unknown.canCopyFromComputer || unknown.canPasteToComputer)
    }

    @Test
    fun theTextCapIsSixtyFourKibOfUtf8() {
        assertEquals(65_536, CLIPBOARD_MAX_TEXT_BYTES)
        assertNull(clipboardSendRefusal("hello"))
        assertNull("exactly the cap", clipboardSendRefusal("a".repeat(CLIPBOARD_MAX_TEXT_BYTES)))
        assertEquals(ClipboardRefusal.TOO_LARGE, clipboardSendRefusal("a".repeat(CLIPBOARD_MAX_TEXT_BYTES + 1)))
        assertEquals(ClipboardRefusal.NO_TEXT, clipboardSendRefusal(""))
    }

    @Test
    fun theCapCountsBytesNotCharacters() {
        // 3 bytes each: 21 845 characters are 65 535 bytes, one more is over.
        val euro = "\u20AC"
        assertNull(clipboardSendRefusal(euro.repeat(21_845)))
        assertEquals(ClipboardRefusal.TOO_LARGE, clipboardSendRefusal(euro.repeat(21_846)))
        // An emoji is 4 bytes (a surrogate pair in a Kotlin string).
        assertEquals(ClipboardRefusal.TOO_LARGE, clipboardSendRefusal("\uD83D\uDE00".repeat(16_385)))
        assertNull(clipboardSendRefusal("\uD83D\uDE00".repeat(16_384)))
    }
}
