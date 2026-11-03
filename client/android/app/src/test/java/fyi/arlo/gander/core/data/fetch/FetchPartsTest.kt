package fyi.arlo.gander.core.data.fetch

import fyi.arlo.gander.core.data.actions.NetworkState
import fyi.arlo.gander.core.data.browse.EntryTarget
import fyi.arlo.gander.core.data.db.FetchRows
import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.protocol.HashAlgo
import fyi.arlo.gander.testing.SampleData
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import java.io.IOException

/** The pure parts around the engine: names and paths, Content-Range, the cellular gate, Room rows, retry. */
class FetchPartsTest {
    @get:Rule
    val temp = TemporaryFolder()

    private val mib = 1024L * 1024

    @Test
    fun safeNameIsOneHarmlessSegment() {
        assertEquals("report.pdf", FetchFiles.safeName("report.pdf"))
        assertEquals("_.._etc_passwd", FetchFiles.safeName("/../etc/passwd"))
        assertEquals("a_b_c_d", FetchFiles.safeName("a\\b:c\u0000d"))
        assertEquals("hidden", FetchFiles.safeName("..hidden.."))
        assertEquals("file", FetchFiles.safeName(".."))
        assertEquals("file", FetchFiles.safeName("   "))
        assertEquals("Grüße 東京.docx", FetchFiles.safeName("Grüße 東京.docx"))
        val long = FetchFiles.safeName("x".repeat(300) + ".blend")
        assertEquals(120, long.length)
        assertTrue(long.endsWith(".blend"))
    }

    @Test
    fun resolveRefusesPathsOutsideTheFetchedFolder() {
        val files = FetchFiles(temp.root)
        assertEquals(java.io.File(temp.root, "fetched/j3/a.txt").canonicalFile, files.resolve("fetched/j3/a.txt"))
        assertNull(files.resolve("fetched/../secret.db"))
        assertNull(files.resolve("databases/gander.db"))
        assertNull(files.resolve(""))
        assertEquals(3L, FetchFiles.jobIdOf("fetched/j3/a.txt"))
        assertEquals("fetched/j7/a_b.txt", files.finalRelativePath(7, "a/b.txt"))
    }

    @Test
    fun sweepKeepsOnlyKnownJobFolders() {
        val files = FetchFiles(temp.root)
        files.preparePartial(1).writeText("x")
        files.preparePartial(2).writeText("y")
        java.io.File(files.root, "stray").mkdirs()
        assertEquals(2, files.sweep(setOf(1)))
        assertTrue(files.jobDir(1).exists())
    }

    @Test
    fun contentRangeParsing() {
        assertEquals(FileDownloader.ContentRange(10, 99, 100), FileDownloader.parseContentRange("bytes 10-99/100"))
        assertEquals(FileDownloader.ContentRange(0, 0, null), FileDownloader.parseContentRange("bytes 0-0/*"))
        assertNull(FileDownloader.parseContentRange("bytes 10-99/50"))
        assertNull(FileDownloader.parseContentRange("bytes */100"))
        assertNull(FileDownloader.parseContentRange(null))
    }

    private fun t(name: String, size: Long) = EntryTarget(SampleData.HOST_ID, "s", name, SampleData.file(name, size = size))

