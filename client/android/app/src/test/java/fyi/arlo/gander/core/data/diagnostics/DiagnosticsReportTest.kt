package fyi.arlo.gander.core.data.diagnostics

import fyi.arlo.gander.core.data.actions.NetworkState
import fyi.arlo.gander.core.data.fetch.CachedFile
import fyi.arlo.gander.core.data.fetch.FetchFailure
import fyi.arlo.gander.core.data.fetch.FetchJob
import fyi.arlo.gander.core.data.fetch.FetchStatus
import fyi.arlo.gander.core.data.fetch.FileKey
import fyi.arlo.gander.core.data.hosts.InMemoryHostRepository
import fyi.arlo.gander.core.data.hosts.PairedHost
import fyi.arlo.gander.core.data.settings.AppSettings
import fyi.arlo.gander.core.net.BearerToken
import fyi.arlo.gander.core.net.protocol.HostOs
import fyi.arlo.gander.testing.SampleData
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** The export as a whole: a snapshot full of planted secrets goes in, a report without them comes out. */
class DiagnosticsReportTest {
    // Planted values (made up).
    private val token = "plantedTokenCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC"
    private val hostName = "Alice Studio Workstation"
    private val deviceName = "Alice's Pixel"
    private val secretFile = "Q3 salary review.xlsx"
    private val secretPath = "HR/Confidential/Q3 salary review.xlsx"
    private val jobFile = "Tax return 2025.pdf"
    private val salt = ByteArray(16) { (it * 3).toByte() }

    private val host: PairedHost = SampleData.host(name = hostName, pairedAs = deviceName).copy(
        addresses = listOf("100.101.102.103", "fd7a:115c:a1e0::beef", "alice-studio.tail1234.ts.net"),
        lastSeenAtMillis = 1_800_000_000_000L,
    )

    private fun cached(name: String, path: String, pinned: Boolean = false) = CachedFile(
        key = FileKey(host.id, "shr_hr", path),
        name = name,
        mime = null,
        localPath = "fetched/j4/$name",
        size = 4096,
        mtime = "2026-11-01T00:00:00Z",
        etag = "\"e\"",
        pinned = pinned,
        lastUsed = 1_800_000_100_000L,
        fetchedAt = 1_800_000_000_000L,
    )

    private fun job(name: String, status: FetchStatus, failure: FetchFailure? = null, code: String? = null) = FetchJob(
        id = 7,
        key = FileKey(host.id, "shr_hr", "Finance/$name"),
        entry = SampleData.file(name, size = 2048),
        status = status,
        bytesDone = 1024,
        failure = failure,
        failureCode = code,
        restarts = 1,
        attempts = 2,
        updatedAt = 1_800_000_200_000L,
    )

    private fun snapshot(jobs: List<FetchJob> = emptyList()) = DiagnosticsSnapshot(
        generatedAtMillis = 1_800_000_300_000L,
        device = DiagnosticsDevice("1.2.3", "16", 36, "Google", "Pixel 9"),
        settings = AppSettings(appLockEnabled = true, appLockMinutes = 5),
        network = NetworkState.METERED,
        privateNetworkUp = true,
        hosts = listOf(host),
        cachedFiles = listOf(cached(secretFile, secretPath, pinned = true)),
        jobs = jobs.ifEmpty { listOf(job(jobFile, FetchStatus.FAILED, FetchFailure.NOT_FOUND, "not_found")) },
        derivedCacheBytes = 12_345,
        freeBytes = 9_999_999,
        secrets = listOf(token),
    )

    private fun render(snapshot: DiagnosticsSnapshot = snapshot()) =
        DiagnosticsReport.render(snapshot, DiagnosticsReport.redactorFor(snapshot, salt))

    private fun assertNoneOf(text: String, vararg forbidden: String) {
        for (value in forbidden) assertFalse("'$value' leaked into:\n$text", text.contains(value, ignoreCase = true))
    }

    @Test
    fun noPlantedSecretReachesTheReport() {
        val report = render()
        assertNoneOf(
            report,
            token,
            "100.101.102.103", "102.103", "a1e0", "beef", "tail1234", "alice-studio",
            hostName, "Alice", deviceName,
            secretFile, "salary", "Confidential", "shr_hr", "HR/",
            jobFile, "Tax return", "Finance",
        )
    }

