package fyi.arlo.gander

import androidx.room.Room
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import fyi.arlo.gander.core.data.db.GanderDatabase
import fyi.arlo.gander.core.data.db.RoomHostRepository
import fyi.arlo.gander.core.data.hosts.AesGcmTokenCipher
import fyi.arlo.gander.core.data.hosts.AndroidKeystoreKeyProvider
import fyi.arlo.gander.core.data.hosts.KeystoreTokenStore
import fyi.arlo.gander.core.data.pairing.DeviceInfo
import fyi.arlo.gander.core.data.pairing.ManualAddress
import fyi.arlo.gander.core.data.pairing.PairingFailure
import fyi.arlo.gander.core.data.pairing.PairingInvite
import fyi.arlo.gander.core.data.pairing.PairingMethod
import fyi.arlo.gander.core.data.pairing.PairingSecret
import fyi.arlo.gander.core.data.pairing.PairingService
import fyi.arlo.gander.core.data.pairing.PairingTarget
import fyi.arlo.gander.core.data.pairing.WaitingForApproval
import fyi.arlo.gander.core.net.GanderApi
import fyi.arlo.gander.core.net.GanderHttpClients
import fyi.arlo.gander.core.net.HostEndpoint
import fyi.arlo.gander.core.net.SpkiPin
import fyi.arlo.gander.ui.pairing.AddHostUiState
import fyi.arlo.gander.ui.pairing.AddHostViewModel
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeout
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Assume.assumeTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File
import java.security.KeyStore
import java.util.Base64
import java.util.UUID

/**
 * Pairs this app with the REAL Rust host (WP-13) from the emulator, flow B, and proves the stored
 * token works. WP-29 starts the host first:
 *
 * ```text
 * gander-host --features test-hooks --test-approve --port <port> --config-dir <tmp> --data-dir <tmp>
 * ```
 *
 * (loopback only; the hook approves loopback pairing requests without a UI, and the emulator's
 * `10.0.2.2` arrives on the host's 127.0.0.1) and passes what it printed as instrumentation
 * arguments:
 *
 * - `gander.host.port`    the port after `listening on 127.0.0.1:<port>` (required)
 * - `gander.host.id`      the host id printed as `host id <id>` (required)
 * - `gander.host.address` where the emulator reaches the host, default `10.0.2.2`
 *
 * `adb shell am instrument -w -e gander.host.port 4xxxx -e gander.host.id <id> -e class
 * fyi.arlo.gander.PairingInstrumentedTest fyi.arlo.gander.test/androidx.test.runner.AndroidJUnitRunner`, or
 * Gradle's `-Pandroid.testInstrumentationRunnerArguments.gander.host.port=4xxxx`.
 *
 * Without the arguments the tests are skipped by `assumeTrue` and say why in the report.
 *
 * The host's `Host` header allow list serves the addresses of its listener plan: in test mode
 * that is 127.0.0.1 only, and `GET /v1/host` with `Host: 10.0.2.2:<port>` is answered 400
 * `bad_host`. If that is still the case, run `adb reverse tcp:<port> tcp:<port>` and pass
 * `gander.host.address=127.0.0.1` (the emulator's own loopback then reaches the host's, and the
 * host still sees a loopback source).
 */
@RunWith(AndroidJUnit4::class)
class PairingInstrumentedTest {
    private val arguments = InstrumentationRegistry.getArguments()
    private val context = InstrumentationRegistry.getInstrumentation().targetContext
    private val address = arguments.getString("gander.host.address") ?: "10.0.2.2"
    private val port = arguments.getString("gander.host.port")?.toIntOrNull()
    private val hostId = arguments.getString("gander.host.id")

    private lateinit var db: GanderDatabase
    private lateinit var scope: CoroutineScope
    private lateinit var tokenFile: File
    private lateinit var alias: String
    private lateinit var repository: RoomHostRepository

    @Before
    fun setUp() {
        assumeTrue("pass -e gander.host.port <port> and -e gander.host.id <id> (see KDoc)", port != null && hostId != null)
        db = Room.inMemoryDatabaseBuilder(context, GanderDatabase::class.java).build()
        scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
        alias = "fyi.arlo.gander.tokens.test.${UUID.randomUUID()}"
        tokenFile = File(context.cacheDir, "pairing-test-${UUID.randomUUID()}.preferences_pb")
        val dataStore = androidx.datastore.preferences.core.PreferenceDataStoreFactory.create(scope = scope) { tokenFile }
        val tokens = KeystoreTokenStore(dataStore, AesGcmTokenCipher(AndroidKeystoreKeyProvider(alias)))
        repository = RoomHostRepository(db.hostDao(), tokens)
    }

    @After
    fun tearDown() {
        if (!::scope.isInitialized) return
        db.close()
        scope.cancel()
        tokenFile.delete()
        runCatching { KeyStore.getInstance("AndroidKeyStore").apply { load(null) }.deleteEntry(alias) }
    }

    private fun service() = PairingService(
        hosts = repository,
        device = { DeviceInfo.fromSystem(context, "0.0.0-instrumented") },
        appVersion = "0.0.0-instrumented",
        pollIntervalMs = 250,
    )

