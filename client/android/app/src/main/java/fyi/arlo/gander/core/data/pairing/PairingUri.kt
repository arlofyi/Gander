package fyi.arlo.gander.core.data.pairing

import fyi.arlo.gander.core.net.SpkiPin
import java.io.ByteArrayOutputStream
import java.nio.ByteBuffer
import java.nio.charset.CharacterCodingException
import java.nio.charset.CodingErrorAction
import java.util.Base64

/**
 * The window secret from a pairing QR code (pairing spec, flow A). It is shown to nobody as text
 * and never logged: [toString] is redacted and there is no `equals` on the text, so a stray
 * `println(invite)` or a crash report cannot leak it.
 */
class PairingSecret(val value: String) {
    override fun toString(): String = "PairingSecret(<redacted>)"
}

/**
 * What a scanned `gander://pair` code says: the pin to verify before anything is sent, the
 * addresses to try and the secret that proves the phone saw the QR code. Never log it.
 */
class PairingInvite(
    /** Host id: the pin the TLS handshake is checked against. */
    val hostId: String,
    /** IP literals to try in order (never empty). */
    val addresses: List<String>,
    val port: Int,
    /** The computer's name, for display only. May be blank. */
    val hostName: String,
    val secret: PairingSecret,
    val magicDns: String?,
) {
    override fun toString(): String =
        "PairingInvite(hostId=$hostId, addresses=$addresses, port=$port, hostName=$hostName, secret=<redacted>)"
}

/** Why a scanned code could not be used. Carries the field name only, never a value. */
sealed interface PairingUriError {
    /** Not a `gander://pair?` URI. */
    data object NotGander : PairingUriError

    /** `v` is missing or not a version this build reads. */
    data object Version : PairingUriError

    /** A required field is missing or malformed. */
    data class Field(val name: String) : PairingUriError
}

sealed interface PairingUriResult {
    class Success(val invite: PairingInvite) : PairingUriResult {
        override fun toString(): String = "Success(<invite>)"
    }

    data class Failure(val error: PairingUriError) : PairingUriResult
}

/**
 * Parser for the flow A QR payload, a mirror of `QrPayload::parse` in
 * `host/gander-core/src/pairing/qr.rs`:
 *
 * ```text
 * gander://pair?v=1&id=<host_id>&a=<addr1>,<addr2>&p=<port>&n=<host name>&s=<secret b64url>&m=<magicdns>
 * ```
 *
 * Every value is percent-encoded (RFC 3986 unreserved characters literal, `%XX` for the rest,
 * UTF-8 for text); `+` is NOT a space. The comma between addresses is literal. `m` is optional.
 * Unknown keys are ignored so later versions can add fields. A repeated key keeps its last value.
 * The URI carries the window secret: never log it.
 */
object PairingUri {
    const val PREFIX = "gander://pair?"
    const val VERSION = 1

    /** Raw secret length in bytes (`SECRET_LEN` on the host). */
    const val SECRET_BYTES = 32

    fun parse(uri: String): PairingUriResult {
        val query = uri.takeIf { it.startsWith(PREFIX) }?.substring(PREFIX.length)
            ?: return fail(PairingUriError.NotGander)
        val values = HashMap<String, String>()
        for (pair in query.split('&')) {
            val eq = pair.indexOf('=')
            val key = if (eq < 0) pair else pair.substring(0, eq)
            val value = if (eq < 0) "" else pair.substring(eq + 1)
            if (key in KNOWN_KEYS) values[key] = value
        }
        val version = values["v"]?.takeIf { v -> v.isNotEmpty() && v.length <= 9 && v.all { it in '0'..'9' } }?.toInt()
        if (version != VERSION) return fail(PairingUriError.Version)
        fun field(name: String): String? = values[name]?.let(::percentDecode)

        val hostId = field("id")?.takeIf { it.isNotEmpty() && isHostId(it) } ?: return fail(PairingUriError.Field("id"))
        val addressesRaw = values["a"] ?: return fail(PairingUriError.Field("a"))
        val addresses = addressesRaw.split(',').filter { it.isNotEmpty() }.map { raw ->
            percentDecode(raw)?.takeIf(IpLiteral::isIpLiteral) ?: return fail(PairingUriError.Field("a"))
        }
        if (addresses.isEmpty()) return fail(PairingUriError.Field("a"))
        val port = values["p"]?.takeIf { p -> p.isNotEmpty() && p.length <= 5 && p.all { it in '0'..'9' } }
            ?.toInt()?.takeIf { it in 1..65535 } ?: return fail(PairingUriError.Field("p"))
        val secret = field("s")?.takeIf { Base64Url.isCanonical(it, SECRET_BYTES) } ?: return fail(PairingUriError.Field("s"))
        val magicDns = when (val raw = values["m"]) {
            null, "" -> null
            else -> percentDecode(raw) ?: return fail(PairingUriError.Field("m"))
        }
        val hostName = field("n") ?: return fail(PairingUriError.Field("n"))
        return PairingUriResult.Success(
            PairingInvite(hostId, addresses, port, hostName, PairingSecret(secret), magicDns),
        )
    }

