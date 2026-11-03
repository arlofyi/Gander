package fyi.arlo.gander

import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import androidx.room.Room
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import fyi.arlo.gander.core.data.browse.BrowserPrefs
import fyi.arlo.gander.core.data.browse.DataStoreBrowserPrefs
import fyi.arlo.gander.core.data.browse.ListQuery
import fyi.arlo.gander.core.data.browse.ListingSnapshot
import fyi.arlo.gander.core.data.browse.SharesSnapshot
import fyi.arlo.gander.core.data.browse.SortField
import fyi.arlo.gander.core.data.browse.SortOrder
import fyi.arlo.gander.core.data.browse.SortSpec
import fyi.arlo.gander.core.data.browse.ViewMode
import fyi.arlo.gander.core.data.db.GanderDatabase
import fyi.arlo.gander.core.data.db.RoomHostRepository
import fyi.arlo.gander.core.data.db.RoomListingSnapshotStore
import fyi.arlo.gander.core.data.db.RoomSharesSnapshotStore
import fyi.arlo.gander.core.data.hosts.InMemoryTokenStore
import fyi.arlo.gander.core.net.BearerToken
import fyi.arlo.gander.testing.SampleData
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File
import java.util.UUID

/** Room tables and the settings DataStore on a real device or emulator (run by WP-29). */
@RunWith(AndroidJUnit4::class)
class StoresInstrumentedTest {
    private val context = InstrumentationRegistry.getInstrumentation().targetContext
    private lateinit var db: GanderDatabase
    private val scopes = mutableListOf<CoroutineScope>()

    @Before
    fun setUp() {
        db = Room.inMemoryDatabaseBuilder(context, GanderDatabase::class.java).build()
    }

    @After
    fun tearDown() {
        db.close()
        scopes.forEach { it.cancel() }
    }

    private fun snapshot(hostId: String, path: String, fetchedAt: Long, query: ListQuery = ListQuery()) = ListingSnapshot(
        hostId = hostId,
        shareId = "shr_projects",
        path = path,
        query = query,
        etag = "W/\"e$fetchedAt\"",
        entries = listOf(SampleData.dir("Sub"), SampleData.file("a.txt")),
        nextCursor = null,
        truncated = false,
        unlistable = 0,
        fetchedAtMillis = fetchedAt,
    )

    @Test
    fun roomHostRepositoryStoresRenamesAndForgets() = runBlocking {
        val tokens = InMemoryTokenStore()
        val repo = RoomHostRepository(db.hostDao(), tokens, clock = { 5L })
        val host = SampleData.host()
        repo.add(host, BearerToken("test-token-not-a-secret"))

        assertEquals(listOf(host), repo.hosts.first())
        assertEquals(BearerToken("test-token-not-a-secret"), repo.token(host.id))

        repo.rename(host.id, "Office PC")
        assertEquals("Office PC", repo.get(host.id)!!.name)

        repo.recordContact(host.id, atMillis = 77L, me = SampleData.me())
        val refreshed = repo.get(host.id)!!
        assertEquals(77L, refreshed.lastSeenAtMillis)
        assertNotNull(refreshed.capabilities)
        assertEquals("Office PC", refreshed.name) // the phone's name for the host is kept

        repo.forget(host.id)
        assertNull(repo.get(host.id))
        assertNull(repo.token(host.id))
    }

    @Test
    fun forgettingAHostTakesItsSharesAndListingsWithIt() = runBlocking {
        val repo = RoomHostRepository(db.hostDao(), InMemoryTokenStore())
        val host = SampleData.host()
        repo.add(host, BearerToken("t"))
        val shares = RoomSharesSnapshotStore(db.shareDao())
        val listings = RoomListingSnapshotStore(db.listingSnapshotDao())
        shares.put(host.id, SharesSnapshot(listOf(SampleData.share()), fetchedAtMillis = 9L))
        listings.put(snapshot(host.id, "", 10L))
        assertNotNull(shares.get(host.id))
        assertNotNull(listings.get(host.id, "shr_projects", ""))

        repo.forget(host.id) // ON DELETE CASCADE
        assertNull(shares.get(host.id))
        assertNull(listings.get(host.id, "shr_projects", ""))
    }

