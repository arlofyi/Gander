package fyi.arlo.gander.core.data.fetch

import fyi.arlo.gander.core.data.actions.FileAction
import fyi.arlo.gander.core.data.actions.FixedNetworkStateMonitor
import fyi.arlo.gander.core.data.actions.InMemoryActionRefusals
import fyi.arlo.gander.core.data.actions.NetworkState
import fyi.arlo.gander.core.data.browse.EntryTarget
import fyi.arlo.gander.core.data.hosts.InMemoryHostRepository
import fyi.arlo.gander.core.data.hosts.PairedHost
import fyi.arlo.gander.core.net.GanderHttpClients
import fyi.arlo.gander.core.net.TestHosts
import fyi.arlo.gander.core.net.protocol.FetchCapabilities
import fyi.arlo.gander.core.net.protocol.HashAlgo
import fyi.arlo.gander.testing.SampleData
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import mockwebserver3.MockWebServer
import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import java.util.concurrent.atomic.AtomicInteger
import kotlin.random.Random

/**
 * The fetch engine against a scripted host over real pinned TLS on 127.0.0.1 (MockWebServer):
 * resume with `Range` + `If-Range`, 412 restart, verification match / mismatch / 503 polling,
 * refusals, offline waiting, pause and resume, two files per host.
 */
class FetchEngineTest {
    @get:Rule
    val temp = TemporaryFolder()

    private val held = TestHosts.selfSigned()
    private val pin = TestHosts.pinOf(held)
    private lateinit var server: MockWebServer
    private lateinit var host: PairedHost
    private lateinit var clients: GanderHttpClients
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val store = InMemoryFetchStore()
    private val refusals = InMemoryActionRefusals()
    private val network = FixedNetworkStateMonitor(NetworkState.UNMETERED)
    private lateinit var files: FetchFiles
    private lateinit var engine: FetchEngine

    private val content = Random(27).nextBytes(3 * 1024 * 1024 + 123)
    private val fake = FakeFileHost(content)

    @Before
    fun setUp() {
        server = TestHosts.httpsServer(held)
        server.dispatcher = fake
        host = PairedHost(id = pin.hostId, name = "studio-pc", pin = pin, addresses = listOf("127.0.0.1"), port = server.port)
        clients = TestHosts.clients(server, pin)
        files = FetchFiles(temp.newFolder("files"))
        engine = newEngine(FetchSettings(verify = VerifyPolicy.ALWAYS))
    }

    @After
    fun tearDown() {
        scope.cancel()
        server.close()
    }

    private fun newEngine(settings: FetchSettings, perHost: Int = FetchEngine.PARALLEL_PER_HOST) = FetchEngine(
        store = store,
        files = files,
        hosts = InMemoryHostRepository(listOf(host)),
        connector = ClientsFetchConnector { clients },
        network = network,
        scope = scope,
        settings = { settings },
        refusals = refusals,
        sleep = { }, // no real backoff in tests
        perHost = perHost,
    )

    private fun target(name: String = "render.bin", size: Long = content.size.toLong()) =
        EntryTarget(host.id, "shr_projects", "out/$name", SampleData.file(name, size = size))

    private suspend fun awaitJob(id: Long, predicate: (FetchJob) -> Boolean): FetchJob = withTimeout(20_000) {
        store.jobs.first { all -> all.firstOrNull { it.id == id }?.let(predicate) == true }.first { it.id == id }
    }

    private suspend fun awaitFinished(id: Long) = awaitJob(id) { it.status in FetchStatus.FINISHED }

    private suspend fun cachedBytes(id: Long): ByteArray {
        val job = store.job(id)!!
        val cached = store.cached(job.key)!!
        return files.resolve(cached.localPath)!!.readBytes()
    }

    @Test
    fun downloadsVerifiesWithBlake3AndRecordsTheCachedFile() = runBlocking {
        val id = engine.enqueue(target())
        val job = awaitFinished(id)
        assertEquals(FetchStatus.DONE, job.status)
        assertEquals(HashAlgo.BLAKE3, job.verifiedWith)
        assertArrayEquals(content, cachedBytes(id))
        assertEquals("\"v1\"", job.etag)
        assertEquals("identity", fake.fileRequests.single().headers["Accept-Encoding"])
        assertNull(fake.fileRequests.single().headers["Range"])
        assertFalse("partial file is gone", files.partialFile(id).exists())
        val cached = store.cached(job.key)!!
        assertEquals(content.size.toLong(), cached.size)
        assertEquals("fetched/j$id/render.bin", cached.localPath)
    }

