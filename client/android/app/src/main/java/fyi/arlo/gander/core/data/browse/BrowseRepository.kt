package fyi.arlo.gander.core.data.browse

import fyi.arlo.gander.core.data.UiErrorKind
import fyi.arlo.gander.core.data.hosts.PairedHost
import fyi.arlo.gander.core.data.toUiError
import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.Listing
import fyi.arlo.gander.core.net.protocol.ListResponse
import fyi.arlo.gander.core.net.protocol.ServerEvent
import fyi.arlo.gander.core.net.protocol.ShareInfo
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.flow

/**
 * Everything the shares and browser screens read from a host, with offline snapshots folded in.
 * The UI and its ViewModels depend on this interface only; tests fake it, or run
 * [DefaultBrowseRepository] over a scripted [HostApi].
 *
 * Failure contract: functions throw [GanderError] (map it with `toUiError()`). The `firstPage`
 * and `shares` calls fall back to the last snapshot ONLY when the host cannot be reached
 * (offline, timeout, connection dropped); a refusal such as `token_revoked`, `permission_denied`
 * or a pin mismatch is never papered over with stale data.
 */
interface BrowseRepository {
    /** The shares last seen for [host], without touching the network; null if never loaded. */
    suspend fun cachedShares(host: PairedHost): Loaded<List<ShareInfo>>?

    /** `GET /shares`, or the snapshot (with its age) when the host is unreachable. */
    suspend fun shares(host: PairedHost): Loaded<List<ShareInfo>>

    /** The stored listing of a folder for instant display, without touching the network. */
    suspend fun cachedFirstPage(host: PairedHost, shareId: String, path: String, query: ListQuery): PageResult?

    /**
     * First page of a folder. Revalidates the stored ETag (a 304 returns the stored pages as
     * live), stores the answer, and falls back to the snapshot when the host is unreachable
     * ([PageResult.cachedAtMillis] set).
     */
    suspend fun firstPage(host: PairedHost, shareId: String, path: String, query: ListQuery): PageResult

    /** The page after [cursor], from the host (never cached on its own). */
    suspend fun nextPage(host: PairedHost, shareId: String, path: String, query: ListQuery, cursor: String): PageResult

    /**
     * `GET /shares/{share}/search` under [path]; [cursor] continues a previous answer. [inside]
     * also searches inside files (`scope=content`); only an indexed share can, and a host that
     * cannot answers with names only (see [SearchPage.via]). A cursor belongs to the [inside] it
     * was made with.
     */
    suspend fun search(
        host: PairedHost,
        shareId: String,
        path: String,
        text: String,
        cursor: String? = null,
        inside: Boolean = false,
    ): SearchPage

    /** `GET /recent` (host-wide; the caller filters to a share). */
    suspend fun recent(host: PairedHost, limit: Int = DEFAULT_RECENT_LIMIT): RecentPage

    /** `/v1/events` of [host] with automatic reconnect; fails for revoked access or a wrong certificate. */
    fun events(host: PairedHost): Flow<ServerEvent>

    companion object {
        const val DEFAULT_RECENT_LIMIT = 100
    }
}

