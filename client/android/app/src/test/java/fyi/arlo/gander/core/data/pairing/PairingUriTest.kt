package fyi.arlo.gander.core.data.pairing

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * `gander://pair` parsing, mirroring the Rust `QrPayload` tests in
 * `host/gander-core/src/pairing/qr.rs` (same placeholder payload, same malformed cases).
 */
class PairingUriTest {

    /** The host-side placeholder from qr.rs: not a real secret, a fixed value of the right shape. */
    private val hostId = "uSb4Dq0KZ3fV8nXgT1oY2wL5rM9cE6pA7hJ0iN3sQ4k"
    private val zeroSecret = "A".repeat(43) // base64url of 32 zero bytes

    /** `QrPayload::to_uri` of the qr.rs test payload, as built by the host's encoder. */
    private val golden =
        "gander://pair?v=1&id=$hostId&a=100.101.102.103,fd7a%3A115c%3Aa1e0%3A%3A1&p=42637" +
            "&n=Studio%20PC%20%26%20Co%20%28L%C3%A6rke%29&s=$zeroSecret&m=studio-pc.tail0000.ts.net"

    private fun invite(uri: String): PairingInvite = when (val result = PairingUri.parse(uri)) {
        is PairingUriResult.Success -> result.invite
        is PairingUriResult.Failure -> throw AssertionError("expected a valid code, got ${result.error}")
    }

    private fun error(uri: String): PairingUriError = when (val result = PairingUri.parse(uri)) {
        is PairingUriResult.Failure -> result.error
        is PairingUriResult.Success -> throw AssertionError("expected a failure")
    }

    @Test
    fun parsesTheHostsPlaceholderPayload() {
        val invite = invite(golden)
        assertEquals(hostId, invite.hostId)
        assertEquals(listOf("100.101.102.103", "fd7a:115c:a1e0::1"), invite.addresses)
        assertEquals(42637, invite.port)
        assertEquals("Studio PC & Co (Lærke)", invite.hostName)
        assertEquals(zeroSecret, invite.secret.value)
        assertEquals("studio-pc.tail0000.ts.net", invite.magicDns)
    }

    @Test
    fun roundTripsThroughTheHostEncoding() {
        val uri = QrUris.build(hostId, listOf("100.101.102.103", "fd7a:115c:a1e0::1"), 42637, "Studio PC & Co (Lærke)", zeroSecret, "studio-pc.tail0000.ts.net")
        assertEquals(golden, uri)
        val noMagic = QrUris.build(hostId, listOf("100.101.102.103"), 42637, "pc", zeroSecret, null)
        assertFalse(noMagic.contains("&m="))
        assertNull(invite(noMagic).magicDns)
    }

    @Test
    fun anEmptyMagicDnsMeansNone() {
        assertNull(invite("$golden&m=").magicDns) // later key wins, here the empty one
        assertNull(invite(golden.replace("&m=studio-pc.tail0000.ts.net", "&m=")).magicDns)
    }

    @Test
    fun rejectsWhatIsNotAGanderPairingCode() {
        assertEquals(PairingUriError.NotGander, error("https://x"))
        assertEquals(PairingUriError.NotGander, error(""))
        assertEquals(PairingUriError.NotGander, error("gander://other?v=1"))
        assertEquals(PairingUriError.NotGander, error("gander://pair"))
        assertEquals(PairingUriError.NotGander, error(" $golden"))
    }

    @Test
    fun rejectsOtherVersions() {
        assertEquals(PairingUriError.Version, error(golden.replace("v=1", "v=2")))
        assertEquals(PairingUriError.Version, error(golden.replace("v=1&", "")))
        assertEquals(PairingUriError.Version, error(golden.replace("v=1", "v=one")))
        assertEquals(PairingUriError.Version, error(golden.replace("v=1", "v=-1")))
    }

