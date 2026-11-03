package fyi.arlo.gander.core.data.browse

import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.update

/** What the browser remembers per share (client spec: "Sort ... Persisted per share"). */
data class BrowserPrefs(
    val sort: SortSpec = SortSpec(),
    val viewMode: ViewMode = ViewMode.LIST,
)

interface BrowserPrefsRepository {
    /** The stored prefs of one share, defaults when nothing is stored; emits on every change. */
    fun observe(hostId: String, shareId: String): Flow<BrowserPrefs>

    suspend fun get(hostId: String, shareId: String): BrowserPrefs = observe(hostId, shareId).first()

    suspend fun setSort(hostId: String, shareId: String, sort: SortSpec)

    suspend fun setViewMode(hostId: String, shareId: String, mode: ViewMode)

    /** Forgets the sort and view mode of every share of [hostId] (the computer was forgotten). */
    suspend fun forgetHost(hostId: String)
}

/**
 * [BrowserPrefsRepository] on the app's settings DataStore: one string value per share,
 * `<sort>|<order>|<view>` (for example `mtime|desc|grid`). A value this version cannot parse
 * (a newer app wrote it) reads as the defaults rather than failing.
 */
class DataStoreBrowserPrefs(private val store: DataStore<Preferences>) : BrowserPrefsRepository {

    override fun observe(hostId: String, shareId: String): Flow<BrowserPrefs> {
        val key = key(hostId, shareId)
        return store.data.map { decode(it[key]) }.distinctUntilChanged()
    }

    override suspend fun setSort(hostId: String, shareId: String, sort: SortSpec) =
        update(hostId, shareId) { it.copy(sort = sort) }

    override suspend fun setViewMode(hostId: String, shareId: String, mode: ViewMode) =
        update(hostId, shareId) { it.copy(viewMode = mode) }

    override suspend fun forgetHost(hostId: String) {
        val prefix = "$PREFIX$hostId."
        store.edit { prefs ->
            prefs.asMap().keys.filter { it.name.startsWith(prefix) }.forEach { key -> prefs.remove(key) }
        }
    }

    private suspend fun update(hostId: String, shareId: String, change: (BrowserPrefs) -> BrowserPrefs) {
        val key = key(hostId, shareId)
        store.edit { prefs -> prefs[key] = encode(change(decode(prefs[key]))) }
    }

    internal companion object {
        private const val PREFIX = "browser."

        fun key(hostId: String, shareId: String) = stringPreferencesKey("$PREFIX$hostId.$shareId")

        fun encode(prefs: BrowserPrefs): String =
            "${prefs.sort.field.wire}|${prefs.sort.order.wire}|${prefs.viewMode.name.lowercase()}"

        fun decode(raw: String?): BrowserPrefs {
            val parts = raw?.split('|') ?: return BrowserPrefs()
            if (parts.size != 3) return BrowserPrefs()
            val field = SortField.entries.firstOrNull { it.wire == parts[0] } ?: SortField.NAME
            val order = SortOrder.entries.firstOrNull { it.wire == parts[1] } ?: SortOrder.ASC
            val view = ViewMode.entries.firstOrNull { it.name.equals(parts[2], ignoreCase = true) } ?: ViewMode.LIST
            return BrowserPrefs(SortSpec(field, order), view)
        }
    }
}

/** Test and preview implementation. */
class InMemoryBrowserPrefs : BrowserPrefsRepository {
    private val state = MutableStateFlow<Map<Pair<String, String>, BrowserPrefs>>(emptyMap())

    override fun observe(hostId: String, shareId: String): Flow<BrowserPrefs> =
        state.map { it[hostId to shareId] ?: BrowserPrefs() }.distinctUntilChanged()

    override suspend fun setSort(hostId: String, shareId: String, sort: SortSpec) =
        change(hostId, shareId) { it.copy(sort = sort) }

    override suspend fun setViewMode(hostId: String, shareId: String, mode: ViewMode) =
        change(hostId, shareId) { it.copy(viewMode = mode) }

    override suspend fun forgetHost(hostId: String) {
        state.update { map -> map.filterKeys { it.first != hostId } }
    }

    private fun change(hostId: String, shareId: String, f: (BrowserPrefs) -> BrowserPrefs) {
        state.update { map ->
            val key = hostId to shareId
            map + (key to f(map[key] ?: BrowserPrefs()))
        }
    }
}