    @Test
    fun cellularGate() {
        val big = t("big.mov", 60 * mib)
        val small = t("small.txt", 1 * mib)
        val none: (EntryTarget) -> Boolean = { false }
        assertEquals(FetchDecision.Start(listOf(big)), FetchGate.decide(listOf(big), NetworkState.UNMETERED, 50 * mib, none))
        assertEquals(
            FetchDecision.ConfirmCellular(listOf(big), 60 * mib),
            FetchGate.decide(listOf(big), NetworkState.METERED, 50 * mib, none),
        )
        assertEquals(FetchDecision.Start(listOf(small)), FetchGate.decide(listOf(small), NetworkState.METERED, 50 * mib, none))
        // Exactly the threshold warns ("from 50 MiB"), the total of a selection counts.
        val exact = t("exact.bin", 50 * mib)
        assertTrue(FetchGate.decide(listOf(exact), NetworkState.METERED, 50 * mib, none) is FetchDecision.ConfirmCellular)
        val many = (1..60).map { t("f$it", 1 * mib) }
        assertEquals(60 * mib, (FetchGate.decide(many, NetworkState.METERED, 50 * mib, none) as FetchDecision.ConfirmCellular).totalBytes)
        // A current saved copy opens instead; in a selection it is skipped.
        assertEquals(FetchDecision.OpenCached(big), FetchGate.decide(listOf(big), NetworkState.METERED, 50 * mib) { it == big })
        assertEquals(FetchDecision.Start(listOf(small)), FetchGate.decide(listOf(big, small), NetworkState.METERED, 50 * mib) { it == big })
        // Folders are not fetched.
        val dir = EntryTarget(SampleData.HOST_ID, "s", "d", SampleData.dir("d"))
        assertEquals(FetchDecision.NothingToFetch, FetchGate.decide(listOf(dir), NetworkState.UNMETERED, 50 * mib, none))
    }

    @Test
    fun cachedCopyMatchesOnlyTheSameVersion() {
        val entry = SampleData.file("a.pdf", size = 10, mtime = "2026-10-30T09:12:44Z")
        val cached = CachedFile(FileKey("h", "s", "a.pdf"), "a.pdf", null, "fetched/j1/a.pdf", 10, entry.mtime, "\"e\"", lastUsed = 0, fetchedAt = 0)
        assertTrue(cached.matches(entry))
        assertTrue(!cached.matches(entry.copy(size = 11)))
        assertTrue(!cached.matches(entry.copy(mtime = "2026-10-31T00:00:00Z")))
    }

    @Test
    fun verifyPolicyAndAlgorithmChoice() {
        assertTrue(FetchSettings().shouldVerify(50 * mib))
        assertTrue(!FetchSettings().shouldVerify(50 * mib - 1))
        assertTrue(FetchSettings(verify = VerifyPolicy.ALWAYS).shouldVerify(1))
        assertTrue(!FetchSettings(verify = VerifyPolicy.NEVER).shouldVerify(Long.MAX_VALUE))
        assertEquals(HashAlgo.BLAKE3, verifyAlgorithm(null))
        assertEquals(HashAlgo.BLAKE3, verifyAlgorithm(SampleData.me().capabilities))
    }

    @Test
    fun roomRowsRoundTrip() {
        val job = FetchJob(
            id = 5,
            key = FileKey("h", "s", "dir/a.pdf"),
            entry = SampleData.file("a.pdf", size = 99),
            status = FetchStatus.VERIFYING,
            bytesDone = 99,
            totalBytes = 99,
            etag = "\"e\"",
            failure = FetchFailure.VERIFY_MISMATCH,
            failureCode = "blake3",
            restarts = 1,
            changedOnHost = true,
            attempts = 2,
            verifiedWith = HashAlgo.SHA256,
            localPath = "fetched/j5/a.pdf",
            createdAt = 1,
            updatedAt = 2,
        )
        assertEquals(job, FetchRows.toJobOrNull(FetchRows.toEntity(job)))
        assertNull(FetchRows.toJobOrNull(FetchRows.toEntity(job).copy(entryJson = "{not json")))
        assertEquals(FetchStatus.FAILED, FetchRows.toJobOrNull(FetchRows.toEntity(job).copy(status = "from-the-future"))!!.status)
        val cached = CachedFile(job.key, "a.pdf", "application/pdf", job.localPath, 99, "m", "\"e\"", true, 3, 4, HashAlgo.BLAKE3)
        assertEquals(cached, FetchRows.toCached(FetchRows.toEntity(cached)))
    }

    @Test
    fun retryBackoffGrowsAndHonoursRetryAfter() {
        val policy = RetryPolicy()
        val network = GanderError.Network(IOException("reset"))
        assertEquals(1_000, policy.delayMs(1, network))
        assertEquals(8_000, policy.delayMs(4, network))
        assertEquals(30_000, policy.delayMs(10, network))
        val busy = GanderError.Host.TooManyRequests(429, "", 45_000)
        assertEquals(45_000, policy.delayMs(1, busy))
    }
}
