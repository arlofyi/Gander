package fyi.arlo.gander.core.data.browse

import fyi.arlo.gander.core.data.hosts.HostRepository
import fyi.arlo.gander.core.data.hosts.PairedHost
import fyi.arlo.gander.core.net.GanderApi
import fyi.arlo.gander.core.net.GanderEvents
import fyi.arlo.gander.core.net.GanderHttpClients
import fyi.arlo.gander.core.net.HostEndpoint
import fyi.arlo.gander.core.net.Listing
import fyi.arlo.gander.core.net.SpkiPin
import fyi.arlo.gander.core.net.protocol.Entry
import fyi.arlo.gander.core.net.protocol.HostHello
import fyi.arlo.gander.core.net.protocol.MeResponse
import fyi.arlo.gander.core.net.protocol.RecentResponse
import fyi.arlo.gander.core.net.protocol.SearchResponse
import fyi.arlo.gander.core.net.protocol.SharesResponse
import fyi.arlo.gander.core.net.protocol.ServerEvent
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.emitAll
import kotlinx.coroutines.flow.flow
import okhttp3.OkHttpClient
import java.util.concurrent.ConcurrentHashMap

/**
 * The slice of [GanderApi] that hosts, shares and browser screens use, as an interface so that
 * repositories and ViewModels can be tested against a scripted host. Every function throws
 * [fyi.arlo.gander.core.net.GanderError] and nothing else.
 */
interface HostApi {
    suspend fun host(): HostHello

    suspend fun me(): MeResponse

    suspend fun shares(): SharesResponse

    /** `GET /shares/{share}/stat`; with [meta] the host adds type-specific metadata (file sheet, WP-25). */
    suspend fun stat(share: String, path: String, meta: Boolean = false): Entry

    suspend fun list(
        share: String,
        path: String,
        cursor: String?,
        limit: Int?,
        sort: String?,
        order: String?,
        kinds: String?,
        ext: String?,
        ifNoneMatch: String?,
    ): Listing

    suspend fun search(share: String, q: String, path: String, limit: Int?, cursor: String?, scope: String? = null): SearchResponse

    suspend fun recent(limit: Int?, since: String? = null): RecentResponse
}

/** One host's API and its `/events` stream. */
interface HostConnection {
    val api: HostApi

    /**
     * Opens one `/v1/events` socket per collection. Completes when the host closes it and fails
     * with a [fyi.arlo.gander.core.net.GanderError] when the connection fails; reconnecting is
     * [DefaultBrowseRepository.events]'s job.
     */
    fun events(): Flow<ServerEvent>
}

/** Turns a [PairedHost] into a [HostConnection]. Tests supply a fake; the app uses [PinnedHostConnector]. */
fun interface HostConnector {
    fun connect(host: PairedHost): HostConnection
}

/** [HostApi] over the real, pinned [GanderApi]. */
class GanderHostApi(private val api: GanderApi) : HostApi {
    override suspend fun host() = api.host()

    override suspend fun me() = api.me()

    override suspend fun shares() = api.shares()

    override suspend fun stat(share: String, path: String, meta: Boolean) = api.stat(share, path, meta)

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
    ): Listing = api.list(share, path, cursor, limit, sort, order, kinds, ext, ifNoneMatch)

    override suspend fun search(share: String, q: String, path: String, limit: Int?, cursor: String?, scope: String?) =
        api.search(share, q, path, limit, cursor, scope)

    override suspend fun recent(limit: Int?, since: String?) = api.recent(limit, since)
}

/**
 * Builds one [GanderHttpClients] per paired host (pin, addresses, port) and reuses it, so the
 * connection pool, the 8-request dispatcher and the remembered working address live as long as
 * the host does. The token is read from [hosts] per request.
 */
class PinnedHostConnector(
    private val hosts: HostRepository,
    private val appVersion: String,
    private val baseClient: OkHttpClient = OkHttpClient(),
) : HostConnector {

    private data class Key(val pin: SpkiPin, val port: Int)

    private class Cached(val key: Key, val clients: GanderHttpClients)

    private val cache = ConcurrentHashMap<String, Cached>()

    override fun connect(host: PairedHost): HostConnection = PinnedConnection(clients(host))

    /**
     * The pinned clients of [host] (same instance as [connect] uses), for callers that need the
     * byte-stream client: Coil thumbnails, previews, Media3 and the fetch engine.
     */
    fun clients(host: PairedHost): GanderHttpClients {
        val key = Key(host.pin, host.port)
        val entry = cache.compute(host.id) { _, existing ->
            existing?.takeIf { it.key == key } ?: Cached(key, build(host))
        }!!
        entry.clients.endpoint.updateAddresses(host.addresses)
        return entry.clients
    }

    /** Drops the cached clients of a forgotten host. */
    fun evict(hostId: String) {
        cache.remove(hostId)
    }

    private fun build(host: PairedHost) = GanderHttpClients(
        pin = host.pin,
        endpoint = HostEndpoint(host.addresses, host.port),
        appVersion = appVersion,
        token = { hosts.token(host.id) },
        base = baseClient,
    )

    private class PinnedConnection(private val clients: GanderHttpClients) : HostConnection {
        override val api: HostApi = GanderHostApi(GanderApi(clients))

        override fun events(): Flow<ServerEvent> = flow {
            val socket = GanderEvents(clients).open()
            try {
                emitAll(socket.events())
            } finally {
                socket.cancel()
            }
        }
    }
}
