package fyi.arlo.gander.ui.browser

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import fyi.arlo.gander.core.data.UiError
import fyi.arlo.gander.core.data.UiErrorKind
import fyi.arlo.gander.core.data.browse.BrowseRepository
import fyi.arlo.gander.core.data.hosts.HostRepository
import fyi.arlo.gander.core.data.hosts.PairedHost
import fyi.arlo.gander.core.data.toUiError
import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.protocol.ServerEvent
import fyi.arlo.gander.core.net.protocol.ShareInfo
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

data class SharesUiState(
    val hostName: String = "",
    val shares: List<ShareInfo> = emptyList(),
    /** First load not finished and nothing cached to show. */
    val loading: Boolean = true,
    /** A reload is running (pull-to-refresh or `shares.updated`). */
    val refreshing: Boolean = false,
    /** Set when [shares] is the stored snapshot because the computer is unreachable. */
    val cachedAtMillis: Long? = null,
    val error: UiError? = null,
)

/**
 * Shares of one host. Shows the stored list at once, then the host's answer; `shares.updated`
 * on the events socket reloads it ([watchEvents], run while the screen is visible).
 */
class SharesViewModel(
    private val hostId: String,
    private val hosts: HostRepository,
    private val repository: BrowseRepository,
) : ViewModel() {

    private val _state = MutableStateFlow(SharesUiState())
    val uiState: StateFlow<SharesUiState> = _state.asStateFlow()

    private var host: PairedHost? = null
    private var loadJob: Job? = null

    init {
        load(initial = true)
    }

    fun refresh() = load(initial = false)

    /** Collects `/events` until cancelled; a `shares.updated` reloads the list. */
    suspend fun watchEvents() {
        val paired = host ?: hosts.get(hostId)?.also { host = it } ?: return
        try {
            repository.events(paired).collect { event ->
                when (event) {
                    is ServerEvent.SharesUpdated -> load(initial = false)
                    is ServerEvent.DeviceRevoked -> _state.update { it.copy(error = UiError(UiErrorKind.REVOKED)) }
                    else -> Unit
                }
            }
        } catch (e: GanderError) {
            // Revoked access or a wrong certificate: say so instead of silently going stale.
            _state.update { it.copy(error = e.toUiError()) }
        }
    }

    private fun load(initial: Boolean) {
        loadJob?.cancel()
        loadJob = viewModelScope.launch {
            val paired = host ?: hosts.get(hostId)?.also { host = it }
            if (paired == null) {
                _state.update { it.copy(loading = false, error = UiError(UiErrorKind.NOT_PAIRED)) }
                return@launch
            }
            _state.update { it.copy(hostName = paired.name, refreshing = !initial, error = null) }
            if (initial) {
                repository.cachedShares(paired)?.let { cached ->
                    _state.update { it.copy(shares = cached.value, loading = false) }
                }
            }
            try {
                val loaded = repository.shares(paired)
                _state.update {
                    it.copy(
                        shares = loaded.value,
                        cachedAtMillis = loaded.cachedAtMillis,
                        loading = false,
                        refreshing = false,
                        error = null,
                    )
                }
            } catch (e: CancellationException) {
                throw e
            } catch (e: GanderError) {
                _state.update { it.copy(loading = false, refreshing = false, error = e.toUiError()) }
            }
        }
    }
}
