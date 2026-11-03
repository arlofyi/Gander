package fyi.arlo.gander

import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.lifecycle.SavedStateHandle
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import androidx.media3.common.PlaybackException
import androidx.media3.common.Player
import androidx.media3.exoplayer.ExoPlayer
import coil3.ImageLoader
import fyi.arlo.gander.core.data.actions.InMemoryActionRefusals
import fyi.arlo.gander.core.data.browse.EntryTarget
import fyi.arlo.gander.core.data.browse.PinnedHostConnector
import fyi.arlo.gander.core.data.hosts.InMemoryHostRepository
import fyi.arlo.gander.core.data.hosts.PairedHost
import fyi.arlo.gander.core.data.preview.ClientPreviewSource
import fyi.arlo.gander.core.data.preview.CoilPagePrefetcher
import fyi.arlo.gander.core.data.preview.PreviewPageComponents
import fyi.arlo.gander.core.data.preview.PreviewPageLoader
import fyi.arlo.gander.core.data.preview.createGanderPlayer
import fyi.arlo.gander.core.data.preview.createPreviewDiskCache
import fyi.arlo.gander.core.data.preview.videoMediaItem
import fyi.arlo.gander.core.data.thumbs.ClientThumbSource
import fyi.arlo.gander.core.data.thumbs.ThumbLoader
import fyi.arlo.gander.core.data.thumbs.createThumbnailImageLoader
import fyi.arlo.gander.core.net.BearerToken
import fyi.arlo.gander.core.net.GanderApi
import fyi.arlo.gander.core.net.GanderHttpClients
import fyi.arlo.gander.core.net.SpkiPin
import fyi.arlo.gander.core.net.protocol.PreviewKind
import fyi.arlo.gander.ui.preview.PagesViewModel
import fyi.arlo.gander.ui.preview.PagesViewerScreen
import fyi.arlo.gander.ui.preview.PreviewTags
import fyi.arlo.gander.ui.theme.GanderTheme
import kotlinx.coroutines.runBlocking
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assume.assumeTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File
import java.util.UUID

/**
 * The preview viewers against the real host's fixture tree, over the real pinned connection (WP-26
 * verify: "PDF page 1 renders from the host; video fixture plays 3 s").
 *
 * DEFERRED to WP-29: it needs a running host with the Browse and Preview APIs (WP-14, WP-16) and
 * its fixture tree. It compiles with the rest of `src/androidTest` and every test is skipped
 * (assumption failure) unless the harness passes these instrumentation arguments, none of which is
 * committed anywhere:
 *
 * - `gander.host.id`: the host id (base64url SHA-256 of its certificate's SPKI, the pin)
 * - `gander.host.addresses`: comma separated, as the emulator reaches the host (`10.0.2.2` for the
 *   development machine)
 * - `gander.host.port`: optional, defaults to 42637
 * - `gander.host.token`: a bearer token of a paired test device
 * - `gander.fixture.share`: id of the share that holds the fixture tree (needs list, preview AND
 *   fetch: the video's bytes come from `/file`)
 * - `gander.fixture.pdf`: share-relative path of a PDF with at least one page (page 1 test)
 * - `gander.fixture.video`: share-relative path of a video the emulator can decode, 3 s or longer
 *   (H.264 in MP4 plays everywhere)
 * - `gander.fixture.text`: share-relative path of a text file (encoding and slice test)
 *
 * Example: `-Pandroid.testInstrumentationRunnerArguments.gander.host.id=...`
 */
@RunWith(AndroidJUnit4::class)
class HostPreviewFixtureTest {
    @get:Rule
    val composeRule = createComposeRule()

    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val context = instrumentation.targetContext
    private val args = InstrumentationRegistry.getArguments()
    private val dirs = mutableListOf<File>()
    private var loader: ImageLoader? = null
    private var player: ExoPlayer? = null

    @After
    fun tearDown() {
        player?.let { instrumentation.runOnMainSync { it.release() } }
        loader?.shutdown()
        dirs.forEach { it.deleteRecursively() }
    }

    private fun arg(name: String): String? = args.getString(name)?.takeIf { it.isNotBlank() }

    /** The host under test, or a skipped test when the harness did not pass one. */
    private class Fixture(
        val host: PairedHost,
        val hosts: InMemoryHostRepository,
        val connector: PinnedHostConnector,
        val share: String,
    ) {
        val clients: GanderHttpClients get() = connector.clients(host)
        val api: GanderApi get() = GanderApi(clients)
    }