    private fun fail(error: PairingUriError) = PairingUriResult.Failure(error)

    private fun isHostId(text: String): Boolean = try {
        SpkiPin.fromHostId(text)
        true
    } catch (_: IllegalArgumentException) {
        false
    }

    /** Percent-decodes [text] as UTF-8; null on a bad escape or invalid UTF-8. */
    internal fun percentDecode(text: String): String? {
        val out = ByteArrayOutputStream(text.length)
        var i = 0
        while (i < text.length) {
            if (text[i] == '%') {
                if (i + 3 > text.length) return null
                val hi = Character.digit(text[i + 1], 16)
                val lo = Character.digit(text[i + 2], 16)
                if (hi < 0 || lo < 0) return null
                out.write((hi shl 4) or lo)
                i += 3
            } else {
                // The host only emits ASCII here; a raw non-ASCII character is kept as its UTF-8 bytes.
                val step = Character.charCount(text.codePointAt(i))
                out.write(text.substring(i, i + step).toByteArray(Charsets.UTF_8))
                i += step
            }
        }
        return try {
            Charsets.UTF_8.newDecoder()
                .onMalformedInput(CodingErrorAction.REPORT)
                .onUnmappableCharacter(CodingErrorAction.REPORT)
                .decode(ByteBuffer.wrap(out.toByteArray()))
                .toString()
        } catch (_: CharacterCodingException) {
            null
        }
    }

    private val KNOWN_KEYS = setOf("v", "id", "a", "p", "n", "s", "m")
}

/** Strict base64url without padding (the wire form of nonces and the QR secret). */
internal object Base64Url {
    private val encoder = Base64.getUrlEncoder().withoutPadding()
    private val decoder = Base64.getUrlDecoder()

    fun encode(bytes: ByteArray): String = encoder.encodeToString(bytes)

    /** Decodes [text] to exactly [size] bytes, or null if it is anything else (padding, other alphabet, wrong length, loose trailing bits). */
    fun decodeFixed(text: String, size: Int): ByteArray? {
        if (text.length != (size * 8 + 5) / 6) return null
        if (!text.all { it in 'A'..'Z' || it in 'a'..'z' || it in '0'..'9' || it == '-' || it == '_' }) return null
        val bytes = try {
            decoder.decode(text)
        } catch (_: IllegalArgumentException) {
            return null
        }
        // Reject non-zero trailing bits like the host's strict decoder does.
        return bytes.takeIf { it.size == size && encode(it) == text }
    }

    fun isCanonical(text: String, size: Int): Boolean = decodeFixed(text, size) != null
}

/** IPv4 and IPv6 literals, parsed without any name lookup (same acceptance as Rust's `IpAddr::from_str`). */
internal object IpLiteral {
    fun isIpLiteral(text: String): Boolean = isIpv4(text) || isIpv6(text)

    fun isIpv4(text: String): Boolean {
        val parts = text.split('.')
        if (parts.size != 4) return false
        return parts.all { part ->
            part.isNotEmpty() && part.length <= 3 && part.all { it in '0'..'9' } &&
                (part.length == 1 || part[0] != '0') && part.toInt() <= 255
        }
    }

    fun isIpv6(text: String): Boolean {
        if (text.isEmpty() || !text.contains(':')) return false
        if (text.any { it !in '0'..'9' && it !in 'a'..'f' && it !in 'A'..'F' && it != ':' && it != '.' }) return false
        val doubleColon = text.indexOf("::")
        if (doubleColon >= 0 && text.indexOf("::", doubleColon + 1) >= 0) return false
        val head: List<String>
        val tail: List<String>
        if (doubleColon >= 0) {
            head = text.substring(0, doubleColon).let { if (it.isEmpty()) emptyList() else it.split(':') }
            tail = text.substring(doubleColon + 2).let { if (it.isEmpty()) emptyList() else it.split(':') }
        } else {
            head = text.split(':')
            tail = emptyList()
        }
        val groups = head + tail
        var width = 0
        for ((index, group) in groups.withIndex()) {
            val last = index == groups.lastIndex
            when {
                group.contains('.') -> {
                    if (!last || !isIpv4(group)) return false
                    width += 2
                }
                group.length in 1..4 -> width += 1
                else -> return false
            }
        }
        return if (doubleColon >= 0) width <= 7 else width == 8
    }
}