class DefaultBrowseRepository(
    private val connector: HostConnector,
    private val listings: ListingSnapshotStore,
    private val sharesStore: SharesSnapshotStore,
    private val clock: () -> Long = System::currentTimeMillis,
    private val pageSize: Int = PAGE_SIZE,
    private val searchPageSize: Int = SEARCH_PAGE_SIZE,
    private val reconnectBaseMs: Long = RECONNECT_BASE_MS,
    private val reconnectMaxMs: Long = RECONNECT_MAX_MS,
) : BrowseRepository {

    private fun api(host: PairedHost): HostApi = connector.connect(host).api

    // Shares ------------------------------------------------------------------------------------

    override suspend fun cachedShares(host: PairedHost): Loaded<List<ShareInfo>>? =
        safely { sharesStore.get(host.id) }?.let { Loaded(it.shares, it.fetchedAtMillis) }

    override suspend fun shares(host: PairedHost): Loaded<List<ShareInfo>> {
        try {
            val shares = api(host).shares().shares
            safely { sharesStore.put(host.id, SharesSnapshot(shares, clock())) }
            return Loaded(shares)
        } catch (e: GanderError) {
            if (e.toUiError().isConnectivity) cachedShares(host)?.let { return it }
            throw e
        }
    }

    // Folders -----------------------------------------------------------------------------------

    override suspend fun cachedFirstPage(
        host: PairedHost,
        shareId: String,
        path: String,
        query: ListQuery,
    ): PageResult? = safely { listings.get(host.id, shareId, path) }?.toPage(query, cachedAt = true)

    override suspend fun firstPage(host: PairedHost, shareId: String, path: String, query: ListQuery): PageResult {
        val snapshot = safely { listings.get(host.id, shareId, path) }
        // The ETag hashes the entry list of one request; only replay it for the same request.
        val validator = snapshot?.takeIf { it.query == query }?.etag
        try {
            val answer = api(host).list(
                share = shareId,
                path = path,
                cursor = null,
                limit = pageSize,
                sort = query.sort.field.wire,
                order = query.sort.order.wire,
                kinds = query.kinds.wire,
                ext = query.extWire,
                ifNoneMatch = validator,
            )
            return when (answer) {
                is Listing.NotModified -> {
                    // A 304 without a validator we sent is a host bug; do not invent a listing.
                    if (snapshot == null || validator == null) {
                        throw GanderError.MalformedResponse(IllegalStateException("304 without If-None-Match"))
                    }
                    safely { listings.put(snapshot.copy(fetchedAtMillis = clock())) }
                    snapshot.toPage(query, cachedAt = false)
                }
                is Listing.Page -> {
                    val response = answer.response
                    safely { listings.put(response.toSnapshot(host.id, shareId, path, query, clock())) }
                    response.toPage()
                }
            }
        } catch (e: GanderError) {
            if (e.toUiError().isConnectivity && snapshot != null) {
                return snapshot.toPage(query, cachedAt = true, offline = true)
            }
            throw e
        }
    }

    override suspend fun nextPage(
        host: PairedHost,
        shareId: String,
        path: String,
        query: ListQuery,
        cursor: String,
    ): PageResult {
        val answer = api(host).list(
            share = shareId,
            path = path,
            cursor = cursor,
            limit = pageSize,
            sort = query.sort.field.wire,
            order = query.sort.order.wire,
            kinds = query.kinds.wire,
            ext = query.extWire,
            ifNoneMatch = null,
        )
        val response = (answer as? Listing.Page)?.response
            ?: throw GanderError.MalformedResponse(IllegalStateException("304 for a request without If-None-Match"))
        appendToSnapshot(host.id, shareId, path, query, cursor, response)
        return response.toPage()
    }

    /**
     * Keeps later pages in the snapshot so an offline folder is not cut at the first page. Only a
     * page that continues the stored one from the same listing (same query, cursor and ETag)
     * is appended; anything else leaves the snapshot alone.
     */
    private suspend fun appendToSnapshot(
        hostId: String,
        shareId: String,
        path: String,
        query: ListQuery,
        cursor: String,
        page: ListResponse,
    ) {
        safely {
            val snapshot = listings.get(hostId, shareId, path) ?: return@safely
            if (snapshot.query != query || snapshot.nextCursor != cursor || snapshot.etag != page.etag) return@safely
            if (snapshot.entries.size + page.entries.size > SNAPSHOT_MAX_ENTRIES) return@safely
            val known = snapshot.entries.mapTo(HashSet()) { it.name }
            listings.put(
                snapshot.copy(
                    entries = snapshot.entries + page.entries.filter { it.name !in known },
                    nextCursor = page.nextCursor,
                    truncated = page.truncated,
                    unlistable = page.unlistable,
                ),
            )
        }
    }

    // Search, recent, events ----------------------------------------------------------------------

    override suspend fun search(
        host: PairedHost,
        shareId: String,
        path: String,
        text: String,
        cursor: String?,
        inside: Boolean,
    ): SearchPage {
        val response = api(host).search(shareId, text, path, searchPageSize, cursor, if (inside) "content" else null)
        return SearchPage(response.entries, response.partial, response.nextCursor, response.via)
    }

    override suspend fun recent(host: PairedHost, limit: Int): RecentPage {
        val response = api(host).recent(limit, null)
        return RecentPage(response.entries, response.partial)
    }

    override fun events(host: PairedHost): Flow<ServerEvent> = flow {
        var wait = reconnectBaseMs
        while (true) {
            try {
                connector.connect(host).events().collect { event ->
                    wait = reconnectBaseMs
                    emit(event)
                }
            } catch (e: GanderError) {
                // These do not get better by retrying; the screen must tell the user.
                val kind = e.toUiError().kind
                if (kind == UiErrorKind.REVOKED || kind == UiErrorKind.WRONG_COMPUTER) throw e
            }
            delay(wait)
            wait = minOf(wait * 2, reconnectMaxMs)
        }
    }

    // Mapping -----------------------------------------------------------------------------------

    private fun ListResponse.toPage() = PageResult(entries, nextCursor, truncated, unlistable)

    private fun ListResponse.toSnapshot(hostId: String, shareId: String, path: String, query: ListQuery, now: Long) =
        ListingSnapshot(hostId, shareId, path, query, etag, entries, nextCursor, truncated, unlistable, now)

    /**
     * [cachedAt] true marks the page as coming from the snapshot (it carries the snapshot's age).
     * [offline] true is the unreachable-host fallback: more pages cannot be fetched, so the cursor
     * is dropped and [PageResult.cacheIncomplete] says the folder was longer.
     */
    private fun ListingSnapshot.toPage(query: ListQuery, cachedAt: Boolean, offline: Boolean = false): PageResult {
        val sameQuery = this.query == query
        val shown = if (sameQuery) entries else EntryOrdering.apply(entries, query)
        // A different sort or filter cannot continue the stored cursor.
        val keepCursor = sameQuery && !offline
        return PageResult(
            entries = shown,
            nextCursor = if (keepCursor) nextCursor else null,
            truncated = truncated,
            unlistable = unlistable,
            cachedAtMillis = if (cachedAt) fetchedAtMillis else null,
            cacheIncomplete = offline && (nextCursor != null || truncated),
        )
    }

    /** Snapshot storage must never break browsing: a failed read or write is a cache miss. */
    private inline fun <T> safely(block: () -> T): T? = try {
        block()
    } catch (e: CancellationException) {
        throw e
    } catch (_: Exception) {
        null
    }

    companion object {
        const val PAGE_SIZE = 200
        const val SEARCH_PAGE_SIZE = 100

        /** A snapshot keeps at most this many entries; it is a cache, not a mirror. */
        const val SNAPSHOT_MAX_ENTRIES = 2000
        const val RECONNECT_BASE_MS = 2_000L
        const val RECONNECT_MAX_MS = 60_000L
    }
}
