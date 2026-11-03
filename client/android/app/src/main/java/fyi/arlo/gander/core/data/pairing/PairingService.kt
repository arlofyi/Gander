package fyi.arlo.gander.core.data.pairing

import fyi.arlo.gander.core.data.hosts.HostRepository
import fyi.arlo.gander.core.data.hosts.PairedHost
import fyi.arlo.gander.core.net.AddressFallbackInterceptor
import fyi.arlo.gander.core.net.BearerToken
import fyi.arlo.gander.core.net.GanderApi
import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.GanderHttpClients
import fyi.arlo.gander.core.net.HostEndpoint
import fyi.arlo.gander.core.net.ObservingTrustManager
import fyi.arlo.gander.core.net.PairPoll
import fyi.arlo.gander.core.net.SpkiPin
import fyi.arlo.gander.core.net.protocol.PairApproved
import fyi.arlo.gander.core.net.protocol.PairRequest
import fyi.arlo.gander.core.net.protocol.PairingDevice
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.delay
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import okhttp3.ConnectionPool
import okhttp3.Dispatcher
import okhttp3.OkHttpClient
import java.security.SecureRandom

/**
 * Pairing flows A and B (pairing spec), on top of the WP-21 network layer.
 *
 * Flow A (QR): the pin comes from the code. The handshake itself is checked against it
 * ([ObservingTrustManager] with `expected`), so a computer with another certificate fails the
 * handshake before a single byte of the request, secret included, is sent. Then `POST /v1/pair`
 * with the secret.
 *
 * Flow B (typed address): `GET /v1/host` on a trust-on-first-use connection yields the
 * certificate; `host_id` must equal its pin. Everything after that (`POST /v1/pair` without a
 * secret, polling) is a NEW connection pinned to that certificate, so the SAS and the token
 * exchange cannot be split across two computers.
 *
 * In both flows the SAS is derived here from the SPKI of the certificate this phone saw plus both
 * nonces; the `sas` string the computer sends is only cross-checked, never displayed. If the two
 * differ, pairing stops: a man in the middle in flow B shows up exactly like that.
 *
 * Success stores host, pin and token through [HostRepository.add] (the token lands in the
 * Keystore-backed store). Nothing here logs; the secret, nonces and token never reach a log or
 * an exception message.
 */
