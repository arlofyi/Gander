package fyi.arlo.gander.core.data.actions

import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import kotlinx.coroutines.channels.awaitClose
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.callbackFlow
import kotlinx.coroutines.flow.conflate
import kotlinx.coroutines.flow.distinctUntilChanged

/**
 * What the file sheet's rule table needs to know about the phone's connection: connected or not,
 * metered or not. An interface so the rules and the sheet's ViewModel are tested without
 * `ConnectivityManager`.
 */
interface NetworkStateMonitor {
    /** The state right now. */
    fun current(): NetworkState

    /** [current] first, then every change. */
    val updates: Flow<NetworkState>
}

/** Test and preview implementation: set [state] to simulate a network change. */
class FixedNetworkStateMonitor(initial: NetworkState = NetworkState.UNMETERED) : NetworkStateMonitor {
    private val flow = MutableStateFlow(initial)

    var state: NetworkState
        get() = flow.value
        set(value) {
            flow.value = value
        }

    override fun current(): NetworkState = flow.value

    override val updates: Flow<NetworkState> = flow
}

/**
 * [NetworkStateMonitor] over `ConnectivityManager`: the default network's `NOT_METERED`
 * capability (what `isActiveNetworkMetered` reports). While a VPN such as Tailscale is the
 * default network, Android carries the underlying network's metering through only if the VPN app
 * declares its underlying networks; when it does not, the VPN network reads as unmetered and the
 * sheet treats a mobile connection as free. That is the platform's answer and is the best this
 * app can know (the Tailscale app exposes no API).
 */
class ConnectivityNetworkStateMonitor(context: Context) : NetworkStateMonitor {
    private val manager = context.applicationContext.getSystemService(ConnectivityManager::class.java)

    override fun current(): NetworkState = read(manager.activeNetwork)

    override val updates: Flow<NetworkState> = callbackFlow {
        val callback = object : ConnectivityManager.NetworkCallback() {
            override fun onAvailable(network: Network) {
                trySend(read(network))
            }

            override fun onCapabilitiesChanged(network: Network, networkCapabilities: NetworkCapabilities) {
                trySend(stateOf(networkCapabilities))
            }

            override fun onLost(network: Network) {
                // Another network may have taken over the default slot; ask instead of guessing.
                trySend(current())
            }
        }
        trySend(current())
        manager.registerDefaultNetworkCallback(callback)
        awaitClose { manager.unregisterNetworkCallback(callback) }
    }.conflate().distinctUntilChanged()

    private fun read(network: Network?): NetworkState {
        val capabilities = network?.let { manager.getNetworkCapabilities(it) } ?: return NetworkState.OFFLINE
        return stateOf(capabilities)
    }

    private fun stateOf(capabilities: NetworkCapabilities) = NetworkState(
        connected = true,
        metered = !capabilities.hasCapability(NetworkCapabilities.NET_CAPABILITY_NOT_METERED),
    )
}
