package fyi.arlo.gander.core.data.browse

import fyi.arlo.gander.core.net.protocol.Entry
import fyi.arlo.gander.core.net.protocol.ShareInfo
import java.util.concurrent.ConcurrentHashMap

/**
 * The last listing of one folder, kept for offline display, instant back-navigation and ETag
 * revalidation (client spec, "Data and cache"). One snapshot per host, share and path: the pages
 * loaded so far, in the order the host sent them for [query].
 */
data class ListingSnapshot(
    val hostId: String,
    val shareId: String,
    val path: String,
    /** The request the entries answer; the ETag is only sent for an identical request. */
    val query: ListQuery,
    /** `ETag` of the first page, sent back as `If-None-Match`. */
    val etag: String,
    val entries: List<Entry>,
    /** Cursor for the page after [entries]; null when the listing is complete. */
    val nextCursor: String?,
    val truncated: Boolean,
    val unlistable: Int,
    val fetchedAtMillis: Long,
)

/** The shares of one host as last seen. */
data class SharesSnapshot(val shares: List<ShareInfo>, val fetchedAtMillis: Long)

interface ListingSnapshotStore {
    suspend fun get(hostId: String, shareId: String, path: String): ListingSnapshot?

    /** Replaces the snapshot of that folder. */
    suspend fun put(snapshot: ListingSnapshot)
}

interface SharesSnapshotStore {
    suspend fun get(hostId: String): SharesSnapshot?

    suspend fun put(hostId: String, snapshot: SharesSnapshot)
}

/** Test and preview implementation of both stores. */
class InMemorySnapshotStore : ListingSnapshotStore, SharesSnapshotStore {
    private data class Key(val hostId: String, val shareId: String, val path: String)

    private val listings = ConcurrentHashMap<Key, ListingSnapshot>()
    private val shares = ConcurrentHashMap<String, SharesSnapshot>()

    override suspend fun get(hostId: String, shareId: String, path: String): ListingSnapshot? =
        listings[Key(hostId, shareId, path)]

    override suspend fun put(snapshot: ListingSnapshot) {
        listings[Key(snapshot.hostId, snapshot.shareId, snapshot.path)] = snapshot
    }

    override suspend fun get(hostId: String): SharesSnapshot? = shares[hostId]

    override suspend fun put(hostId: String, snapshot: SharesSnapshot) {
        shares[hostId] = snapshot
    }

    fun listingCount(): Int = listings.size
}
