package fyi.arlo.gander

import androidx.compose.foundation.layout.Column
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import coil3.ImageLoader
import fyi.arlo.gander.core.data.browse.PinnedHostConnector
import fyi.arlo.gander.core.data.hosts.InMemoryHostRepository
import fyi.arlo.gander.core.data.hosts.PairedHost
import fyi.arlo.gander.core.data.thumbs.ClientThumbSource
import fyi.arlo.gander.core.data.thumbs.ThumbLoader
import fyi.arlo.gander.core.data.thumbs.createThumbnailImageLoader
import fyi.arlo.gander.core.net.BearerToken
import fyi.arlo.gander.core.net.GanderApi
import fyi.arlo.gander.core.net.SpkiPin
import fyi.arlo.gander.ui.browser.EntryDisplay
import fyi.arlo.gander.ui.browser.EntryRow
import fyi.arlo.gander.ui.browser.LocalThumbnailScope
import fyi.arlo.gander.ui.browser.ThumbnailScope
import fyi.arlo.gander.ui.browser.ThumbnailTags
import fyi.arlo.gander.ui.theme.GanderTheme
import kotlinx.coroutines.runBlocking
import org.junit.After
import org.junit.Assume.assumeTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File
import java.util.UUID

/**
 * Thumbnails of the host's fixture images, over the real pinned connection (WP-24 verify:
 * "thumbnails appear for fixture images within 5 s on the emulator").
 *
 * DEFERRED to WP-29: it needs a running host with the Browse API (WP-14) and its fixture tree.
 * It compiles with the rest of `src/androidTest` and is skipped (assumption failure) unless the
 * harness passes these instrumentation arguments, none of which is committed anywhere:
 *
 * - `gander.host.id`: the host id (base64url SHA-256 of its certificate's SPKI, the pin)
 * - `gander.host.addresses`: comma separated, as the emulator reaches the host (`10.0.2.2` for the
 *   development machine)
 * - `gander.host.port`: optional, defaults to 42637
 * - `gander.host.token`: a bearer token of a paired test device
 * - `gander.fixture.share`: id of the share that holds the fixture tree
 * - `gander.fixture.images`: comma separated share-relative paths of fixture images
 *
 * Example: `-Pandroid.testInstrumentationRunnerArguments.gander.host.id=...`
 */
@RunWith(AndroidJUnit4::class)
class HostThumbnailsFixtureTest {
    @get:Rule
    val composeRule = createComposeRule()

    private val context = InstrumentationRegistry.getInstrumentation().targetContext
    private val args = InstrumentationRegistry.getArguments()
    private val cacheDir = File(context.cacheDir, "thumbs-fixture-${UUID.randomUUID()}")
    private var loader: ImageLoader? = null

    @After
    fun tearDown() {
        loader?.shutdown()
        cacheDir.deleteRecursively()
    }

    private fun arg(name: String): String? = args.getString(name)?.takeIf { it.isNotBlank() }

    @Test
    fun thumbnailsOfTheFixtureImagesAppearWithinFiveSeconds() {
        val hostId = arg("gander.host.id")
        val addresses = arg("gander.host.addresses")?.split(',')?.map { it.trim() }
        val token = arg("gander.host.token")
        val share = arg("gander.fixture.share")
        val images = arg("gander.fixture.images")?.split(',')?.map { it.trim() }.orEmpty()
        assumeTrue(
            "needs a running host and its fixture tree (deferred to WP-29)",
            hostId != null && addresses != null && token != null && share != null && images.isNotEmpty(),
        )

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
        val connector = PinnedHostConnector(hosts, "androidTest")
        val imageLoader = createThumbnailImageLoader(
            context = context,
            loader = ThumbLoader(ClientThumbSource { connector.clients(host) }),
            cacheDir = cacheDir,
        ).also { loader = it }

        // The entries exactly as the host lists them (mtime and size feed the cache key).
        val api = GanderApi(connector.clients(host))
        val entries = runBlocking { images.map { it to api.stat(share!!, it) } }

        composeRule.setContent {
            GanderTheme {
                CompositionLocalProvider(
                    LocalThumbnailScope provides ThumbnailScope(host.id, share!!, imageLoader = imageLoader),
                ) {
                    Column {
                        entries.forEach { (path, entry) ->
                            EntryRow(EntryDisplay(entry, path), onClick = {}, onLongClick = {})
                        }
                    }
                }
            }
        }
        composeRule.waitUntil(timeoutMillis = 5_000) {
            images.all { path ->
                // the row merges its children: the thumbnail's tag is in the unmerged tree
                composeRule.onAllNodes(hasTestTag(ThumbnailTags.loaded(path)), useUnmergedTree = true)
                    .fetchSemanticsNodes().isNotEmpty()
            }
        }
    }
}
