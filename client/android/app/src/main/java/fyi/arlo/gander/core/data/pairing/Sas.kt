package fyi.arlo.gander.core.data.pairing

import java.security.MessageDigest
import java.util.Locale

/** A six digit short authentication string, `0..999999`. */
@JvmInline
value class SasCode(val value: Int) {
    init {
        require(value in 0..999_999) { "SAS out of range" }
    }

    /** The six digits without the space, for example `482913`. */
    val digits: String get() = String.format(Locale.ROOT, "%06d", value)

    /** The display form, `482 913` (pairing spec: grouped 3+3). */
    val display: String get() = String.format(Locale.ROOT, "%03d %03d", value / 1000, value % 1000)

    /** True if [text] (`482 913` or `482913`) is this code. Anything else is false. */
    fun matches(text: String): Boolean = text.filterNot { it == ' ' } == digits && text.count { it == ' ' } <= 1
}

/**
 * The short authentication string both screens show during pairing (pairing spec, "Flow A").
 *
 * ```text
 * digest = SHA-256( "gander-sas-v1" || host_spki || device_nonce || host_nonce )
 * v20    = digest[0] << 12 | digest[1] << 4 | digest[2] >> 4        (first 20 bits, big-endian)
 * SAS    = v20 mod 1_000_000, zero-padded to six digits, shown "DDD DDD"
 * ```
 *
 * Byte-level choices are pinned by `protocol/test-vectors/sas.json` (the same file the Rust host
 * runs): `host_spki` is the DER SubjectPublicKeyInfo of the certificate the phone saw in the TLS
 * handshake (not its hash), the nonces are the raw 16 bytes, device nonce first, and the label is
 * its 13 ASCII bytes with no terminator or length prefix.
 *
 * In flow B a man in the middle presents another certificate, so the phone hashes another SPKI
 * than the host does and the two codes differ. That is why the phone must derive the code from
 * what it saw and never display the `sas` the host sends.
 */
object Sas {
    const val NONCE_BYTES = 16
    private val LABEL = "gander-sas-v1".toByteArray(Charsets.US_ASCII)

    fun digest(hostSpkiDer: ByteArray, deviceNonce: ByteArray, hostNonce: ByteArray): ByteArray {
        require(deviceNonce.size == NONCE_BYTES && hostNonce.size == NONCE_BYTES) { "nonces are 16 bytes" }
        return MessageDigest.getInstance("SHA-256").run {
            update(LABEL)
            update(hostSpkiDer)
            update(deviceNonce)
            update(hostNonce)
            digest()
        }
    }

    /** The first 20 bits of [digest], big-endian. */
    fun first20Bits(digest: ByteArray): Int =
        ((digest[0].toInt() and 0xFF) shl 12) or ((digest[1].toInt() and 0xFF) shl 4) or ((digest[2].toInt() and 0xFF) ushr 4)

    fun derive(hostSpkiDer: ByteArray, deviceNonce: ByteArray, hostNonce: ByteArray): SasCode =
        SasCode(first20Bits(digest(hostSpkiDer, deviceNonce, hostNonce)) % 1_000_000)
}
