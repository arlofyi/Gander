package fyi.arlo.gander.core.data.diagnostics

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The redactor against planted secrets: every value below is made up for the test, and the
 * assertions are that none of it comes out, and that what support needs does.
 */
class DiagnosticsRedactorTest {
    // Planted values. None of these is a real credential or a real person.
    private val token = "plantedTokenAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
    private val otherToken = "anotherTokenBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB"
    private val salt = ByteArray(16) { it.toByte() }

    private fun redactor(
        secrets: List<String> = emptyList(),
        names: Map<String, String> = emptyMap(),
        salt: ByteArray = this.salt,
    ) = DiagnosticsRedactor(secrets, names, salt)

    private fun assertNoneOf(text: String, vararg forbidden: String) {
        for (value in forbidden) assertFalse("'$value' leaked into: $text", text.contains(value, ignoreCase = true))
    }

    // Tokens and secrets -----------------------------------------------------------------------

    @Test
    fun bearerTokensGoEvenWhenTheRedactorDoesNotKnowThem() {
        val out = redactor().redact("request failed: Authorization: Bearer $token (status 401)")
        assertNoneOf(out, token)
        assertTrue(out, out.contains("status 401"))
        assertNoneOf(redactor().redact("bearer   $otherToken"), otherToken)
    }

    @Test
    fun knownSecretsAreBlankedWhereverTheyAppear() {
        val text = "a=$token b=prefix${token}suffix url=https://x.example/?t=$token log=[$otherToken]"
        val out = redactor(secrets = listOf(token, otherToken)).redact(text)
        assertNoneOf(out, token, otherToken)
        assertTrue(out, out.contains("<redacted>"))
    }

    @Test
    fun tokenLikeFieldsInJsonAndKeyValueTextAreBlanked() {
        val out = redactor().redact(
            """{"token":"abcdef123456","secret": "s3cr3t-value","note":"ok"} password=hunter2; api_key: k-123456 pairing_secret=zzz999""",
        )
        assertNoneOf(out, "abcdef123456", "s3cr3t-value", "hunter2", "k-123456", "zzz999")
        assertTrue(out, out.contains("\"note\":\"ok\""))
    }

    @Test
    fun pairingLinksAreRemovedWhole() {
        val out = redactor().redact(
            "scanned gander://pair?id=HOSTIDHOSTID&secret=TOPSECRET123&addr=100.64.1.2%3A42637&name=Alice done",
        )
        assertNoneOf(out, "TOPSECRET123", "100.64.1.2", "Alice", "HOSTIDHOSTID")
        assertTrue(out, out.endsWith("done"))
    }

    @Test
    fun secretsShorterThanTheMinimumAreNotUsedToMangleText() {
        val out = redactor(secrets = listOf("ab")).redact("table of contents about abc")
        assertEquals("table of contents about abc", out)
    }

    // Addresses --------------------------------------------------------------------------------

    @Test
    fun ipv4AddressesAreMaskedButKeepTheirNetwork() {
        val out = redactor().redact("connect to 100.101.102.103:42637 failed, fallback 192.168.1.20")
        assertNoneOf(out, "100.101.102.103", "102.103", "192.168.1.20", "1.20")
        assertTrue(out, out.contains("100.101.*.*:42637"))
        assertTrue(out, out.contains("192.168.*.*"))
    }

    @Test
    fun versionsAndNonAddressesAreLeftAlone() {
        val text = "app 1.2.3 build 4.5.6.7.8 octets 999.1.1.1 size=1234 at 2026-11-02T10:00:00Z took 12:30:45"
        assertEquals(text, redactor().redact(text))
    }

    @Test
    fun ipv6AddressesAreMaskedButTimesAreNot() {
        val out = redactor().redact("tried fd7a:115c:a1e0:ab12:4843:cd96:6270:abcd and fd7a:115c:a1e0::1234 at 10:00:00")
        assertNoneOf(out, "a1e0", "abcd", "1234")
        assertTrue(out, out.contains("fd7a:115c:*"))
        assertTrue(out, out.endsWith("at 10:00:00"))
    }

    @Test
    fun urlsLoseCredentialsHostAndPath() {
        val out = redactor().redact(
            "GET https://alice:hunter2@studio-pc.tail1234.ts.net:42637/v1/shares/shr_1/list?path=Secret%20Plans failed",
        )
        assertNoneOf(out, "alice", "hunter2", "studio-pc", "tail1234", "shr_1", "Secret")
        assertTrue(out, out.contains("https://*.ts.net:42637/<path>"))
        assertTrue(out, out.endsWith("failed"))

        val v6 = redactor().redact("https://[fd7a:115c:a1e0::1]:42637/v1/me")
        assertNoneOf(v6, "a1e0", "/v1/me")
        assertTrue(v6, v6.contains("42637"))
    }