    @Test
    fun manualPairingAgainstTheRealHostStoresAWorkingTokenAndPin() = runBlocking {
        val waiting = mutableListOf<WaitingForApproval>()
        val host = service().pair(PairingTarget.Manual(ManualAddress(address, port!!))) { waiting += it }

        // The host id is the pin of the certificate this phone saw, and it is the one the host printed.
        assertEquals(hostId, host.id)
        assertEquals(SpkiPin.fromHostId(hostId!!), host.pin)
        assertEquals(port, host.port)
        assertEquals("the address that worked comes first", address, host.addresses.first())

        // The SAS derived on the phone was shown and has the documented shape.
        val shown = waiting.single()
        assertEquals(PairingMethod.MANUAL, shown.method)
        assertTrue(Regex("""\d{3} \d{3}""").matches(shown.sas.display))

        // Stored in Room and in the encrypted store, and the token is accepted by the host.
        assertEquals(listOf(host.id), repository.hosts.first().map { it.id })
        val token = repository.token(host.id)
        assertNotNull("the token is in the Keystore-backed store", token)
        val clients = GanderHttpClients(host.pin, HostEndpoint(host.addresses, host.port), "0.0.0-instrumented", { repository.token(host.id) })
        val me = GanderApi(clients).me()
        assertEquals(host.id, me.host.id)
        assertEquals(DeviceInfo.fromSystem(context, "0.0.0-instrumented").name.take(64), me.device.name)

        // The file on disk holds ciphertext only.
        scope.cancel() // lets DataStore flush and release the file
        val onDisk = tokenFile.readBytes()
        assertTrue("token file exists", onDisk.isNotEmpty())
        val plain = token!!.value.toByteArray()
        assertTrue("no plaintext token in the store file", indexOf(onDisk, plain) < 0)
        assertTrue("not even base64url of it", indexOf(onDisk, Base64.getEncoder().encode(plain)) < 0)
    }

    /**
     * The Add host screen runs `pair` in `viewModelScope`, i.e. on the MAIN thread. Closing the
     * pairing connections there used to throw `NetworkOnMainThreadException` (closing a TLS socket
     * writes close_notify) after the token was collected and stored, so the screen said "Pairing
     * failed." although the host had been added. Found on a real tablet (Android 13).
     */
    @Test
    fun manualPairingRunOnTheMainThreadFinishes() = runBlocking {
        val host = withContext(Dispatchers.Main) {
            service().pair(PairingTarget.Manual(ManualAddress(address, port!!))) {}
        }
        assertEquals(hostId, host.id)
        assertEquals(listOf(host.id), repository.hosts.first().map { it.id })
        assertNotNull(repository.token(host.id))
    }

    /**
     * Flow A exactly as the Add host screen runs it (the ViewModel on the main thread), minus the
     * camera: `gander.pair.qr` is a code from the host's open pairing window (`cargo xtask
     * e2e-android` asks the test host for one with `qr <file>` on its stdin). Skipped without it.
     */
    @Test
    fun qrPairingThroughTheAddHostLogicFinishes() = runBlocking {
        val scanned = arguments.getString("gander.pair.qr")
        assumeTrue("pass -e gander.pair.qr <code from the host's qr command>", !scanned.isNullOrBlank())
        // The invite lists the host's own addresses (127.0.0.1, its tailnet IP), which the emulator
        // cannot reach; it reaches the host at [address] (10.0.2.2, or 127.0.0.1 over adb reverse).
        // Only the address list is replaced: the host id (the pin the TLS handshake is checked
        // against), the port and the window secret stay as the host wrote them, so flow A's
        // security check is the same one the app runs.
        val code = withReachableAddress(scanned!!, address)
        val viewModel = withContext(Dispatchers.Main) {
            AddHostViewModel(service()).also {
                it.onChooseScan()
                it.onQrScanned(code)
            }
        }
        val end = withTimeout(60_000) {
            viewModel.state.first {
                it is AddHostUiState.Done || it is AddHostUiState.Failed || (it is AddHostUiState.Scan && it.notice != null)
            }
        }
        assertTrue("flow A ended in $end", end is AddHostUiState.Done)
        val host = (end as AddHostUiState.Done).host
        assertEquals(hostId, host.id)
        assertNotNull(repository.token(host.id))
    }

    @Test
    fun aCodeNamingAnotherComputerIsRefusedBeforeAnythingIsSent() = runBlocking {
        val otherId = SpkiPin.fromHostId(Base64.getUrlEncoder().withoutPadding().encodeToString(ByteArray(32) { 7 })).hostId
        val invite = PairingInvite(
            hostId = otherId,
            addresses = listOf(address),
            port = port!!,
            hostName = "someone else",
            secret = PairingSecret(Base64.getUrlEncoder().withoutPadding().encodeToString(ByteArray(32) { 9 })), // placeholder
            magicDns = null,
        )
        try {
            service().pair(PairingTarget.Qr(invite)) {}
            fail("a computer with another key must be refused")
        } catch (e: PairingFailure) {
            assertEquals(PairingFailure.Kind.WRONG_COMPUTER, e.kind)
        }
        assertTrue(repository.hosts.first().isEmpty())
        assertNull(repository.token(otherId))
    }

    private fun indexOf(haystack: ByteArray, needle: ByteArray): Int {
        outer@ for (i in 0..haystack.size - needle.size) {
            for (j in needle.indices) if (haystack[i + j] != needle[j]) continue@outer
            return i
        }
        return -1
    }

    /** [uri] with its `a=` address list set to [address]; every other field kept byte for byte. */
    private fun withReachableAddress(uri: String, address: String): String {
        val replaced = Regex("([?&])a=[^&]*").replace(uri) { "${it.groupValues[1]}a=$address" }
        assertTrue("the invite has an address list", replaced != uri || uri.contains("a=$address"))
        return replaced
    }
}
