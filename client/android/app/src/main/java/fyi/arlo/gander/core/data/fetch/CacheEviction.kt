package fyi.arlo.gander.core.data.fetch

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.launch
import java.util.concurrent.atomic.AtomicBoolean

/** What [CachePolicy.plan] decided: delete [victims], oldest use first. */
data class EvictionPlan(
    val victims: List<CachedFile>,
    /** Bytes the cache holds before anything is deleted. */
    val usedBytes: Long,
    /**
     * Set when the cache stays over the cap only because the next copy was used a moment ago:
     * try again in this many milliseconds.
     */
    val retryInMillis: Long?,
)

/** What [OfflineFiles.evictToCap] actually deleted. */
data class EvictionResult(
    val evicted: List<CachedFile>,
    val freedBytes: Long,
    val usedBytes: Long,
    val retryInMillis: Long?,
)

/**
 * The LRU rule of the file cache (client spec: "Cache cap default 2 GiB, LRU; pinned files are
 * exempt"), as a pure function so it is covered by plain JVM tests.
 *
 * - Pinned copies ("Keep offline") are never evicted and still count towards the bytes used.
 * - The rest go least recently used first ([CachedFile.lastUsed], then older fetch, then key, so the
 *   order is stable) until the total is at or under the cap.
 * - A copy used or fetched less than [GRACE_MILLIS] ago is not evicted, even over the cap: it was
 *   probably just handed to another app through the `FileProvider`, or the user is about to open
 *   it. The plan says when to look again ([EvictionPlan.retryInMillis]).
 */
object CachePolicy {
    const val GRACE_MILLIS: Long = 10 * 60 * 1000L

    fun plan(files: List<CachedFile>, capBytes: Long, nowMillis: Long, graceMillis: Long = GRACE_MILLIS): EvictionPlan {
        val used = files.sumOf { it.size }
        if (used <= capBytes) return EvictionPlan(emptyList(), used, null)
        val candidates = files.filter { !it.pinned }.sortedWith(LEAST_RECENTLY_USED_FIRST)
        var left = used
        var retry: Long? = null
        val victims = mutableListOf<CachedFile>()
        for (candidate in candidates) {
            if (left <= capBytes) break
            val idle = nowMillis - candidate.lastUsed
            if (idle < graceMillis) {
                // Sorted by use, so every later candidate is at least as recent: stop here.
                retry = graceMillis - idle
                break
            }
            victims += candidate
            left -= candidate.size
        }
        return EvictionPlan(victims, used, retry)
    }

    private val LEAST_RECENTLY_USED_FIRST: Comparator<CachedFile> =
        compareBy<CachedFile> { it.lastUsed }
            .thenBy { it.fetchedAt }
            .thenBy { it.key.hostId }
            .thenBy { it.key.shareId }
            .thenBy { it.key.path }
}

/**
 * Keeps the cache under the user's cap by itself: runs [OfflineFiles.evictToCap] whenever the set
 * of cached files or the cap changes (a finished fetch, a smaller cap in Settings), and again when
 * a copy that was too recent to evict has aged past the grace period. Start it once per process.
 */
class CacheEnforcer(
    private val offline: OfflineFiles,
    private val capBytes: Flow<Long>,
    private val scope: CoroutineScope,
    private val sleep: suspend (Long) -> Unit = { delay(it) },
) {
    private val started = AtomicBoolean(false)

    fun start() {
        if (!started.compareAndSet(false, true)) return
        scope.launch {
            combine(offline.all, capBytes) { _, cap -> cap }.collectLatest { cap -> enforce(cap) }
        }
    }

    /** Evicts to [capBytes], waiting out the grace period when the only candidates are too recent. */
    suspend fun enforce(capBytes: Long) {
        while (true) {
            val result = offline.evictToCap(capBytes)
            val wait = result.retryInMillis ?: return
            sleep(wait + 1)
        }
    }
}
