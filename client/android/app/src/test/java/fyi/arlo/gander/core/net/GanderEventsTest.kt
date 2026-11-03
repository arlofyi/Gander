package fyi.arlo.gander.core.net

import fyi.arlo.gander.core.net.protocol.ClientEvent
import fyi.arlo.gander.core.net.protocol.ServerEvent
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import mockwebserver3.MockResponse
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit

class GanderEventsTest {
    @Test
    fun receivesKnownEventsSkipsUnknownAndSends() = runBlocking<Unit> {
        val host = TestHosts.selfSigned()
        val fromClient = LinkedBlockingQueue<String>()
        TestHosts.httpsServer(host).use { server ->
            server.enqueue(
                MockResponse.Builder().webSocketUpgrade(
                    object : WebSocketListener() {
                        override fun onOpen(webSocket: WebSocket, response: Response) {
                            webSocket.send(ProtocolExamples.exampleJson("EventHello"))
                            webSocket.send("""{"type":"host.future","x":1}""")
                            webSocket.send("not json")
                            webSocket.send(ProtocolExamples.exampleJson("EventShareChanged"))
                            webSocket.send(ProtocolExamples.exampleJson("EventDeviceRevoked"))
                        }

                        override fun onMessage(webSocket: WebSocket, text: String) {
                            fromClient += text
                            webSocket.close(1000, null)
                        }
                    },
                ).build(),
            )
            val socket = GanderEvents(TestHosts.clients(server, TestHosts.pinOf(host), BearerToken("tok_ws"))).open()
            withTimeout(10_000) {
                assertTrue(socket.incoming.receive() is ServerEvent.Hello)
                val changed = socket.incoming.receive() as ServerEvent.ShareChanged
                assertEquals("Clients/Acme", changed.path)
                assertEquals(ServerEvent.DeviceRevoked, socket.incoming.receive())
                assertTrue(socket.send(ClientEvent.Ping))
                assertEquals("""{"type":"ping"}""", fromClient.poll(10, TimeUnit.SECONDS))
                // The host closes after the ping; the channel completes without error.
                val rest = mutableListOf<ServerEvent>()
                for (event in socket.incoming) rest += event
                assertTrue(rest.isEmpty())
            }
            val upgrade = server.takeRequest()
            assertEquals("/v1/events", upgrade.url.encodedPath)
            assertEquals("Bearer tok_ws", upgrade.headers["Authorization"])
            assertEquals("1", upgrade.headers["X-Gander-Protocol"])
        }
    }

    @Test
    fun refusedUpgradeFailsWithTheHostError() = runBlocking<Unit> {
        val host = TestHosts.selfSigned()
        TestHosts.httpsServer(host).use { server ->
            server.enqueue(
                MockResponse.Builder().code(401)
                    .body("""{"error":{"code":"token_revoked","message":"Revoked.","retry_after_ms":0}}""")
                    .build(),
            )
            val socket = GanderEvents(TestHosts.clients(server, TestHosts.pinOf(host))).open()
            try {
                withTimeout(10_000) { socket.incoming.receive() }
                fail("expected the channel to fail")
            } catch (_: GanderError.Host.TokenRevoked) {
                // expected
            }
        }
    }

    @Test
    fun impostorHostFailsWithPinMismatch() = runBlocking<Unit> {
        TestHosts.httpsServer(TestHosts.selfSigned("impostor")).use { server ->
            val socket = GanderEvents(TestHosts.clients(server, TestHosts.pinOf(TestHosts.selfSigned()))).open()
            try {
                withTimeout(10_000) { socket.incoming.receive() }
                fail("expected PinMismatch")
            } catch (_: GanderError.PinMismatch) {
                // expected
            }
        }
    }
}
