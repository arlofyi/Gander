package fyi.arlo.gander.core.data.settings

import androidx.datastore.preferences.core.edit
import fyi.arlo.gander.core.data.actions.StreamOnlyTypes
import fyi.arlo.gander.core.data.fetch.VerifyPolicy
import fyi.arlo.gander.testing.InMemoryPreferencesDataStore
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Settings persistence: the keys, the defaults of the client spec, tolerance of damaged values, and
 * the mapping into what the fetch engine and the file sheet read. The file-backed DataStore itself
 * is covered by the instrumented `SettingsScreenTest` (see [InMemoryPreferencesDataStore]).
 */
class SettingsRepositoryTest {
    private val mib = 1024L * 1024
    private val gib = 1024 * mib
    private val store = InMemoryPreferencesDataStore()
    private val repo = DataStoreSettings(store)

    @Test
    fun defaultsAreTheSpecs() = runBlocking {
        val s = repo.current()
        assertEquals(VerifyPolicy.LARGE_FILES, s.verify)
        assertEquals(50 * mib, s.cellularWarnBytes)
        assertEquals(2 * gib, s.cacheCapBytes)
        assertEquals(StreamOnlyTypes.DEFAULT, s.streamOnlyTypes)
        assertFalse(s.appLockEnabled)
        assertEquals(1, s.appLockMinutes)
        assertTrue("computers show in the Files app by default", s.filesAppEnabled)
        assertEquals(AppSettings(), s)
    }

    @Test
    fun everySettingIsSavedAndReadBack() = runBlocking {
        repo.setVerify(VerifyPolicy.ALWAYS)
        repo.setCellularWarnBytes(10 * mib)
        repo.setCacheCapBytes(5 * gib)
        repo.setStreamOnlyTypes(setOf("psd", "blend"))
        repo.setAppLockEnabled(true)
        repo.setAppLockMinutes(15)
        repo.setFilesAppEnabled(false)

        val s = repo.current()
        assertEquals(VerifyPolicy.ALWAYS, s.verify)
        assertEquals(10 * mib, s.cellularWarnBytes)
        assertEquals(5 * gib, s.cacheCapBytes)
        assertEquals(setOf("psd", "blend"), s.streamOnlyTypes)
        assertTrue(s.appLockEnabled)
        assertEquals(15, s.appLockMinutes)
        assertFalse(s.filesAppEnabled)
    }

    @Test
    fun aNewRepositoryOnTheSameStoreSeesWhatWasSaved() = runBlocking {
        repo.setVerify(VerifyPolicy.NEVER)
        repo.setCacheCapBytes(gib)
        repo.setAppLockEnabled(true)

        // a new session of the app reads the same stored values
        val later = DataStoreSettings(store).current()
        assertEquals(VerifyPolicy.NEVER, later.verify)
        assertEquals(gib, later.cacheCapBytes)
        assertTrue(later.appLockEnabled)
    }

    @Test
    fun settingsAndOtherFeaturesShareTheStoreWithoutTouchingEachOther() = runBlocking {
        // the browser's and the default actions' keys live in the same file
        store.edit { it[DataStoreBrowserKey] = "mtime|desc|grid" }
        repo.setCacheCapBytes(gib)
        assertEquals("mtime|desc|grid", store.data.first()[DataStoreBrowserKey])
        repo.setStreamOnlyTypes(null)
        assertEquals("mtime|desc|grid", store.data.first()[DataStoreBrowserKey])
    }

    @Test
    fun theCacheCapHasAFloorAndTheCellularThresholdCannotGoNegative() = runBlocking {
        repo.setCacheCapBytes(1)
        assertEquals(AppSettings.MIN_CACHE_CAP_BYTES, repo.current().cacheCapBytes)
        repo.setCellularWarnBytes(-5)
        assertEquals(0L, repo.current().cellularWarnBytes)
        repo.setAppLockMinutes(-3)
        assertEquals(0, repo.current().appLockMinutes)
        repo.setAppLockMinutes(Int.MAX_VALUE)
        assertEquals(AppSettings.MAX_LOCK_MINUTES, repo.current().appLockMinutes)
    }

    @Test
    fun neverAskIsAThresholdNoFileReaches() = runBlocking {
        repo.setCellularWarnBytes(AppSettings.CELLULAR_NEVER)
        val s = repo.current()
        assertEquals(Long.MAX_VALUE, s.cellularWarnBytes)
        assertEquals(Long.MAX_VALUE, s.toFetchSettings().cellularWarnBytes)
        assertEquals(Long.MAX_VALUE, s.toRuleConfig().cellularWarnBytes)
    }

