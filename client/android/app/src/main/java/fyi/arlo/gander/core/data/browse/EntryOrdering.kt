package fyi.arlo.gander.core.data.browse

import fyi.arlo.gander.core.net.protocol.Entry
import fyi.arlo.gander.core.net.protocol.EntryKind
import java.util.Locale

/**
 * Client-side mirror of the host's list ordering. The host does the real sorting (it has the whole
 * folder); this exists to re-order a cached snapshot when the user changes sort while offline.
 */
object EntryOrdering {

    /** Filters by [ListQuery.kinds] and extension, then sorts, like the host would. */
    fun apply(entries: List<Entry>, query: ListQuery): List<Entry> {
        val extensions = query.ext.map { it.lowercase(Locale.ROOT).removePrefix(".") }.toSet()
        val filtered = entries.filter { entry ->
            val kindOk = when (query.kinds) {
                KindsFilter.ALL -> true
                KindsFilter.FILES -> entry.kind != EntryKind.DIR
                KindsFilter.FOLDERS -> entry.kind == EntryKind.DIR
            }
            // The ext filter applies to files; folders stay (the host keeps them visible too).
            val extOk = extensions.isEmpty() || entry.kind == EntryKind.DIR ||
                entry.extension() in extensions
            kindOk && extOk
        }
        return filtered.sortedWith(comparator(query.sort))
    }

    /** Directories first, then [sort]; descending reverses the second key only. */
    fun comparator(sort: SortSpec): Comparator<Entry> {
        val primary: Comparator<Entry> = when (sort.field) {
            SortField.NAME -> Comparator { a, b -> naturalCompare(a.name, b.name) }
            SortField.MTIME -> compareBy<Entry> { it.mtime }.thenComparing { a, b -> naturalCompare(a.name, b.name) }
            SortField.SIZE -> compareBy<Entry> { it.size }.thenComparing { a, b -> naturalCompare(a.name, b.name) }
            SortField.KIND -> Comparator<Entry> { a, b -> a.extension().compareTo(b.extension()) }
                .thenComparing { a, b -> naturalCompare(a.name, b.name) }
        }
        val ordered = if (sort.order == SortOrder.DESC) primary.reversed() else primary
        return compareBy<Entry> { if (it.kind == EntryKind.DIR) 0 else 1 }.then(ordered)
    }

    /**
     * Case-insensitive "natural" order: digit runs compare by value (`file2` before `file10`),
     * the rest by code point. Ties fall back to the raw strings so the order is total and stable.
     */
    fun naturalCompare(a: String, b: String): Int {
        var i = 0
        var j = 0
        while (i < a.length && j < b.length) {
            val ca = a[i]
            val cb = b[j]
            if (ca.isDigit() && cb.isDigit()) {
                val endA = digitRunEnd(a, i)
                val endB = digitRunEnd(b, j)
                val runA = a.substring(i, endA).trimStart('0')
                val runB = b.substring(j, endB).trimStart('0')
                if (runA.length != runB.length) return runA.length.compareTo(runB.length)
                val byValue = runA.compareTo(runB)
                if (byValue != 0) return byValue
                i = endA
                j = endB
            } else {
                val byChar = ca.lowercaseChar().compareTo(cb.lowercaseChar())
                if (byChar != 0) return byChar
                i++
                j++
            }
        }
        val byLength = (a.length - i).compareTo(b.length - j)
        return if (byLength != 0) byLength else a.compareTo(b)
    }

    private fun digitRunEnd(s: String, start: Int): Int {
        var end = start
        while (end < s.length && s[end].isDigit()) end++
        return end
    }

    private fun Entry.extension(): String =
        (ext ?: name.substringAfterLast('.', "")).lowercase(Locale.ROOT)
}
