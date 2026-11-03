package fyi.arlo.gander.ui.filesheet

import java.time.ZoneId
import java.util.Locale
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class FileSheetFormatTest {
    private val utc = ZoneId.of("UTC")

    @Test
    fun durationsReadAsMinutesAndSecondsThenHours() {
        assertEquals("0:00", formatDuration(0))
        assertEquals("0:01", formatDuration(1_000))
        assertEquals("3:05", formatDuration(185_000))
        assertEquals("59:59", formatDuration(3_599_000))
        assertEquals("1:00:00", formatDuration(3_600_000))
        assertEquals("1:02:03", formatDuration(3_723_000))
    }

    @Test
    fun durationsRoundToTheNearestSecond() {
        assertEquals("0:01", formatDuration(1_400))
        assertEquals("0:02", formatDuration(1_500))
        assertEquals("0:00", formatDuration(-5_000))
    }

    @Test
    fun anIsoInstantIsShownInTheGivenZone() {
        val text = formatDateTime("2026-10-30T09:12:44Z", utc, Locale.US)
        assertTrue(text, text.contains("Oct 30, 2026"))
        assertTrue(text, text.contains("9:12"))
        val tokyo = formatDateTime("2026-10-30T23:30:00Z", ZoneId.of("Asia/Tokyo"), Locale.US)
        assertTrue(tokyo, tokyo.contains("Oct 31, 2026"))
    }

    @Test
    fun anUnreadableOrMissingDateIsEmptyNotACrash() {
        assertEquals("", formatDateTime(null, utc, Locale.US))
        assertEquals("", formatDateTime("yesterday", utc, Locale.US))
    }

    @Test
    fun anExifTimeIsParsedWithoutAZone() {
        val text = formatExifTime("2026:10:28 18:40:01", Locale.US)
        assertTrue(text, text.contains("Oct 28, 2026"))
        assertTrue(text, text.contains("6:40"))
    }

    @Test
    fun exifTextInAnotherLayoutIsShownAsItCame() {
        assertEquals("sometime in autumn", formatExifTime("  sometime in autumn ", Locale.US))
        assertEquals("", formatExifTime(null, Locale.US))
        assertEquals("", formatExifTime("  ", Locale.US))
    }
}
