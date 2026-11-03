package fyi.arlo.gander.core.data.preview

import okhttp3.Headers
import okio.ByteString.Companion.encodeUtf8
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** How a `preview/text` answer becomes a slice, with the optional headers present or absent. */
class TextChunkTest {
    private fun chunk(offset: Long, body: String, vararg headers: Pair<String, String>): TextChunk {
        val builder = Headers.Builder()
        headers.forEach { (name, value) -> builder.add(name, value) }
        return TextChunk.fromResponse(offset, body.encodeUtf8(), builder.build())
    }

    @Test
    fun withTheNextHeaderTheSliceContinuesWhereTheHostSays() {
        // the body had an invalid byte that the host replaced by a 3 byte U+FFFD: next is not offset + size
        val c = chunk(0, "ab", "X-Gander-Text-Next" to "4", "X-Gander-Text-Total" to "10")
        assertEquals(4, c.nextOffset)
        assertTrue(c.hasMore)
    }

    @Test
    fun withoutTheNextHeaderTheBytesReceivedDecide() {
        val c = chunk(100, "héllo", "X-Gander-Text-Total" to "1000")
        assertEquals("é is two bytes", 106, c.nextOffset)
        assertTrue(c.hasMore)
    }

    @Test
    fun theLastSliceHasNoMore() {
        val c = chunk(0, "abc", "X-Gander-Text-Total" to "3", "X-Gander-Text-Encoding" to "utf-8")
        assertEquals(3, c.nextOffset)
        assertFalse(c.hasMore)
        assertEquals("utf-8", c.encoding)
        assertEquals(3L, c.totalBytes)
    }

    @Test
    fun withoutATotalANonEmptySliceMayHaveMoreAndAnEmptyOneEndsIt() {
        assertTrue(chunk(0, "abc").hasMore)
        assertNull(chunk(0, "abc").totalBytes)
        assertFalse(chunk(3, "").hasMore)
    }

    @Test
    fun aNextOffsetThatDoesNotAdvanceNeverLoopsForever() {
        // a host bug: it says "continue at the offset you just asked for". Stop instead of asking again.
        val stuck = chunk(50, "x", "X-Gander-Text-Next" to "50", "X-Gander-Text-Total" to "9999")
        assertEquals(50, stuck.nextOffset)
        assertFalse(stuck.hasMore)

        // a next before the offset is nonsense and ignored: the bytes received decide
        val behind = chunk(50, "x", "X-Gander-Text-Next" to "10", "X-Gander-Text-Total" to "9999")
        assertEquals(51, behind.nextOffset)
        assertTrue(behind.hasMore)
    }

    @Test
    fun unreadableHeadersAreIgnored() {
        val c = chunk(
            0,
            "abc",
            "X-Gander-Text-Total" to "lots",
            "X-Gander-Text-Next" to "soon",
            "X-Gander-Text-Encoding" to "  ",
        )
        assertEquals(3, c.nextOffset)
        assertNull(c.totalBytes)
        assertNull(c.encoding)
        assertTrue(c.hasMore)
    }

    @Test
    fun textIsDecodedAsUtf8() {
        assertEquals("Grüße 日本語", chunk(0, "Grüße 日本語").text)
    }
}