    @Test
    fun aDroppedConnectionResumesWithRangeAndIfRange() = runBlocking {
        val dropAt = 1024L * 1024 + 4321
        fake.dropAt = dropAt
        val id = engine.enqueue(target())
        val job = awaitFinished(id)
        assertEquals(FetchStatus.DONE, job.status)
        assertArrayEquals(content, cachedBytes(id))
        assertEquals(2, fake.fileRequests.size)
        val resumed = fake.fileRequests[1]
        assertEquals("bytes=$dropAt-", resumed.headers["Range"])
        assertEquals("\"v1\"", resumed.headers["If-Range"])
        assertFalse(job.changedOnHost)
        assertEquals(0, job.restarts)
    }

    @Test
    fun a412FileChangedRestartsFromZeroAndTellsTheUser() = runBlocking {
        val changed = Random(99).nextBytes(2 * 1024 * 1024)
        fake.dropAt = 512L * 1024
        fake.afterDrop = {
            fake.content = changed
            fake.etag = "\"v2\""
        }
        val id = engine.enqueue(target())
        val job = awaitFinished(id)
        assertEquals(FetchStatus.DONE, job.status)
        assertArrayEquals(changed, cachedBytes(id))
        assertTrue(job.changedOnHost)
        assertEquals(1, job.restarts)
        assertEquals("\"v2\"", job.etag)
        // 1: dropped; 2: Range + stale If-Range -> 412; 3: from zero, no Range.
        assertEquals(3, fake.fileRequests.size)
        assertEquals("\"v1\"", fake.fileRequests[1].headers["If-Range"])
        assertNull(fake.fileRequests[2].headers["Range"])
    }

    @Test
    fun aFileThatKeepsChangingFailsAfterTheRestartLimit() = runBlocking {
        val version = AtomicInteger(1)
        fun dropAndChange() {
            fake.dropAt = 1000
            fake.afterDrop = {
                fake.etag = "\"v${version.incrementAndGet()}\""
                dropAndChange()
            }
        }
        dropAndChange()
        val id = engine.enqueue(target())
        val job = awaitFinished(id)
        assertEquals(FetchStatus.FAILED, job.status)
        assertEquals(FetchFailure.FILE_CHANGED, job.failure)
        assertFalse(files.jobDir(id).exists())
    }

    @Test
    fun aVerificationMismatchDeletesTheCopyAndRetryFetchesAgain() = runBlocking {
        fake.hashOverride = "00".repeat(32)
        val id = engine.enqueue(target())
        val failed = awaitFinished(id)
        assertEquals(FetchStatus.FAILED, failed.status)
        assertEquals(FetchFailure.VERIFY_MISMATCH, failed.failure)
        assertEquals(0, failed.bytesDone)
        assertFalse("bad copy deleted", files.jobDir(id).exists())
        assertNull(store.cached(failed.key))

        fake.hashOverride = null
        engine.retry(id)
        val done = awaitJob(id) { it.status == FetchStatus.DONE }
        assertEquals(HashAlgo.BLAKE3, done.verifiedWith)
        assertArrayEquals(content, cachedBytes(id))
        assertNull("retry starts from zero", fake.fileRequests.last().headers["Range"])
    }

    @Test
    fun hash503IsPolledUntilTheHostHasIt() = runBlocking {
        fake.hashGenerating = 3
        val id = engine.enqueue(target())
        val job = awaitFinished(id)
        assertEquals(FetchStatus.DONE, job.status)
        assertEquals(4, fake.hashRequests.size)
        assertEquals(HashAlgo.BLAKE3, job.verifiedWith)
    }

    @Test
    fun sha256IsUsedWhenTheHostOffersOnlyThat() = runBlocking {
        host = host.copy(capabilities = SampleData.me().capabilities.copy(fetch = FetchCapabilities(range = true, hash = listOf(HashAlgo.SHA256))))
        engine = newEngine(FetchSettings(verify = VerifyPolicy.ALWAYS))
        val id = engine.enqueue(target())
        val job = awaitFinished(id)
        assertEquals(HashAlgo.SHA256, job.verifiedWith)
        assertEquals("sha256", fake.hashRequests.single().url.queryParameter("algo"))
    }

    @Test
    fun smallFilesAreNotVerifiedByDefault() = runBlocking {
        engine = newEngine(FetchSettings())
        val id = engine.enqueue(target())
        val job = awaitFinished(id)
        assertEquals(FetchStatus.DONE, job.status)
        assertNull(job.verifiedWith)
        assertTrue(fake.hashRequests.isEmpty())
    }

    @Test
    fun aHostRefusalFailsAtOnceAndTellsTheFileSheet() = runBlocking {
        fake.fileError = 403 to "permission_denied"
        val t = target()
        val id = engine.enqueue(t)
        val job = awaitFinished(id)
        assertEquals(FetchStatus.FAILED, job.status)
        assertEquals(FetchFailure.HOST_REFUSED, job.failure)
        assertEquals("permission_denied", job.failureCode)
        assertEquals(1, fake.fileRequests.size)
        assertNotNull(refusals.observe(t).first()[FileAction.FETCH])
    }

