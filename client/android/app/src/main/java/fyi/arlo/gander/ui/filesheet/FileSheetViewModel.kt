package fyi.arlo.gander.ui.filesheet

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import fyi.arlo.gander.core.data.UiError
import fyi.arlo.gander.core.data.UiErrorKind
import fyi.arlo.gander.core.data.actions.ActionInput
import fyi.arlo.gander.core.data.actions.ActionPlan
import fyi.arlo.gander.core.data.actions.ActionRefusals
import fyi.arlo.gander.core.data.actions.ActionReason
import fyi.arlo.gander.core.data.actions.ActionRules
import fyi.arlo.gander.core.data.actions.CachedFileLookup
import fyi.arlo.gander.core.data.actions.DefaultActionPrefsRepository
import fyi.arlo.gander.core.data.actions.EntryStatSource
import fyi.arlo.gander.core.data.actions.FileAction
import fyi.arlo.gander.core.data.actions.HostStateInfo
import fyi.arlo.gander.core.data.actions.InMemoryActionRefusals
import fyi.arlo.gander.core.data.actions.NetworkState
import fyi.arlo.gander.core.data.actions.NetworkStateMonitor
import fyi.arlo.gander.core.data.actions.PhoneHandlerLookup
import fyi.arlo.gander.core.data.actions.extension
import fyi.arlo.gander.core.data.actions.hasHandlerFor
import fyi.arlo.gander.core.data.actions.toInfo
import fyi.arlo.gander.core.data.browse.BrowseRepository
import fyi.arlo.gander.core.data.browse.EntryTarget
import fyi.arlo.gander.core.data.browse.parentPath
import fyi.arlo.gander.core.data.hosts.HostProber
import fyi.arlo.gander.core.data.hosts.HostRepository
import fyi.arlo.gander.core.data.hosts.HostStatus
import fyi.arlo.gander.core.data.hosts.PairedHost
import fyi.arlo.gander.core.data.toUiError
import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.protocol.Capabilities
import fyi.arlo.gander.core.net.protocol.Entry
import fyi.arlo.gander.core.net.protocol.Permission
import fyi.arlo.gander.core.net.protocol.ServerEvent
import fyi.arlo.gander.core.net.protocol.ShareInfo
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

/** Progress of `stat?meta=1`. */
sealed interface DetailsState {
    /** The host is being asked; the listing's entry is shown meanwhile. */
    data object Loading : DetailsState

    /** The entry (and its metadata) is what the host just said. */
    data object Loaded : DetailsState

    /** The host could not answer; what is shown is what the folder listing said. */
    data class Failed(val error: UiError) : DetailsState
}

data class FileSheetUiState(
    val target: EntryTarget,
    /** The listing's entry until `stat` answers, then the host's fresh one (with `meta`). */
    val entry: Entry,
    /** Lower-case extension without the dot; null when the file has none (nothing to remember). */
    val extension: String?,
    val hostName: String = "",
    val shareName: String = "",
    val details: DetailsState = DetailsState.Loading,
    val network: NetworkState = NetworkState.UNMETERED,
    /** The three actions in display order with enabled state, reason, hint and the default. */
    val plan: ActionPlan,
    /** The action remembered for [extension], whether or not it is usable right now. */
    val remembered: FileAction? = null,
    /** "Remember this choice" is ticked: the next action the user runs becomes the default for [extension]. */
    val rememberChoice: Boolean = false,
    val relayed: Boolean = false,
    val cachedLocally: Boolean = false,
)

/**
 * Logic of the file sheet (client spec, "File sheet"): shows the entry from the listing at once,
 * asks the host for `stat?meta=1`, gathers the rule table's inputs (share permissions, host
 * capabilities, network, remembered choice, phone handler, host state, refusals) and republishes
 * the [ActionPlan] whenever one of them changes.
 *
 * The actions themselves are not run here. [onActionClick] returns the [EntryTarget] to hand to
 * the screen that runs it (preview viewers WP-26, fetch engine WP-27, stream viewer WP-54), and
 * those screens report failures through [ActionRefusals] so the sheet explains them with the
 * protocol's reason instead of a generic failure.
 *
 * Plain constructor arguments, no Android types, so the JVM unit tests drive it with a scripted host.
 */
