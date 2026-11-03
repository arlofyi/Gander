package fyi.arlo.gander.core.data.fetch

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import java.io.File

/**
 * The LRU cache (client spec: "Cache cap default 2 GiB, LRU; pinned files are exempt"): the pure
 * rule in [CachePolicy], the deletion in [OfflineFiles.evictToCap] and the automatic trigger in
 * [CacheEnforcer].
 */
@OptIn(ExperimentalCoroutinesApi::class)
class CacheEvictionTest {
    @get:Rule
    val temp = TemporaryFolder()

    private val mib = 1024L * 1024
    private val grace = CachePolicy.GRACE_MILLIS
    private val now = 10 * grace // far enough from 0 that "idle" is large for every old copy

    private fun cached(
        name: String,
        size: Long,
        lastUsed: Long,
        pinned: Boolean = false,
        host: String = "h",
        fetchedAt: Long = lastUsed,
    ) = CachedFile(
        key = FileKey(host, "s", name),
        name = name,
        mime = null,
        localPath = "fetched/j${name.hashCode().toUInt()}/$name",
        size = size,
        mtime = "2026-11-01T00:00:00Z",
        etag = "\"e\"",
        pinned = pinned,
        lastUsed = lastUsed,
        fetchedAt = fetchedAt,
    )

    // The rule ---------------------------------------------------------------------------------

    @Test
    fun underTheCapNothingIsEvicted() {
        val files = listOf(cached("a", 400 * mib, 1), cached("b", 400 * mib, 2))
        val plan = CachePolicy.plan(files, capBytes = 800 * mib, nowMillis = now)
        assertTrue(plan.victims.isEmpty())
        assertEquals(800 * mib, plan.usedBytes)
        assertNull(plan.retryInMillis)
    }

    @Test
    fun theLeastRecentlyUsedGoFirstAndOnlyAsManyAsNeeded() {
        val files = listOf(
            cached("newest", 300 * mib, lastUsed = 4_000),
            cached("oldest", 300 * mib, lastUsed = 1_000),
            cached("middle", 300 * mib, lastUsed = 2_000),
            cached("recent", 300 * mib, lastUsed = 3_000),
        )
        // 1200 MiB used, cap 700: dropping the two oldest leaves 600
        val plan = CachePolicy.plan(files, capBytes = 700 * mib, nowMillis = now)
        assertEquals(listOf("oldest", "middle"), plan.victims.map { it.name })
        assertNull(plan.retryInMillis)
    }

    @Test
    fun exactlyAtTheCapIsFine() {
        val files = listOf(cached("a", 500 * mib, 1), cached("b", 500 * mib, 2))
        assertTrue(CachePolicy.plan(files, capBytes = 1000 * mib, nowMillis = now).victims.isEmpty())
        assertEquals(listOf("a"), CachePolicy.plan(files, capBytes = 999 * mib, nowMillis = now).victims.map { it.name })
    }

    @Test
    fun pinnedCopiesAreNeverEvictedButStillCountTowardsTheTotal() {
        val files = listOf(
            cached("kept-old", 600 * mib, lastUsed = 1, pinned = true),
            cached("a", 300 * mib, lastUsed = 2),
            cached("b", 300 * mib, lastUsed = 3),
            cached("c", 300 * mib, lastUsed = 4),
        )
        // 1500 used, cap 1000: the pinned file is the oldest but stays, so a and b go (600 left + c)
        val plan = CachePolicy.plan(files, capBytes = 1000 * mib, nowMillis = now)
        assertEquals(listOf("a", "b"), plan.victims.map { it.name })
        assertFalse(plan.victims.any { it.pinned })
    }

    @Test
    fun whenPinnedCopiesAloneExceedTheCapEveryUnpinnedOneGoAndThePinnedStay() {
        val files = listOf(
            cached("kept-1", 700 * mib, lastUsed = 1, pinned = true),
            cached("kept-2", 700 * mib, lastUsed = 2, pinned = true),
            cached("other", 100 * mib, lastUsed = 3),
        )
        val plan = CachePolicy.plan(files, capBytes = 1000 * mib, nowMillis = now)
        assertEquals(listOf("other"), plan.victims.map { it.name })
    }

    @Test
    fun aCopyUsedAMomentAgoIsNotEvictedEvenOverTheCapAndTheRetryHintSaysWhen() {
        val files = listOf(
            cached("old", 300 * mib, lastUsed = 1_000),
            cached("just-opened", 900 * mib, lastUsed = now - 60_000),
        )
        val plan = CachePolicy.plan(files, capBytes = 500 * mib, nowMillis = now)
        assertEquals(listOf("old"), plan.victims.map { it.name })
        // still 900 MiB used against 500, held back for the rest of its grace period
        assertEquals(grace - 60_000, plan.retryInMillis)
    }

