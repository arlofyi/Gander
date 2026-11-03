package fyi.arlo.gander.core.data.browse

import fyi.arlo.gander.core.net.protocol.Entry
import fyi.arlo.gander.core.net.protocol.SearchVia

/** `sort` query values of `GET /shares/{share}/list`. */
enum class SortField(val wire: String) {
    NAME("name"),
    MTIME("mtime"),
    SIZE("size"),
    KIND("kind"),
}

enum class SortOrder(val wire: String) {
    ASC("asc"),
    DESC("desc"),
    ;

    fun toggled(): SortOrder = if (this == ASC) DESC else ASC
}

/** How a folder is sorted. Directories always come first (protocol, unless `kinds=file`). */
data class SortSpec(val field: SortField = SortField.NAME, val order: SortOrder = SortOrder.ASC)

/** The `kinds` filter: everything, only folders (`dir`) or only files (`file`). */
enum class KindsFilter(val wire: String?) {
    ALL(null),
    FILES("file"),
    FOLDERS("dir"),
}

enum class ViewMode { LIST, GRID }

/** What a listing request asks for besides the folder. Two pages of one listing share one query. */
data class ListQuery(
    val sort: SortSpec = SortSpec(),
    val kinds: KindsFilter = KindsFilter.ALL,
    /** Lower-case extensions without the dot (`ext=pdf,pptx`); empty means any. */
    val ext: List<String> = emptyList(),
) {
    val extWire: String? get() = ext.takeIf { it.isNotEmpty() }?.joinToString(",")
}

/** A value from the host or, when the host cannot be reached, from the last snapshot of it. */
data class Loaded<out T>(val value: T, val cachedAtMillis: Long? = null) {
    val isCached: Boolean get() = cachedAtMillis != null
}

/** One page of a folder listing. [cachedAtMillis] is set when it came from a snapshot, offline. */
data class PageResult(
    val entries: List<Entry>,
    val nextCursor: String?,
    /** The host stopped listing at its size limit; the folder has more than it will show. */
    val truncated: Boolean = false,
    /** Entries the host could not read (permissions, reparse points); not shown. */
    val unlistable: Int = 0,
    val cachedAtMillis: Long? = null,
    /** Cached page of a folder whose listing was longer than what was stored: more exists on the host. */
    val cacheIncomplete: Boolean = false,
)

data class SearchPage(
    val entries: List<Entry>,
    val partial: Boolean,
    val nextCursor: String?,
    /** What answered (the index or the walk); null from a host that predates it. */
    val via: SearchVia? = null,
)

data class RecentPage(val entries: List<Entry>, val partial: Boolean)

/**
 * A file or folder the user picked, with everything later screens need. The file sheet (WP-25)
 * and the fetch queue (WP-27) take this: [path] is share-relative with `/` separators and is
 * what goes into `?path=`.
 */
data class EntryTarget(
    val hostId: String,
    val shareId: String,
    val path: String,
    val entry: Entry,
)

/** `parent` + `name` as a share-relative path (`""` is the share root). */
fun childPath(parent: String, name: String): String = if (parent.isEmpty()) name else "$parent/$name"

/** The folder part of a share-relative path (`""` for a top-level entry). */
fun parentPath(path: String): String = path.substringBeforeLast('/', missingDelimiterValue = "")

/** Share-relative path of [entry] listed in folder [parent]; search and recent carry it themselves. */
fun Entry.fullPath(parent: String): String = path ?: childPath(parent, name)
