package fyi.arlo.gander.core.data.preview

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

class PreviewTileRefTest {
    private fun ref(
        version: String = "2026-10-30T09:12:44Z#3500000",
        page: Int = 2,
        level: Int = 3,
        x: Int = 1,
        y: Int = 4,
        path: String = "Clients/Acme/Plan|A1.pdf",
    ) = PreviewTileRef("host_A", "shr_1", path, version, page, level, x, y)

    @Test
    fun theKeyHasTheDocumentedShapeWithThePathLast() {
        assertEquals(
            "gander-tile/1|host_A|shr_1|2|3|1|4|2026-10-30T09:12:44Z#3500000|Clients/Acme/Plan|A1.pdf",
            ref().cacheKey(),
        )
    }

    @Test
    fun anEditedFileHasNewKeys() {
        assertNotEquals(ref().cacheKey(), ref(version = "2026-10-31T10:00:00Z#3500001").cacheKey())
        assertTrue("mtime#size is in the key", ref().cacheKey().contains("2026-10-30T09:12:44Z#3500000"))
    }

    @Test
    fun everyCoordinateIsPartOfTheKey() {
        val keys = setOf(
            ref().cacheKey(),
            ref(page = 3).cacheKey(),
            ref(level = 4).cacheKey(),
            ref(x = 2).cacheKey(),
            ref(y = 5).cacheKey(),
        )
        assertEquals(5, keys.size)
        // 1|12 and 11|2 must not collide
        assertNotEquals(ref(x = 1, y = 12).cacheKey(), ref(x = 11, y = 2).cacheKey())
    }

    @Test
    fun aTileIsNeverAPageKey() {
        val page = PreviewPageRef("host_A", "shr_1", "Clients/Acme/Plan|A1.pdf", "v", 2, 1080)
        assertFalse(ref().cacheKey().startsWith("gander-preview/"))
        assertNotEquals(page.cacheKey(), ref().cacheKey())
    }

    @Test
    fun theTextFormKeepsThePathOutOfLogs() {
        val text = ref().toString()
        assertFalse(text, text.contains("Plan"))
        assertFalse(text, text.contains("Acme"))
        assertTrue(text, text.contains("level=3") && text.contains("x=1") && text.contains("y=4"))
    }

    @Test
    fun theBearerTokenHasNowhereToLive() {
        // the model carries hosts, shares, paths and coordinates only; a token cannot reach a key
        val fields = PreviewTileRef::class.java.declaredFields.map { it.name }.toSet()
        assertFalse(fields.any { it.contains("token", ignoreCase = true) })
    }

    @Test
    fun impossibleCoordinatesAreRefused() {
        for (bad in listOf<() -> PreviewTileRef>({ ref(page = 0) }, { ref(level = -1) }, { ref(x = -1) }, { ref(y = -1) })) {
            try {
                bad()
                fail("expected IllegalArgumentException")
            } catch (_: IllegalArgumentException) {
                // expected
            }
        }
    }
}
