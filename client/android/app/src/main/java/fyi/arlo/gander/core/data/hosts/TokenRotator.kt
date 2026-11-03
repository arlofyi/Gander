package fyi.arlo.gander.core.data.hosts

import fyi.arlo.gander.core.net.BearerToken
import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.protocol.TokenRotateResponse
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.withContext
import kotlinx.serialization.SerializationException

/** Replaces a host's bearer token (host details, "Rotate token"). Throws [GanderError] on failure. */
fun interface TokenRotator {
    suspend fun rotate(host: PairedHost)
}

/**
 * `POST /v1/token/rotate`, then the new token into [hosts]. The host stops accepting the old token
 * at once, so once the answer is in, storing the new one must not be cut short by the screen
 * closing: that step is not cancellable. [request] is the call itself (the app passes
 * `GanderApi.rotateToken` on the host's pinned client).
 */
class ApiTokenRotator(
    private val hosts: HostRepository,
    private val request: suspend (PairedHost) -> TokenRotateResponse,
) : TokenRotator {
    override suspend fun rotate(host: PairedHost) {
        val response = request(host)
        val token = try {
            BearerToken(response.token)
        } catch (_: IllegalArgumentException) {
            // The host answered something that is not a token. Say so without echoing it.
            throw GanderError.MalformedResponse(SerializationException("rotate answer is not a token"))
        }
        withContext(NonCancellable) { hosts.updateToken(host.id, token) }
    }
}
