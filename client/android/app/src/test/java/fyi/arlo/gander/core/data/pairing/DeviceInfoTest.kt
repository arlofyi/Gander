package fyi.arlo.gander.core.data.pairing

import org.junit.Assert.assertEquals
import org.junit.Test

class DeviceInfoTest {
    @Test
    fun cleansWhatTheHostWouldRefuse() {
        assertEquals("Pixel 9", DeviceInfo.sanitize("  Pixel 9\n", "fallback"))
        assertEquals("Pixel 9", DeviceInfo.sanitize("Pixel\u0000 9\u0007", "fallback"))
        assertEquals("PixelX", DeviceInfo.sanitize("Pixel\tX", "fallback")) // control characters are dropped, not spaced
        assertEquals("fallback", DeviceInfo.sanitize(null, "fallback"))
        assertEquals("fallback", DeviceInfo.sanitize("   \n\t ", "fallback"))
    }

    @Test
    fun cutsToTheHostsLimitWithoutSplittingACharacter() {
        val long = "x".repeat(100)
        assertEquals(64, DeviceInfo.sanitize(long, "f").length)
        val emoji = "📱".repeat(70) // 70 code points, 140 UTF-16 units
        val cut = DeviceInfo.sanitize(emoji, "f")
        assertEquals(64, cut.codePointCount(0, cut.length))
        assertEquals(emoji.substring(0, 128), cut)
    }

    @Test
    fun describesThePhoneInProtocolTerms() {
        val device = DeviceInfo.describe(userName = "Alice's phone", model = "Pixel 9", appVersion = "0.1.0")
        assertEquals("Alice's phone", device.name)
        assertEquals("android", device.platform)
        assertEquals("Pixel 9", device.model)
        assertEquals("0.1.0", device.appVersion)
    }

    @Test
    fun theModelNamesAPhoneWithoutAUserGivenName() {
        val device = DeviceInfo.describe(userName = null, model = "SM-X710", appVersion = "0.1.0")
        assertEquals("SM-X710", device.name)
        val bare = DeviceInfo.describe(userName = " ", model = "", appVersion = "")
        assertEquals("Android device", bare.name)
        assertEquals("Android device", bare.model)
        assertEquals("unknown", bare.appVersion)
    }
}
