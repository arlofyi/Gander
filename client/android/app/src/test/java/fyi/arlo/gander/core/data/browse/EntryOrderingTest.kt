package fyi.arlo.gander.core.data.browse

import fyi.arlo.gander.testing.SampleData.dir
import fyi.arlo.gander.testing.SampleData.file
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class EntryOrderingTest {

    private fun names(entries: List<fyi.arlo.gander.core.net.protocol.Entry>) = entries.map { it.name }

    @Test
    fun naturalOrderComparesDigitRunsByValueAndIgnoresCase() {
        val sorted = listOf("file10.txt", "file2.txt", "File1.txt", "file1a.txt", "file01.txt")
            .sortedWith { a, b -> EntryOrdering.naturalCompare(a, b) }
        // File1 and file01 are equal by value and case; the raw strings break the tie.
        assertEquals(listOf("File1.txt", "file01.txt", "file1a.txt", "file2.txt", "file10.txt"), sorted)
        assertTrue(EntryOrdering.naturalCompare("file2", "file10") < 0)
        assertTrue(EntryOrdering.naturalCompare("a", "B") < 0)
        assertTrue(EntryOrdering.naturalCompare("abc", "abcd") < 0)
        assertEquals(0, EntryOrdering.naturalCompare("same", "same"))
    }

    @Test
    fun hugeDigitRunsDoNotOverflow() {
        val a = "x99999999999999999999999999"
        val b = "x100000000000000000000000000"
        assertTrue(EntryOrdering.naturalCompare(a, b) < 0)
    }

    @Test
    fun directoriesComeFirstForEverySortAndOrder() {
        val entries = listOf(file("a.txt"), dir("zeta"), file("b.txt"), dir("alpha"))
        for (field in SortField.entries) {
            for (order in SortOrder.entries) {
                val sorted = EntryOrdering.apply(entries, ListQuery(SortSpec(field, order)))
                assertEquals("dirs first for $field $order", listOf(true, true, false, false), sorted.map { it.kind.wire == "dir" })
            }
        }
    }

    @Test
    fun nameAscendingAndDescendingReverseTheFilesOnly() {
        val entries = listOf(file("b2"), file("b10"), dir("d"), file("a1"))
        assertEquals(listOf("d", "a1", "b2", "b10"), names(EntryOrdering.apply(entries, ListQuery())))
        assertEquals(
            listOf("d", "b10", "b2", "a1"),
            names(EntryOrdering.apply(entries, ListQuery(SortSpec(SortField.NAME, SortOrder.DESC)))),
        )
    }

    @Test
    fun sortsBySizeAndModified() {
        val entries = listOf(
            file("mid", size = 50, mtime = "2026-10-02T00:00:00Z"),
            file("big", size = 500, mtime = "2026-10-01T00:00:00Z"),
            file("small", size = 5, mtime = "2026-10-03T00:00:00Z"),
        )
        assertEquals(listOf("small", "mid", "big"), names(EntryOrdering.apply(entries, ListQuery(SortSpec(SortField.SIZE)))))
        assertEquals(
            listOf("small", "mid", "big"),
            names(EntryOrdering.apply(entries, ListQuery(SortSpec(SortField.MTIME, SortOrder.DESC)))),
        )
        assertEquals(
            listOf("big", "mid", "small"),
            names(EntryOrdering.apply(entries, ListQuery(SortSpec(SortField.MTIME, SortOrder.ASC)))),
        )
    }

    @Test
    fun sortsByKindThenName() {
        val entries = listOf(file("z.pdf"), file("a.txt"), file("b.pdf"), file("noext"))
        assertEquals(
            listOf("noext", "b.pdf", "z.pdf", "a.txt"),
            names(EntryOrdering.apply(entries, ListQuery(SortSpec(SortField.KIND)))),
        )
    }

    @Test
    fun kindsFilterKeepsOnlyFilesOrOnlyFolders() {
        val entries = listOf(file("a.txt"), dir("d"))
        assertEquals(listOf("a.txt"), names(EntryOrdering.apply(entries, ListQuery(kinds = KindsFilter.FILES))))
        assertEquals(listOf("d"), names(EntryOrdering.apply(entries, ListQuery(kinds = KindsFilter.FOLDERS))))
    }

    @Test
    fun extensionFilterKeepsFoldersAndMatchingFiles() {
        val entries = listOf(file("a.pdf"), file("b.txt"), dir("d"), file("c.PDF"))
        val result = EntryOrdering.apply(entries, ListQuery(ext = listOf("pdf")))
        assertEquals(listOf("d", "a.pdf", "c.PDF"), names(result))
    }
}
