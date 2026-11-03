package fyi.arlo.gander.ui.settings

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import fyi.arlo.gander.core.data.actions.DefaultActionPrefsRepository
import fyi.arlo.gander.core.data.actions.FileAction
import fyi.arlo.gander.core.data.diagnostics.DiagnosticsCollector
import fyi.arlo.gander.core.data.fetch.VerifyPolicy
import fyi.arlo.gander.core.data.hosts.HostRepository
import fyi.arlo.gander.core.data.hosts.PairedHost
import fyi.arlo.gander.core.data.settings.AppSettings
import fyi.arlo.gander.core.data.settings.ExtensionList
import fyi.arlo.gander.core.data.settings.SettingsRepository
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/** A remembered choice on the Settings screen ("always Preview a .pdf"). */
data class DefaultActionRow(val ext: String, val action: FileAction)

/** Where the diagnostics export is. */
sealed interface DiagnosticsExport {
    data object Idle : DiagnosticsExport

    data object Working : DiagnosticsExport

    /** The finished, redacted [text]; the screen hands it to the share sheet and calls [SettingsViewModel.exportHandled]. */
    data class Ready(val text: String) : DiagnosticsExport

    data object Failed : DiagnosticsExport
}

data class SettingsUiState(
    val settings: AppSettings = AppSettings(),
    /** Remembered default actions, by extension. */
    val defaultActions: List<DefaultActionRow> = emptyList(),
    /** The computers this phone is paired with (the audit of this phone's hosts). */
    val hosts: List<PairedHost> = emptyList(),
    val export: DiagnosticsExport = DiagnosticsExport.Idle,
    /** False until the stored settings have been read, so the screen does not show defaults for a frame. */
    val loaded: Boolean = false,
)

/** Settings screen logic. Every change is written at once; there is no Save button. */
class SettingsViewModel(
    private val settings: SettingsRepository,
    private val defaultActions: DefaultActionPrefsRepository,
    hosts: HostRepository,
    private val diagnostics: DiagnosticsCollector,
) : ViewModel() {

    private val export = MutableStateFlow<DiagnosticsExport>(DiagnosticsExport.Idle)

    val uiState: StateFlow<SettingsUiState> = combine(
        settings.settings,
        defaultActions.observeAll(),
        hosts.hosts,
        export,
    ) { current, remembered, paired, exporting ->
        SettingsUiState(
            settings = current,
            defaultActions = remembered.entries.sortedBy { it.key }.map { DefaultActionRow(it.key, it.value) },
            hosts = paired,
            export = exporting,
            loaded = true,
        )
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(STOP_TIMEOUT_MS), SettingsUiState())

    fun setVerify(policy: VerifyPolicy) = launch { settings.setVerify(policy) }

    fun setCellularWarnBytes(bytes: Long) = launch { settings.setCellularWarnBytes(bytes) }

    fun setCacheCapBytes(bytes: Long) = launch { settings.setCacheCapBytes(bytes) }

    /** [text] is what the user typed ("psd, ai blend"); null goes back to the shipped list. */
    fun setStreamOnlyTypes(text: String?) = launch {
        settings.setStreamOnlyTypes(text?.let(ExtensionList::parse))
    }

    /** The caller has already checked the user with the device credential (see `SettingsScreen`). */
    fun setAppLockEnabled(enabled: Boolean) = launch { settings.setAppLockEnabled(enabled) }

    fun setAppLockMinutes(minutes: Int) = launch { settings.setAppLockMinutes(minutes) }

    fun setFilesAppEnabled(enabled: Boolean) = launch { settings.setFilesAppEnabled(enabled) }

    fun setDefaultAction(ext: String, action: FileAction) = launch { defaultActions.set(ext, action) }

    /** Forgets the choice for [ext]; the rule table decides again. */
    fun resetDefaultAction(ext: String) = launch { defaultActions.clear(ext) }

    fun resetAllDefaultActions() = launch {
        for (row in uiState.value.defaultActions) defaultActions.clear(row.ext)
    }

    /** Builds the redacted report; when it is ready [uiState] carries [DiagnosticsExport.Ready]. */
    fun exportDiagnostics() {
        if (export.value == DiagnosticsExport.Working) return
        export.value = DiagnosticsExport.Working
        viewModelScope.launch {
            export.value = try {
                DiagnosticsExport.Ready(diagnostics.report())
            } catch (e: CancellationException) {
                throw e
            } catch (_: Exception) {
                DiagnosticsExport.Failed
            }
        }
    }

    fun exportHandled() {
        export.update { DiagnosticsExport.Idle }
    }

    private fun launch(block: suspend () -> Unit) {
        viewModelScope.launch { block() }
    }

    private companion object {
        const val STOP_TIMEOUT_MS = 5_000L
    }
}
