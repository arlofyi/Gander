package fyi.arlo.gander.core.data.preview

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class PreviewPageRefTest {
    private fun ref(
        path: String = "Docs/Report.pdf",
        version: String = "2026-10-30T09:12:44Z#2048",
        page: Int = 3,
        width: Int = 1080,
    ) = PreviewPageRef("host_A", "shr_1", path, version, page, width)

    @Test
    fun theKeyNamesFileVersionPageAndWidth() {
        assertEquals("gander-preview/1|host_A|shr_1|1080|3|2026-10-30T09:12:44Z#2048|Docs/Report.pdf", ref().cacheKey())
    }

    @Test
    fun everyInputThatChangesThePictureChangesTheKey() {
        val base = ref().cacheKey()
        assertNotEquals("another page", base, ref(page = 4).cacheKey())
        assertNotEquals("another width", base, ref(width = 720).cacheKey())
        assertNotEquals("an edited file", base, ref(version = "2026-11-01T00:00:00Z#2048").cacheKey())
        assertNotEquals("another file", base, ref(path = "Docs/Other.pdf").cacheKey())
    }

    @Test
    fun aPathWithTheSeparatorCannotImpersonateAnotherKey() {
        // the path is last, so a `|` inside it cannot move into the fixed fields
        assertNotEquals(ref(path = "a|b").cacheKey(), ref(path = "a", version = "b|v").cacheKey())
    }

    @Test
    fun fileNamesStayOutOfLogs() {
        val text = ref(path = "Private/Divorce.pdf").toString()
        assertFalse(text.contains("Divorce"))
        assertTrue(text.contains("page=3"))
    }

    @Test(expected = IllegalArgumentException::class)
    fun pagesStartAtOne() {
        ref(page = 0)
    }

    @Test
    fun theWidthIsTheViewportClampedToTheHostsMaximum() {
        assertEquals("a phone", 1080, PreviewWidths.pick(1080, hostMax = 2048))
        assertEquals("a tablet, capped by the host", 2048, PreviewWidths.pick(2560, hostMax = 2048))
        assertEquals("a host with a lower cap", 1000, PreviewWidths.pick(1440, hostMax = 1000))
        assertEquals("capabilities not known yet", 2048, PreviewWidths.pick(4000, hostMax = null))
        assertEquals("not measured yet", PreviewWidths.MIN, PreviewWidths.pick(1, hostMax = 2048))
        assertEquals("nonsense from a host", PreviewWidths.MIN, PreviewWidths.pick(10, hostMax = 0))
    }
}