class FileSheetViewModel(
    private val target: EntryTarget,
    private val hosts: HostRepository,
    private val repository: BrowseRepository,
    private val stat: EntryStatSource,
    private val prefs: DefaultActionPrefsRepository,
    private val network: NetworkStateMonitor,
    private val handlers: PhoneHandlerLookup,
    private val cachedFiles: CachedFileLookup = CachedFileLookup.None,
    /** Optional: asked once for the relay path and fresh capabilities. */
    private val prober: HostProber? = null,
    private val refusalLog: ActionRefusals = InMemoryActionRefusals(),
    private val rules: ActionRules = ActionRules(),
) : ViewModel() {

    private val ext: String? = target.entry.extension()

    private var entry: Entry = target.entry
    private var phoneHasHandler = handlers.hasHandlerFor(entry)
    private var capabilities: Capabilities? = null
    private var permissions: Set<Permission>? = null
    private var shareName = ""
    private var hostName = ""
    private var hostState: HostStateInfo? = null
    private var relayed = false
    private var remembered: FileAction? = null
    private var rememberChoice = false
    private var cached = false
    private var netState: NetworkState = network.current()
    private var details: DetailsState = DetailsState.Loading
    private var fileBlock: ActionReason? = null
    private var refusals: Map<FileAction, ActionReason> = emptyMap()

    private val _state = MutableStateFlow(snapshot())
    val uiState: StateFlow<FileSheetUiState> = _state.asStateFlow()

    private val hostReady = CompletableDeferred<PairedHost?>()
    private var host: PairedHost? = null
    private var detailsJob: Job? = null

    init {
        viewModelScope.launch {
            var wasConnected = netState.connected
            network.updates.collect { now ->
                netState = now
                publish()
                // The network came back after an offline failure: ask again without the user having to.
                val failed = (details as? DetailsState.Failed)?.error
                if (now.connected && !wasConnected && failed?.kind == UiErrorKind.OFFLINE) reload()
                wasConnected = now.connected
            }
        }
        viewModelScope.launch {
            refusalLog.observe(target).collect {
                refusals = it
                publish()
            }
        }
        if (ext != null) {
            viewModelScope.launch {
                prefs.observe(ext).collect {
                    remembered = it
                    publish()
                }
            }
        }
        viewModelScope.launch { bootstrap() }
    }

    // Loading -------------------------------------------------------------------------------------

    private suspend fun bootstrap() {
        val paired = hosts.get(target.hostId)
        host = paired
        hostReady.complete(paired)
        if (paired == null) {
            val error = UiError(UiErrorKind.NOT_PAIRED)
            details = DetailsState.Failed(error)
            fileBlock = ActionReason.fromUiError(error)
            publish()
            return
        }
        hostName = paired.name
        capabilities = paired.capabilities
        cached = safely { cachedFiles.isCached(target) } ?: false
        publish()
        viewModelScope.launch { loadShare(paired) }
        viewModelScope.launch { probe(paired) }
        loadDetails(paired, showLoading = true)
    }

    /** Permissions and the share's name: the stored snapshot first, then the host's current answer. */
    private suspend fun loadShare(paired: PairedHost) {
        repository.cachedShares(paired)?.value?.firstOrNull { it.id == target.shareId }?.let(::applyShare)
        try {
            repository.shares(paired).value.firstOrNull { it.id == target.shareId }?.let(::applyShare)
        } catch (e: CancellationException) {
            throw e
        } catch (_: GanderError) {
            // `stat` reports the problem; permissions stay as last known.
        }
    }

    private fun applyShare(share: ShareInfo) {
        permissions = share.permissions.toSet()
        shareName = share.name
        publish()
    }

    /** Relay path (Stream warns on a relayed connection) and capabilities as fresh as `/me` can say. */
    private suspend fun probe(paired: PairedHost) {
        val status = prober?.probe(paired) as? HostStatus.Online ?: return
        relayed = status.isRelayed
        hosts.get(paired.id)?.capabilities?.let { capabilities = it }
        publish()
    }

    private suspend fun loadDetails(paired: PairedHost, showLoading: Boolean) {
        if (showLoading) {
            details = DetailsState.Loading
            publish()
        }
        try {
            val fresh = stat.stat(paired, target.shareId, target.path)
            if (fresh.mime != entry.mime || fresh.ext != entry.ext) phoneHasHandler = handlers.hasHandlerFor(fresh)
            entry = fresh
            details = DetailsState.Loaded
            fileBlock = null
        } catch (e: CancellationException) {
            throw e
        } catch (e: GanderError) {
            val error = e.toUiError()
            details = DetailsState.Failed(error)
            fileBlock = ActionReason.fromUiError(error)
        }
        publish()
    }

    /** Asks the host again: the sheet's Retry, and what a changed folder or a returning network triggers. */
    fun reload() {
        val paired = host ?: return
        detailsJob?.cancel()
        // Whatever an earlier attempt learned (locked, busy...) may be over; the user asked again.
        refusalLog.clear(target)
        detailsJob = viewModelScope.launch { loadDetails(paired, showLoading = true) }
    }

    // Actions -------------------------------------------------------------------------------------

    /**
     * The user tapped [action]. Returns the target to run it on (with the freshest entry), or null
     * when the action is off. If "remember this choice" is ticked, [action] becomes the remembered
     * default for the file's extension.
     */
    fun onActionClick(action: FileAction): EntryTarget? {
        val option = _state.value.plan.options.firstOrNull { it.action == action } ?: return null
        if (!option.enabled) return null
        val key = ext
        if (rememberChoice && key != null) viewModelScope.launch { prefs.set(key, action) }
        return target.copy(entry = entry)
    }

    fun setRememberChoice(on: Boolean) {
        if (rememberChoice == on) return
        rememberChoice = on
        publish()
    }

    /** Forgets the remembered action for this extension; the rule table chooses again. */
    fun resetDefault() {
        val key = ext ?: return
        viewModelScope.launch { prefs.clear(key) }
    }

    /**
     * An action failed after the sheet handed it over (a 412 `host_locked` from `POST /stream`, a
     * 415 from the preview...). The sheet then shows that reason on the action until the host
     * says otherwise or the sheet reloads. Screens outside the sheet call
     * [ActionRefusals.report] instead; this is the same thing for code that holds the ViewModel.
     */
    fun onActionRefused(action: FileAction, error: GanderError) {
        refusalLog.report(target, action, error)
    }

    // Events --------------------------------------------------------------------------------------

    /** Collects `/events` until cancelled (run while the sheet is visible). */
    suspend fun watchEvents() {
        val paired = hostReady.await() ?: return
        try {
            repository.events(paired).collect { event ->
                when (event) {
                    is ServerEvent.HostState -> {
                        hostState = event.toInfo()
                        dropResolvedStreamRefusal(event.toInfo())
                        publish()
                    }
                    is ServerEvent.SharesUpdated -> viewModelScope.launch { loadShare(paired) }
                    is ServerEvent.ShareChanged ->
                        // The folder holding this file changed: size and metadata may be stale.
                        if (event.share == target.shareId && event.path == parentPath(target.path)) {
                            detailsJob?.cancel()
                            detailsJob = viewModelScope.launch { loadDetails(paired, showLoading = false) }
                        }
                    is ServerEvent.DeviceRevoked -> block(UiError(UiErrorKind.REVOKED))
                    else -> Unit
                }
            }
        } catch (e: GanderError) {
            val error = e.toUiError()
            if (error.kind == UiErrorKind.REVOKED || error.kind == UiErrorKind.WRONG_COMPUTER) block(error)
        }
    }

    private fun block(error: UiError) {
        details = DetailsState.Failed(error)
        fileBlock = ActionReason.fromUiError(error)
        publish()
    }

    private fun dropResolvedStreamRefusal(state: HostStateInfo) {
        val refusal = refusals[FileAction.STREAM]
        val resolved = (refusal == ActionReason.HostLocked && !state.locked) ||
            (refusal == ActionReason.HostUnattended && state.attended)
        if (resolved) refusalLog.clear(target, FileAction.STREAM)
    }

    // State -----------------------------------------------------------------------------------------

    private fun publish() {
        _state.value = snapshot()
    }

    private fun snapshot(): FileSheetUiState {
        val plan = rules.plan(
            ActionInput(
                entry = entry,
                capabilities = capabilities,
                permissions = permissions,
                network = netState,
                remembered = remembered,
                phoneHasHandler = phoneHasHandler,
                cachedLocally = cached,
                hostState = hostState,
                relayed = relayed,
                refusals = refusals,
                fileBlock = fileBlock,
            ),
        )
        return FileSheetUiState(
            target = target,
            entry = entry,
            extension = ext,
            hostName = hostName,
            shareName = shareName,
            details = details,
            network = netState,
            plan = plan,
            remembered = remembered,
            rememberChoice = rememberChoice,
            relayed = relayed,
            cachedLocally = cached,
        )
    }

    /** A failing lookup must never break the sheet: it reads as "nothing known". */
    private inline fun <T> safely(block: () -> T): T? = try {
        block()
    } catch (e: CancellationException) {
        throw e
    } catch (_: Exception) {
        null
    }
}
