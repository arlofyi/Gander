package fyi.arlo.gander.core.data

import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.testing.SampleData
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.IOException

class UiErrorTest {
    private fun host(status: Int, code: String, message: String = "m") =
        GanderError.fromResponse(status, """{"error":{"code":"$code","message":"$message","retry_after_ms":0}}""")

    @Test
    fun connectivityFailuresAreOffline() {
        assertEquals(UiErrorKind.OFFLINE, GanderError.Unreachable(listOf("a"), null).toUiError().kind)
        assertEquals(UiErrorKind.OFFLINE, GanderError.Timeout(IOException()).toUiError().kind)
        assertEquals(UiErrorKind.OFFLINE, GanderError.Network(IOException()).toUiError().kind)
        assertTrue(GanderError.Unreachable(listOf("a"), null).toUiError().isConnectivity)
    }

    @Test
    fun onlyConnectivityAllowsAStaleFallback() {
        val others = listOf(
            host(401, "token_revoked"),
            host(403, "permission_denied"),
            host(404, "not_found"),
            host(500, "internal"),
            GanderError.PinMismatch(SampleData.pin(), null, null),
            GanderError.Tls(IOException()),
            GanderError.MalformedResponse(IOException()),
        )
        others.forEach { assertFalse("$it", it.toUiError().isConnectivity) }
    }

    @Test
    fun pinMismatchIsItsOwnKindAndNeverGenericTls() {
        assertEquals(UiErrorKind.WRONG_COMPUTER, GanderError.PinMismatch(SampleData.pin(), null, null).toUiError().kind)
        assertEquals(UiErrorKind.SECURE_CONNECTION, GanderError.Tls(IOException()).toUiError().kind)
    }

    @Test
    fun everyTokenFailureMeansRevoked() {
        for (code in listOf("token_missing", "token_invalid", "token_revoked")) {
            assertEquals(UiErrorKind.REVOKED, host(401, code).toUiError().kind)
        }
    }

    @Test
    fun permissionDeniedKeepsTheHostExplanation() {
        val error = host(403, "permission_denied", "needs the fetch permission").toUiError()
        assertEquals(UiErrorKind.NO_PERMISSION, error.kind)
        assertEquals("needs the fetch permission", error.detail)
        assertNull(host(404, "share_not_found").toUiError().detail)
    }

    @Test
    fun theRestMapToTheirOwnKinds() {
        assertEquals(UiErrorKind.SHARE_GONE, host(404, "share_not_found").toUiError().kind)
        assertEquals(UiErrorKind.NOT_FOUND, host(404, "not_found").toUiError().kind)
        assertEquals(UiErrorKind.NOT_FOUND, host(400, "bad_path").toUiError().kind)
        assertEquals(UiErrorKind.BUSY, host(429, "too_many_requests").toUiError().kind)
        assertEquals(UiErrorKind.IDENTITY_MISMATCH, host(403, "network_identity_mismatch").toUiError().kind)
        assertEquals(UiErrorKind.INCOMPATIBLE, GanderError.MalformedResponse(IOException()).toUiError().kind)
        assertEquals(UiErrorKind.OTHER, host(500, "internal", "boom").toUiError().kind)
    }
}
