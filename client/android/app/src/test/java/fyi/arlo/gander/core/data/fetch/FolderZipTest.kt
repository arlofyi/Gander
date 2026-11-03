package fyi.arlo.gander.core.data.fetch

import fyi.arlo.gander.core.data.actions.FixedNetworkStateMonitor
import fyi.arlo.gander.core.data.actions.InMemoryActionRefusals
import fyi.arlo.gander.core.data.actions.NetworkState
import fyi.arlo.gander.core.data.browse.EntryTarget
import fyi.arlo.gander.core.data.hosts.InMemoryHostRepository
import fyi.arlo.gander.core.data.hosts.PairedHost
import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.GanderHttpClients
import fyi.arlo.gander.core.net.TestHosts
import fyi.arlo.gander.core.net.protocol.EntryKind
import fyi.arlo.gander.core.net.protocol.ZipPlan
import fyi.arlo.gander.testing.SampleData
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import mockwebserver3.Dispatcher
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import mockwebserver3.RecordedRequest
import mockwebserver3.SocketEffect
import okio.Buffer
import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import java.io.ByteArrayOutputStream
import java.util.concurrent.CopyOnWriteArrayList
import java.util.zip.ZipEntry
import java.util.zip.ZipInputStream
import java.util.zip.ZipOutputStream
import kotlin.random.Random

/**
 * Folder download (WP-68) through the fetch engine against a scripted host: the dry run, the
 * non-resumable zip job (no `Range`, no verification, a broken connection starts over from zero),
 * the refusal for a folder over the host's limits, and the marker helpers.
 */
class FolderZipTest {
    @get:Rule
    val temp = TemporaryFolder()

    private val held = TestHosts.selfSigned()
    private val pin = TestHosts.pinOf(held)
    private lateinit var server: MockWebServer
    private lateinit var host: PairedHost
    private lateinit var clients: GanderHttpClients
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val store = InMemoryFetchStore()
    private val network = FixedNetworkStateMonitor(NetworkState.UNMETERED)
    private lateinit var files: FetchFiles
    private lateinit var engine: FetchEngine

    /** A real zip of two files, so the test can open what the phone stored. */
    private val zipBytes: ByteArray = ByteArrayOutputStream().also { bytes ->
        ZipOutputStream(bytes).use { z ->
            z.putNextEntry(ZipEntry("Docs/"))
            z.closeEntry()
            z.putNextEntry(ZipEntry("Docs/a.bin"))
            z.write(Random(68).nextBytes(900_000))
            z.closeEntry()
            z.putNextEntry(ZipEntry("Docs/b.txt"))
            z.write("hello".toByteArray())
            z.closeEntry()
        }
    }.toByteArray()

    private val plan = ZipPlan(entries = 2, bytes = 900_005, skipped = 1, cloudOnly = 0)

    private val fake = object : Dispatcher() {
        val zipRequests = CopyOnWriteArrayList<RecordedRequest>()
        val planRequests = CopyOnWriteArrayList<RecordedRequest>()
        val other = CopyOnWriteArrayList<RecordedRequest>()

        @Volatile var dropOnce: Int? = null

        @Volatile var tooLarge = false

        override fun dispatch(request: RecordedRequest): MockResponse {
            if (request.url.encodedPath != "/v1/zip") {
                other += request
                return json(404, """{"error":{"code":"not_found","message":"","retry_after_ms":0}}""")
            }
            if (request.url.queryParameter("dry_run") == "1") {
                planRequests += request
                if (tooLarge) {
                    return json(
                        413,
                        """{"error":{"code":"too_large","message":"This folder is over the limit for a zip download (4 GiB and 20,000 items at most).","retry_after_ms":0}}""",
                    )
                }
                return json(200, """{"entries":${plan.entries},"bytes":${plan.bytes},"skipped":${plan.skipped},"cloud_only":${plan.cloudOnly}}""")
            }
            zipRequests += request
            val drop = dropOnce
            if (drop != null) {
                dropOnce = null
                // Promises the whole archive, sends part of it and closes the socket: a broken download.
                return MockResponse.Builder().code(200).body(Buffer().write(zipBytes, 0, drop))
                    .setHeader("Content-Length", zipBytes.size)
                    .setHeader("Content-Type", "application/zip")
                    .onResponseEnd(SocketEffect.CloseSocket())
                    .build()
            }
            // Like the host: no Content-Length (it streams an archive it has not finished building).
            return MockResponse.Builder().code(200)
                .body(Buffer().write(zipBytes))
                .removeHeader("Content-Length")
                .setHeader("Content-Type", "application/zip")
                .setHeader("Accept-Ranges", "none")
                .setHeader("X-Gander-Zip-Entries", plan.entries)
                .setHeader("X-Gander-Zip-Bytes", plan.bytes)
                .build()
        }

        private fun json(status: Int, body: String) = MockResponse.Builder().code(status)
            .setHeader("Content-Type", "application/json").body(body).build()
    }

    @Before
    fun setUp() {
        server = TestHosts.httpsServer(held)
        server.dispatcher = fake
        host = PairedHost(id = pin.hostId, name = "studio-pc", pin = pin, addresses = listOf("127.0.0.1"), port = server.port)
        clients = TestHosts.clients(server, pin)
        files = FetchFiles(temp.newFolder("files"))
        engine = FetchEngine(
            store = store,
            files = files,
            hosts = InMemoryHostRepository(listOf(host)),
            connector = ClientsFetchConnector { clients },
            network = network,
            scope = scope,
            // Verify everything: a zip must still not be verified (there is no hash of an archive).
            settings = { FetchSettings(verify = VerifyPolicy.ALWAYS) },
            refusals = InMemoryActionRefusals(),
            sleep = { },
        )
    }

