package fyi.arlo.gander.core.data.stream

import fyi.arlo.gander.core.net.GanderApi
import fyi.arlo.gander.core.net.GanderEvents
import fyi.arlo.gander.core.net.GanderHttpClients
import fyi.arlo.gander.core.net.EventsSocket
import fyi.arlo.gander.core.net.protocol.ClientEvent
import fyi.arlo.gander.core.net.protocol.Entry
import fyi.arlo.gander.core.net.protocol.MeResponse
import fyi.arlo.gander.core.net.protocol.ServerEvent
import fyi.arlo.gander.core.net.protocol.StreamCreated
import fyi.arlo.gander.core.net.protocol.StreamRequest
import kotlinx.coroutines.flow.Flow

/** [StreamHostApi] over one host's pinned [GanderApi]. */
class GanderStreamHostApi(private val api: GanderApi) : StreamHostApi {
    constructor(clients: GanderHttpClients) : this(GanderApi(clients))

    override suspend fun me(): MeResponse = api.me()

    override suspend fun startStream(request: StreamRequest): StreamCreated = api.startStream(request)

    override suspend fun endStream(session: String, closeApp: Boolean) = api.endStream(session, closeApp)

    override suspend fun stat(share: String, path: String): Entry = api.stat(share, path)
}

/** Opens a `/v1/events` socket of the host on the pinned client, one per stream session. */
class GanderStreamEvents(private val events: GanderEvents) : StreamEventsSource {
    constructor(clients: GanderHttpClients) : this(GanderEvents(clients))

    override fun open(): StreamEventsConnection = SocketConnection(events.open())

    private class SocketConnection(private val socket: EventsSocket) : StreamEventsConnection {
        // The channel behind the socket can be consumed once; the session collects it once per socket.
        override val incoming: Flow<ServerEvent> = socket.events()

        override fun send(event: ClientEvent): Boolean = socket.send(event)

        override fun close() {
            socket.cancel()
        }
    }
}