    @Test
    fun listingSnapshotsRoundTripAndAreReplacedPerFolder() = runBlocking {
        val repo = RoomHostRepository(db.hostDao(), InMemoryTokenStore())
        val host = SampleData.host()
        repo.add(host, BearerToken("t"))
        val listings = RoomListingSnapshotStore(db.listingSnapshotDao())

        val first = snapshot(host.id, "Clients", 1L, ListQuery(SortSpec(SortField.MTIME, SortOrder.DESC)))
        listings.put(first)
        assertEquals(first, listings.get(host.id, "shr_projects", "Clients"))

        val second = first.copy(etag = "W/\"new\"", fetchedAtMillis = 2L)
        listings.put(second)
        assertEquals(second, listings.get(host.id, "shr_projects", "Clients"))
        assertNull(listings.get(host.id, "shr_projects", "Other"))
    }

    @Test
    fun theSnapshotTableKeepsOnlyTheNewestFolders() = runBlocking {
        val repo = RoomHostRepository(db.hostDao(), InMemoryTokenStore())
        val host = SampleData.host()
        repo.add(host, BearerToken("t"))
        val listings = RoomListingSnapshotStore(db.listingSnapshotDao(), maxSnapshots = 3)
        for (i in 1..5) listings.put(snapshot(host.id, "folder$i", fetchedAt = i.toLong()))

        assertEquals(3, db.listingSnapshotDao().count())
        assertNull(listings.get(host.id, "shr_projects", "folder1"))
        assertNull(listings.get(host.id, "shr_projects", "folder2"))
        assertNotNull(listings.get(host.id, "shr_projects", "folder5"))
    }

    @Test
    fun sharesAreReplacedAsAWholeAndKeepTheirOrder() = runBlocking {
        val repo = RoomHostRepository(db.hostDao(), InMemoryTokenStore())
        val host = SampleData.host()
        repo.add(host, BearerToken("t"))
        val shares = RoomSharesSnapshotStore(db.shareDao())
        shares.put(host.id, SharesSnapshot(listOf(SampleData.share(name = "B"), SampleData.share(id = "shr_a", name = "A")), 1L))
        assertEquals(listOf("B", "A"), shares.get(host.id)!!.shares.map { it.name })

        shares.put(host.id, SharesSnapshot(listOf(SampleData.share(id = "shr_c", name = "C")), 2L))
        val stored = shares.get(host.id)!!
        assertEquals(listOf("C"), stored.shares.map { it.name })
        assertEquals(2L, stored.fetchedAtMillis)
    }

    @Test
    fun browserPrefsSurviveReopeningTheDataStoreFile() = runBlocking {
        val file = File(context.cacheDir, "prefs-${UUID.randomUUID()}.preferences_pb")
        try {
            fun open(): DataStoreBrowserPrefs {
                val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO).also { scopes += it }
                return DataStoreBrowserPrefs(PreferenceDataStoreFactory.create(scope = scope) { file })
            }
            val first = open()
            first.setSort("h", "s", SortSpec(SortField.SIZE, SortOrder.DESC))
            first.setViewMode("h", "s", ViewMode.GRID)
            scopes.forEach { it.cancel() } // the process ends, releasing the file
            scopes.clear()

            val second = open()
            assertEquals(BrowserPrefs(SortSpec(SortField.SIZE, SortOrder.DESC), ViewMode.GRID), second.get("h", "s"))
            assertEquals(BrowserPrefs(), second.get("h", "other-share"))
        } finally {
            scopes.forEach { it.cancel() }
            scopes.clear()
            file.delete()
            assertTrue(!file.exists() || file.delete())
        }
    }
}
