package fyi.arlo.gander.core.data.documents

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** Files-app document ids (WP-60): stable, round-tripping, strict, and built from ids only. */
class DocumentIdsTest {
    private val host = "q1Wb3x9Hn2rTn5aC8xLmY0pQe7vKdS4uJw6zFh1oIgE"
    private val share = "shr_01JBX3F9Z6Q2K8M4N7P5R0S1T"

    @Test
    fun everyKindRoundTrips() {
        for (ref in listOf(
            DocumentRef.Host(host),
            DocumentRef.Item(host, share, ""),
            DocumentRef.Item(host, share, "photos/2026/IMG_0001.jpg"),
            DocumentRef.Item(host, share, "odd: names/with spaces/ünïcødé ✓/a:b"),
        )) {
            assertEquals(ref, DocumentIds.decode(DocumentIds.encode(ref)))
        }
    }

    @Test
    fun theSameFileAlwaysGetsTheSameId() {
        val a = DocumentIds.item(host, share, "docs/report.pdf")
        val b = DocumentIds.item(host, share, "docs/report.pdf")
        assertEquals(a, b)
        assertTrue(a.startsWith("g1:"))
        // No raw separators from the path survive into the id: it is opaque.
        assertFalse(a.substringAfter("g1:").contains('/'))
    }

    @Test
    fun theIdOnlyCarriesHostShareAndPath() {
        // The encoder has no way to take a token; an id decodes to exactly three strings.
        val ref = DocumentIds.decode(DocumentIds.item(host, share, "a/b")) as DocumentRef.Item
        assertEquals(listOf(host, share, "a/b"), listOf(ref.hostId, ref.shareId, ref.path))
        assertEquals(4, DocumentIds.item(host, share, "a/b").split(':').size)
    }

    @Test
    fun malformedOrForgedIdsAreRejected() {
        val good = DocumentIds.item(host, share, "a/b")
        val bad = listOf(
            "",
            "locked",
            "g2:" + good.substringAfter("g1:"),
            "$good:extra",
            good.substringBeforeLast(':'),
            "g1:", // empty host
            "g1:YQ:Yg:Yw=", // padding is not canonical
            "g1:YQ:Yg:Y+", // not base64url
            DocumentIds.item(host, share, "a/b") + "x",
        )
        for (id in bad) assertNull(id, DocumentIds.decode(id))
        for (path in listOf("../etc", "a/../b", "a//b", "/a", "a/", "./a", "a\\b", "a\u0000b")) {
            assertNull(path, DocumentIds.decode(DocumentIds.item(host, share, path)))
        }
    }

    @Test
    fun childrenAreFoundAtAnyDepthButNeverAcrossSharesOrHosts() {
        val hostRoot = DocumentRef.Host(host)
        val shareRoot = DocumentRef.Item(host, share, "")
        val folder = DocumentRef.Item(host, share, "a")
        val deep = DocumentRef.Item(host, share, "a/b/c.txt")
        val sibling = DocumentRef.Item(host, share, "ab/c.txt")
        assertTrue(DocumentIds.isDescendant(hostRoot, shareRoot))
        assertTrue(DocumentIds.isDescendant(hostRoot, deep))
        assertTrue(DocumentIds.isDescendant(shareRoot, folder))
        assertTrue(DocumentIds.isDescendant(folder, deep))
        assertFalse(DocumentIds.isDescendant(folder, sibling))
        assertFalse(DocumentIds.isDescendant(folder, folder))
        assertFalse(DocumentIds.isDescendant(shareRoot, shareRoot))
        assertFalse(DocumentIds.isDescendant(deep, folder))
        assertFalse(DocumentIds.isDescendant(shareRoot, DocumentRef.Item(host, "shr_other", "a")))
        assertFalse(DocumentIds.isDescendant(hostRoot, DocumentRef.Item("other", share, "a")))
        assertFalse(DocumentIds.isDescendant(shareRoot, hostRoot))
    }
}
