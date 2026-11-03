package fyi.arlo.gander.testing

import fyi.arlo.gander.core.data.browse.EntryOrdering
import fyi.arlo.gander.core.data.browse.HostApi
import fyi.arlo.gander.core.data.browse.HostConnection
import fyi.arlo.gander.core.data.browse.HostConnector
import fyi.arlo.gander.core.data.browse.KindsFilter
import fyi.arlo.gander.core.data.browse.ListQuery
import fyi.arlo.gander.core.data.browse.SortField
import fyi.arlo.gander.core.data.browse.SortOrder
import fyi.arlo.gander.core.data.browse.SortSpec
import fyi.arlo.gander.core.data.hosts.PairedHost
import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.Listing
import fyi.arlo.gander.core.net.protocol.Entry
import fyi.arlo.gander.core.net.protocol.HostHello
import fyi.arlo.gander.core.net.protocol.ListResponse
import fyi.arlo.gander.core.net.protocol.MeResponse
import fyi.arlo.gander.core.net.protocol.RecentResponse
import fyi.arlo.gander.core.net.protocol.SearchResponse
import fyi.arlo.gander.core.net.protocol.ServerEvent
import fyi.arlo.gander.core.net.protocol.ShareInfo
import fyi.arlo.gander.core.net.protocol.SharesResponse
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableSharedFlow

/** One recorded `list` request. */
data class ListCall(
    val path: String,
    val cursor: String?,
    val limit: Int?,
    val sort: String?,
    val order: String?,
    val kinds: String?,
    val ifNoneMatch: String?,
)

data class SearchCall(val q: String, val path: String, val cursor: String?, val scope: String? = null)

data class StatCall(val share: String, val path: String, val meta: Boolean)

/**
 * A scripted host behind [HostApi]: folders are a map from share-relative path to entries; list
 * answers sort, filter, page by cursor (`o<offset>`) and validate `If-None-Match` like the
 * protocol describes. Set [failure] to make every call throw, [listGate] to hold list calls open.
 */
class FakeHostApi : HostApi {
    var hello: HostHello = HostHello(listOf(1), false, SampleData.HOST_ID)
    var me: MeResponse = SampleData.me()
    var shares: List<ShareInfo> = listOf(SampleData.share())
    val folders: MutableMap<String, List<Entry>> = mutableMapOf()

    /** When set, every call throws it (offline, revoked, ...). */
    var failure: GanderError? = null

    /** When set, `me()` throws it instead of answering (host reachable, /me refused). */
    var meFailure: GanderError? = null

    /** When set, `list` suspends until it completes. */
    var listGate: CompletableDeferred<Unit>? = null

    var searchHandler: (call: SearchCall) -> SearchResponse = { SearchResponse(emptyList(), partial = false, nextCursor = null) }
    var recentResponse: RecentResponse = RecentResponse(emptyList(), partial = false)

    /** When set, `stat` suspends until it completes. */
    var statGate: CompletableDeferred<Unit>? = null

    /** Answers `stat`; the default looks the path up in [folders]. Set it to script failures or metadata. */
    var statHandler: (call: StatCall) -> Entry = { call ->
        val folder = call.path.substringBeforeLast('/', missingDelimiterValue = "")
        val name = call.path.substringAfterLast('/')
        folders[folder]?.firstOrNull { it.name == name } ?: throw GanderError.fromResponse(404, NOT_FOUND_BODY)
    }

    val listCalls = mutableListOf<ListCall>()
    val statCalls = mutableListOf<StatCall>()
    val searchCalls = mutableListOf<SearchCall>()
    var hostCalls = 0
    var meCalls = 0
    var sharesCalls = 0

    override suspend fun host(): HostHello {
        hostCalls++
        failure?.let { throw it }
        return hello
    }

    override suspend fun me(): MeResponse {
        meCalls++
        failure?.let { throw it }
        meFailure?.let { throw it }
        return me
    }

    override suspend fun shares(): SharesResponse {
        sharesCalls++
        failure?.let { throw it }
        return SharesResponse(shares)
    }

    override suspend fun stat(share: String, path: String, meta: Boolean): Entry {
        val call = StatCall(share, path, meta)
        statCalls += call
        statGate?.await()
        failure?.let { throw it }
        return statHandler(call)
    }

    override suspend fun list(
        share: String,
        path: String,
        cursor: String?,
        limit: Int?,
        sort: String?,
        order: String?,
        kinds: String?,
        ext: String?,
        ifNoneMatch: String?,
    ): Listing {
        listCalls += ListCall(path, cursor, limit, sort, order, kinds, ifNoneMatch)
        listGate?.await()
        failure?.let { throw it }
        val all = folders[path] ?: throw GanderError.fromResponse(404, NOT_FOUND_BODY)
        val query = ListQuery(
            sort = SortSpec(
                SortField.entries.firstOrNull { it.wire == sort } ?: SortField.NAME,
                SortOrder.entries.firstOrNull { it.wire == order } ?: SortOrder.ASC,
            ),
            kinds = KindsFilter.entries.firstOrNull { it.wire == kinds } ?: KindsFilter.ALL,
        )
        val ordered = EntryOrdering.apply(all, query)
        val etag = "W/\"${ordered.joinToString("|") { it.name + it.size + it.mtime }.hashCode()}-${query.hashCode()}\""
        if (cursor == null && ifNoneMatch == etag) return Listing.NotModified
        val offset = cursor?.removePrefix("o")?.toInt() ?: 0
        val page = ordered.drop(offset).take(limit ?: 200)
        val next = if (offset + page.size < ordered.size) "o${offset + page.size}" else null
        return Listing.Page(ListResponse(path, page, next, truncated = false, unlistable = 0, etag = etag))
    }

    override suspend fun search(share: String, q: String, path: String, limit: Int?, cursor: String?, scope: String?): SearchResponse {
        val call = SearchCall(q, path, cursor, scope)
        searchCalls += call
        failure?.let { throw it }
        return searchHandler(call)
    }

    override suspend fun recent(limit: Int?, since: String?): RecentResponse {
        failure?.let { throw it }
        return recentResponse
    }

    companion object {
        const val NOT_FOUND_BODY = """{"error":{"code":"not_found","message":"No such folder.","retry_after_ms":0}}"""
    }
}

/** A [HostConnector] that always hands out the same [api] and [events] flow. */
class FakeHostConnector(val api: FakeHostApi = FakeHostApi()) : HostConnector {
    val events = MutableSharedFlow<ServerEvent>(extraBufferCapacity = 16)
    val connectedHosts = mutableListOf<PairedHost>()

    /** When set, `events()` fails with it on collection. */
    var eventsFailure: GanderError? = null
    var eventsOpened = 0

    override fun connect(host: PairedHost): HostConnection {
        connectedHosts += host
        return object : HostConnection {
            override val api: HostApi = this@FakeHostConnector.api

            override fun events(): Flow<ServerEvent> = kotlinx.coroutines.flow.flow {
                eventsOpened++
                eventsFailure?.let { throw it }
                this@FakeHostConnector.events.collect { emit(it) }
            }
        }
    }
}
