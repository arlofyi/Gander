package fyi.arlo.gander

import androidx.room.Room
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import fyi.arlo.gander.core.data.actions.FixedNetworkStateMonitor
import fyi.arlo.gander.core.data.browse.EntryTarget
import fyi.arlo.gander.core.data.browse.PinnedHostConnector
import fyi.arlo.gander.core.data.db.GanderDatabase
import fyi.arlo.gander.core.data.db.RoomFetchStore
import fyi.arlo.gander.core.data.fetch.ClientsFetchConnector
import fyi.arlo.gander.core.data.fetch.FetchConnector
import fyi.arlo.gander.core.data.fetch.FetchEngine
import fyi.arlo.gander.core.data.fetch.FetchFiles
import fyi.arlo.gander.core.data.fetch.FetchSettings
import fyi.arlo.gander.core.data.fetch.FetchStatus
import fyi.arlo.gander.core.data.fetch.VerifyPolicy
import fyi.arlo.gander.core.data.fetch.hash.hashFile
import fyi.arlo.gander.core.data.hosts.InMemoryHostRepository
import fyi.arlo.gander.core.data.hosts.PairedHost
import fyi.arlo.gander.core.net.BearerToken
import fyi.arlo.gander.core.net.GanderApi
import fyi.arlo.gander.core.net.SpkiPin
import fyi.arlo.gander.core.net.protocol.HashAlgo
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import okhttp3.Interceptor
import okhttp3.OkHttpClient
import okhttp3.Response
import okhttp3.ResponseBody.Companion.asResponseBody
import okio.Buffer
import okio.ForwardingSource
import okio.buffer
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Assume.assumeTrue
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File
import java.io.IOException
import java.util.Collections
import java.util.UUID
import java.util.concurrent.atomic.AtomicBoolean

/**
 * WP-27 verify, instrumented half: the 50 MiB fixture downloads from the real host with a forced
 * reconnect mid-way, resumes with `Range` + `If-Range` on a new connection, and verifies against
 * the host's BLAKE3.
 *
 * DEFERRED to WP-29 (needs a running host and the emulator). It compiles with the rest of
 * `src/androidTest` and is skipped (assumption failure) unless these instrumentation arguments
 * are given (same convention as `HostThumbnailsFixtureTest`; nothing secret is committed):
 *
 * - `gander.host.id`: host id (base64url SHA-256 of the certificate's SPKI)
 * - `gander.host.addresses`: comma separated, as the emulator reaches the host (`10.0.2.2`)
 * - `gander.host.port`: optional, defaults to 42637
 * - `gander.host.token`: bearer token of a paired test device
 * - `gander.fixture.share`: id of the share holding the fixture tree
 * - `gander.fixture.large`: share-relative path of a file of about 50 MiB (any content)
 * - `gander.fixture.drop_at`: optional, byte offset of the forced disconnect (default 20 MiB)
 */
@RunWith(AndroidJUnit4::class)
class HostFetchFixtureTest {
    private val context = InstrumentationRegistry.getInstrumentation().targetContext
    private val args = InstrumentationRegistry.getArguments()
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val root = File(context.cacheDir, "fetch-fixture-${UUID.randomUUID()}")
    private var db: GanderDatabase? = null

    @After
    fun tearDown() {
        scope.cancel()
        db?.close()
        root.deleteRecursively()
    }

    private fun arg(name: String): String? = args.getString(name)?.takeIf { it.isNotBlank() }

    /** Fails the first file response's body once after [dropAt] bytes, like a dropped Wi-Fi. */
    private class DropOnce(private val dropAt: Long) : Interceptor {
        val dropped = AtomicBoolean(false)

        override fun intercept(chain: Interceptor.Chain): Response {
            val response = chain.proceed(chain.request())
            if (dropped.get() || !chain.request().url.encodedPath.endsWith("/file")) return response
            val body = response.body
            val source = object : ForwardingSource(body.source()) {
                var seen = 0L

                override fun read(sink: Buffer, byteCount: Long): Long {
                    if (seen >= dropAt && dropped.compareAndSet(false, true)) {
                        chain.call().cancel() // the socket goes, as on a network change
                        throw IOException("forced disconnect for the test")
                    }
                    val n = super.read(sink, minOf(byteCount, (dropAt - seen).coerceAtLeast(1)))
                    if (n > 0) seen += n
                    return n
                }
            }
            return response.newBuilder().body(source.buffer().asResponseBody(body.contentType(), body.contentLength())).build()
        }
    }

