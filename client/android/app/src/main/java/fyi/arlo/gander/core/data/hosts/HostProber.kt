package fyi.arlo.gander.core.data.hosts

import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
import fyi.arlo.gander.core.data.UiError
import fyi.arlo.gander.core.data.UiErrorKind
import fyi.arlo.gander.core.data.browse.HostConnector
import fyi.arlo.gander.core.data.toUiError
import fyi.arlo.gander.core.net.GanderError
import kotlinx.coroutines.channels.awaitClose
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.callbackFlow
import kotlinx.coroutines.flow.conflate
import kotlinx.coroutines.flow.distinctUntilChanged

/** Asks one host whether it is there; the Hosts screen calls it every 20 s while visible. */
fun interface HostProber {
    /** Never throws [GanderError]: failures become [HostStatus.Unavailable]. */
    suspend fun probe(host: PairedHost): HostStatus
}

/**
 * `GET /v1/host` (unauthenticated: is anything there, is it the host we pinned) then `GET /v1/me`
 * (relay path, refreshed addresses and capabilities). Both go through the pinned client, so a
 * wrong certificate fails the handshake before any byte of either request is sent.
 */
class ConnectorHostProber(
    private val connector: HostConnector,
    private val hosts: HostRepository,
    private val clock: () -> Long = System::currentTimeMillis,
) : HostProber {

    override suspend fun probe(host: PairedHost): HostStatus {
        val api = connector.connect(host).api
        try {
            val hello = api.host()
            if (hello.hostId != host.pin.hostId) {
                return HostStatus.Unavailable(UiError(UiErrorKind.WRONG_COMPUTER))
            }
        } catch (e: GanderError) {
            return HostStatus.Unavailable(e.toUiError())
        }
        return try {
            val me = api.me()
            hosts.recordContact(host.id, clock(), me)
            HostStatus.Online(relay = me.network.relayed)
        } catch (e: GanderError) {
            val error = e.toUiError()
            if (error.kind == UiErrorKind.REVOKED || error.kind == UiErrorKind.WRONG_COMPUTER) {
                HostStatus.Unavailable(error)
            } else {
                // Reachable, but /me was refused or failed: say so without calling the host offline.
                hosts.recordContact(host.id, clock(), null)
                HostStatus.Online(notice = error)
            }
        }
    }
}

/**
 * Whether a VPN is active on this phone, for the Hosts screen hint "Private network: connected"
 * (client spec). Which VPN cannot be determined (Tailscale exposes no API), so this only says
 * that one is up.
 */
interface PrivateNetworkMonitor {
    val vpnActive: Flow<Boolean>
}

class ConnectivityPrivateNetworkMonitor(context: Context) : PrivateNetworkMonitor {
    private val manager = context.applicationContext.getSystemService(ConnectivityManager::class.java)

    override val vpnActive: Flow<Boolean> = callbackFlow {
        val active = HashSet<Network>()
        val callback = object : ConnectivityManager.NetworkCallback() {
            override fun onAvailable(network: Network) {
                active += network
                trySend(true)
            }

            override fun onLost(network: Network) {
                active -= network
                trySend(active.isNotEmpty())
            }
        }
        val request = NetworkRequest.Builder()
            .addTransportType(NetworkCapabilities.TRANSPORT_VPN)
            .removeCapability(NetworkCapabilities.NET_CAPABILITY_NOT_VPN)
            .build()
        trySend(false)
        manager.registerNetworkCallback(request, callback)
        awaitClose { manager.unregisterNetworkCallback(callback) }
    }.conflate().distinctUntilChanged()
}
