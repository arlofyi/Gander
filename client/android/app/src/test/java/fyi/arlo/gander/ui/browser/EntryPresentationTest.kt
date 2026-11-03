package fyi.arlo.gander.ui.browser

import fyi.arlo.gander.core.net.protocol.EntryFlag
import fyi.arlo.gander.core.net.protocol.PreviewKind
import fyi.arlo.gander.testing.SampleData.dir
import fyi.arlo.gander.testing.SampleData.file
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import java.time.ZoneId
import java.util.Locale

class EntryPresentationTest {

    @Test
    fun fileSizesUseExplorerStyleUnits() {
        assertEquals("0 B", formatFileSize(0))
        assertEquals("0 B", formatFileSize(-5))
        assertEquals("1023 B", formatFileSize(1023))
        assertEquals("1 KB", formatFileSize(1024))
        assertEquals("1.5 KB", formatFileSize(1536))
        assertEquals("12 KB", formatFileSize(12 * 1024))
        assertEquals("46 MB", formatFileSize(48_213_001))
        assertEquals("100 MB", formatFileSize(100L * 1024 * 1024))
        assertEquals("100 MB", formatFileSize(104_857_000)) // 99.9995 MB rounds up, not "100.0"
        assertEquals("1.8 GB", formatFileSize(1_932_735_283))
        assertEquals("3 TB", formatFileSize(3L * 1024 * 1024 * 1024 * 1024))
    }

    @Test
    fun theGlyphFollowsThePreviewKindThenTheExtension() {
        assertEquals(GlyphKind.FOLDER, glyphKind(dir("Photos")))
        assertEquals(GlyphKind.IMAGE, glyphKind(file("x", preview = PreviewKind.IMAGE)))
        assertEquals(GlyphKind.PDF, glyphKind(file("x", preview = PreviewKind.PDF)))
        assertEquals(GlyphKind.OFFICE, glyphKind(file("Deck.pptx", preview = PreviewKind.OFFICE)))
        assertEquals(GlyphKind.VIDEO, glyphKind(file("clip.mp4", preview = PreviewKind.VIDEO_POSTER)))
        // no preview from the host: fall back to the extension, any case
        assertEquals(GlyphKind.VIDEO, glyphKind(file("movie.MKV")))
        assertEquals(GlyphKind.AUDIO, glyphKind(file("song.flac")))
        assertEquals(GlyphKind.ARCHIVE, glyphKind(file("backup.zip")))
        assertEquals(GlyphKind.OFFICE, glyphKind(file("budget.xlsx")))
        assertEquals(GlyphKind.PDF, glyphKind(file("scan.pdf")))
        assertEquals(GlyphKind.TEXT, glyphKind(file("README.md")))
        assertEquals(GlyphKind.FILE, glyphKind(file("model.blend")))
        assertEquals(GlyphKind.FILE, glyphKind(file("noextension")))
    }

    @Test
    fun modifiedDatesAreLocalisedAndTolerant() {
        assertEquals("Oct 30, 2026", formatModified("2026-10-30T09:12:44Z", ZoneId.of("UTC"), Locale.US))
        // the same instant is the next day further east
        assertEquals("Oct 31, 2026", formatModified("2026-10-30T23:30:00Z", ZoneId.of("Asia/Tokyo"), Locale.US))
        assertEquals("", formatModified("not a date", ZoneId.of("UTC"), Locale.US))
        assertEquals("", formatModified("", ZoneId.of("UTC"), Locale.US))
    }

    @Test
    fun onlyPlainFilesAreSelectableAndLinksAreNotOpenable() {
        assertTrue(file("a").isSelectable())
        assertTrue(file("a", flags = listOf(EntryFlag.CLOUD_ONLY)).isSelectable()) // fetch warns, it is not refused
        assertFalse(dir("d").isSelectable())
        assertFalse(file("l", flags = listOf(EntryFlag.LINK)).isSelectable())
        assertTrue(dir("d").isOpenable())
        assertFalse(file("l", flags = listOf(EntryFlag.LINK)).isOpenable())
        assertFalse(dir("d", flags = listOf(EntryFlag.LINK)).isOpenable())
    }
}