    @Test
    fun fiftyMebibyteFixtureResumesAfterAForcedReconnectAndVerifies() {
        val hostId = arg("gander.host.id")
        val addresses = arg("gander.host.addresses")?.split(',')?.map { it.trim() }
        val token = arg("gander.host.token")
        val share = arg("gander.fixture.share")
        val path = arg("gander.fixture.large")
        assumeTrue(
            "needs a running host and its 50 MiB fixture (deferred to WP-29)",
            hostId != null && addresses != null && token != null && share != null && path != null,
        )
        val dropAt = arg("gander.fixture.drop_at")?.toLong() ?: (20L * 1024 * 1024)

        val pin = SpkiPin.fromHostId(hostId!!)
        val host = PairedHost(
            id = pin.hostId,
            name = "fixture",
            pin = pin,
            addresses = addresses!!,
            port = arg("gander.host.port")?.toInt() ?: 42637,
        )
        val hosts = InMemoryHostRepository(listOf(host))
        runBlocking { hosts.seedToken(host.id, BearerToken(token!!)) }
        val pinned = PinnedHostConnector(hosts, "androidTest")
        val base = ClientsFetchConnector { pinned.clients(it) }
        val drop = DropOnce(dropAt)
        val requests = Collections.synchronizedList(mutableListOf<Pair<String?, String?>>())
        val transfer: OkHttpClient = pinned.clients(host).transfer.newBuilder()
            .addInterceptor(drop)
            .addNetworkInterceptor { chain ->
                if (chain.request().url.encodedPath.endsWith("/file")) {
                    requests += chain.request().header("Range") to chain.request().header("If-Range")
                }
                chain.proceed(chain.request())
            }
            .build()
        val connector = object : FetchConnector by base {
            override fun transfer(host: PairedHost) = transfer
        }

        val database = Room.inMemoryDatabaseBuilder(context, GanderDatabase::class.java).build().also { db = it }
        val store = RoomFetchStore(database.fetchDao())
        val files = FetchFiles(root)
        val engine = FetchEngine(
            store = store,
            files = files,
            hosts = hosts,
            connector = connector,
            network = FixedNetworkStateMonitor(),
            scope = scope,
            settings = { FetchSettings(verify = VerifyPolicy.ALWAYS) },
        )

        runBlocking {
            val entry = GanderApi(pinned.clients(host)).stat(share!!, path!!)
            assertTrue("the fixture should be about 50 MiB", entry.size >= 40L * 1024 * 1024)
            val id = engine.enqueue(EntryTarget(host.id, share, path, entry))
            val job = withTimeout(10 * 60 * 1000L) {
                store.jobs.first { all -> all.any { it.id == id && it.status in FetchStatus.FINISHED } }.first { it.id == id }
            }
            assertEquals("job failed: ${job.failure} ${job.failureCode}", FetchStatus.DONE, job.status)
            assertTrue("the disconnect happened", drop.dropped.get())
            assertEquals(HashAlgo.BLAKE3, job.verifiedWith)
            assertEquals(entry.size, job.bytesDone)

            // First request from zero; the next one resumes where the bytes stopped, same version.
            assertTrue("at least two requests: $requests", requests.size >= 2)
            assertEquals(null, requests[0].first)
            val resumeAt = requests[1].first?.removePrefix("bytes=")?.removeSuffix("-")?.toLong()
            assertTrue("resumed from a non-zero offset: ${requests[1].first}", resumeAt != null && resumeAt > 0)
            assertEquals(job.etag, requests[1].second)

            val cached = store.cached(job.key)!!
            val file = files.resolve(cached.localPath)!!
            assertEquals(entry.size, file.length())
            val hostHash = GanderApi(pinned.clients(host)).hash(share, path, HashAlgo.BLAKE3)
            assertEquals(hostHash.hex, hashFile(file, HashAlgo.BLAKE3))
        }
    }
}