    @Test
    fun theReportKeepsWhatSupportNeeds() {
        val report = render()
        // public identity and masked addresses
        assertTrue(report, report.contains("id=${host.pin.hostId}"))
        assertTrue(report, report.contains("100.101.*.*"))
        assertTrue(report, report.contains("fd7a:115c:*"))
        assertTrue(report, report.contains("*.ts.net"))
        assertTrue(report, report.contains("os=windows"))
        assertTrue(report, report.contains("port=${host.port}"))
        // app, settings and network
        assertTrue(report, report.contains("version: 1.2.3"))
        assertTrue(report, report.contains("android: 16 (api 36)"))
        assertTrue(report, report.contains("app_lock: on, after 5 min"))
        assertTrue(report, report.contains("metered: true"))
        // storage and downloads: sizes, states, extensions and error codes, not names
        assertTrue(report, report.contains("kept_offline: 1 (4096 bytes)"))
        assertTrue(report, report.contains("ext=xlsx"))
        assertTrue(report, report.contains("ext=pdf"))
        assertTrue(report, report.contains("status=failed"))
        assertTrue(report, report.contains("failure=not_found code=not_found"))
        assertTrue(report, report.contains("bytes=1024/2048"))
        assertTrue(report, report.contains("by_status: failed=1"))
    }

    @Test
    fun theSameFileHasTheSameTagEverywhereInOneReport() {
        val sameName = "Budget.xlsx"
        val snap = snapshot(listOf(job(sameName, FetchStatus.DONE))).copy(cachedFiles = listOf(cached(sameName, "x/$sameName")))
        val report = render(snap)
        val redactor = DiagnosticsReport.redactorFor(snap, salt)
        val tag = redactor.tag("file", sameName)
        assertEquals(report, 2, Regex(Regex.escape(tag)).findAll(report).count())
        assertNoneOf(report, sameName.substringBefore('.'))
    }

    @Test
    fun hostsAreTaggedSoLinesAboutTheSameComputerMatch() {
        val report = render()
        val tag = DiagnosticsReport.redactorFor(snapshot(), salt).tag("host", hostName)
        // the computer line, the cached file line and the job line
        assertEquals(report, 3, Regex(Regex.escape(tag)).findAll(report).count())
    }

    @Test
    fun aSecretThatSlipsIntoAStructuredFieldIsStillCaughtByTheFinalPass() {
        val leaky = job(
            name = "plain.txt",
            status = FetchStatus.FAILED,
            failure = FetchFailure.NETWORK,
            code = "boom Bearer $token via https://100.64.7.9:42637/v1/shares/shr_hr/file?path=C:\\Users\\bob\\x.txt for bob@example.com",
        )
        val report = render(snapshot(listOf(leaky)))
        assertNoneOf(report, token, "100.64.7.9", "shr_hr", "bob", "example.com", "Users")
    }

    @Test
    fun aForgottenComputersRowsStillRenderWithoutItsName() {
        val snap = snapshot().copy(hosts = emptyList())
        val report = render(snap)
        assertTrue(report, report.contains("[computers] 0"))
        assertNoneOf(report, hostName, "forgotten-")
    }

    @Test
    fun aHugeListIsCutOff() {
        val many = (1..300).map { cached("file-$it.bin", "dir/file-$it.bin") }
        val snap = snapshot().copy(cachedFiles = many)
        val report = render(snap)
        assertEquals(DiagnosticsReport.MAX_FILE_LINES, report.lines().count { it.startsWith("file file#") })
        assertTrue(report, report.contains("... 100 more"))
    }

    @Test
    fun tagsDifferBetweenExports() {
        val one = DiagnosticsReport.render(snapshot(), DiagnosticsReport.redactorFor(snapshot(), ByteArray(16) { 1 }))
        val two = DiagnosticsReport.render(snapshot(), DiagnosticsReport.redactorFor(snapshot(), ByteArray(16) { 2 }))
        assertTrue(one != two)
    }

    @Test
    fun theCollectorGathersTheStoresAndUsesTheRealTokensAsSecrets() = runBlocking {
        val hosts = InMemoryHostRepository(listOf(host))
        hosts.seedToken(host.id, BearerToken(token))
        val collector = DiagnosticsCollector(
            device = { DiagnosticsDevice("1.2.3", "16", 36, "Google", "Pixel 9") },
            settings = { AppSettings() },
            network = { NetworkState.UNMETERED },
            privateNetworkUp = { null },
            hosts = hosts,
            cachedFiles = { listOf(cached(secretFile, secretPath)) },
            jobs = { emptyList() },
            derivedCacheBytes = { 0 },
            freeBytes = { null },
            clock = { 1_800_000_000_000L },
        )
        val snap = collector.collect()
        assertEquals(listOf(token), snap.secrets)
        assertEquals(1, snap.hosts.size)
        val report = collector.report()
        assertNoneOf(report, token, hostName, secretFile, "100.101.102.103")
        assertTrue(report, report.contains("private_network_up: unknown"))
        assertTrue(report, report.contains("free_bytes: unknown"))
    }

    @Test
    fun theSampleHostIsWindowsForTheFixtureToMeanWhatItSays() {
        assertEquals(HostOs.WINDOWS, host.os)
    }
}
