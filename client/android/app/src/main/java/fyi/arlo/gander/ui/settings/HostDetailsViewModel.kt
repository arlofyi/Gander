package fyi.arlo.gander.ui.settings

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import fyi.arlo.gander.core.data.UiError
import fyi.arlo.gander.core.data.UiErrorKind
import fyi.arlo.gander.core.data.hosts.HostForgetter
import fyi.arlo.gander.core.data.hosts.HostRepository
import fyi.arlo.gander.core.data.hosts.PairedHost
import fyi.arlo.gander.core.data.hosts.TokenRotator
import fyi.arlo.gander.core.data.toUiError
import fyi.arlo.gander.core.net.GanderError
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/** What happened to the last "Rotate token" press. */
sealed interface RotateOutcome {
    data object None : RotateOutcome

    data object Working : RotateOutcome

    data object Done : RotateOutcome

    data class Failed(val error: UiError) : RotateOutcome
}

data class HostDetailsUiState(
    /** Null while loading, and after the computer was forgotten. */
    val host: PairedHost? = null,
    val loaded: Boolean = false,
    val rotate: RotateOutcome = RotateOutcome.None,
    /** True once the computer is gone from this phone: the screen closes. */
    val forgotten: Boolean = false,
)

/** Host details: what this phone knows about a computer, token rotation, and forgetting it. */
class HostDetailsViewModel(
    private val hostId: String,
    hosts: HostRepository,
    private val rotator: TokenRotator,
    private val forgetter: HostForgetter,
) : ViewModel() {

    private val rotate = MutableStateFlow<RotateOutcome>(RotateOutcome.None)
    private val forgetting = MutableStateFlow(false)

    val uiState: StateFlow<HostDetailsUiState> = combine(
        hosts.hosts.map { list -> list.firstOrNull { it.id == hostId } },
        rotate,
        forgetting,
    ) { host, rotation, wasForgotten ->
        HostDetailsUiState(host = host, loaded = true, rotate = rotation, forgotten = host == null || wasForgotten)
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(STOP_TIMEOUT_MS), HostDetailsUiState())

    /** `POST /token/rotate` and store the new token. The old one stops working at once on the computer. */
    fun rotateToken() {
        val host = uiState.value.host ?: return
        if (rotate.value == RotateOutcome.Working) return
        rotate.value = RotateOutcome.Working
        viewModelScope.launch {
            rotate.value = try {
                rotator.rotate(host)
                RotateOutcome.Done
            } catch (e: CancellationException) {
                throw e
            } catch (e: GanderError) {
                RotateOutcome.Failed(e.toUiError())
            } catch (_: Exception) {
                RotateOutcome.Failed(UiError(UiErrorKind.OTHER))
            }
        }
    }

    fun dismissRotateMessage() {
        rotate.update { if (it is RotateOutcome.Working) it else RotateOutcome.None }
    }

    fun forget() {
        viewModelScope.launch {
            forgetter.forget(hostId)
            forgetting.value = true
        }
    }

    private companion object {
        const val STOP_TIMEOUT_MS = 5_000L
    }
}
