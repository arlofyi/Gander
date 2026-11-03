package fyi.arlo.gander.ui.hosts

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import fyi.arlo.gander.core.data.hosts.HostProber
import fyi.arlo.gander.core.data.hosts.HostRepository
import fyi.arlo.gander.core.data.hosts.HostStatus
import fyi.arlo.gander.core.data.hosts.PairedHost
import fyi.arlo.gander.core.data.hosts.PrivateNetworkMonitor
import kotlinx.coroutines.async
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.onStart
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/** One card on the Hosts screen. */
data class HostCard(val host: PairedHost, val status: HostStatus)

data class HostsUiState(
    val cards: List<HostCard> = emptyList(),
    /** Pull-to-refresh in progress. */
    val refreshing: Boolean = false,
    /** True when a VPN is up on this phone ("Private network: connected"). */
    val privateNetworkUp: Boolean = false,
    /** False until the first emission of the host list, so the empty state does not flash. */
    val loaded: Boolean = false,
)

/**
 * Hosts screen logic. Status comes from [HostProber]; [pingWhileVisible] repeats it every
 * [pingIntervalMs] (client spec: 20 s while visible) and is meant to be run from a
 * `repeatOnLifecycle(STARTED)` block so it stops when the screen is not on top.
 */
class HostsViewModel(
    private val hosts: HostRepository,
    private val prober: HostProber,
    privateNetwork: PrivateNetworkMonitor,
    private val pingIntervalMs: Long = PING_INTERVAL_MS,
    /** Forgets a computer completely (WP-28: fetched files, thumbnails and previews go too). */
    private val forgetHost: suspend (hostId: String) -> Unit = { hosts.forget(it) },
) : ViewModel() {

    private val statuses = MutableStateFlow<Map<String, HostStatus>>(emptyMap())
    private val refreshing = MutableStateFlow(false)

    val uiState: StateFlow<HostsUiState> = combine(
        hosts.hosts,
        statuses,
        refreshing,
        privateNetwork.vpnActive.onStart { emit(false) },
    ) { list, status, isRefreshing, vpn ->
        HostsUiState(
            cards = list.map { HostCard(it, status[it.id] ?: HostStatus.Checking) },
            refreshing = isRefreshing,
            privateNetworkUp = vpn,
            loaded = true,
        )
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(STOP_TIMEOUT_MS), HostsUiState())

    /**
     * Probes every host now and again every [pingIntervalMs], restarting when the host list
     * changes (a newly paired host is probed at once). Runs until cancelled.
     */
    suspend fun pingWhileVisible() {
        // Keyed on the SET of hosts, not the hosts: a probe records last-seen and addresses, which
        // re-emits the list, and that must not restart the loop it came from.
        hosts.hosts.map { list -> list.map { it.id }.toSet() }.distinctUntilChanged().collectLatest {
            while (true) {
                probeAll(hosts.hosts.first())
                delay(pingIntervalMs)
            }
        }
    }

    /** Pull-to-refresh. */
    fun refresh() {
        viewModelScope.launch {
            refreshing.value = true
            try {
                probeAll(hosts.hosts.first())
            } finally {
                refreshing.value = false
            }
        }
    }

    fun rename(hostId: String, name: String) {
        viewModelScope.launch { hosts.rename(hostId, name) }
    }

    fun forget(hostId: String) {
        viewModelScope.launch {
            forgetHost(hostId)
            statuses.update { it - hostId }
        }
    }

    /** Probes all [list] in parallel; each status lands as soon as its host answers. */
    private suspend fun probeAll(list: List<PairedHost>) = coroutineScope {
        list.map { host ->
            async {
                // Probe first: StateFlow.update may run its lambda again when two hosts finish together.
                val status = prober.probe(host)
                statuses.update { it + (host.id to status) }
            }
        }.forEach { it.await() }
    }

    companion object {
        const val PING_INTERVAL_MS = 20_000L
        private const val STOP_TIMEOUT_MS = 5_000L
    }
}
