package fyi.arlo.gander.core.data.db

import fyi.arlo.gander.core.data.browse.KindsFilter
import fyi.arlo.gander.core.data.browse.ListQuery
import fyi.arlo.gander.core.data.browse.ListingSnapshot
import fyi.arlo.gander.core.data.browse.ListingSnapshotStore
import fyi.arlo.gander.core.data.browse.SharesSnapshot
import fyi.arlo.gander.core.data.browse.SharesSnapshotStore
import fyi.arlo.gander.core.data.browse.SortField
import fyi.arlo.gander.core.data.browse.SortOrder
import fyi.arlo.gander.core.data.browse.SortSpec
import fyi.arlo.gander.core.data.hosts.HostRepository
import fyi.arlo.gander.core.data.hosts.PairedHost
import fyi.arlo.gander.core.data.hosts.TokenStore
import fyi.arlo.gander.core.data.hosts.withMe
import fyi.arlo.gander.core.net.BearerToken
import fyi.arlo.gander.core.net.GanderJson
import fyi.arlo.gander.core.net.SpkiPin
import fyi.arlo.gander.core.net.protocol.Capabilities
import fyi.arlo.gander.core.net.protocol.Entry
import fyi.arlo.gander.core.net.protocol.HostOs
import fyi.arlo.gander.core.net.protocol.MeResponse
import fyi.arlo.gander.core.net.protocol.ShareInfo
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.withContext
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.builtins.serializer

/**
 * `hosts` table behind [HostRepository]. [add] (called when pairing succeeds, WP-22) stores the
 * token in [tokens], the Keystore-backed [TokenStore], and the row in Room. The two writes are not
 * one transaction, so a failing row write puts the previous token state back: a failed pairing
 * leaves no orphaned token, and a failed re-pairing keeps the old working token.
 */
class RoomHostRepository(
    private val dao: HostDao,
    private val tokens: TokenStore,
    private val clock: () -> Long = System::currentTimeMillis,
) : HostRepository {

    override val hosts: Flow<List<PairedHost>> = dao.observeAll().map { rows -> rows.mapNotNull { it.toDomainOrNull() } }

    override suspend fun get(hostId: String): PairedHost? = dao.get(hostId)?.toDomainOrNull()

    override fun token(hostId: String): BearerToken? = tokens.get(hostId)

    override suspend fun add(host: PairedHost, token: BearerToken) {
        val previous = tokens.get(host.id)
        tokens.put(host.id, token)
        try {
            dao.upsert(host.toEntity(pairedAt = clock()))
        } catch (e: Throwable) {
            // Cancellation included: a half-stored pairing is worse than none. Best effort, the
            // original failure is what the caller must see.
            withContext(NonCancellable) {
                runCatching { if (previous != null) tokens.put(host.id, previous) else tokens.remove(host.id) }
            }
            throw e
        }
    }

    override suspend fun updateToken(hostId: String, token: BearerToken) = tokens.put(hostId, token)

    override suspend fun rename(hostId: String, name: String) {
        val trimmed = name.trim()
        if (trimmed.isNotEmpty()) dao.rename(hostId, trimmed)
    }

    override suspend fun forget(hostId: String) {
        tokens.remove(hostId)
        dao.delete(hostId) // shares and listing snapshots go with it (ON DELETE CASCADE)
    }

    override suspend fun recordContact(hostId: String, atMillis: Long, me: MeResponse?) {
        if (me == null) {
            dao.updateLastSeen(hostId, atMillis)
            return
        }
        val current = dao.get(hostId)?.toDomainOrNull() ?: return
        val updated = current.withMe(me, atMillis)
        dao.updateFromMe(
            id = hostId,
            addresses = RowCodec.encodeAddresses(updated.addresses),
            os = updated.os.wire,
            pairedAs = updated.pairedAs,
            capabilities = updated.capabilities?.let(RowCodec::encodeCapabilities),
            atMillis = atMillis,
        )
    }
}

/** Room-backed [ListingSnapshotStore]. Keeps the newest [maxSnapshots] folders. */
class RoomListingSnapshotStore(
    private val dao: ListingSnapshotDao,
    private val maxSnapshots: Int = DEFAULT_MAX_SNAPSHOTS,
) : ListingSnapshotStore {

    override suspend fun get(hostId: String, shareId: String, path: String): ListingSnapshot? =
        dao.get(hostId, shareId, path)?.let(RowCodec::toSnapshotOrNull)

    override suspend fun put(snapshot: ListingSnapshot) =
        dao.upsertAndPrune(RowCodec.toEntity(snapshot), maxSnapshots)

    companion object {
        const val DEFAULT_MAX_SNAPSHOTS = 300
    }
}

/** Room-backed [SharesSnapshotStore]. */
class RoomSharesSnapshotStore(private val dao: ShareDao) : SharesSnapshotStore {

