package fyi.arlo.gander.core.data.hosts

import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import fyi.arlo.gander.core.data.browse.DataStoreBrowserPrefs
import fyi.arlo.gander.core.data.browse.InMemoryBrowserPrefs
import fyi.arlo.gander.core.data.browse.SortField
import fyi.arlo.gander.core.data.browse.SortOrder
import fyi.arlo.gander.core.data.browse.SortSpec
import fyi.arlo.gander.core.data.browse.ViewMode
import fyi.arlo.gander.core.data.cache.DerivedCaches
import fyi.arlo.gander.core.net.BearerToken
import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.protocol.TokenRotateResponse
import fyi.arlo.gander.testing.InMemoryPreferencesDataStore
import fyi.arlo.gander.testing.SampleData
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.IOException

/** Forgetting a computer leaves nothing of it behind; rotating a token never loses the working one. */
class HostForgetterTest {
    private val studio = SampleData.host(name = "studio-pc")
    private val laptop = SampleData.host(name = "laptop", id = "laptop-id")
    private val calls = mutableListOf<String>()

    private val inner = InMemoryHostRepository(listOf(studio, laptop))

    private val hosts = object : HostRepository by inner {
        var failForget = false

        override suspend fun forget(hostId: String) {
            calls += "forget-host"
            if (failForget) throw IOException("database is locked")
            inner.forget(hostId)
        }
    }

    private val caches = object : DerivedCaches {
        var failClear = false

        override suspend fun sizeBytes() = 0L

        override suspend fun clear() {
            calls += "clear-caches"
            if (failClear) throw IOException("cache busy")
        }
    }

    private val prefs = InMemoryBrowserPrefs()

    private fun forgetter(failFetched: Boolean = false) = HostForgetter(
        hosts = hosts,
        derivedCaches = caches,
        browserPrefs = prefs,
        purgeFetched = { id ->
            calls += "purge-fetched:$id"
            if (failFetched) throw IOException("disk error")
        },
        onForgotten = { id -> calls += "evict-clients:$id" },
    )

    @Test
    fun everyKindOfDataOfTheComputerIsRemovedInOrder() = runBlocking {
        prefs.setSort(studio.id, "shr_a", SortSpec(SortField.SIZE, SortOrder.DESC))
        prefs.setViewMode(laptop.id, "shr_b", ViewMode.GRID)

        forgetter().forget(studio.id)

        assertEquals(
            listOf("purge-fetched:${studio.id}", "forget-host", "evict-clients:${studio.id}", "clear-caches"),
            calls,
        )
        assertEquals(listOf("laptop"), hosts.hosts.first().map { it.name })
        // its remembered sort and view mode are gone, the other computer's stay
        assertEquals(fyi.arlo.gander.core.data.browse.BrowserPrefs(), prefs.get(studio.id, "shr_a"))
        assertEquals(ViewMode.GRID, prefs.get(laptop.id, "shr_b").viewMode)
    }

    @Test
    fun aFailingCleanupStepDoesNotStopTheForget() = runBlocking {
        caches.failClear = true
        forgetter(failFetched = true).forget(studio.id)
        assertEquals(listOf("laptop"), hosts.hosts.first().map { it.name })
        assertTrue("later steps still ran", "clear-caches" in calls)
    }

    @Test
    fun ifTheComputerCannotBeForgottenTheCallerHearsAboutItAndNothingElseIsPurged() {
        hosts.failForget = true
        assertThrows(IOException::class.java) { runBlocking { forgetter().forget(studio.id) } }
        // the computer is still paired, so its thumbnails and settings must stay too
        assertFalse("clear-caches" in calls)
        assertFalse(calls.any { it.startsWith("evict-clients") })
    }

    // Browser prefs on the real DataStore keys ----------------------------------------------------

    @Test
    fun theDataStoreBrowserPrefsDropOnlyTheForgottenHostsKeys() = runBlocking {
        val store = InMemoryPreferencesDataStore()
        val repo = DataStoreBrowserPrefs(store)
        repo.setSort("host-a", "s1", SortSpec(SortField.MTIME, SortOrder.DESC))
        repo.setViewMode("host-a", "s2", ViewMode.GRID)
        repo.setSort("host-b", "s1", SortSpec(SortField.SIZE, SortOrder.ASC))
        // a host whose id merely starts with the same characters is a different host
        repo.setViewMode("host-ab", "s1", ViewMode.GRID)
        store.edit { it[stringPreferencesKey("settings.unrelated")] = "keep" }

        repo.forgetHost("host-a")

        assertEquals(fyi.arlo.gander.core.data.browse.BrowserPrefs(), repo.get("host-a", "s1"))
        assertEquals(fyi.arlo.gander.core.data.browse.BrowserPrefs(), repo.get("host-a", "s2"))
        assertEquals(SortField.SIZE, repo.get("host-b", "s1").sort.field)
        assertEquals(ViewMode.GRID, repo.get("host-ab", "s1").viewMode)
        assertEquals("keep", store.data.first()[stringPreferencesKey("settings.unrelated")])
    }

    // Token rotation -----------------------------------------------------------------------------

    private val oldToken = BearerToken("oldTokenAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
    private val newToken = "newTokenBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB"

    private fun seeded() = InMemoryHostRepository(listOf(studio)).also { runBlocking { it.seedToken(studio.id, oldToken) } }

    @Test
    fun aRotatedTokenReplacesTheOldOneAtOnce() = runBlocking {
        val repo = seeded()
        ApiTokenRotator(repo) { TokenRotateResponse(newToken) }.rotate(studio)
        assertEquals(newToken, repo.token(studio.id)?.value)
    }

    @Test
    fun aFailedRotationKeepsTheWorkingToken() = runBlocking {
        val repo = seeded()
        val rotator = ApiTokenRotator(repo) { throw GanderError.Unreachable(listOf("x"), null) }
        assertThrows(GanderError.Unreachable::class.java) { runBlocking { rotator.rotate(studio) } }
        assertEquals(oldToken, repo.token(studio.id))
    }

    @Test
    fun anAnswerThatIsNotATokenIsRefusedWithoutEchoingItAndKeepsTheOldToken() = runBlocking {
        val repo = seeded()
        val rotator = ApiTokenRotator(repo) { TokenRotateResponse("not a token ${"x".repeat(5)}") }
        val error = assertThrows(GanderError.MalformedResponse::class.java) { runBlocking { rotator.rotate(studio) } }
        assertFalse(error.message.orEmpty().contains("not a token"))
        assertEquals(oldToken, repo.token(studio.id))
        assertNotNull(repo.get(studio.id))
        assertNull(repo.token("someone-else"))
    }
}