    @Test
    fun maskedAddressKeepWhatTroubleshootingNeeds() {
        val r = redactor()
        assertEquals("100.64.*.*", r.maskAddress("100.64.12.34"))
        assertEquals("fd7a:115c:*", r.maskAddress("fd7a:115c:a1e0::1"))
        assertEquals("fd7a:115c:*", r.maskAddress("[fd7a:115c:a1e0::1]"))
        assertEquals("*.ts.net", r.maskAddress("studio-pc.tail1234.ts.net"))
        assertEquals("*.local", r.maskAddress("studio-pc.local"))
        assertEquals("*", r.maskAddress("studio-pc"))
        assertEquals("", r.maskAddress("  "))
    }

    // Names, paths, logins ---------------------------------------------------------------------

    @Test
    fun knownNamesBecomeStableTagsAndNeverAppear() {
        val r = redactor(names = mapOf("Studio PC" to "host", "Q3 salary review.xlsx" to "file", "Alice's Pixel" to "device"))
        val out = r.redact("host Studio PC opened Q3 salary review.xlsx from Alice's Pixel; studio pc again")
        assertNoneOf(out, "Studio PC", "salary", "Alice")
        val hostTag = r.tag("host", "Studio PC")
        assertTrue(out, out.contains(hostTag))
        assertEquals("both spellings are the same host", 2, Regex(Regex.escape(hostTag)).findAll(out).count())
        assertTrue(hostTag, Regex("host#[0-9a-f]{8}").matches(hostTag))
    }

    @Test
    fun tagsAreStableWithinAnExportAndDifferBetweenExports() {
        val a = redactor(salt = ByteArray(16) { 1 })
        val b = redactor(salt = ByteArray(16) { 2 })
        assertEquals(a.tag("file", "report.pdf"), a.tag("file", "REPORT.pdf"))
        assertNotEquals(a.tag("file", "report.pdf"), a.tag("file", "other.pdf"))
        assertNotEquals("a per-export salt: no dictionary of common names", a.tag("file", "report.pdf"), b.tag("file", "report.pdf"))
    }

    @Test
    fun shortNamesAreNotReplacedInFreeText() {
        val out = redactor(names = mapOf("pc" to "host")).redact("the pc and the pcb")
        assertEquals("the pc and the pcb", out)
    }

    @Test
    fun filePathsAreRemoved() {
        val windows = redactor().redact("open failed: C:\\Users\\alice\\Documents\\Tax Return 2025.pdf (denied)")
        assertNoneOf(windows, "alice", "Documents", "Tax", "2025")
        assertTrue(windows, windows.startsWith("open failed: <path>"))

        val unc = redactor().redact("share \\\\NAS01\\finance\\payroll.xlsx")
        assertNoneOf(unc, "NAS01", "finance", "payroll")

        val posix = redactor().redact("read /data/user/0/fyi.arlo.gander/files/fetched/j3/holiday.jpg failed")
        assertNoneOf(posix, "/data/user", "holiday", "fyi.arlo.gander/files")
        assertTrue(posix, posix.endsWith("failed"))
    }

    @Test
    fun relativeNamesAndApiWordsAreNotMistakenForPaths() {
        val text = "fetched/j3 and/or n/a 50 MiB/s"
        assertEquals(text, redactor().redact(text))
    }

    @Test
    fun loginNamesAreRemoved() {
        val out = redactor().redact(
            "identity: { login: alice@example.com, node: pixel-9 } user=bob username: carol account=dave CORP\\erin",
        )
        assertNoneOf(out, "alice", "example.com", "bob", "carol", "dave", "erin")
        assertTrue(out, out.contains("node: pixel-9"))
    }

    // Behaviour that must hold for every input -------------------------------------------------

    @Test
    fun hostIdsTimestampsAndCountersSurvive() {
        val pin = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"
        val text = "computer host#1a2b3c4d id=$pin os=windows port=42637 bytes=10/20 updated=2026-11-02T10:00:00.123Z restarts=0"
        assertEquals(text, redactor().redact(text))
    }

    @Test
    fun redactingTwiceChangesNothingMore() {
        val r = redactor(secrets = listOf(token), names = mapOf("Studio PC" to "host"))
        val messy = """
            Bearer $token Studio PC 100.64.1.2 fd7a:115c:a1e0::1 https://u:p@h.example:1/x/y?z=1
            C:\Users\a b\c.txt /data/user/0/x/y alice@example.com login: bob {"token":"zzzzzz"} gander://pair?secret=1
        """.trimIndent()
        val once = r.redact(messy)
        assertEquals(once, r.redact(once))
        assertNoneOf(once, token, "Studio PC", "100.64.1.2", "a1e0", "u:p", "alice", "bob", "zzzzzz")
    }

    @Test
    fun emptyAndPlainTextPassThrough() {
        assertEquals("", redactor().redact(""))
        assertEquals("nothing to hide here", redactor().redact("nothing to hide here"))
    }
}
