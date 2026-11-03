package fyi.arlo.gander.core.net

import android.annotation.SuppressLint
import java.security.MessageDigest
import java.security.cert.Certificate
import java.security.cert.CertificateException
import java.security.cert.X509Certificate
import java.util.Base64
import javax.net.ssl.HostnameVerifier
import javax.net.ssl.SSLPeerUnverifiedException
import javax.net.ssl.SSLSession
import javax.net.ssl.X509TrustManager

/**
 * A host pin: the base64url (no padding) SHA-256 of the host certificate's SubjectPublicKeyInfo.
 * It is also the host id (pairing spec, "Host identity"). Not a secret, but compared in constant
 * time anyway.
 */
class SpkiPin private constructor(private val digest: ByteArray) {
    /** The base64url form, as in `GET /v1/host` `host_id` and the QR `id` parameter. */
    val hostId: String = ENCODER.encodeToString(digest)

    fun matches(certificate: X509Certificate): Boolean =
        MessageDigest.isEqual(digest, sha256(certificate.publicKey.encoded))

    override fun equals(other: Any?): Boolean =
        other is SpkiPin && MessageDigest.isEqual(digest, other.digest)

    override fun hashCode(): Int = digest.contentHashCode()

    override fun toString(): String = "SpkiPin($hostId)"

    companion object {
        private val ENCODER = Base64.getUrlEncoder().withoutPadding()

        /** Parses a host id; throws [IllegalArgumentException] if it is not a base64url SHA-256. */
        fun fromHostId(hostId: String): SpkiPin {
            val bytes = try {
                Base64.getUrlDecoder().decode(hostId)
            } catch (e: IllegalArgumentException) {
                throw IllegalArgumentException("host id is not base64url", e)
            }
            require(bytes.size == SHA256_BYTES) { "host id is not a SHA-256 digest" }
            return SpkiPin(bytes)
        }

        /** The pin of [certificate]'s public key. */
        fun of(certificate: X509Certificate): SpkiPin = SpkiPin(sha256(certificate.publicKey.encoded))

        private fun sha256(bytes: ByteArray): ByteArray = MessageDigest.getInstance("SHA-256").digest(bytes)

        private const val SHA256_BYTES = 32
    }
}

/** Thrown by [GanderTrustManager] when the leaf certificate is not the paired host's. */
class SpkiPinMismatchException(val expected: SpkiPin, val actual: SpkiPin?) :
    CertificateException("host certificate does not match the pinned host id")

/**
 * Accepts a server chain only if the leaf certificate's SPKI SHA-256 equals [pin]. The system
 * trust store is never consulted: a certificate signed by a public or user-installed CA is
 * rejected exactly like any other certificate that is not the paired host's.
 *
 * Validity dates are deliberately not checked: the host certificate is self-signed with a
 * 10-year lifetime and the pin is the trust anchor (like an SSH host key), and a phone with a
 * wrong clock must not lock its owner out. Key rotation means re-pairing.
 */
@SuppressLint("CustomX509TrustManager") // pinning to the host's own key is the point (ADR-0005)
class GanderTrustManager(val pin: SpkiPin) : X509TrustManager {

    override fun checkServerTrusted(chain: Array<out X509Certificate>?, authType: String?) {
        val leaf = chain?.firstOrNull() ?: throw CertificateException("empty certificate chain")
        if (!pin.matches(leaf)) throw SpkiPinMismatchException(pin, SpkiPin.of(leaf))
    }

    override fun checkClientTrusted(chain: Array<out X509Certificate>?, authType: String?) {
        throw CertificateException("Gander clients do not accept client connections")
    }

    /** No issuers: nothing chains to a CA here. */
    override fun getAcceptedIssuers(): Array<X509Certificate> = emptyArray()
}

/**
 * The host certificate names a host id, not the address the phone dialled, so ordinary
 * hostname checks cannot apply. Instead of returning true unconditionally, this verifier
 * re-checks that the session's peer is the pinned host, so it stays safe even if it were ever
 * paired with a different trust manager by mistake.
 */
class PinnedHostnameVerifier(private val pin: SpkiPin) : HostnameVerifier {
    override fun verify(hostname: String?, session: SSLSession?): Boolean {
        val peer: Array<Certificate> = try {
            session?.peerCertificates ?: return false
        } catch (_: SSLPeerUnverifiedException) {
            return false
        }
        val leaf = peer.firstOrNull() as? X509Certificate ?: return false
        return pin.matches(leaf)
    }
}

/**
 * First contact for pairing flow B (manual address): trust on first use for `GET /v1/host` and
 * `POST /v1/pair` ONLY. It accepts any certificate and records the leaf it saw, so the caller can
 * check that `host_id` equals [observedPin] and compute the SAS from the certificate the phone
 * actually saw (a MITM's different key gives a different code). Never use it with a token.
 *
 * With [expected] set (WP-22) it is the pairing client for everything AFTER the first contact: it
 * records the leaf like above but fails the handshake with [SpkiPinMismatchException] unless the
 * leaf is the [expected] key, so nothing (in particular the QR secret of flow A) is sent to a
 * different computer. The recorded leaf then gives the exact SPKI bytes for the SAS.
 */
@SuppressLint("CustomX509TrustManager", "TrustAllX509TrustManager") // TOFU step, see KDoc
class ObservingTrustManager(private val expected: SpkiPin? = null) : X509TrustManager {
    @Volatile
    var observedLeaf: X509Certificate? = null
        private set

    val observedPin: SpkiPin? get() = observedLeaf?.let(SpkiPin::of)

    override fun checkServerTrusted(chain: Array<out X509Certificate>?, authType: String?) {
        val leaf = chain?.firstOrNull() ?: throw CertificateException("empty certificate chain")
        if (expected != null && !expected.matches(leaf)) throw SpkiPinMismatchException(expected, SpkiPin.of(leaf))
        observedLeaf = leaf
    }

    override fun checkClientTrusted(chain: Array<out X509Certificate>?, authType: String?) {
        throw CertificateException("Gander clients do not accept client connections")
    }

    override fun getAcceptedIssuers(): Array<X509Certificate> = emptyArray()
}