    @Test
    fun aCopyThatIsPastTheGracePeriodCanGo() {
        val files = listOf(cached("a", 900 * mib, lastUsed = now - grace))
        assertEquals(listOf("a"), CachePolicy.plan(files, capBytes = 500 * mib, nowMillis = now).victims.map { it.name })
    }

    @Test
    fun tiesAreBrokenByFetchTimeThenNameSoTheOrderIsStable() {
        val files = listOf(
            cached("b", 100 * mib, lastUsed = 5, fetchedAt = 9),
            cached("a", 100 * mib, lastUsed = 5, fetchedAt = 9),
            cached("c", 100 * mib, lastUsed = 5, fetchedAt = 1),
        )
        val plan = CachePolicy.plan(files, capBytes = 0, nowMillis = now)
        assertEquals(listOf("c", "a", "b"), plan.victims.map { it.name })
    }

    // The deletion -----------------------------------------------------------------------------

    private class Fixture(root: File, clock: () -> Long) {
        val store = InMemoryFetchStore()
        val files = FetchFiles(root)
        val offline = OfflineFiles(store, files, clock)

        /** A finished download of [size] bytes: the file on disk and its row. */
        suspend fun add(
            jobId: Long,
            name: String,
            size: Int,
            lastUsed: Long,
            pinned: Boolean = false,
            host: String = "h",
        ): CachedFile {
            val dir = files.jobDir(jobId).also { it.mkdirs() }
            File(dir, name).writeBytes(ByteArray(size))
            val row = CachedFile(
                key = FileKey(host, "s", name),
                name = name,
                mime = null,
                localPath = files.finalRelativePath(jobId, name),
                size = size.toLong(),
                mtime = "m",
                etag = "\"e\"",
                pinned = pinned,
                lastUsed = lastUsed,
                fetchedAt = lastUsed,
            )
            store.complete(
                FetchJob(id = jobId, key = row.key, entry = fyi.arlo.gander.testing.SampleData.file(name, size.toLong())),
                row,
            )
            return row
        }

        fun exists(row: CachedFile) = files.resolve(row.localPath)?.isFile == true
    }

    @Test
    fun evictToCapDeletesTheRowsAndTheBytesOfTheLeastRecentlyUsed() = runBlocking {
        val fx = Fixture(temp.root) { now }
        val oldest = fx.add(1, "oldest.bin", 400, lastUsed = 1_000)
        val middle = fx.add(2, "middle.bin", 400, lastUsed = 2_000)
        val newest = fx.add(3, "newest.bin", 400, lastUsed = 3_000)

        val result = fx.offline.evictToCap(capBytes = 900)

        assertEquals(listOf("oldest.bin"), result.evicted.map { it.name })
        assertEquals(400L, result.freedBytes)
        assertEquals(800L, result.usedBytes)
        assertNull(fx.offline.get(oldest.key))
        assertFalse(fx.exists(oldest))
        assertFalse("the whole job folder goes", fx.files.jobDir(1).exists())
        assertTrue(fx.exists(middle) && fx.exists(newest))
        assertEquals(800L, fx.offline.usedBytes())
    }

    @Test
    fun evictToCapLeavesKeptOfflineFilesEvenWhenFarOverTheCap() = runBlocking {
        val fx = Fixture(temp.root) { now }
        val kept = fx.add(1, "kept.bin", 1000, lastUsed = 1, pinned = true)
        val other = fx.add(2, "other.bin", 100, lastUsed = 2)

        val result = fx.offline.evictToCap(capBytes = 10)

        assertEquals(listOf("other.bin"), result.evicted.map { it.name })
        assertTrue(fx.exists(kept))
        assertNotNull(fx.offline.get(kept.key))
        assertFalse(fx.exists(other))
        assertEquals(1000L, result.usedBytes)
    }

    @Test
    fun openingACopyCountsAsUseSoItOutlivesOlderOnes() = runBlocking {
        var clock = now
        val fx = Fixture(temp.root) { clock }
        val a = fx.add(1, "a.bin", 400, lastUsed = 1_000)
        val b = fx.add(2, "b.bin", 400, lastUsed = 2_000)
        // a is older, but is opened long after b was last used
        clock = now + 5_000
        assertNotNull(fx.offline.open(a.key))
        clock = now + 2 * grace

        val result = fx.offline.evictToCap(capBytes = 500)

        assertEquals(listOf("b.bin"), result.evicted.map { it.name })
        assertTrue(fx.exists(a))
        assertFalse(fx.exists(b))
    }

