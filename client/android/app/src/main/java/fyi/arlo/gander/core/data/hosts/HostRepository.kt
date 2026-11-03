package fyi.arlo.gander.core.data.hosts

import fyi.arlo.gander.core.net.BearerToken
import fyi.arlo.gander.core.net.protocol.MeResponse
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.update
import java.util.concurrent.ConcurrentHashMap

/**
 * The paired hosts, their tokens and last-contact bookkeeping.
 *
 * WP-23 defines this contract and ships two implementations: [InMemoryHostRepository] (tests,
 * previews) and `RoomHostRepository` (the `hosts` table; the token side is a [TokenStore] that
 * WP-22 backs with the Keystore-wrapped DataStore blob). WP-22's pairing flow calls [add].
 */
interface HostRepository {
    /** Every paired host, oldest pairing first. Emits again on any change. */
    val hosts: Flow<List<PairedHost>>

    suspend fun get(hostId: String): PairedHost?

    /**
     * The bearer token for [hostId], or null if none is stored. Called on an OkHttp thread for
     * every request (so a rotated token applies at once): implementations must answer from memory
     * and never block on I/O.
     */
    fun token(hostId: String): BearerToken?

    /** Stores a newly paired host and its token (pairing success, WP-22). Replaces an existing id. */
    suspend fun add(host: PairedHost, token: BearerToken)

    /** Replaces the token after `POST /v1/token/rotate`. */
    suspend fun updateToken(hostId: String, token: BearerToken)

    /** Renames the host on this phone only. A blank [name] is ignored. */
    suspend fun rename(hostId: String, name: String)

    /** Forgets the host: its row, token, shares and cached listings go. */
    suspend fun forget(hostId: String)

    /**
     * The host answered at [atMillis]. When [me] is given, refreshes what the host says about
     * itself: addresses (with MagicDNS), OS, capabilities and the name it knows this phone by.
     * The user's own display name is never overwritten.
     */
    suspend fun recordContact(hostId: String, atMillis: Long, me: MeResponse? = null)
}

/**
 * Where bearer tokens live. The app graph wires [KeystoreTokenStore] (AES-GCM under an Android
 * Keystore key); [InMemoryTokenStore] is for tests and previews.
 */
interface TokenStore {
    /** Non-blocking read (see [HostRepository.token]). */
    fun get(hostId: String): BearerToken?

    suspend fun put(hostId: String, token: BearerToken)

    suspend fun remove(hostId: String)
}

class InMemoryTokenStore : TokenStore {
    private val tokens = ConcurrentHashMap<String, BearerToken>()

    override fun get(hostId: String): BearerToken? = tokens[hostId]

    override suspend fun put(hostId: String, token: BearerToken) {
        tokens[hostId] = token
    }

    override suspend fun remove(hostId: String) {
        tokens.remove(hostId)
    }
}

/** Applies what `/me` says about the host to [this], keeping the user's own display name. */
internal fun PairedHost.withMe(me: MeResponse, atMillis: Long): PairedHost = copy(
    addresses = (me.host.addresses + listOfNotNull(me.host.magicDns)).distinct().ifEmpty { addresses },
    os = me.host.os,
    pairedAs = me.device.name,
    capabilities = me.capabilities,
    lastSeenAtMillis = atMillis,
)

/** Test and preview implementation; also the reference for the contract above. */
class InMemoryHostRepository(
    initial: List<PairedHost> = emptyList(),
    private val tokens: TokenStore = InMemoryTokenStore(),
) : HostRepository {
    private val state = MutableStateFlow(initial)

    override val hosts: Flow<List<PairedHost>> = state

    /** Seeds a token for a host passed to the constructor. */
    suspend fun seedToken(hostId: String, token: BearerToken) = tokens.put(hostId, token)

    override suspend fun get(hostId: String): PairedHost? = state.value.firstOrNull { it.id == hostId }

    override fun token(hostId: String): BearerToken? = tokens.get(hostId)

    override suspend fun add(host: PairedHost, token: BearerToken) {
        tokens.put(host.id, token)
        state.update { list -> list.filterNot { it.id == host.id } + host }
    }

    override suspend fun updateToken(hostId: String, token: BearerToken) {
        tokens.put(hostId, token)
    }

    override suspend fun rename(hostId: String, name: String) {
        val trimmed = name.trim()
        if (trimmed.isEmpty()) return
        state.update { list -> list.map { if (it.id == hostId) it.copy(name = trimmed) else it } }
    }

    override suspend fun forget(hostId: String) {
        tokens.remove(hostId)
        state.update { list -> list.filterNot { it.id == hostId } }
    }

    override suspend fun recordContact(hostId: String, atMillis: Long, me: MeResponse?) {
        state.update { list ->
            list.map { host ->
                when {
                    host.id != hostId -> host
                    me != null -> host.withMe(me, atMillis)
                    else -> host.copy(lastSeenAtMillis = atMillis)
                }
            }
        }
    }
}
