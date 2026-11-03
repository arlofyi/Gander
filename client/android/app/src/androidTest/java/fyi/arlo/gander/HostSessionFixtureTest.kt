package fyi.arlo.gander

import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import fyi.arlo.gander.core.data.UiErrorKind
import fyi.arlo.gander.core.data.browse.DefaultBrowseRepository
import fyi.arlo.gander.core.data.browse.InMemorySnapshotStore
import fyi.arlo.gander.core.data.browse.ListQuery
import fyi.arlo.gander.core.data.browse.PinnedHostConnector
import fyi.arlo.gander.core.data.hosts.InMemoryHostRepository
import fyi.arlo.gander.core.data.hosts.PairedHost
import fyi.arlo.gander.core.data.toUiError
import fyi.arlo.gander.core.net.BearerToken
import fyi.arlo.gander.core.net.GanderApi
import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.SpkiPin
import fyi.arlo.gander.core.net.protocol.EntryKind
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Assume.assumeTrue
import org.junit.Test
import org.junit.runner.RunWith

/**
 * Listing and revocation against the real host (WP-29, client spec "Instrumented tests": list,
 * revoke). `cargo xtask e2e-android` pairs two test devices from the development machine and
 * passes their tokens as instrumentation arguments (never committed, never logged):
 *
 * - `gander.host.id`, `gander.host.addresses`, `gander.host.port`: as in `HostThumbnailsFixtureTest`
 * - `gander.host.token`: bearer token of the fixture device (list, preview and fetch granted)
 * - `gander.fixture.share`: id of the fixture share
 * - `gander.fixture.list`: share-relative folder to list
 * - `gander.fixture.list_expect`: comma separated names that folder must contain
 * - `gander.revoke.token`: bearer token of a second device that the harness revokes mid-suite
 *
 * The harness runs [theRevokeDeviceIsAcceptedBeforeTheRevoke] with the main suite, then writes
 * `revoke <device>` to the host's stdin, then runs [aRevokedTokenIsReportedAsTokenRevoked] on its
 * own with `gander.revoke.done=true`. Without the arguments each test is skipped (assumption).
 */
@RunWith(AndroidJUnit4::class)
class HostSessionFixtureTest {
    private val args = InstrumentationRegistry.getArguments()

    private fun arg(name: String): String? = args.getString(name)?.takeIf { it.isNotBlank() }

    private class Fixture(val host: PairedHost, val connector: PinnedHostConnector, val share: String) {
        val api: GanderApi get() = GanderApi(connector.clients(host))
        private val snapshots = InMemorySnapshotStore()
        val browse = DefaultBrowseRepository(connector, listings = snapshots, sharesStore = snapshots)
    }

    private fun fixture(tokenArg: String): Fixture {
        val hostId = arg("gander.host.id")
        val addresses = arg("gander.host.addresses")?.split(',')?.map { it.trim() }
        val token = arg(tokenArg)
        val share = arg("gander.fixture.share")
        assumeTrue(
            "needs a running host, its fixture share and $tokenArg (cargo xtask e2e-android)",
            hostId != null && addresses != null && token != null && share != null,
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
        return Fixture(host, PinnedHostConnector(hosts, "androidTest"), share!!)
    }

    @Test
    fun theFixtureShareAndFolderAreListed() {
        val fixture = fixture("gander.host.token")
        val folder = arg("gander.fixture.list")
        val expected = arg("gander.fixture.list_expect")?.split(',')?.map { it.trim() }.orEmpty()
        assumeTrue("needs gander.fixture.list and gander.fixture.list_expect", folder != null && expected.isNotEmpty())

        runBlocking {
            val shares = fixture.browse.shares(fixture.host)
            assertFalse("live, not a snapshot", shares.isCached)
            assertTrue("the fixture share is listed: ${shares.value.map { it.id }}", shares.value.any { it.id == fixture.share })

            val root = fixture.browse.firstPage(fixture.host, fixture.share, "", ListQuery())
            assertTrue(
                "the share root shows $folder: ${root.entries.map { it.name }}",
                root.entries.any { it.name == folder!!.substringBefore('/') && it.kind == EntryKind.DIR },
            )

            val page = fixture.browse.firstPage(fixture.host, fixture.share, folder!!, ListQuery())
            val names = page.entries.map { it.name }
            expected.forEach { assertTrue("$it is listed in $folder: $names", it in names) }
            assertEquals("one page is enough for the fixture folder", null, page.nextCursor)
        }
    }

    @Test
    fun theRevokeDeviceIsAcceptedBeforeTheRevoke() {
        val fixture = fixture("gander.revoke.token")
        assumeTrue("runs before the revoke", arg("gander.revoke.done") == null)
        runBlocking {
            assertEquals(fixture.host.id, fixture.api.me().host.id)
        }
    }

    @Test
    fun aRevokedTokenIsReportedAsTokenRevoked() {
        val fixture = fixture("gander.revoke.token")
        assumeTrue("runs after the harness revoked the device", arg("gander.revoke.done") == "true")
        runBlocking {
            val me = runCatching { fixture.api.me() }.exceptionOrNull()
            assertTrue("GET /v1/me after the revoke: $me", me is GanderError.Host.TokenRevoked)
            assertEquals(401, (me as GanderError.Host).status)
            assertEquals("the screen tells the user the pairing is gone", UiErrorKind.REVOKED, me.toUiError().kind)

            // The browse repository does not hide a refusal behind an old snapshot.
            try {
                fixture.browse.shares(fixture.host)
                fail("shares after the revoke must throw")
            } catch (e: GanderError.Host.TokenRevoked) {
                assertEquals(401, e.status)
            }
        }
    }
}
