package fyi.arlo.gander.ui.settings

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import fyi.arlo.gander.core.data.cache.DerivedCaches
import fyi.arlo.gander.core.data.fetch.CachedFile
import fyi.arlo.gander.core.data.fetch.OfflineFiles
import fyi.arlo.gander.core.data.hosts.HostRepository
import fyi.arlo.gander.core.data.settings.AppSettings
import fyi.arlo.gander.core.data.settings.SettingsRepository
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch

/** The saved copies of one computer. [hostName] is null when the computer was forgotten but rows linger. */
data class HostStorage(
    val hostId: String,
    val hostName: String?,
    val files: List<CachedFile>,
) {
    val bytes: Long get() = files.sumOf { it.size }
    val pinnedBytes: Long get() = files.filter { it.pinned }.sumOf { it.size }
}

data class StorageUiState(
    val loaded: Boolean = false,
    val capBytes: Long = AppSettings.DEFAULT_CACHE_CAP_BYTES,
    /** Bytes of every saved copy, kept offline or not. */
    val usedBytes: Long = 0,
    val pinnedBytes: Long = 0,
    /** Thumbnail and preview caches: rebuilt from the computer whenever needed. */
    val derivedBytes: Long = 0,
    val hosts: List<HostStorage> = emptyList(),
) {
    val unpinnedBytes: Long get() = usedBytes - pinnedBytes

    /** Kept-offline copies alone are over the cap: nothing more can be freed automatically. */
    val pinnedOverCap: Boolean get() = pinnedBytes > capBytes
}

/** Storage screen logic: usage per computer, Keep offline, delete, and clearing the rebuildable caches. */
class StorageViewModel(
    private val offline: OfflineFiles,
    hosts: HostRepository,
    settings: SettingsRepository,
    private val derived: DerivedCaches,
) : ViewModel() {

    private val derivedBytes = MutableStateFlow(0L)

    val uiState: StateFlow<StorageUiState> = combine(
        offline.all,
        hosts.hosts,
        settings.settings.map { it.cacheCapBytes },
        derivedBytes,
    ) { cached, paired, cap, derivedSize ->
        val names = paired.associate { it.id to it.name }
        StorageUiState(
            loaded = true,
            capBytes = cap,
            usedBytes = cached.sumOf { it.size },
            pinnedBytes = cached.filter { it.pinned }.sumOf { it.size },
            derivedBytes = derivedSize,
            hosts = cached.groupBy { it.key.hostId }
                .map { (hostId, files) ->
                    HostStorage(hostId, names[hostId], files.sortedByDescending { it.lastUsed })
                }
                // Known computers first (by name), leftovers of forgotten ones last.
                .sortedWith(compareBy<HostStorage>({ it.hostName == null }, { it.hostName?.lowercase() }, { it.hostId })),
        )
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(STOP_TIMEOUT_MS), StorageUiState())

    init {
        refreshDerived()
    }

    fun setKeepOffline(file: CachedFile, keep: Boolean) {
        viewModelScope.launch { offline.setPinned(file.key, keep) }
    }

    fun remove(file: CachedFile) {
        viewModelScope.launch { offline.remove(file.key) }
    }

    /** Deletes every copy that is not kept offline, of one computer or of all of them. */
    fun clearUnpinned(hostId: String? = null) {
        viewModelScope.launch { offline.removeUnpinned(hostId) }
    }

    /** Empties the thumbnail and preview caches. */
    fun clearDerivedCaches() {
        viewModelScope.launch {
            derived.clear()
            refreshDerived()
        }
    }

    fun refreshDerived() {
        viewModelScope.launch { derivedBytes.value = derived.sizeBytes() }
    }

    private companion object {
        const val STOP_TIMEOUT_MS = 5_000L
    }
}
