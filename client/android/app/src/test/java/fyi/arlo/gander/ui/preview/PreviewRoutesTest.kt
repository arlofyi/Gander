package fyi.arlo.gander.ui.preview

import fyi.arlo.gander.core.net.GanderJson
import fyi.arlo.gander.core.net.protocol.Entry
import fyi.arlo.gander.core.net.protocol.EntryMeta
import fyi.arlo.gander.core.net.protocol.PdfMeta
import fyi.arlo.gander.core.net.protocol.PreviewKind
import fyi.arlo.gander.testing.SampleData
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Test

/** The route arguments of the preview destination, decoded back into the file sheet's target. */
class PreviewRoutesTest {
    private val entry = SampleData.file("Report #1 (final).pdf", size = 2_000_000, preview = PreviewKind.PDF)
        .copy(meta = EntryMeta(pdf = PdfMeta(pages = 42)))

    private fun json(e: Entry) = GanderJson.json.encodeToString(Entry.serializer(), e)

    @Test
    fun theTargetRoundTripsWithItsMetadata() {
        val target = PreviewRoutes.target("host_A", "shr_1", "Clients/été/Report #1 (final).pdf", json(entry))!!
        assertEquals("host_A", target.hostId)
        assertEquals("shr_1", target.shareId)
        assertEquals("Clients/été/Report #1 (final).pdf", target.path)
        assertEquals(entry, target.entry)
        assertEquals(42, target.entry.meta?.pdf?.pages)
    }

    @Test
    fun theRootOfAShareIsAValidEmptyPath() {
        assertNotNull(PreviewRoutes.target("host_A", "shr_1", "", json(entry)))
    }

    @Test
    fun damagedArgumentsCloseTheViewerInsteadOfCrashingIt() {
        assertNull(PreviewRoutes.target(null, "shr_1", "a", json(entry)))
        assertNull(PreviewRoutes.target("h", null, "a", json(entry)))
        assertNull(PreviewRoutes.target("h", "s", null, json(entry)))
        assertNull(PreviewRoutes.target("h", "s", "a", null))
        assertNull(PreviewRoutes.target("h", "s", "a", "{not json"))
        assertNull(PreviewRoutes.target("h", "s", "a", """{"name":"x"}"""))
    }
}
