package fyi.arlo.gander.core.net

import fyi.arlo.gander.core.net.protocol.ErrorCode
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.IOException
import java.net.SocketTimeoutException
import javax.net.ssl.SSLHandshakeException

class GanderErrorTest {
    private fun body(code: String, retry: Long = 0) =
        """{"error":{"code":"$code","message":"m","retry_after_ms":$retry}}"""

    @Test
    fun everyProtocolErrorCodeMapsToItsOwnSubclass() {
        val seen = mutableSetOf<Class<*>>()
        for (code in ErrorCode.entries.filter { it != ErrorCode.UNKNOWN }) {
            val error = GanderError.fromResponse(code.status, body(code.wire))
            assertEquals(code, error.code)
            assertEquals(code.status, error.status)
            assertEquals("m", error.hostMessage)
            assertTrue("$code shares a subclass", seen.add(error.javaClass))
            assertTrue(error !is GanderError.Host.Unrecognised)
        }
    }

    @Test
    fun retryAfterComesFromBodyOrHeader() {
        assertEquals(1500L, GanderError.fromResponse(503, body("generating", 1500)).retryAfterMs)
        assertEquals(2000L, GanderError.fromResponse(429, body("too_many_requests"), "2").retryAfterMs)
    }

    @Test
    fun unknownCodesAndNonProtocolBodiesAreUnrecognised() {
        val newer = GanderError.fromResponse(418, body("future_code")) as GanderError.Host.Unrecognised
        assertEquals("future_code", newer.rawCode)
        assertEquals(418, newer.status)
        val html = GanderError.fromResponse(502, "<html>bad gateway</html>") as GanderError.Host.Unrecognised
        assertNull(html.rawCode)
        val empty = GanderError.fromResponse(401, "") as GanderError.Host.Unrecognised
        assertEquals(401, empty.status)
    }

    @Test
    fun transportFailuresMap() {
        val pin = TestHosts.pinOf(TestHosts.selfSigned())
        val mismatch = SSLHandshakeException("x").apply { initCause(SpkiPinMismatchException(pin, null)) }
        assertTrue(GanderError.fromThrowable(mismatch) is GanderError.PinMismatch)
        assertTrue(GanderError.fromThrowable(SSLHandshakeException("proto")) is GanderError.Tls)
        assertTrue(GanderError.fromThrowable(SocketTimeoutException("timeout")) is GanderError.Timeout)
        assertTrue(GanderError.fromThrowable(IOException("reset")) is GanderError.Network)
        val unreachable = HostUnreachableException(listOf("a", "b"), listOf(IOException("1"), IOException("2")))
        assertEquals(listOf("a", "b"), (GanderError.fromThrowable(unreachable) as GanderError.Unreachable).tried)
    }
}