    @Test
    fun damagedOrUnknownValuesReadAsDefaults() = runBlocking {
        store.edit {
            it[DataStoreSettings.VERIFY] = "from-a-newer-app"
            it[DataStoreSettings.CELLULAR_WARN] = -1L
            it[DataStoreSettings.CACHE_CAP] = 5L
            it[DataStoreSettings.LOCK_MINUTES] = -7
        }
        assertEquals(AppSettings(), repo.current())
    }

    @Test
    fun streamOnlyTypesCanBeEditedEmptiedAndResetToTheShippedList() = runBlocking {
        repo.setStreamOnlyTypes(setOf("xyz"))
        assertEquals(setOf("xyz"), repo.current().streamOnlyTypes)
        repo.setStreamOnlyTypes(emptySet())
        assertEquals(emptySet<String>(), repo.current().streamOnlyTypes)
        repo.setStreamOnlyTypes(null)
        assertEquals(StreamOnlyTypes.DEFAULT, repo.current().streamOnlyTypes)
    }

    @Test
    fun theFetchEngineAndTheFileSheetReadTheSameThresholds() = runBlocking {
        repo.setVerify(VerifyPolicy.NEVER)
        repo.setCellularWarnBytes(100 * mib)
        repo.setStreamOnlyTypes(setOf("abc"))
        val s = repo.current()

        val fetch = SettingsFetchSource(repo).current()
        assertEquals(VerifyPolicy.NEVER, fetch.verify)
        assertEquals(100 * mib, fetch.cellularWarnBytes)
        assertFalse("never verifies, whatever the size", fetch.shouldVerify(10 * gib))

        val rules = s.toRuleConfig()
        assertEquals(100 * mib, rules.cellularWarnBytes)
        assertEquals(setOf("abc"), rules.streamOnlyTypes)
        // the metered handler threshold is not a user setting
        assertEquals(25 * mib, rules.meteredHandlerFetchBytes)
    }

    @Test
    fun lockConfigCarriesTheTimeoutInMilliseconds() {
        assertEquals(60_000L, AppSettings().lock.timeoutMillis)
        assertEquals(0L, LockConfig(enabled = true, minutes = 0).timeoutMillis)
        assertEquals(15 * 60_000L, LockConfig(enabled = true, minutes = 15).timeoutMillis)
    }

    @Test
    fun theOfferedChoicesIncludeTheDefaults() {
        assertTrue(AppSettings().cellularWarnBytes in AppSettings.CELLULAR_CHOICES)
        assertTrue(AppSettings().cacheCapBytes in AppSettings.CACHE_CAP_CHOICES)
        assertTrue(AppSettings().appLockMinutes in AppSettings.LOCK_MINUTES_CHOICES)
    }

    @Test
    fun theInMemoryFakeBehavesLikeTheDataStoreOne() = runBlocking {
        val fake = InMemorySettings()
        fake.setCacheCapBytes(1)
        fake.setStreamOnlyTypes(setOf("a1"))
        fake.setAppLockEnabled(true)
        assertEquals(AppSettings.MIN_CACHE_CAP_BYTES, fake.current().cacheCapBytes)
        assertEquals(setOf("a1"), fake.current().streamOnlyTypes)
        assertTrue(fake.current().appLockEnabled)
        fake.setStreamOnlyTypes(null)
        assertEquals(StreamOnlyTypes.DEFAULT, fake.current().streamOnlyTypes)
    }

    @Test
    fun extensionsAreParsedFromWhatTheUserTypes() {
        assertEquals(setOf("psd", "ai", "blend", "kicad_pcb"), ExtensionList.parse("PSD, .ai  *.blend;kicad_pcb"))
        assertEquals(emptySet<String>(), ExtensionList.parse("  ,, "))
        // not plausible extensions are dropped, never half-kept
        assertEquals(setOf("ok"), ExtensionList.parse("ok ../etc a/b \"x\" ${"y".repeat(40)}"))
        assertEquals("ai blend psd", ExtensionList.format(setOf("psd", "blend", "ai")))
    }

    private companion object {
        val DataStoreBrowserKey = androidx.datastore.preferences.core.stringPreferencesKey("browser.h.s")
    }
}