    private fun fixture(vararg needs: String): Fixture {
        val hostId = arg("gander.host.id")
        val addresses = arg("gander.host.addresses")?.split(',')?.map { it.trim() }
        val token = arg("gander.host.token")
        val share = arg("gander.fixture.share")
        assumeTrue(
            "needs a running host and its fixture tree (deferred to WP-29)",
            hostId != null && addresses != null && token != null && share != null,
        )
        needs.forEach { assumeTrue("needs the instrumentation argument $it", arg(it) != null) }

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
        return Fixture(host, hosts, PinnedHostConnector(hosts, "androidTest"), share!!)
    }

    private fun Fixture.imageLoader(source: ClientPreviewSource): ImageLoader {
        val id = UUID.randomUUID()
        val thumbs = File(context.cacheDir, "fixture-thumbs-$id").also { dirs += it }
        val previews = File(context.cacheDir, "fixture-previews-$id").also { dirs += it }
        return createThumbnailImageLoader(
            context = context,
            loader = ThumbLoader(ClientThumbSource { connector.clients(host) }),
            cacheDir = thumbs,
            previews = PreviewPageComponents(
                loader = PreviewPageLoader(source),
                diskCache = createPreviewDiskCache(context, previews),
            ),
        ).also { loader = it }
    }

    @Test
    fun pageOneOfThePdfRendersFromTheHost() {
        val fixture = fixture("gander.fixture.pdf")
        val path = arg("gander.fixture.pdf")!!
        val entry = runBlocking { fixture.api.stat(fixture.share, path) }
        assertEquals("the fixture is a PDF the host can render", PreviewKind.PDF, entry.preview)

        val source = ClientPreviewSource { fixture.connector.clients(fixture.host) }
        val imageLoader = fixture.imageLoader(source)
        val viewModel = PagesViewModel(
            target = EntryTarget(fixture.host.id, fixture.share, path, entry),
            hosts = fixture.hosts,
            source = source,
            prefetcher = CoilPagePrefetcher(context, imageLoader),
            refusalLog = InMemoryActionRefusals(),
            saved = SavedStateHandle(),
        )

        composeRule.setContent {
            GanderTheme {
                PagesViewerScreen(viewModel, entry.name, imageLoader, onBack = {}, onFetch = {})
            }
        }
        // a cold PDF render is well under a second; the allowance covers a busy desktop
        composeRule.waitUntil(timeoutMillis = 30_000) {
            composeRule.onAllNodes(hasTestTag(PreviewTags.pageLoaded(1))).fetchSemanticsNodes().isNotEmpty()
        }
        assertTrue("the host reported a page count", (viewModel.uiState.value.pageCount ?: 0) >= 1)
    }

    @Test
    fun theVideoPlaysThreeSecondsOverTheHostsRangeRequests() {
        val fixture = fixture("gander.fixture.video")
        val path = arg("gander.fixture.video")!!
        val clients = fixture.clients

        var failure: PlaybackException? = null
        instrumentation.runOnMainSync {
            val exo = createGanderPlayer(context, clients)
            player = exo
            exo.addListener(object : Player.Listener {
                override fun onPlayerError(error: PlaybackException) {
                    failure = error
                }
            })
            exo.setMediaItem(videoMediaItem(clients, fixture.share, path))
            exo.prepare()
            exo.playWhenReady = true
        }

        val deadline = System.nanoTime() + 45_000_000_000L
        var position = 0L
        while (System.nanoTime() < deadline && position < 3_000 && failure == null) {
            instrumentation.runOnMainSync { position = player!!.currentPosition }
            Thread.sleep(100)
        }
        assertNull("playback failed: ${failure?.errorCodeName}", failure)
        assertTrue("played $position ms, wanted 3000", position >= 3_000)
        var isPlaying = false
        instrumentation.runOnMainSync { isPlaying = player!!.isPlaying || player!!.playbackState == Player.STATE_READY }
        assertTrue(isPlaying)
    }

    @Test
    fun theTextFileComesInSlicesWithItsEncoding() {
        val fixture = fixture("gander.fixture.text")
        val path = arg("gander.fixture.text")!!
        val source = ClientPreviewSource { fixture.connector.clients(fixture.host) }

        val first = runBlocking { source.text(fixture.host.id, fixture.share, path, offset = 0, limit = 1024) }
        assertNotNull("the host names the encoding it detected", first.encoding)
        assertTrue(first.text.isNotEmpty())
        assertTrue(first.nextOffset > 0)
        if (first.hasMore) {
            val second = runBlocking {
                source.text(fixture.host.id, fixture.share, path, offset = first.nextOffset, limit = 1024)
            }
            assertEquals(first.nextOffset, second.offset)
            assertTrue("the next slice goes on after the first", second.nextOffset > first.nextOffset)
        }
    }
}