class PairingService(
    private val hosts: HostRepository,
    private val device: () -> PairingDevice,
    private val appVersion: String,
    private val clientBase: OkHttpClient = OkHttpClient(),
    private val random: SecureRandom = SecureRandom(),
    private val pollIntervalMs: Long = POLL_INTERVAL_MS,
    private val approvalTimeoutMs: Long = APPROVAL_TIMEOUT_MS,
    private val maxPollFailures: Int = MAX_POLL_FAILURES,
    /** Where the pairing connections are closed: closing a TLS socket writes, so never the main thread. */
    private val io: CoroutineDispatcher = Dispatchers.IO,
) : PairingFlow {

    override suspend fun pair(target: PairingTarget, onWaiting: (WaitingForApproval) -> Unit): PairedHost {
        val endpoint = HostEndpoint(target.addresses, target.port)
        val clients = mutableListOf<OkHttpClient>()
        try {
            val pin = when (target) {
                is PairingTarget.Qr -> pinFromInvite(target.invite)
                is PairingTarget.Manual -> discover(endpoint, clients)
            }

            val trust = ObservingTrustManager(expected = pin)
            val client = newClient(endpoint, trust).also(clients::add)
            val api = GanderApi(client, endpoint)
            val deviceInfo = device()
            val deviceNonce = ByteArray(Sas.NONCE_BYTES).also(random::nextBytes)
            val request = PairRequest(
                secret = (target as? PairingTarget.Qr)?.invite?.secret?.value,
                device = deviceInfo,
                nonce = Base64Url.encode(deviceNonce),
            )
            val started = guard { api.startPairing(request) }

            val leaf = trust.observedLeaf ?: throw PairingFailure(PairingFailure.Kind.INCOMPATIBLE)
            val hostNonce = Base64Url.decodeFixed(started.hostNonce, Sas.NONCE_BYTES)
                ?: throw PairingFailure(PairingFailure.Kind.INCOMPATIBLE)
            val sas = Sas.derive(leaf.publicKey.encoded, deviceNonce, hostNonce)
            if (!sas.matches(started.sas)) throw PairingFailure(PairingFailure.Kind.SAS_MISMATCH)

            onWaiting(WaitingForApproval(hostLabel = target.label, sas = sas, method = target.method))
            val approved = awaitApproval(api, started.pairingId)
            return store(target, endpoint, pin, deviceInfo, approved)
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            throw PairingFailure.fromUnexpected(e)
        } finally {
            // The caller is usually viewModelScope (the main thread). Closing a pooled TLS
            // connection sends close_notify, which StrictMode refuses there with
            // NetworkOnMainThreadException: that turned every successful pairing into "Pairing
            // failed." on a real tablet. Close on [io], and never let cleanup replace the result.
            withContext(NonCancellable + io) {
                clients.forEach { runCatching { shutDown(it) } }
            }
        }
    }

    private fun pinFromInvite(invite: PairingInvite): SpkiPin = try {
        SpkiPin.fromHostId(invite.hostId)
    } catch (e: IllegalArgumentException) {
        throw PairingFailure(PairingFailure.Kind.INCOMPATIBLE, cause = e)
    }

    /** Flow B step 1: trust on first use for `GET /v1/host` only. Returns the pin of the certificate seen. */
    private suspend fun discover(endpoint: HostEndpoint, clients: MutableList<OkHttpClient>): SpkiPin {
        val observing = ObservingTrustManager()
        val client = newClient(endpoint, observing).also(clients::add)
        val hello = guard { GanderApi(client, endpoint).host() }
        val pin = observing.observedPin ?: throw PairingFailure(PairingFailure.Kind.INCOMPATIBLE)
        // The host id it claims must be the certificate it presented.
        if (hello.hostId != pin.hostId) throw PairingFailure(PairingFailure.Kind.WRONG_COMPUTER)
        if (SUPPORTED_PROTOCOL !in hello.protocol) throw PairingFailure(PairingFailure.Kind.INCOMPATIBLE)
        return pin
    }

    private suspend fun awaitApproval(api: GanderApi, pairingId: String): PairApproved {
        var approved: PairApproved? = null
        var failures = 0
        withTimeoutOrNull(approvalTimeoutMs) {
            while (approved == null) {
                try {
                    when (val poll = api.pollPairing(pairingId)) {
                        PairPoll.Pending -> failures = 0
                        PairPoll.Denied -> throw PairingFailure(PairingFailure.Kind.DENIED)
                        is PairPoll.Approved -> approved = poll.approved
                    }
                } catch (e: GanderError) {
                    // A blip on the VPN must not abandon a pairing the owner is about to approve.
                    if (!e.isTransientForPolling() || ++failures > maxPollFailures) throw e.toPairingFailure()
                }
                if (approved == null) delay(pollIntervalMs)
            }
        }
        return approved ?: throw PairingFailure(PairingFailure.Kind.EXPIRED)
    }

    private suspend fun store(
        target: PairingTarget,
        endpoint: HostEndpoint,
        pin: SpkiPin,
        deviceInfo: PairingDevice,
        approved: PairApproved,
    ): PairedHost {
        // The token is bound to this pin; a host card that names another id would be a lie.
        if (approved.host.id != pin.hostId) throw PairingFailure(PairingFailure.Kind.WRONG_COMPUTER)
        val token = try {
            BearerToken(approved.token)
        } catch (e: IllegalArgumentException) {
            throw PairingFailure(PairingFailure.Kind.INCOMPATIBLE, cause = e)
        }
        val details = approved.host
        // The address that just worked first (it is reachable from THIS phone by definition), then
        // what the computer says about itself, then what the code said.
        val addresses = (
            listOf(endpoint.current) + details.addresses + listOfNotNull(details.magicDns) + target.addresses
            ).map { it.trim() }.filter { it.isNotEmpty() }.distinct()
        val host = PairedHost(
            id = pin.hostId,
            name = details.name.ifBlank { target.label },
            pin = pin,
            addresses = addresses,
            port = endpoint.port,
            os = details.os,
            pairedAs = deviceInfo.name,
        )
        try {
            hosts.add(host, token)
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            throw PairingFailure(PairingFailure.Kind.STORAGE, cause = e)
        }
        return host
    }

    private fun newClient(endpoint: HostEndpoint, trust: ObservingTrustManager): OkHttpClient {
        // Own dispatcher and pool: they are shut down when this pairing ends, the shared base stays usable.
        val base = clientBase.newBuilder()
            .dispatcher(Dispatcher())
            .connectionPool(ConnectionPool())
            .addInterceptor(AddressFallbackInterceptor(endpoint))
            .build()
        return GanderHttpClients.firstContact(appVersion, trust, base)
    }

    private fun shutDown(client: OkHttpClient) {
        client.dispatcher.executorService.shutdown()
        client.connectionPool.evictAll()
    }

    private suspend fun <T> guard(block: suspend () -> T): T = try {
        block()
    } catch (e: GanderError) {
        throw e.toPairingFailure()
    }

    companion object {
        /** Poll every 1-2 s (protocol spec). */
        const val POLL_INTERVAL_MS = 1_500L

        /** The host drops an unanswered request after 2 minutes; a little slack for the last poll. */
        const val APPROVAL_TIMEOUT_MS = 135_000L

        /** Consecutive failed polls tolerated before giving up. */
        const val MAX_POLL_FAILURES = 5

        private const val SUPPORTED_PROTOCOL = 1
    }
}