    @Test
    fun rejectsMalformedFieldsNamingOnlyTheField() {
        assertEquals(PairingUriError.Field("p"), error(golden.replace("p=42637", "p=0")))
        assertEquals(PairingUriError.Field("p"), error(golden.replace("p=42637", "p=65536")))
        assertEquals(PairingUriError.Field("p"), error(golden.replace("p=42637", "p=port")))
        assertEquals(PairingUriError.Field("p"), error(golden.replace("&p=42637", "")))
        assertEquals(PairingUriError.Field("a"), error(golden.replace("100.101.102.103", "999.1.1.1")))
        assertEquals(PairingUriError.Field("a"), error(golden.replace(Regex("&a=[^&]*"), "&a=")))
        assertEquals(PairingUriError.Field("a"), error(golden.replace(Regex("&a=[^&]*"), "")))
        assertEquals(PairingUriError.Field("a"), error(golden.replace("100.101.102.103", "studio-pc"))) // names are not addresses
        assertEquals(PairingUriError.Field("s"), error("${golden.substringBefore("&s=")}&s=AAAA"))
        assertEquals(PairingUriError.Field("s"), error(golden.replace(zeroSecret, zeroSecret + "="))) // padding
        assertEquals(PairingUriError.Field("s"), error(golden.replace(zeroSecret, "+".repeat(43)))) // standard alphabet
        assertEquals(PairingUriError.Field("s"), error(golden.replace(zeroSecret, "A".repeat(42) + "B"))) // loose trailing bits
        assertEquals(PairingUriError.Field("id"), error(golden.replace("id=$hostId", "id=")))
        assertEquals(PairingUriError.Field("id"), error(golden.replace("id=$hostId", "id=tooshort")))
        assertEquals(PairingUriError.Field("n"), error(golden.replace(Regex("&n=[^&]*"), "")))
    }

    @Test
    fun rejectsBadPercentEscapesAndInvalidUtf8() {
        assertEquals(PairingUriError.Field("n"), error(golden.replace("Studio%20PC", "Studio%2")))
        assertEquals(PairingUriError.Field("n"), error(golden.replace("Studio%20PC", "Studio%zzPC")))
        assertEquals(PairingUriError.Field("n"), error(golden.replace("Studio%20PC", "Studio%C3%28")))
        assertEquals(PairingUriError.Field("m"), error(golden.replace("studio-pc.tail0000.ts.net", "bad%")))
    }

    @Test
    fun ignoresUnknownKeysAndKeepsTheLastOfARepeatedKey() {
        assertEquals(42637, invite("$golden&x=1").port)
        assertEquals(1234, invite("$golden&p=1234").port)
        assertEquals("a b", invite(golden.replace("n=Studio%20PC%20%26%20Co%20%28L%C3%A6rke%29", "n=a%20b")).hostName)
    }

    @Test
    fun plusIsNotASpace() {
        assertEquals("a+b", invite(golden.replace("n=Studio%20PC%20%26%20Co%20%28L%C3%A6rke%29", "n=a+b")).hostName)
    }

    @Test
    fun acceptsAnyIpLiteralTheHostCanWrite() {
        fun accepts(address: String) {
            val encoded = QrUris.encode(address)
            val uri = golden.replace(Regex("&a=[^&]*"), "&a=$encoded")
            assertEquals(address, invite(uri).addresses.single())
        }
        listOf("0.0.0.0", "255.255.255.255", "::", "::1", "fe80::1", "fd7a:115c:a1e0::1", "1:2:3:4:5:6:7:8", "::ffff:1.2.3.4", "2001:db8::").forEach(::accepts)
        fun rejects(address: String) {
            val uri = golden.replace(Regex("&a=[^&]*"), "&a=${QrUris.encode(address)}")
            assertEquals("address '$address'", PairingUriError.Field("a"), error(uri))
        }
        listOf("01.2.3.4", "1.2.3", "1.2.3.4.5", "256.1.1.1", "1:2:3:4:5:6:7:8:9", "1::2::3", ":::", "12345::1", "::1%eth0", "g::1", "1:2:3:4:5:6:7").forEach(::rejects)
    }

    @Test
    fun secretsStayOutOfToStringAndErrors() {
        val invite = invite(golden)
        assertFalse(invite.toString().contains(zeroSecret))
        assertFalse(invite.secret.toString().contains(zeroSecret))
        assertTrue(invite.toString().contains("<redacted>"))
        val target = PairingTarget.Qr(invite)
        assertFalse(target.toString().contains(zeroSecret))
        val failure = PairingUri.parse(golden.replace("p=42637", "p=0"))
        assertFalse(failure.toString().contains(zeroSecret))
        assertFalse(PairingUri.parse(golden).toString().contains(zeroSecret))
    }
}
