package fyi.arlo.gander.ui.pairing

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import fyi.arlo.gander.core.data.hosts.PairedHost
import fyi.arlo.gander.core.data.pairing.ManualAddress
import fyi.arlo.gander.core.data.pairing.PairingFailure
import fyi.arlo.gander.core.data.pairing.PairingFlow
import fyi.arlo.gander.core.data.pairing.PairingMethod
import fyi.arlo.gander.core.data.pairing.PairingTarget
import fyi.arlo.gander.core.data.pairing.PairingUri
import fyi.arlo.gander.core.data.pairing.PairingUriError
import fyi.arlo.gander.core.data.pairing.PairingUriResult
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

/** Why a scanned code was not used; the camera keeps looking. */
enum class ScanNotice { NOT_GANDER, VERSION, MALFORMED }

/** The Add host screens. One state at a time; the ViewModel is the state machine. */
sealed interface AddHostUiState {
    /** Scan or type: the first screen, with the "only add your own computer" notice. */
    data object Choose : AddHostUiState

    /** Camera open, looking for the QR code (flow A). */
    data class Scan(val notice: ScanNotice? = null) : AddHostUiState

    /** Address entry (flow B). [invalid] marks the field after a rejected submit. */
    data class Manual(val address: String = "", val invalid: Boolean = false) : AddHostUiState

    /** Reaching the computer and sending the request. */
    data class Connecting(val hostLabel: String) : AddHostUiState

    /** The request is with the owner. [sas] is the six digits, "482 913", derived on this phone. */
    data class Waiting(val hostLabel: String, val sas: String, val method: PairingMethod) : AddHostUiState

    data class Failed(
        val kind: PairingFailure.Kind,
        val detail: String?,
        val method: PairingMethod,
        /** True if "Try again" repeats the same attempt; false if it goes back to scanning or typing. */
        val retrySame: Boolean,
    ) : AddHostUiState

    /** Paired and stored. */
    data class Done(val host: PairedHost) : AddHostUiState
}

/**
 * True while the window shows the QR code being scanned, the code in flight or the SAS: the client
 * spec asks for `FLAG_SECURE` then (no screenshots, no recents thumbnail).
 */
val AddHostUiState.showsSecrets: Boolean
    get() = this is AddHostUiState.Scan || this is AddHostUiState.Connecting || this is AddHostUiState.Waiting

/**
 * Logic of the Add host flow: choose, scan or type, connect, show the SAS, wait for the owner,
 * done or a failure with a way forward. The QR secret lives only in [lastTarget] (never in a UI
 * state, so it is not in anything that gets rendered or restored) and is dropped on success or
 * when the user leaves the flow.
 */
class AddHostViewModel(private val pairing: PairingFlow) : ViewModel() {

    private val _state = MutableStateFlow<AddHostUiState>(AddHostUiState.Choose)
    val state: StateFlow<AddHostUiState> = _state.asStateFlow()

    private var job: Job? = null
    private var lastTarget: PairingTarget? = null

    fun onChooseScan() {
        _state.value = AddHostUiState.Scan()
    }

    fun onChooseManual() {
        _state.value = AddHostUiState.Manual()
    }

    /** The scanner read [raw]. Anything outside the Scan state (a late frame) is ignored. */
    fun onQrScanned(raw: String) {
        val current = _state.value as? AddHostUiState.Scan ?: return
        when (val parsed = PairingUri.parse(raw)) {
            is PairingUriResult.Success -> start(PairingTarget.Qr(parsed.invite))
            is PairingUriResult.Failure -> {
                val notice = when (parsed.error) {
                    PairingUriError.NotGander -> ScanNotice.NOT_GANDER
                    PairingUriError.Version -> ScanNotice.VERSION
                    is PairingUriError.Field -> ScanNotice.MALFORMED
                }
                // Frames repeat; only a change is news.
                if (current.notice != notice) _state.value = AddHostUiState.Scan(notice)
            }
        }
    }

    fun onManualAddressChanged(text: String) {
        if (_state.value is AddHostUiState.Manual) _state.value = AddHostUiState.Manual(text)
    }

    fun onManualSubmit() {
        val current = _state.value as? AddHostUiState.Manual ?: return
        val address = ManualAddress.parse(current.address)
        if (address == null) {
            _state.value = current.copy(invalid = true)
        } else {
            start(PairingTarget.Manual(address))
        }
    }

    /** "Try again" on a failure. */
    fun onRetry() {
        val failed = _state.value as? AddHostUiState.Failed ?: return
        val target = lastTarget
        when {
            failed.retrySame && target != null -> start(target)
            failed.method == PairingMethod.QR -> {
                lastTarget = null
                _state.value = AddHostUiState.Scan()
            }
            else -> {
                val typed = (target as? PairingTarget.Manual)?.address?.toEntryText().orEmpty()
                lastTarget = null
                _state.value = AddHostUiState.Manual(typed)
            }
        }
    }

    /** Cancel on the progress screens, or back from a failure: stop, forget the code, start over. */
    fun onCancel() {
        job?.cancel()
        job = null
        lastTarget = null
        _state.value = AddHostUiState.Choose
    }

    /**
     * System back. Returns true if the flow handled it (went one step back or cancelled), false
     * if the screen should close.
     */
    fun onBack(): Boolean = when (_state.value) {
        AddHostUiState.Choose, is AddHostUiState.Done -> false
        is AddHostUiState.Connecting, is AddHostUiState.Waiting, is AddHostUiState.Failed -> {
            onCancel()
            true
        }
        is AddHostUiState.Scan, is AddHostUiState.Manual -> {
            _state.value = AddHostUiState.Choose
            true
        }
    }

    private fun start(target: PairingTarget) {
        job?.cancel()
        lastTarget = target
        _state.value = AddHostUiState.Connecting(target.label)
        job = viewModelScope.launch {
            try {
                val host = pairing.pair(target) { waiting ->
                    _state.value = AddHostUiState.Waiting(waiting.hostLabel, waiting.sas.display, waiting.method)
                }
                lastTarget = null
                _state.value = AddHostUiState.Done(host)
            } catch (e: CancellationException) {
                throw e
            } catch (e: PairingFailure) {
                fyi.arlo.gander.core.DebugLog.failure("pairing (${e.kind})", e)
                _state.value = AddHostUiState.Failed(e.kind, e.detail, target.method, e.canRetrySameTarget)
            } catch (e: Exception) {
                fyi.arlo.gander.core.DebugLog.failure("pairing (unexpected)", e)
                // Never show the exception text: it may carry URLs or addresses. The kind (and at
                // most the exception's class name) says enough.
                val failure = PairingFailure.fromUnexpected(e)
                _state.value = AddHostUiState.Failed(failure.kind, failure.detail, target.method, failure.canRetrySameTarget)
            }
        }
    }

    override fun onCleared() {
        lastTarget = null
    }
}
