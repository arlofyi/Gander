package fyi.arlo.gander

import android.database.Cursor
import android.net.Uri
import android.os.ParcelFileDescriptor
import android.provider.DocumentsContract
import android.provider.DocumentsContract.Document
import android.system.Os
import android.util.Log
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import fyi.arlo.gander.core.data.documents.DocumentIds
import fyi.arlo.gander.core.data.documents.ReadStats
import fyi.arlo.gander.core.data.hosts.PairedHost
import fyi.arlo.gander.core.data.settings.AppSettings
import fyi.arlo.gander.core.net.BearerToken
import fyi.arlo.gander.core.net.SpkiPin
import fyi.arlo.gander.documents.FilesAppReads
import kotlinx.coroutines.runBlocking
import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Assume.assumeTrue
import org.junit.Test
import org.junit.runner.RunWith
import java.io.FileInputStream
import kotlin.random.Random

/**
 * WP-60 verify, instrumented half: the app's own `DocumentsProvider` against the real host on the
 * emulator. The paired computer appears as a root, its share and folders as documents, and the
 * 50 MiB fixture opens through `ContentResolver.openFileDescriptor` (a proxy fd backed by Range
 * requests) and reads back byte for byte: sequentially with a 1 MiB buffer, with random `pread`s,
 * and in a player-like pattern (two readers 2 MiB apart, small reads with forward gaps). The
 * provider's per-fd counts (requests, bytes fetched) are logged under the tag
 * `GanderFilesAppTest` and checked against loose bounds.
 *
 * Same process as the app, so `MANAGE_DOCUMENTS` is not needed (same-uid callers pass). It uses the
 * app's real graph (Room, Keystore token store, settings) and forgets the host afterwards.
 *
 * Skipped (assumption failure) unless these instrumentation arguments are given (same convention
 * as `HostFetchFixtureTest`; `cargo xtask e2e-android` passes them):
 *
 * - `gander.host.id`, `gander.host.addresses`, `gander.host.port` (optional), `gander.host.token`
 * - `gander.fixture.share`: id of the share holding the fixture tree
 * - `gander.fixture.large`: share-relative path of the large file (`large/fifty.bin`)
 * - `gander.fixture.large_pattern`: `e2e-android-v1`, the generator of that file's bytes (50 times
 *   a 1 MiB pattern `(i * 131) % 253` followed by the block index as 8 little-endian bytes), so
 *   every byte read can be checked without a second download
 */
@RunWith(AndroidJUnit4::class)
class FilesAppProviderFixtureTest {
    private val context = InstrumentationRegistry.getInstrumentation().targetContext
    private val args = InstrumentationRegistry.getArguments()
    private val resolver = context.contentResolver
    private val authority = context.packageName + ".documents"
    private val graph = context.appGraph
    private var host: PairedHost? = null
    private var settingsBefore: AppSettings? = null

    private fun arg(name: String): String? = args.getString(name)?.takeIf { it.isNotBlank() }

    @After
    fun tearDown() {
        // A block body: JUnit refuses an @After that returns a value (runBlocking's result).
        runBlocking {
            host?.let { graph.hostRepository.forget(it.id) }
            settingsBefore?.let {
                graph.settingsRepository.setFilesAppEnabled(it.filesAppEnabled)
                graph.settingsRepository.setAppLockEnabled(it.appLockEnabled)
            }
        }
    }

