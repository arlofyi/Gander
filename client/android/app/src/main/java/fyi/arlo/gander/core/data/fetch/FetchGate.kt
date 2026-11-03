package fyi.arlo.gander.core.data.fetch

import fyi.arlo.gander.core.data.actions.CachedFileLookup
import fyi.arlo.gander.core.data.actions.NetworkState
import fyi.arlo.gander.core.data.browse.EntryTarget
import fyi.arlo.gander.core.net.protocol.EntryKind

/** What to do when the user asks to fetch one or more files. */
sealed interface FetchDecision {
    /** Nothing fetchable was selected (folders only). */
    data object NothingToFetch : FetchDecision

    /** One file whose current version is already on the phone: open it, no data spent. */
    data class OpenCached(val target: EntryTarget) : FetchDecision

    /** Metered network and at or over the threshold: ask first (client spec, "Cellular rule"). */
    data class ConfirmCellular(val targets: List<EntryTarget>, val totalBytes: Long) : FetchDecision

    data class Start(val targets: List<EntryTarget>) : FetchDecision
}

/**
 * The cellular rule and the saved-copy shortcut, as plain Kotlin (JVM-tested):
 * - folders are dropped (no folder fetch in v1);
 * - a single file whose current version is cached opens directly;
 * - files of a selection that are cached and current are not fetched again;
 * - on a metered network, a total at or over [cellularWarnBytes] needs confirmation.
 */
object FetchGate {
    fun decide(
        targets: List<EntryTarget>,
        network: NetworkState,
        cellularWarnBytes: Long,
        isCachedCurrent: (EntryTarget) -> Boolean,
    ): FetchDecision {
        val files = targets.filter { it.entry.kind == EntryKind.FILE }
        if (files.isEmpty()) return FetchDecision.NothingToFetch
        if (files.size == 1 && isCachedCurrent(files[0])) return FetchDecision.OpenCached(files[0])
        val needed = files.filterNot(isCachedCurrent)
        if (needed.isEmpty()) return FetchDecision.NothingToFetch
        val total = needed.sumOf { it.entry.size.coerceAtLeast(0) }
        return if (network.metered && total >= cellularWarnBytes) {
            FetchDecision.ConfirmCellular(needed, total)
        } else {
            FetchDecision.Start(needed)
        }
    }
}

/**
 * The file sheet's "saved copy" check (WP-25 [CachedFileLookup]) over [OfflineFiles]: true when a
 * copy of the same version (size and modification time) is on the phone.
 */
class OfflineCachedFileLookup(private val offline: OfflineFiles) : CachedFileLookup {
    override suspend fun isCached(target: EntryTarget): Boolean {
        val cached = offline.get(target.fileKey) ?: return false
        return cached.matches(target.entry) && offline.fileOf(cached) != null
    }

    /**
     * The saved copy of [target] when it is the current version (same rule as [isCached]), for
     * callers that hand the file itself out (the Files-app provider serves it as a real fd).
     * Counts as a use for the LRU.
     */
    suspend fun currentCopy(target: EntryTarget): java.io.File? {
        val (cached, file) = offline.open(target.fileKey) ?: return null
        return file.takeIf { cached.matches(target.entry) }
    }
}
