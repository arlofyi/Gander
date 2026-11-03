package fyi.arlo.gander.core.data.preview

import androidx.media3.common.PlaybackException
import fyi.arlo.gander.core.data.UiErrorKind
import fyi.arlo.gander.core.data.actions.ActionReason
import fyi.arlo.gander.core.data.actions.FileAction
import fyi.arlo.gander.core.data.browse.EntryTarget
import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.protocol.PreviewKind
import fyi.arlo.gander.testing.SampleData
import java.io.IOException
import java.net.ConnectException
import javax.net.ssl.SSLHandshakeException
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class PreviewFailureTest {
    private val target = EntryTarget(
        SampleData.HOST_ID,
        "shr_projects",
        "Clients/Report.pdf",
        SampleData.file("Report.pdf", preview = PreviewKind.PDF),
    )

    @Test
    fun everyHostAnswerBecomesTheSentenceThatExplainsIt() {
        assertEquals(PreviewFailure.Unsupported, GanderError.Host.UnsupportedType(415, "").toPreviewFailure())
        assertEquals(PreviewFailure.TooLarge, GanderError.Host.TooLarge(413, "").toPreviewFailure())
        assertEquals(PreviewFailure.StillGenerating, GanderError.Host.Generating(503, "", 1).toPreviewFailure())

        val offline = GanderError.Unreachable(listOf("100.64.0.1"), null).toPreviewFailure()
        assertEquals(UiErrorKind.OFFLINE, (offline as PreviewFailure.Host).error.kind)
        val revoked = GanderError.Host.TokenRevoked(401, "").toPreviewFailure()
        assertEquals(UiErrorKind.REVOKED, (revoked as PreviewFailure.Host).error.kind)
        val denied = GanderError.Host.PermissionDenied(403, "needs preview").toPreviewFailure()
        assertEquals(UiErrorKind.NO_PERMISSION, (denied as PreviewFailure.Host).error.kind)
        assertEquals("needs preview", denied.error.detail)
    }

    @Test
    fun onlyAnswersThatAreNotRetryableAreFinal() {
        assertFalse(PreviewFailure.Unsupported.canRetry)
        assertFalse(PreviewFailure.TooLarge.canRetry)
        assertFalse(PreviewFailure.CodecUnsupported.canRetry)
        assertTrue(PreviewFailure.StillGenerating.canRetry)
    }

    @Test
    fun aDecoderFailureIsTheComputersUnusableAnswer() {
        val decode = IllegalStateException("bad image")
        assertTrue(decode.toPreviewError() is GanderError.MalformedResponse)
        val host = GanderError.Host.NotFound(404, "")
        assertEquals(host, host.toPreviewError())
    }

    @Test
    fun onlyLastingRefusalsReachTheFileSheet() {
        assertTrue(shouldReportRefusal(target, GanderError.Host.UnsupportedType(415, "")))
        assertTrue(shouldReportRefusal(target, GanderError.Host.TooLarge(413, "")))
        assertTrue(shouldReportRefusal(target, GanderError.Host.PermissionDenied(403, "needs preview")))
        assertTrue(shouldReportRefusal(target, GanderError.Host.NotFound(404, "")))
        assertTrue(shouldReportRefusal(target, GanderError.Host.TokenRevoked(401, "")))

        // an outage, a busy renderer and a server hiccup say nothing about this file
        assertFalse(shouldReportRefusal(target, GanderError.Unreachable(listOf("a"), null)))
        assertFalse(shouldReportRefusal(target, GanderError.Timeout(IOException())))
        assertFalse(shouldReportRefusal(target, GanderError.Host.Generating(503, "", 1)))
        assertFalse(shouldReportRefusal(target, GanderError.Host.TooManyRequests(429, "", 1)))
        assertFalse(shouldReportRefusal(target, GanderError.Host.Internal(500, "")))
    }

    @Test
    fun aReportedRefusalNamesTheNoRendererReasonForThePreviewAction() {
        val reason = ActionReason.fromError(FileAction.PREVIEW, GanderError.Host.UnsupportedType(415, ""), "pdf")
        assertEquals(ActionReason.NoRenderer("pdf"), reason)
    }

    // Playback ---------------------------------------------------------------------------------

    @Test
    fun aPhoneThatCannotDecodeTheVideoSaysSoInsteadOfAGenericFailure() {
        listOf(
            PlaybackException.ERROR_CODE_DECODER_INIT_FAILED,
            PlaybackException.ERROR_CODE_DECODER_QUERY_FAILED,
            PlaybackException.ERROR_CODE_DECODING_FAILED,
            PlaybackException.ERROR_CODE_DECODING_FORMAT_EXCEEDS_CAPABILITIES,
            PlaybackException.ERROR_CODE_DECODING_FORMAT_UNSUPPORTED,
            PlaybackException.ERROR_CODE_PARSING_CONTAINER_UNSUPPORTED,
        ).forEach { code ->
            assertEquals(PlaybackFailure.CodecUnsupported, playbackFailureOf(code, null))
        }
    }

    @Test
    fun aConnectionFailureWhilePlayingIsTheSameErrorAsEverywhereElse() {
        val offline = playbackFailureOf(
            PlaybackException.ERROR_CODE_IO_NETWORK_CONNECTION_FAILED,
            ConnectException("refused"),
        )
        assertTrue((offline as PlaybackFailure.Host).error is GanderError.Network)

        val tls = playbackFailureOf(
            PlaybackException.ERROR_CODE_IO_UNSPECIFIED,
            RuntimeException("wrapper", SSLHandshakeException("handshake")),
        )
        // a wrapper without a type of its own is a network failure; the TLS cause is kept for diagnostics
        assertTrue((tls as PlaybackFailure.Host).error is GanderError.Network)
    }

    @Test
    fun anythingElseIsOtherWithItsCodeForDiagnostics() {
        val other = playbackFailureOf(PlaybackException.ERROR_CODE_UNSPECIFIED, null)
        assertEquals(PlaybackFailure.Other(PlaybackException.ERROR_CODE_UNSPECIFIED), other)
        val noCause = playbackFailureOf(PlaybackException.ERROR_CODE_IO_NETWORK_CONNECTION_TIMEOUT, null)
        assertEquals(PlaybackFailure.Other(PlaybackException.ERROR_CODE_IO_NETWORK_CONNECTION_TIMEOUT), noCause)
    }
}
