package fyi.arlo.gander.core.data.hosts

import fyi.arlo.gander.core.data.browse.BrowserPrefsRepository
import fyi.arlo.gander.core.data.cache.DerivedCaches
import fyi.arlo.gander.core.data.stream.StreamPrefsRepository
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.withContext

/**
 * Forgetting a computer (host details and the Hosts card's menu): nothing of it stays on the phone.
 *
 * In this order:
 * 1. its fetch jobs stop and its jobs, cached files and bytes are deleted (`FetchEngine.forgetHost`);
 * 2. [HostRepository.forget]: token, host row, shares and listing snapshots (the one step whose
 *    failure is reported, because then the host is still paired);
 * 3. [onForgotten] drops the pinned HTTP clients, and the thumbnail and preview caches, the
 *    computer's remembered sort and view modes are purged.
 *
 * Steps 1 and 3 are cleanup that heals itself (the fetch engine sweeps rows of unknown hosts at
 * start; caches are evicted by size), so a failure there does not stop the forget. The whole thing
 * is not cancellable once started: a half-forgotten host is worse than a slow one.
 */
class HostForgetter(
    private val hosts: HostRepository,
    private val derivedCaches: DerivedCaches,
    private val browserPrefs: BrowserPrefsRepository,
    private val purgeFetched: suspend (hostId: String) -> Unit,
    private val onForgotten: (hostId: String) -> Unit = {},
    /** The stream viewer's remembered touch mode of the computer (WP-54). */
    private val streamPrefs: StreamPrefsRepository? = null,
) {
    suspend fun forget(hostId: String) = withContext(NonCancellable) {
        attempt { purgeFetched(hostId) }
        hosts.forget(hostId)
        attempt { onForgotten(hostId) }
        attempt { derivedCaches.clear() }
        attempt { browserPrefs.forgetHost(hostId) }
        attempt { streamPrefs?.forgetHost(hostId) }
    }

    private suspend fun attempt(step: suspend () -> Unit) {
        try {
            step()
        } catch (e: CancellationException) {
            throw e
        } catch (_: Exception) {
            // Cleanup only; see the class comment.
        }
    }
}