private fun GanderError.isTransientForPolling(): Boolean = when (this) {
    is GanderError.Unreachable, is GanderError.Timeout, is GanderError.Network,
    is GanderError.Host.TooManyRequests, is GanderError.Host.Internal,
    -> true
    else -> false
}

/** Maps a transport or protocol failure to what the pairing screens explain. */
internal fun GanderError.toPairingFailure(): PairingFailure {
    val detail = (this as? GanderError.Host)?.hostMessage?.takeIf { it.isNotBlank() }
    return when (this) {
        is GanderError.PinMismatch -> PairingFailure(PairingFailure.Kind.WRONG_COMPUTER, cause = this)
        is GanderError.Unreachable, is GanderError.Timeout, is GanderError.Network ->
            PairingFailure(PairingFailure.Kind.UNREACHABLE, cause = this)
        is GanderError.Tls -> PairingFailure(PairingFailure.Kind.SECURE_CONNECTION, cause = this)
        is GanderError.MalformedResponse -> PairingFailure(PairingFailure.Kind.INCOMPATIBLE, cause = this)
        is GanderError.Host.PairingClosed -> PairingFailure(PairingFailure.Kind.CLOSED, cause = this)
        is GanderError.Host.PairingRateLimited ->
            PairingFailure(PairingFailure.Kind.RATE_LIMITED, retryAfterMs = retryAfterMs, cause = this)
        is GanderError.Host.PairingInProgress -> PairingFailure(PairingFailure.Kind.IN_PROGRESS, cause = this)
        is GanderError.Host.Forbidden -> PairingFailure(PairingFailure.Kind.CODE_REJECTED, detail, cause = this)
        is GanderError.Host.PairingExpired, is GanderError.Host.NotFound ->
            PairingFailure(PairingFailure.Kind.EXPIRED, cause = this)
        is GanderError.Host -> PairingFailure(PairingFailure.Kind.OTHER, detail, cause = this)
    }
}
