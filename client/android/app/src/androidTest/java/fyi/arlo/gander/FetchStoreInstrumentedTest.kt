package fyi.arlo.gander

import androidx.room.Room
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import fyi.arlo.gander.core.data.db.GanderDatabase
import fyi.arlo.gander.core.data.db.RoomFetchStore
import fyi.arlo.gander.core.data.fetch.CachedFile
import fyi.arlo.gander.core.data.fetch.FetchFiles
import fyi.arlo.gander.core.data.fetch.FetchJob
import fyi.arlo.gander.core.data.fetch.FetchStatus
import fyi.arlo.gander.core.data.fetch.FileKey
import fyi.arlo.gander.core.net.protocol.HashAlgo
import fyi.arlo.gander.testing.SampleData
import fyi.arlo.gander.ui.fetch.FileIntents
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File

/**
 * The fetch tables (database version 2) and the FileProvider on a device or emulator. Needs no
 * host; compiled now and run by WP-29 with the rest of `src/androidTest`.
 */
@RunWith(AndroidJUnit4::class)
class FetchStoreInstrumentedTest {
    private val context = InstrumentationRegistry.getInstrumentation().targetContext
    private lateinit var db: GanderDatabase
    private lateinit var store: RoomFetchStore

    @Before
    fun setUp() {
        db = Room.inMemoryDatabaseBuilder(context, GanderDatabase::class.java).build()
        store = RoomFetchStore(db.fetchDao())
    }

    @After
    fun tearDown() {
        db.close()
    }

    private val key = FileKey(SampleData.HOST_ID, "shr_projects", "out/render.bin")

    @Test
    fun jobsPersistProgressAndCompleteIntoACachedFile() = runBlocking {
        val id = store.insert(FetchJob(key = key, entry = SampleData.file("render.bin", size = 10), createdAt = 1, updatedAt = 1))
        assertEquals(id, store.latestJob(key, FetchStatus.ACTIVE)!!.id)
        store.updateProgress(id, 4, 10, "\"e\"", 2)
        val running = store.job(id)!!
        assertEquals(4, running.bytesDone)
        assertEquals("\"e\"", running.etag)

        val done = running.copy(status = FetchStatus.DONE, bytesDone = 10, localPath = "fetched/j$id/render.bin", verifiedWith = HashAlgo.BLAKE3)
        val cached = CachedFile(key, "render.bin", null, done.localPath, 10, running.entry.mtime, "\"e\"", lastUsed = 3, fetchedAt = 3, verifiedWith = HashAlgo.BLAKE3)
        store.complete(done, cached)
        assertNull(store.latestJob(key, FetchStatus.ACTIVE))
        assertEquals(cached, store.cached(key))
        assertTrue(store.setPinned(key, true))
        assertTrue(store.cachedFiles.first().single().pinned)
        assertFalse(store.setPinned(key.copy(path = "other"), true))

        assertEquals(1, store.deleteFinishedJobs())
        assertTrue(store.jobs.first().isEmpty())
        assertEquals(cached.copy(pinned = true), store.cached(key)) // the cached file stays

        store.deleteHost(key.hostId)
        assertNull(store.cached(key))
    }

    @Test
    fun fileProviderServesFetchedFilesOnly() {
        val files = FetchFiles(context.filesDir)
        val inside = File(files.jobDir(999_001).apply { mkdirs() }, "a.txt").apply { writeText("x") }
        try {
            val uri = FileIntents.uriFor(context, inside)
            assertEquals("content", uri.scheme)
            assertEquals(FileIntents.AUTHORITY, uri.authority)
            val outside = File(context.filesDir, "not-shared.txt").apply { writeText("y") }
            try {
                FileIntents.uriFor(context, outside)
                fail("a file outside fetched/ must not get a URI")
            } catch (_: IllegalArgumentException) {
                // expected
            } finally {
                outside.delete()
            }
        } finally {
            files.deleteJobDir(999_001)
        }
    }
}
