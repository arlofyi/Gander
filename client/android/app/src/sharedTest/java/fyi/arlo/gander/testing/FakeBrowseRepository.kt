package fyi.arlo.gander.testing

import fyi.arlo.gander.core.data.browse.BrowseRepository
import fyi.arlo.gander.core.data.browse.EntryOrdering
import fyi.arlo.gander.core.data.browse.ListQuery
import fyi.arlo.gander.core.data.browse.Loaded
import fyi.arlo.gander.core.data.browse.PageResult
import fyi.arlo.gander.core.data.browse.RecentPage
import fyi.arlo.gander.core.data.browse.SearchPage
import fyi.arlo.gander.core.data.hosts.PairedHost
import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.protocol.Entry
import fyi.arlo.gander.core.net.protocol.ServerEvent
import fyi.arlo.gander.core.net.protocol.ShareInfo
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.emptyFlow

/**
 * A canned [BrowseRepository] for UI tests: folders by path, paging by `o<offset>` cursors,
 * substring search, and an [offline] switch that answers from "cache" with an age.
 */
class FakeBrowseRepository(
    var shares: List<ShareInfo> = listOf(SampleData.share()),
    val folders: MutableMap<String, List<Entry>> = mutableMapOf(),
    var pageSize: Int = 200,
) : BrowseRepository {

    /** Answer folder and share calls as the stored snapshot of this age, like an unreachable host. */
    var offlineCachedAtMillis: Long? = null

    /** Throw this from every network call. */
    var failure: GanderError? = null

    var searchPartial = false
    var recentEntries: List<Entry> = emptyList()
    var recentPartial = false

    override suspend fun cachedShares(host: PairedHost): Loaded<List<ShareInfo>>? = Loaded(shares, 1L)

    override suspend fun shares(host: PairedHost): Loaded<List<ShareInfo>> {
        failure?.let { throw it }
        return Loaded(shares, offlineCachedAtMillis)
    }

    override suspend fun cachedFirstPage(host: PairedHost, shareId: String, path: String, query: ListQuery): PageResult? =
        null

    override suspend fun firstPage(host: PairedHost, shareId: String, path: String, query: ListQuery): PageResult {
        failure?.let { throw it }
        return page(path, query, offset = 0, cachedAt = offlineCachedAtMillis)
    }

    override suspend fun nextPage(
        host: PairedHost,
        shareId: String,
        path: String,
        query: ListQuery,
        cursor: String,
    ): PageResult {
        failure?.let { throw it }
        return page(path, query, offset = cursor.removePrefix("o").toInt(), cachedAt = null)
    }

    /** The `inside` of each search, in order (WP-66). */
    val searchInside = mutableListOf<Boolean>()

    override suspend fun search(host: PairedHost, shareId: String, path: String, text: String, cursor: String?, inside: Boolean): SearchPage {
        searchInside += inside
        failure?.let { throw it }
        val hits = folders.flatMap { (folder, entries) ->
            entries.filter { it.name.contains(text, ignoreCase = true) }
                .map { entry -> entry.copy(path = if (folder.isEmpty()) entry.name else "$folder/${entry.name}") }
        }
        return SearchPage(hits, searchPartial, nextCursor = null)
    }

    override suspend fun recent(host: PairedHost, limit: Int): RecentPage {
        failure?.let { throw it }
        return RecentPage(recentEntries, recentPartial)
    }

    override fun events(host: PairedHost): Flow<ServerEvent> = emptyFlow()

    private fun page(path: String, query: ListQuery, offset: Int, cachedAt: Long?): PageResult {
        val all = folders[path] ?: throw GanderError.fromResponse(404, FakeHostApi.NOT_FOUND_BODY)
        val ordered = EntryOrdering.apply(all, query)
        val slice = ordered.drop(offset).take(pageSize)
        val next = if (cachedAt == null && offset + slice.size < ordered.size) "o${offset + slice.size}" else null
        return PageResult(slice, next, cachedAtMillis = cachedAt)
    }
}