    @After
    fun tearDown() {
        scope.cancel()
        server.close()
    }

    private fun folder(path: String = "docs", name: String = "Docs") =
        EntryTarget(host.id, "shr_projects", path, SampleData.dir(name))

    private suspend fun finished(id: Long): FetchJob = withTimeout(20_000) {
        store.jobs.first { all -> all.firstOrNull { it.id == id }?.status in FetchStatus.FINISHED }.first { it.id == id }
    }

    private suspend fun enqueueZip(path: String = "docs", name: String = "Docs"): Long =
        engine.enqueue(FolderZip.target(folder(path, name), plan, mtime = "2026-11-03T10:00:00Z"))

    private fun names(bytes: ByteArray): List<String> = buildList {
        ZipInputStream(bytes.inputStream()).use { zin ->
            while (true) add((zin.nextEntry ?: break).name)
        }
    }

    @Test
    fun theDryRunReturnsTheTotalsAndTheShareIsAQueryParameter() = runBlocking {
        val got = engine.planZip(folder("clients/Acme", "Acme"))
        assertEquals(plan, got)
        val request = fake.planRequests.single()
        assertEquals("/v1/zip", request.url.encodedPath)
        assertEquals("shr_projects", request.url.queryParameter("share"))
        assertEquals("clients/Acme", request.url.queryParameter("path"))
        assertEquals("1", request.url.queryParameter("dry_run"))
    }

    @Test
    fun aFolderOverTheLimitIsTooLargeWithTheHostsOwnWords() = runBlocking {
        fake.tooLarge = true
        try {
            engine.planZip(folder())
            fail("expected too_large")
        } catch (e: GanderError.Host.TooLarge) {
            assertEquals(413, e.status)
            assertTrue(e.hostMessage, e.hostMessage.contains("over the limit"))
        }
    }

    @Test
    fun downloadsTheZipWithoutRangeOrVerificationAndKeepsItAsFolderZip() = runBlocking {
        val id = enqueueZip()
        val job = finished(id)
        assertEquals(job.failure?.toString(), FetchStatus.DONE, job.status)
        assertNull("an archive is never verified", job.verifiedWith)
        val request = fake.zipRequests.single()
        assertNull(request.headers["Range"])
        assertNull(request.headers["If-Range"])
        assertEquals("identity", request.headers["Accept-Encoding"])
        assertEquals("shr_projects", request.url.queryParameter("share"))
        assertEquals("docs", request.url.queryParameter("path"))
        assertNull("the zip URL has no dry_run", request.url.queryParameter("dry_run"))
        assertTrue("no /hash request", fake.other.none { it.url.encodedPath.endsWith("/hash") })

        val cached = store.cached(job.key)!!
        assertEquals("Docs.zip", cached.name)
        assertEquals("application/zip", cached.mime)
        assertEquals("fetched/j$id/Docs.zip", cached.localPath)
        val stored = files.resolve(cached.localPath)!!.readBytes()
        assertArrayEquals(zipBytes, stored)
        assertEquals(listOf("Docs/", "Docs/a.bin", "Docs/b.txt"), names(stored))
        assertFalse("partial file is gone", files.partialFile(id).exists())
        assertTrue(FolderZip.isZip(job.key))
        assertEquals("docs", FolderZip.folderPath(job.key))
    }

    @Test
    fun aBrokenConnectionStartsTheZipOverFromZeroNeverWithRange() = runBlocking {
        fake.dropOnce = 300_000
        val id = enqueueZip()
        val job = finished(id)
        assertEquals(job.failure?.toString(), FetchStatus.DONE, job.status)
        assertEquals(2, fake.zipRequests.size)
        fake.zipRequests.forEach {
            assertNull("no Range on a zip", it.headers["Range"])
            assertNull("no If-Range on a zip", it.headers["If-Range"])
        }
        assertFalse(job.changedOnHost)
        assertArrayEquals(zipBytes, files.resolve(store.cached(job.key)!!.localPath)!!.readBytes())
    }

    @Test
    fun theSameFolderIsNotQueuedTwiceWhileItIsActive() = runBlocking {
        fake.dropOnce = 1000
        val first = enqueueZip()
        val second = enqueueZip()
        assertEquals(first, second)
        assertEquals(FetchStatus.DONE, finished(first).status)
    }

    @Test
    fun theMarkerCannotBeARealSharePath() {
        val target = FolderZip.target(folder("a/b", "b"), plan, mtime = "2026-11-03T10:00:00Z")
        assertEquals("zip:a/b", target.path)
        assertEquals(EntryKind.FILE, target.entry.kind)
        assertEquals("b.zip", target.entry.name)
        assertEquals("application/zip", target.entry.mime)
        // The host refuses ':' in a path segment, so no file can ever carry this key.
        assertTrue(target.path.contains(':'))
        assertFalse(FolderZip.isZip(FileKey("h", "s", "a/b")))
        assertEquals("a/b", FolderZip.folderPath(FileKey("h", "s", "zip:a/b")))
        assertEquals("", FolderZip.folderPath(FileKey("h", "s", "zip:")))
    }

    @Test
    fun theZipNameAndTheEstimate() {
        assertEquals("Docs.zip", FolderZip.fileName("Docs"))
        assertEquals("Docs.zip", FolderZip.fileName("Docs.zip"))
        assertEquals("a_b.zip", FolderZip.fileName("a/b"))
        assertEquals("folder.zip", FolderZip.fileName(""))
        val estimate = FolderZip.estimatedSize(ZipPlan(entries = 100, bytes = 1_000_000, skipped = 0, cloudOnly = 0))
        assertTrue("headers add a little: $estimate", estimate in 1_000_001L..1_100_000L)
        assertTrue(FolderZip.estimatedSize(ZipPlan(0, 0, 0, 0)) > 0)
    }
}
