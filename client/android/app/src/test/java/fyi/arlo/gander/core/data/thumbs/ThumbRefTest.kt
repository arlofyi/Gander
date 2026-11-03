package fyi.arlo.gander.core.data.thumbs

import fyi.arlo.gander.core.net.protocol.EntryFlag
import fyi.arlo.gander.core.net.protocol.PreviewKind
import fyi.arlo.gander.testing.SampleData.dir
import fyi.arlo.gander.testing.SampleData.file
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class ThumbRefTest {

    private fun ref(
        host: String = "host_A",
        share: String = "shr_1",
        path: String = "Photos/a.jpg",
        version: String = "2026-10-30T09:12:44Z#1024",
        size: Int = 256,
    ) = ThumbRef(host, share, path, version, size)

    @Test
    fun theCacheKeyChangesWithAnythingThatChangesThePixels() {
        val base = ref().cacheKey()
        assertEquals(base, ref().cacheKey())
        assertNotEquals(base, ref(host = "host_B").cacheKey())
        assertNotEquals(base, ref(share = "shr_2").cacheKey())
        assertNotEquals(base, ref(path = "Photos/b.jpg").cacheKey())
        assertNotEquals(base, ref(size = 128).cacheKey())
        // an edited file: same path, new mtime or size
        assertNotEquals(base, ref(version = "2026-10-31T09:12:44Z#1024").cacheKey())
        assertNotEquals(base, ref(version = "2026-10-30T09:12:44Z#2048").cacheKey())
    }

    @Test
    fun theCacheKeyIsHostIdentityAndFileOnly() {
        // no address, no token: both change without the picture changing (or must stay secret)
        assertEquals("gander-thumb/1|host_A|shr_1|256|2026-10-30T09:12:44Z#1024|Photos/a.jpg", ref().cacheKey())
        // the path is last, so a path containing the separator cannot move into another field
        assertNotEquals(ref(path = "a|b").cacheKey(), ref(path = "a").cacheKey())
    }

    @Test
    fun entriesGetTheirVersionFromMtimeAndSize() {
        val entry = file("a.jpg", size = 4096, mtime = "2026-11-01T10:00:00Z", preview = PreviewKind.IMAGE)
        assertEquals("2026-11-01T10:00:00Z#4096", entry.thumbVersion())
        val edited = entry.copy(mtime = "2026-11-02T10:00:00Z")
        assertNotEquals(entry.thumbVersion(), edited.thumbVersion())
    }

    @Test
    fun onlyFilesTheHostPredictsAThumbnailForAreRequested() {
        assertTrue(file("a.jpg", preview = PreviewKind.IMAGE).wantsThumbnail())
        assertFalse(file("notes.md").wantsThumbnail()) // thumb = false
        assertFalse(dir("Photos").wantsThumbnail())
        assertFalse(file("l.jpg", preview = PreviewKind.IMAGE, flags = listOf(EntryFlag.LINK)).wantsThumbnail())
    }

    @Test
    fun theRefNeverPrintsTheFileName() {
        assertFalse(ref(path = "Taxes/secret-plan.pdf").toString().contains("secret-plan"))
    }

    @Test
    fun aPhonePicksTheSmallestHostSizeThatCoversTheViewCappedAt256() {
        val host = listOf(128, 256, 512)
        // 40 dp list icon at 2.625 density = 105 px -> 128; 64 dp grid cell = 168 px -> 256
        assertEquals(128, ThumbSizes.pick(ThumbSizes.displayPx(40f, 2.625f), host, tablet = false))
        assertEquals(256, ThumbSizes.pick(ThumbSizes.displayPx(64f, 2.625f), host, tablet = false))
        // xxxhdpi grid cell: 256 px exactly fits
        assertEquals(256, ThumbSizes.pick(ThumbSizes.displayPx(64f, 4f), host, tablet = false))
        // never above the phone ceiling, however dense
        assertEquals(256, ThumbSizes.pick(ThumbSizes.displayPx(200f, 4f), host, tablet = false))
    }

    @Test
    fun aTabletMayGoUpTo512() {
        val host = listOf(128, 256, 512)
        assertEquals(512, ThumbSizes.pick(ThumbSizes.displayPx(200f, 2.5f), host, tablet = true))
        assertEquals(512, ThumbSizes.pick(ThumbSizes.displayPx(400f, 3f), host, tablet = true))
        assertEquals(128, ThumbSizes.pick(ThumbSizes.displayPx(40f, 2f), host, tablet = true))
    }

    @Test
    fun theSizeIsAlwaysOneTheHostRenders() {
        // the host answers 400 for a size that is not in thumb_sizes
        assertEquals(256, ThumbSizes.pick(100, listOf(256), tablet = false))
        assertEquals(256, ThumbSizes.pick(100, listOf(512, 256, 256), tablet = false))
        // only sizes above the ceiling on offer: take the smallest rather than refuse
        assertEquals(512, ThumbSizes.pick(100, listOf(512, 1024), tablet = false))
        // a host that lists nothing: the protocol default
        assertEquals(ThumbSizes.FALLBACK, ThumbSizes.pick(100, emptyList(), tablet = false))
        assertEquals(ThumbSizes.FALLBACK, ThumbSizes.pick(100, listOf(0, -4), tablet = false))
    }

    @Test
    fun displayPixelsRoundUpAndStayPositive() {
        assertEquals(105, ThumbSizes.displayPx(40f, 2.625f))
        assertEquals(1, ThumbSizes.displayPx(0f, 3f))
    }
}
