package fyi.arlo.gander.core.data.browse

import androidx.datastore.preferences.core.edit
import fyi.arlo.gander.testing.InMemoryPreferencesDataStore
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Test

/** "Sort persisted per share": per-share keys, the stored string, and tolerance of unknown values. */
class BrowserPrefsTest {
    private val store = InMemoryPreferencesDataStore()
    private val prefs = DataStoreBrowserPrefs(store)

    @Test
    fun defaultsAreNameAscendingAsAList() = runBlocking {
        assertEquals(BrowserPrefs(SortSpec(SortField.NAME, SortOrder.ASC), ViewMode.LIST), prefs.get("host", "share"))
    }

    @Test
    fun sortAndViewModeArePersistedPerShare() = runBlocking {
        prefs.setSort("host", "photos", SortSpec(SortField.MTIME, SortOrder.DESC))
        prefs.setViewMode("host", "photos", ViewMode.GRID)
        prefs.setSort("host", "docs", SortSpec(SortField.SIZE, SortOrder.ASC))

        assertEquals(BrowserPrefs(SortSpec(SortField.MTIME, SortOrder.DESC), ViewMode.GRID), prefs.get("host", "photos"))
        assertEquals(BrowserPrefs(SortSpec(SortField.SIZE, SortOrder.ASC), ViewMode.LIST), prefs.get("host", "docs"))
        // another host with the same share id is its own entry
        assertEquals(BrowserPrefs(), prefs.get("other-host", "photos"))
    }

    @Test
    fun changingTheSortKeepsTheViewModeAndTheOtherWayRound() = runBlocking {
        prefs.setViewMode("h", "s", ViewMode.GRID)
        prefs.setSort("h", "s", SortSpec(SortField.KIND, SortOrder.DESC))
        assertEquals(BrowserPrefs(SortSpec(SortField.KIND, SortOrder.DESC), ViewMode.GRID), prefs.get("h", "s"))
    }

    @Test
    fun aNewRepositoryOnTheSameStoreSeesWhatWasSaved() = runBlocking {
        prefs.setSort("h", "s", SortSpec(SortField.SIZE, SortOrder.DESC))
        prefs.setViewMode("h", "s", ViewMode.GRID)
        // a fresh repository (a new session of the app) reads the same stored value
        val later = DataStoreBrowserPrefs(store)
        assertEquals(BrowserPrefs(SortSpec(SortField.SIZE, SortOrder.DESC), ViewMode.GRID), later.get("h", "s"))
    }

    @Test
    fun theStoredValueIsOneReadableStringPerShare() = runBlocking {
        prefs.setSort("h", "s", SortSpec(SortField.MTIME, SortOrder.DESC))
        prefs.setViewMode("h", "s", ViewMode.GRID)
        val raw = store.data.first()[DataStoreBrowserPrefs.key("h", "s")]
        assertEquals("mtime|desc|grid", raw)
    }

    @Test
    fun aValueTheAppCannotParseReadsAsDefaults() = runBlocking {
        store.edit { it[DataStoreBrowserPrefs.key("h", "s")] = "from-a-newer-app" }
        store.edit { it[DataStoreBrowserPrefs.key("h", "t")] = "weird|up|spiral" }
        assertEquals(BrowserPrefs(), prefs.get("h", "s"))
        // unknown parts fall back one by one
        assertEquals(BrowserPrefs(SortSpec(SortField.NAME, SortOrder.ASC), ViewMode.LIST), prefs.get("h", "t"))
    }

    @Test
    fun everyCombinationRoundTripsThroughTheStoredString() {
        for (field in SortField.entries) for (order in SortOrder.entries) for (mode in ViewMode.entries) {
            val value = BrowserPrefs(SortSpec(field, order), mode)
            assertEquals(value, DataStoreBrowserPrefs.decode(DataStoreBrowserPrefs.encode(value)))
        }
    }

    @Test
    fun observeEmitsOnlyWhenThisSharesPrefsChange() = runBlocking {
        val seen = mutableListOf<BrowserPrefs>()
        val job = launch(Dispatchers.Unconfined) {
            prefs.observe("h", "s").collect { seen += it }
        }
        prefs.setSort("h", "other", SortSpec(SortField.SIZE, SortOrder.DESC)) // another share: no emission
        prefs.setViewMode("h", "s", ViewMode.GRID)
        job.cancel()
        assertEquals(listOf(BrowserPrefs(), BrowserPrefs(viewMode = ViewMode.GRID)), seen)
    }

    @Test
    fun inMemoryPrefsBehaveTheSame() = runBlocking {
        val memory = InMemoryBrowserPrefs()
        assertEquals(BrowserPrefs(), memory.get("h", "s"))
        memory.setSort("h", "s", SortSpec(SortField.MTIME, SortOrder.DESC))
        memory.setViewMode("h", "s", ViewMode.GRID)
        assertEquals(BrowserPrefs(SortSpec(SortField.MTIME, SortOrder.DESC), ViewMode.GRID), memory.get("h", "s"))
        assertEquals(BrowserPrefs(), memory.get("h", "other"))
    }
}