    @Test
    fun serverErrorsAreRetriedThenFail() = runBlocking {
        fake.fileError = 500 to "internal"
        val id = engine.enqueue(target())
        val job = awaitFinished(id)
        assertEquals(FetchFailure.NETWORK, job.failure)
        assertEquals(RetryPolicy().maxAttempts, fake.fileRequests.size)
    }

    @Test
    fun waitsOfflineAndStartsWhenTheNetworkReturns() = runBlocking {
        network.state = NetworkState.OFFLINE
        val id = engine.enqueue(target())
        awaitJob(id) { it.status == FetchStatus.WAITING }
        assertTrue(fake.fileRequests.isEmpty())
        network.state = NetworkState.UNMETERED
        assertEquals(FetchStatus.DONE, awaitFinished(id).status)
    }

    @Test
    fun pauseKeepsProgressAndResumeContinuesFromIt() = runBlocking {
        fake.throttleBytes = 64L * 1024
        val id = engine.enqueue(target())
        awaitJob(id) { it.status == FetchStatus.RUNNING && it.bytesDone > 256L * 1024 }
        engine.pauseAll()
        val paused = store.job(id)!!
        assertEquals(FetchStatus.PAUSED, paused.status)
        assertTrue(paused.bytesDone > 0)
        assertEquals(paused.bytesDone, files.partialFile(id).length())

        fake.throttleBytes = null
        assertEquals(1, engine.resumePaused())
        val job = awaitFinished(id)
        assertEquals(FetchStatus.DONE, job.status)
        assertEquals("bytes=${paused.bytesDone}-", fake.fileRequests.last().headers["Range"])
        assertArrayEquals(content, cachedBytes(id))
    }

    @Test
    fun cancelDeletesThePartialBytes() = runBlocking {
        fake.throttleBytes = 32L * 1024
        val id = engine.enqueue(target())
        awaitJob(id) { it.bytesDone > 0 }
        engine.cancel(id)
        assertEquals(FetchStatus.CANCELLED, store.job(id)!!.status)
        assertFalse(files.jobDir(id).exists())
    }

    @Test
    fun atMostTwoFilesPerHostRunAtOnce() = runBlocking {
        fake.throttleBytes = 256L * 1024
        val maxRunning = AtomicInteger(0)
        val watcher = launch(Dispatchers.Default) {
            store.jobs.collect { all -> maxRunning.accumulateAndGet(all.count { it.status == FetchStatus.RUNNING }, ::maxOf) }
        }
        val ids = engine.enqueueAll(listOf(target("a.bin"), target("b.bin"), target("c.bin"), target("d.bin")))
        ids.forEach { assertEquals(FetchStatus.DONE, awaitFinished(it).status) }
        watcher.cancel()
        assertEquals(2, maxRunning.get())
    }

    @Test
    fun theSameFileIsNotQueuedTwiceAndFoldersAreSkipped() = runBlocking {
        fake.throttleBytes = 32L * 1024
        val first = engine.enqueue(target())
        assertEquals(first, engine.enqueue(target()))
        val dir = EntryTarget(host.id, "shr_projects", "out", SampleData.dir("out"))
        assertEquals(listOf(first), engine.enqueueAll(listOf(target(), dir)))
        engine.cancel(first)
    }

    @Test
    fun aJobLeftRunningByADeadProcessIsResumedOnStart() = runBlocking {
        val t = target()
        // Persisted further than the bytes on disk: resume must trust the smaller of the two.
        val id = store.insert(
            FetchJob(
                key = t.fileKey,
                entry = t.entry,
                status = FetchStatus.RUNNING,
                bytesDone = 150_000,
                totalBytes = content.size.toLong(),
                etag = "\"v1\"",
            ),
        )
        files.preparePartial(id).writeBytes(content.copyOfRange(0, 100_000))
        engine.start()
        val job = awaitFinished(id)
        assertEquals(FetchStatus.DONE, job.status)
        assertEquals("bytes=100000-", fake.fileRequests.single().headers["Range"])
        assertArrayEquals(content, cachedBytes(id))
    }

    @Test
    fun keepOfflineSurvivesARefetch() = runBlocking {
        val first = engine.enqueue(target())
        awaitFinished(first)
        assertTrue(store.setPinned(store.job(first)!!.key, true))
        val oldPath = store.cached(store.job(first)!!.key)!!.localPath
        val second = engine.enqueue(target())
        awaitFinished(second)
        val cached = store.cached(store.job(second)!!.key)!!
        assertTrue(cached.pinned)
        assertEquals("fetched/j$second/render.bin", cached.localPath)
        // The old copy goes right after the new one is recorded.
        withTimeout(5_000) { while (files.resolve(oldPath)?.exists() == true) delay(10) }
        assertTrue(files.resolve(cached.localPath)!!.isFile)
    }
}