    @Test
    fun aCopyPinnedBetweenThePlanAndTheDeleteSurvives() = runBlocking {
        val fx = Fixture(temp.root) { now }
        val a = fx.add(1, "a.bin", 400, lastUsed = 1)
        fx.add(2, "b.bin", 400, lastUsed = 2)
        // The user switches Keep offline on the moment the cache is being trimmed.
        val racing = object : CachedFileStore by fx.store {
            override suspend fun cached(key: FileKey): CachedFile? {
                if (key == a.key) fx.store.setPinned(a.key, true)
                return fx.store.cached(key)
            }
        }
        val offline = OfflineFiles(racing, fx.files) { now }

        val result = offline.evictToCap(capBytes = 500)

        assertTrue(result.evicted.isEmpty())
        assertTrue(fx.exists(a))
    }

    @Test
    fun removeUnpinnedKeepsPinnedAndCanBeLimitedToOneComputer() = runBlocking {
        val fx = Fixture(temp.root) { now }
        val studioFile = fx.add(1, "studio.bin", 100, lastUsed = 1, host = "studio")
        val studioKept = fx.add(2, "studio-kept.bin", 100, lastUsed = 1, host = "studio", pinned = true)
        val laptopFile = fx.add(3, "laptop.bin", 100, lastUsed = 1, host = "laptop")

        val removed = fx.offline.removeUnpinned("studio")

        assertEquals(Removed(1, 100), removed)
        assertFalse(fx.exists(studioFile))
        assertTrue(fx.exists(studioKept))
        assertTrue(fx.exists(laptopFile))

        assertEquals(Removed(1, 100), fx.offline.removeUnpinned())
        assertFalse(fx.exists(laptopFile))
        assertTrue(fx.exists(studioKept))
    }

    // The trigger ------------------------------------------------------------------------------

    private fun TestScope.enforcer(fx: Fixture, cap: MutableStateFlow<Long>) =
        CacheEnforcer(fx.offline, cap, backgroundScope, sleep = { kotlinx.coroutines.delay(it) })

    @Test
    fun theEnforcerTrimsWhenAFinishedFetchPushesTheCacheOverTheCap() = runTest {
        // Virtual time starts at 0; give "now" a head start so old copies are past the grace period.
        val fx = Fixture(temp.root) { now + testScheduler.currentTime }
        val cap = MutableStateFlow(900L)
        enforcer(fx, cap).start()
        fx.add(1, "a.bin", 400, lastUsed = 1_000)
        fx.add(2, "b.bin", 400, lastUsed = 2_000)
        runCurrent()
        assertEquals(2, fx.store.cachedFiles.first().size)

        val c = fx.add(3, "c.bin", 400, lastUsed = 3_000)
        runCurrent()

        assertEquals(listOf("c.bin", "b.bin"), fx.store.cachedFiles.first().map { it.name })
        assertTrue(fx.exists(c))
        assertEquals(800L, fx.offline.usedBytes())
    }

    @Test
    fun loweringTheCapInSettingsTrimsAtOnce() = runTest {
        val fx = Fixture(temp.root) { now + testScheduler.currentTime }
        val cap = MutableStateFlow(10_000L)
        fx.add(1, "a.bin", 400, lastUsed = 1_000)
        fx.add(2, "b.bin", 400, lastUsed = 2_000)
        enforcer(fx, cap).start()
        runCurrent()
        assertEquals(2, fx.store.cachedFiles.first().size)

        cap.value = 500
        runCurrent()

        assertEquals(listOf("b.bin"), fx.store.cachedFiles.first().map { it.name })
    }

    @Test
    fun aCopyHeldBackByTheGracePeriodIsEvictedWhenItExpires() = runTest {
        val fx = Fixture(temp.root) { now + testScheduler.currentTime }
        val cap = MutableStateFlow(500L)
        enforcer(fx, cap).start()
        // fetched "just now": past the cap but inside the grace period
        fx.add(1, "big.bin", 900, lastUsed = now)
        runCurrent()
        assertEquals(1, fx.store.cachedFiles.first().size)

        advanceTimeBy(grace - 1_000)
        assertEquals("still inside the grace period", 1, fx.store.cachedFiles.first().size)

        advanceTimeBy(2_000)
        runCurrent()
        assertTrue("evicted once the grace period is over", fx.store.cachedFiles.first().isEmpty())
    }
}