    override suspend fun get(hostId: String): SharesSnapshot? {
        val rows = dao.forHost(hostId)
        if (rows.isEmpty()) return null
        val shares = rows.mapNotNull { RowCodec.decodeShareOrNull(it.json) }
        return SharesSnapshot(shares, rows.maxOf { it.fetchedAt })
    }

    override suspend fun put(hostId: String, snapshot: SharesSnapshot) {
        val rows = snapshot.shares.mapIndexed { index, share ->
            ShareEntity(hostId, share.id, index, RowCodec.encodeShare(share), snapshot.fetchedAtMillis)
        }
        dao.replaceForHost(hostId, rows)
    }
}

/** Row <-> domain conversion; pure Kotlin so JVM unit tests cover it without a database. */
internal object RowCodec {
    private val addressesSerializer = ListSerializer(String.serializer())
    private val entriesSerializer = ListSerializer(Entry.serializer())

    fun encodeAddresses(addresses: List<String>): String =
        GanderJson.json.encodeToString(addressesSerializer, addresses)

    fun encodeCapabilities(capabilities: Capabilities): String =
        GanderJson.json.encodeToString(Capabilities.serializer(), capabilities)

    fun encodeShare(share: ShareInfo): String = GanderJson.json.encodeToString(ShareInfo.serializer(), share)

    fun decodeShareOrNull(json: String): ShareInfo? =
        runCatching { GanderJson.json.decodeFromString(ShareInfo.serializer(), json) }.getOrNull()

    fun encodeQuery(query: ListQuery): String =
        "${query.sort.field.wire}|${query.sort.order.wire}|${query.kinds.wire.orEmpty()}|${query.ext.joinToString(",")}"

    fun decodeQuery(raw: String): ListQuery? {
        val parts = raw.split('|')
        if (parts.size != 4) return null
        val field = SortField.entries.firstOrNull { it.wire == parts[0] } ?: return null
        val order = SortOrder.entries.firstOrNull { it.wire == parts[1] } ?: return null
        val kinds = KindsFilter.entries.firstOrNull { (it.wire ?: "") == parts[2] } ?: return null
        val ext = parts[3].split(',').filter { it.isNotEmpty() }
        return ListQuery(SortSpec(field, order), kinds, ext)
    }

    fun toEntity(snapshot: ListingSnapshot) = ListingSnapshotEntity(
        hostId = snapshot.hostId,
        shareId = snapshot.shareId,
        path = snapshot.path,
        query = encodeQuery(snapshot.query),
        etag = snapshot.etag,
        entriesJson = GanderJson.json.encodeToString(entriesSerializer, snapshot.entries),
        nextCursor = snapshot.nextCursor,
        truncated = snapshot.truncated,
        unlistable = snapshot.unlistable,
        fetchedAt = snapshot.fetchedAtMillis,
    )

    /** Null for a row this version cannot read (written by a newer app): treated as no snapshot. */
    fun toSnapshotOrNull(row: ListingSnapshotEntity): ListingSnapshot? {
        val query = decodeQuery(row.query) ?: return null
        val entries = runCatching { GanderJson.json.decodeFromString(entriesSerializer, row.entriesJson) }
            .getOrNull() ?: return null
        return ListingSnapshot(
            hostId = row.hostId,
            shareId = row.shareId,
            path = row.path,
            query = query,
            etag = row.etag,
            entries = entries,
            nextCursor = row.nextCursor,
            truncated = row.truncated,
            unlistable = row.unlistable,
            fetchedAtMillis = row.fetchedAt,
        )
    }

    fun toEntity(host: PairedHost, pairedAt: Long) = HostEntity(
        id = host.id,
        name = host.name,
        addresses = encodeAddresses(host.addresses),
        port = host.port,
        pin = host.pin.hostId,
        os = host.os.wire,
        pairedAs = host.pairedAs,
        lastSeen = host.lastSeenAtMillis,
        capabilities = host.capabilities?.let(::encodeCapabilities),
        pairedAt = pairedAt,
    )

    fun toDomainOrNull(row: HostEntity): PairedHost? = runCatching {
        PairedHost(
            id = row.id,
            name = row.name,
            pin = SpkiPin.fromHostId(row.pin),
            addresses = GanderJson.json.decodeFromString(addressesSerializer, row.addresses),
            port = row.port,
            os = HostOs.entries.firstOrNull { it.wire == row.os } ?: HostOs.UNKNOWN,
            pairedAs = row.pairedAs,
            lastSeenAtMillis = row.lastSeen,
            capabilities = row.capabilities?.let {
                runCatching { GanderJson.json.decodeFromString(Capabilities.serializer(), it) }.getOrNull()
            },
        )
    }.getOrNull()
}

private fun PairedHost.toEntity(pairedAt: Long): HostEntity = RowCodec.toEntity(this, pairedAt)

private fun HostEntity.toDomainOrNull(): PairedHost? = RowCodec.toDomainOrNull(this)
