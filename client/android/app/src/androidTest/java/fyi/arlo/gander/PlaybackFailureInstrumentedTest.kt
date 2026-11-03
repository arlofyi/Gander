package fyi.arlo.gander

import android.net.Uri
import androidx.annotation.OptIn
import androidx.media3.common.PlaybackException
import androidx.media3.common.util.UnstableApi
import androidx.media3.datasource.DataSpec
import androidx.media3.datasource.HttpDataSource
import androidx.test.ext.junit.runners.AndroidJUnit4
import fyi.arlo.gander.core.data.preview.PlaybackFailure
import fyi.arlo.gander.core.data.preview.toPlaybackFailure
import fyi.arlo.gander.core.net.GanderError
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

/**
 * What Media3 reports when the host refuses a video request. `InvalidResponseCodeException` needs
 * `android.net.Uri` for its `DataSpec`, so this runs on a device or emulator (WP-29), not on the JVM.
 */
@OptIn(UnstableApi::class)
@RunWith(AndroidJUnit4::class)
class PlaybackFailureInstrumentedTest {
    private fun playbackError(status: Int, body: String, headers: Map<String, List<String>> = emptyMap()): PlaybackException {
        val cause = HttpDataSource.InvalidResponseCodeException(
            status,
            "status",
            null,
            headers,
            DataSpec(Uri.parse("https://100.64.0.1:42637/v1/shares/shr_1/file")),
            body.toByteArray(),
        )
        return PlaybackException("bad status", cause, PlaybackException.ERROR_CODE_IO_BAD_HTTP_STATUS)
    }

    @Test
    fun aShareThatDoesNotAllowFetchingBecomesPermissionDeniedWithTheHostsWords() {
        val failure = playbackError(
            403,
            """{"error":{"code":"permission_denied","message":"needs fetch permission","retry_after_ms":0}}""",
        ).toPlaybackFailure()
        val error = (failure as PlaybackFailure.Host).error as GanderError.Host.PermissionDenied
        assertEquals("needs fetch permission", error.hostMessage)
    }

    @Test
    fun aRevokedTokenBecomesTokenRevoked() {
        val failure = playbackError(
            401,
            """{"error":{"code":"token_revoked","message":"Revoked.","retry_after_ms":0}}""",
        ).toPlaybackFailure()
        assertTrue((failure as PlaybackFailure.Host).error is GanderError.Host.TokenRevoked)
    }

    @Test
    fun aBusyHostKeepsItsRetryHintFromTheHeaderWhenTheBodyHasNone() {
        val failure = playbackError(
            503,
            """{"error":{"code":"generating","message":"Busy.","retry_after_ms":0}}""",
            mapOf("retry-after" to listOf("2")),
        ).toPlaybackFailure()
        val error = (failure as PlaybackFailure.Host).error as GanderError.Host.Generating
        assertEquals(2_000, error.retryAfterMs)
    }

    @Test
    fun aMissingFileBecomesNotFound() {
        val failure = playbackError(
            404,
            """{"error":{"code":"not_found","message":"No such file.","retry_after_ms":0}}""",
        ).toPlaybackFailure()
        assertTrue((failure as PlaybackFailure.Host).error is GanderError.Host.NotFound)
    }

    @Test
    fun aBodyThatIsNotAProtocolErrorIsStillAnError() {
        val failure = playbackError(502, "<html>proxy</html>").toPlaybackFailure()
        assertTrue((failure as PlaybackFailure.Host).error is GanderError.Host.Unrecognised)
    }
}
