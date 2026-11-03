package fyi.arlo.gander.core.net

import fyi.arlo.gander.core.net.protocol.ClientEvent
import fyi.arlo.gander.core.net.protocol.ServerEvent
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.channels.ReceiveChannel
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.consumeAsFlow
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener

/**
 * Opens `/v1/events` on the pinned [GanderHttpClients.api] client (token in the `Authorization`
 * header, OkHttp ping frames every 20 s). Reconnect policy belongs to the caller (the host
 * repository): open a new socket after [EventsSocket.incoming] completes.
 */
class GanderEvents(private val client: OkHttpClient, private val endpoint: HostEndpoint) {

    constructor(clients: GanderHttpClients) : this(clients.api, clients.endpoint)

    /** Starts connecting; events arrive on [EventsSocket.incoming]. */
    fun open(): EventsSocket {
        val channel = Channel<ServerEvent>(Channel.UNLIMITED)
        val listener = Listener(channel)
        val request = Request.Builder().url(endpoint.baseUrl().newBuilder().addPathSegment("events").build()).build()
        val webSocket = client.newWebSocket(request, listener)
        return EventsSocket(webSocket, channel)
    }

    private class Listener(private val channel: Channel<ServerEvent>) : WebSocketListener() {
        override fun onMessage(webSocket: WebSocket, text: String) {
            val event = try {
                GanderJson.decodeServerEvent(text)
            } catch (_: IllegalArgumentException) { // includes SerializationException
                // A frame this client cannot parse is dropped, like an unknown type; the frame
                // text is not logged (it may name paths).
                return
            }
            if (event is ServerEvent.Unknown) return
            channel.trySend(event)
        }

        override fun onClosing(webSocket: WebSocket, code: Int, reason: String) {
            webSocket.close(NORMAL_CLOSURE, null)
        }

        override fun onClosed(webSocket: WebSocket, code: Int, reason: String) {
            channel.close()
        }

        override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
            val error = if (response != null && !response.isSuccessful && response.code != SWITCHING_PROTOCOLS) {
                val body = runCatching { response.peekBody(MAX_ERROR_BODY).string() }.getOrNull()
                GanderError.fromResponse(response.code, body, response.header("Retry-After"))
            } else {
                GanderError.fromThrowable(t)
            }
            channel.close(error)
        }
    }

    companion object {
        const val NORMAL_CLOSURE = 1000
        private const val SWITCHING_PROTOCOLS = 101
        private const val MAX_ERROR_BODY = 64L * 1024
    }
}

/**
 * One `/v1/events` connection. [incoming] delivers known events in order (unknown types are
 * dropped, protocol compatibility rule) and completes when the socket closes; it fails with a
 * [GanderError] when the connection fails (for example [GanderError.Host.TokenRevoked] on the
 * upgrade, or [GanderError.Unreachable]). After [ServerEvent.DeviceRevoked] the host closes it.
 */
class EventsSocket internal constructor(
    private val webSocket: WebSocket,
    val incoming: ReceiveChannel<ServerEvent>,
) {
    /** [incoming] as a Flow; collect it once. */
    fun events(): Flow<ServerEvent> = incoming.consumeAsFlow()

    /** Queues [event]; false if the socket is closing or closed. */
    fun send(event: ClientEvent): Boolean =
        webSocket.send(GanderJson.json.encodeToString(ClientEvent.serializer(), event))

    /** Closes politely; [incoming] completes once the host acknowledges. */
    fun close(): Boolean = webSocket.close(GanderEvents.NORMAL_CLOSURE, null)

    /** Drops the connection immediately (kill switch, app going away). */
    fun cancel() = webSocket.cancel()
}
