package fyi.arlo.gander.core.data.pairing

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class ManualAddressTest {
    private fun parse(text: String) = ManualAddress.parse(text)

    @Test
    fun anIpv4AddressTakesTheDefaultPort() {
        assertEquals(ManualAddress("100.101.102.103", 42637), parse("100.101.102.103"))
        assertEquals(ManualAddress("100.101.102.103", 42637), parse("  100.101.102.103 \n"))
    }

    @Test
    fun anExplicitPortIsUsed() {
        assertEquals(ManualAddress("10.0.2.2", 5000), parse("10.0.2.2:5000"))
        assertEquals(ManualAddress("studio-pc.tail1234.ts.net", 1), parse("studio-pc.tail1234.ts.net:1"))
        assertEquals(ManualAddress("studio-pc", 65535), parse("studio-pc:65535"))
    }

    @Test
    fun ipv6LiteralsWorkBareOrInBrackets() {
        assertEquals(ManualAddress("fd7a:115c:a1e0::1", 42637), parse("fd7a:115c:a1e0::1"))
        assertEquals(ManualAddress("fd7a:115c:a1e0::1", 42637), parse("[fd7a:115c:a1e0::1]"))
        assertEquals(ManualAddress("fd7a:115c:a1e0::1", 4000), parse("[fd7a:115c:a1e0::1]:4000"))
    }

    @Test
    fun hostNamesAreChecked() {
        assertEquals(ManualAddress("Studio-PC.local", 42637), parse("Studio-PC.local"))
        assertEquals(ManualAddress("studio-pc.tail1234.ts.net.", 42637), parse("studio-pc.tail1234.ts.net."))
    }

    @Test
    fun rejectsEverythingElse() {
        listOf(
            "", "   ", ":42637", "100.101.102.103:", "100.101.102.103:0", "100.101.102.103:65536", "100.101.102.103:port",
            "100.101.102.103:42637:1", "999.1.1.1", "1.2.3", "https://studio-pc", "studio pc", "-bad.example", "bad-.example",
            "under_score.example", "[fd7a::1", "[fd7a::1]x", "[studio-pc]:1", "fd7a::1::2", "a".repeat(64) + ".example",
            "gander://pair?v=1",
        ).forEach { assertNull("'$it' must not parse", parse(it)) }
    }

    @Test
    fun entryTextRoundTrips() {
        listOf("100.101.102.103", "10.0.2.2:5000", "studio-pc:1234", "[fd7a:115c:a1e0::1]:4000", "fd7a:115c:a1e0::1").forEach {
            val parsed = parse(it)!!
            assertEquals(parsed, parse(parsed.toEntryText()))
        }
        assertEquals("100.101.102.103", parse("100.101.102.103")!!.toEntryText())
        assertEquals("[fd7a::1]:4000", parse("[fd7a::1]:4000")!!.toEntryText())
    }
}