    @Test
    fun aHostFileReadsThroughTheProviderSequentiallyRandomlyAndLikeAPlayer() {
        val hostId = arg("gander.host.id")
        val addresses = arg("gander.host.addresses")?.split(',')?.map { it.trim() }
        val token = arg("gander.host.token")
        val share = arg("gander.fixture.share")
        val large = arg("gander.fixture.large")
        val pattern = arg("gander.fixture.large_pattern")
        assumeTrue(
            "needs a running host, its large fixture and the pattern name (cargo xtask e2e-android)",
            hostId != null && addresses != null && token != null && share != null && large != null &&
                pattern == "e2e-android-v1",
        )

        val pin = SpkiPin.fromHostId(hostId!!)
        val paired = PairedHost(
            id = pin.hostId,
            name = "Files-app fixture",
            pin = pin,
            addresses = addresses!!,
            port = arg("gander.host.port")?.toInt() ?: 42637,
        )
        runBlocking {
            settingsBefore = graph.settingsRepository.current()
            graph.settingsRepository.setFilesAppEnabled(true)
            graph.settingsRepository.setAppLockEnabled(false)
            graph.hostRepository.add(paired, BearerToken(token!!))
            graph.hostRepository.recordContact(paired.id, System.currentTimeMillis())
        }
        host = paired

        // Roots: the paired computer is one.
        val rootDoc = DocumentIds.host(paired.id)
        resolver.query(DocumentsContract.buildRootsUri(authority), null, null, null, null).use { c ->
            assertNotNull(c)
            val ids = c!!.strings(DocumentsContract.Root.COLUMN_DOCUMENT_ID)
            assertTrue("roots: ${ids.size}", rootDoc in ids)
        }

        // Shares, then the folders down to the large file (each listing refreshes in the background).
        val shareDoc = childNamed(rootDoc, null) { id -> id == DocumentIds.item(paired.id, share!!, "") }
        val folder = large!!.substringBeforeLast('/')
        val folderDoc = childNamed(shareDoc, folder)
        val fileDoc = childNamed(folderDoc, large.substringAfterLast('/'))
        fileDocId = fileDoc
        val expectedSize = 50L * (MIB + 8)
        val uri = DocumentsContract.buildDocumentUri(authority, fileDoc)
        resolver.query(uri, null, null, null, null).use { c ->
            assertTrue(c!!.moveToFirst())
            assertEquals(expectedSize, c.getLong(c.getColumnIndexOrThrow(Document.COLUMN_SIZE)))
            val flags = c.getInt(c.getColumnIndexOrThrow(Document.COLUMN_FLAGS))
            val writeFlags = Document.FLAG_SUPPORTS_WRITE or Document.FLAG_SUPPORTS_DELETE or
                Document.FLAG_SUPPORTS_RENAME or Document.FLAG_SUPPORTS_MOVE or Document.FLAG_SUPPORTS_REMOVE
            assertEquals("read only", 0, flags and writeFlags)
        }

        // Write modes are refused.
        for (mode in listOf("w", "rw", "wt")) {
            try {
                resolver.openFileDescriptor(uri, mode)?.close()
                fail("mode $mode must be refused")
            } catch (_: Exception) {
            }
        }

        FilesAppReads.clear()
        val sequential = measure("sequential 1 MiB buffer") { pfd ->
            assertEquals(expectedSize, pfd.statSize)
            FileInputStream(pfd.fileDescriptor).use { input ->
                val buf = ByteArray(MIB)
                var pos = 0L
                while (true) {
                    val n = input.read(buf)
                    if (n < 0) break
                    check(pos, buf, n)
                    pos += n
                }
                assertEquals(expectedSize, pos)
            }
        }
        val random = measure("random pread 200 x 4 KiB + 50 x 64 KiB") { pfd ->
            val rng = Random(60)
            repeat(200) { preadAndCheck(pfd, rng.nextLong(expectedSize - 4096), 4096) }
            repeat(50) { preadAndCheck(pfd, rng.nextLong(expectedSize - 65536), 65536) }
        }
        val player = measure("player-like: two readers 2 MiB apart, 4-20 KiB reads, 8-20 KiB gaps") { pfd ->
            val rng = Random(47)
            var video = 3L * MIB
            var audio = video + 2 * MIB
            while (audio < 20L * MIB) {
                val v = listOf(4, 8, 8, 12, 16, 20)[rng.nextInt(6)] * 1024
                preadAndCheck(pfd, video, v)
                video += v + rng.nextInt(8, 21) * 1024
                val a = listOf(4, 8, 8)[rng.nextInt(3)] * 1024
                preadAndCheck(pfd, audio, a)
                audio += a + rng.nextInt(8, 21) * 1024
            }
            // A seek, as a player does: a new position and more of the same pattern.
            var pos = 40L * MIB
            repeat(200) {
                preadAndCheck(pfd, pos, 8192)
                pos += 8192 + rng.nextInt(8, 21) * 1024
            }
        }

        // Loose bounds: the point is the request count, not a benchmark on an emulator.
        // The kernel may ask for a range twice on one fd (measured: about 440 KiB more than the file);
        // the cache answers those from memory, so they cost no request.
        assertTrue("sequential served ${sequential.bytesServed}", sequential.bytesServed >= expectedSize)
        assertTrue("sequential over-fetch ${sequential.overFetch}", sequential.overFetch <= 1.05)
        assertTrue("sequential requests ${sequential.requests} for ${sequential.reads} reads", sequential.requests * 4 < sequential.reads)
        assertTrue("random over-fetch ${random.overFetch}", random.overFetch <= 1.5)
        assertTrue("player requests ${player.requests} for ${player.reads} reads", player.requests * 5 < player.reads)
        assertTrue("player memory ${player.maxResidentBytes}", player.maxResidentBytes <= 4L * MIB)
    }

