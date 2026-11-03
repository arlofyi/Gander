package fyi.arlo.gander.ui.pairing

import com.google.zxing.BarcodeFormat
import com.google.zxing.EncodeHintType
import com.google.zxing.qrcode.QRCodeWriter
import com.google.zxing.qrcode.decoder.ErrorCorrectionLevel
import fyi.arlo.gander.core.data.pairing.PairingInvite
import fyi.arlo.gander.core.data.pairing.PairingUri
import fyi.arlo.gander.core.data.pairing.PairingUriResult
import fyi.arlo.gander.core.data.pairing.QrUris
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.nio.ByteBuffer

/**
 * The scan path without a camera: a QR code is rendered with ZXing's writer into a luminance
 * plane (what CameraX's Y plane is) and read back by [QrFrameDecoder], including a plane whose
 * rows are padded the way camera buffers are.
 */
class QrFrameDecoderTest {
    private val hostId = "uSb4Dq0KZ3fV8nXgT1oY2wL5rM9cE6pA7hJ0iN3sQ4k"
    private val secret = "A".repeat(43) // placeholder, not a window secret
    private val uri = QrUris.build(
        hostId, listOf("100.101.102.103", "fd7a:115c:a1e0::1"), 42637, "Studio PC & Co (Lærke)", secret, "studio-pc.tail0000.ts.net",
    )

    /** [text] as a [size] x [size] luminance plane, black modules 0, white 255. */
    private fun render(text: String, size: Int = 480): ByteArray {
        val hints = mapOf(EncodeHintType.ERROR_CORRECTION to ErrorCorrectionLevel.M, EncodeHintType.MARGIN to 4)
        val matrix = QRCodeWriter().encode(text, BarcodeFormat.QR_CODE, size, size, hints)
        return ByteArray(size * size) { i -> if (matrix.get(i % size, i / size)) 0 else 0xFF.toByte() }
    }

    @Test
    fun readsThePairingUriFromARenderedCode() {
        val decoded = QrFrameDecoder().decode(render(uri), 480, 480)
        assertEquals(uri, decoded)
        val invite: PairingInvite = (PairingUri.parse(decoded!!) as PairingUriResult.Success).invite
        assertEquals(hostId, invite.hostId)
        assertEquals(42637, invite.port)
    }

    @Test
    fun readsACodeFromARowPaddedCameraBuffer() {
        val size = 480
        val stride = 512
        val plane = render(uri, size)
        val padded = ByteArray(stride * size) { 0x55 } // padding bytes must not matter
        for (row in 0 until size) plane.copyInto(padded, row * stride, row * size, row * size + size)
        val tight = QrFrameDecoder.tightLuma(ByteBuffer.wrap(padded), stride, size, size)
        assertEquals(plane.toList(), tight.toList())
        assertEquals(uri, QrFrameDecoder().decode(tight, size, size))
    }

    @Test
    fun aFrameWithoutACodeYieldsNothing() {
        val blank = ByteArray(320 * 240) { 0x80.toByte() }
        assertNull(QrFrameDecoder().decode(blank, 320, 240))
        val noise = ByteArray(320 * 240) { ((it * 31) xor (it shr 3)).toByte() }
        assertNull(QrFrameDecoder().decode(noise, 320, 240))
    }

    @Test
    fun aCodeThatIsNotGanderIsStillReadSoTheScreenCanSayWhy() {
        val decoded = QrFrameDecoder().decode(render("https://example.com/"), 480, 480)
        assertEquals("https://example.com/", decoded)
        assertTrue(PairingUri.parse(decoded!!) is PairingUriResult.Failure)
    }

    @Test
    fun theDecoderIsReusableAcrossFrames() {
        val decoder = QrFrameDecoder()
        assertNull(decoder.decode(ByteArray(100 * 100) { 0x80.toByte() }, 100, 100))
        assertEquals(uri, decoder.decode(render(uri), 480, 480))
        assertNull(decoder.decode(ByteArray(100 * 100) { 0x80.toByte() }, 100, 100))
        assertEquals(uri, decoder.decode(render(uri), 480, 480))
    }
}
