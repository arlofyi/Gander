package fyi.arlo.gander.ui.pairing

import com.google.zxing.BarcodeFormat
import com.google.zxing.BinaryBitmap
import com.google.zxing.ChecksumException
import com.google.zxing.DecodeHintType
import com.google.zxing.FormatException
import com.google.zxing.NotFoundException
import com.google.zxing.PlanarYUVLuminanceSource
import com.google.zxing.common.HybridBinarizer
import com.google.zxing.qrcode.QRCodeReader

/**
 * Reads a QR code from one camera frame's luminance (Y) plane with ZXing. Pure JVM, so unit tests
 * render a code and decode it without a camera. Not thread-safe: use one per analysis thread.
 *
 * QR decoding does not depend on rotation, so frames are passed as the sensor delivers them.
 * The decoded text is the pairing URI and carries the window secret: callers must not log it.
 */
class QrFrameDecoder {
    private val reader = QRCodeReader()
    private val hints = mapOf(
        DecodeHintType.POSSIBLE_FORMATS to listOf(BarcodeFormat.QR_CODE),
        DecodeHintType.TRY_HARDER to true,
    )

    /**
     * [luma] holds `width * height` luminance bytes, row after row without padding. Returns the
     * text of the first QR code found, or null.
     */
    fun decode(luma: ByteArray, width: Int, height: Int): String? {
        require(luma.size >= width * height) { "luma plane smaller than width * height" }
        val source = PlanarYUVLuminanceSource(luma, width, height, 0, 0, width, height, false)
        return try {
            reader.decode(BinaryBitmap(HybridBinarizer(source)), hints).text
        } catch (_: NotFoundException) {
            null
        } catch (_: ChecksumException) {
            null
        } catch (_: FormatException) {
            null
        } finally {
            reader.reset()
        }
    }

    companion object {
        /**
         * Copies the Y plane out of a camera buffer whose rows are [rowStride] bytes apart (the
         * stride is usually wider than the image). The result has no padding.
         */
        fun tightLuma(plane: java.nio.ByteBuffer, rowStride: Int, width: Int, height: Int): ByteArray {
            val out = ByteArray(width * height)
            val buffer = plane.duplicate()
            for (row in 0 until height) {
                buffer.position(row * rowStride)
                buffer.get(out, row * width, width)
            }
            return out
        }
    }
}