    /** Opens the document, runs [block], closes it and returns the provider's counts for that fd. */
    private fun measure(label: String, block: (ParcelFileDescriptor) -> Unit): ReadStats {
        val uri = DocumentsContract.buildDocumentUri(authority, fileDocId!!)
        val before = FilesAppReads.recent().size
        val started = System.nanoTime()
        val pfd = resolver.openFileDescriptor(uri, "r") ?: error("no fd")
        val openedMs = (System.nanoTime() - started) / 1_000_000
        pfd.use(block)
        val ms = (System.nanoTime() - started) / 1_000_000
        val deadline = System.currentTimeMillis() + 5_000
        while (FilesAppReads.recent().size <= before && System.currentTimeMillis() < deadline) Thread.sleep(20)
        val stats = FilesAppReads.recent().lastOrNull() ?: error("the provider recorded no stats")
        Log.i(
            TAG,
            "$label: open=${openedMs}ms total=${ms}ms reads=${stats.reads} served=${stats.bytesServed} " +
                "requests=${stats.requests} fetched=${stats.bytesFetched} overFetch=${"%.3f".format(stats.overFetch)} " +
                "prefetches=${stats.prefetches} prefetchHits=${stats.prefetchHits} jumps=${stats.jumps} " +
                "maxResident=${stats.maxResidentBytes}",
        )
        return stats
    }

    private var fileDocId: String? = null

    /**
     * Waits (re-querying, as the Files app does after `notifyChange`) until the folder lists a child
     * named [name] (or matching [match]) and returns its document id.
     */
    private fun childNamed(parent: String, name: String?, match: (String) -> Boolean = { false }): String {
        val uri = DocumentsContract.buildChildDocumentsUri(authority, parent)
        val deadline = System.currentTimeMillis() + 30_000
        while (System.currentTimeMillis() < deadline) {
            resolver.query(uri, null, null, null, null)?.use { c ->
                while (c.moveToNext()) {
                    val id = c.getString(c.getColumnIndexOrThrow(Document.COLUMN_DOCUMENT_ID))
                    val display = c.getString(c.getColumnIndexOrThrow(Document.COLUMN_DISPLAY_NAME))
                    if (match(id) || (name != null && display == name)) return id
                }
            }
            Thread.sleep(250)
        }
        fail("no child ${name ?: "matching"} under the folder within 30 s")
        error("unreachable")
    }

    private fun Cursor.strings(column: String): List<String> {
        val out = mutableListOf<String>()
        while (moveToNext()) out += getString(getColumnIndexOrThrow(column))
        return out
    }

    private fun preadAndCheck(pfd: ParcelFileDescriptor, offset: Long, size: Int) {
        val buf = ByteArray(size)
        var done = 0
        while (done < size) {
            val n = Os.pread(pfd.fileDescriptor, buf, done, size - done, offset + done)
            if (n <= 0) break
            done += n
        }
        assertEquals("bytes at $offset", size, done)
        check(offset, buf, size)
    }

    private fun check(offset: Long, buf: ByteArray, n: Int) {
        val expected = ByteArray(n) { expectedByte(offset + it) }
        if (!expected.contentEquals(buf.copyOf(n))) {
            assertArrayEquals("content at $offset", expected, buf.copyOf(n))
        }
    }

    private companion object {
        const val TAG = "GanderFilesAppTest"
        const val MIB = 1024 * 1024

        /** `cargo xtask e2e-android`'s `large/fifty.bin` (pattern `e2e-android-v1`). */
        fun expectedByte(offset: Long): Byte {
            val block = MIB + 8L
            val k = offset / block
            val r = offset % block
            return if (r < MIB) ((r * 131) % 253).toByte() else ((k shr (8 * (r - MIB).toInt())) and 0xff).toByte()
        }
    }
}
