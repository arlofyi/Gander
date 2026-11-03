package fyi.arlo.gander

import android.content.Context
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.test.assert
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.assertIsOff
import androidx.compose.ui.test.assertIsOn
import androidx.compose.ui.test.SemanticsNodeInteraction
import androidx.compose.ui.test.hasScrollAction
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.test.performScrollToNode
import androidx.compose.ui.test.performTextClearance
import androidx.compose.ui.test.performTextInput
import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import androidx.datastore.preferences.core.Preferences
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import fyi.arlo.gander.core.data.actions.DataStoreDefaultActionPrefs
import fyi.arlo.gander.core.data.actions.FileAction
import fyi.arlo.gander.core.data.actions.NetworkState
import fyi.arlo.gander.core.data.diagnostics.DiagnosticsCollector
import fyi.arlo.gander.core.data.diagnostics.DiagnosticsDevice
import fyi.arlo.gander.core.data.fetch.VerifyPolicy
import fyi.arlo.gander.core.data.hosts.InMemoryHostRepository
import fyi.arlo.gander.core.data.settings.AppSettings
import fyi.arlo.gander.core.data.settings.DataStoreSettings
import fyi.arlo.gander.testing.FakeDeviceAuthenticator
import fyi.arlo.gander.testing.SampleData
import fyi.arlo.gander.ui.lock.AuthAvailability
import fyi.arlo.gander.ui.lock.AuthResult
import fyi.arlo.gander.ui.lock.LocalDeviceAuthenticator
import fyi.arlo.gander.ui.settings.SettingsScreen
import fyi.arlo.gander.ui.settings.SettingsTags
import fyi.arlo.gander.ui.settings.SettingsViewModel
import fyi.arlo.gander.ui.theme.GanderTheme
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.runBlocking
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File

/**
 * Settings persistence on the real file-backed DataStore (the JVM tests use an in-memory one): a
 * choice made on the screen is in the settings file, and a screen opened on an existing file shows
 * what was saved. A "new session" is a second DataStore on the same file after the first one's scope
 * is cancelled (two active DataStores on one file are not allowed). Run by WP-29 on the emulator.
 */
@RunWith(AndroidJUnit4::class)
class SettingsScreenTest {
    @get:Rule
    val composeRule = createComposeRule()

    private val context: Context get() = InstrumentationRegistry.getInstrumentation().targetContext
    private fun str(id: Int, vararg args: Any) = context.getString(id, *args)

    private lateinit var file: File
    private var scope = newScope()
    private lateinit var store: DataStore<Preferences>
    private val authenticator = FakeDeviceAuthenticator()

    private fun newScope() = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    private fun openStore(): DataStore<Preferences> = PreferenceDataStoreFactory.create(scope = scope) { file }

    @Before
    fun setUp() {
        file = File(context.cacheDir, "settings-test-${System.nanoTime()}/settings.preferences_pb")
        file.parentFile?.mkdirs()
        store = openStore()
    }

    @After
    fun tearDown() {
        scope.cancel()
        file.parentFile?.deleteRecursively()
    }

    /** Ends this session and starts the next one on the same file, as a restart of the app would. */
    private fun restart() {
        scope.cancel()
        scope = newScope()
        store = openStore()
    }

    private fun repository() = DataStoreSettings(store)

    private fun show() {
        val hosts = InMemoryHostRepository(listOf(SampleData.host(name = "studio-pc")))
        val repository = repository()
        val viewModel = SettingsViewModel(
            settings = repository,
            defaultActions = DataStoreDefaultActionPrefs(store),
            hosts = hosts,
            diagnostics = DiagnosticsCollector(
                device = { DiagnosticsDevice("test", "16", 36, "Google", "Pixel") },
                settings = { repository.current() },
                network = { NetworkState.UNMETERED },
                privateNetworkUp = { null },
                hosts = hosts,
                cachedFiles = { emptyList() },
                jobs = { emptyList() },
                derivedCacheBytes = { 0 },
                freeBytes = { null },
            ),
        )
        composeRule.setContent {
            GanderTheme(dynamicColor = false) {
                CompositionLocalProvider(LocalDeviceAuthenticator provides authenticator) {
                    SettingsScreen(viewModel = viewModel, onBack = {}, onOpenStorage = {}, onOpenHost = {})
                }
            }
        }
    }

    private fun waitForText(text: String) {
        composeRule.waitUntil(timeoutMillis = 5_000) { composeRule.onAllNodesWithText(text).fetchSemanticsNodes().isNotEmpty() }
    }

    /** Scrolls the lazy list until [tag] is composed (it may be off screen and not yet exist), then to it. */
    private fun scrollTo(tag: String): SemanticsNodeInteraction {
        composeRule.onNode(hasScrollAction()).performScrollToNode(hasTestTag(tag))
        return composeRule.onNodeWithTag(tag).performScrollTo()
    }

    private fun choose(rowTag: String, choiceIndex: Int) {
        scrollTo(rowTag).performClick()
        composeRule.onNodeWithTag(SettingsTags.choice(choiceIndex)).performClick()
    }

    @Test
    fun theScreenShowsTheDefaultsOfTheSpec() {
        show()
        waitForText(str(R.string.settings_cellular_from, "50 MB"))
        // the cellular row reads the same words ("Files of 50 MB or more"), so check the verify row itself
        composeRule.onNodeWithTag(SettingsTags.VERIFY)
            .assertIsDisplayed()
            .assert(hasText(str(R.string.settings_verify_large, "50 MB")))
        scrollTo(SettingsTags.CACHE_CAP)
        composeRule.onNodeWithText("2 GB").assertIsDisplayed()
        // the lock is off, so its timeout is not editable
        scrollTo(SettingsTags.LOCK_MINUTES).assertIsNotEnabled()
    }

    @Test
    fun choicesMadeOnTheScreenAreWrittenToTheSettingsFile() {
        show()
        waitForText(str(R.string.settings_cellular_from, "50 MB"))

        choose(SettingsTags.CELLULAR, AppSettings.CELLULAR_CHOICES.indexOf(100L * 1024 * 1024))
        waitForText(str(R.string.settings_cellular_from, "100 MB"))
        choose(SettingsTags.VERIFY, VerifyPolicy.entries.indexOf(VerifyPolicy.ALWAYS))
        waitForText(str(R.string.settings_verify_always))
        choose(SettingsTags.CACHE_CAP, AppSettings.CACHE_CAP_CHOICES.indexOf(5L * 1024 * 1024 * 1024))
        waitForText("5 GB")

        // in the settings file, read by a brand-new DataStore after a "restart"
        restart()
        val saved = runBlocking { repository().current() }
        assertEquals(100L * 1024 * 1024, saved.cellularWarnBytes)
        assertEquals(VerifyPolicy.ALWAYS, saved.verify)
        assertEquals(5L * 1024 * 1024 * 1024, saved.cacheCapBytes)
    }

    @Test
    fun aScreenOpenedOnAnExistingFileShowsWhatWasSaved() {
        runBlocking {
            val before = repository()
            before.setVerify(VerifyPolicy.NEVER)
            before.setCellularWarnBytes(AppSettings.CELLULAR_NEVER)
            before.setCacheCapBytes(10L * 1024 * 1024 * 1024)
            before.setAppLockMinutes(15)
        }
        restart()
        show()

        waitForText(str(R.string.settings_cellular_never))
        composeRule.onNodeWithText(str(R.string.settings_verify_never)).assertIsDisplayed()
        scrollTo(SettingsTags.CACHE_CAP)
        composeRule.onNodeWithText("10 GB").assertIsDisplayed()
        scrollTo(SettingsTags.LOCK_MINUTES)
        composeRule.onNodeWithText("After 15 minutes away").assertIsDisplayed()
    }

    @Test
    fun streamFirstTypesAreEditedAndSaved() {
        show()
        waitForText(str(R.string.settings_cellular_from, "50 MB"))

        scrollTo(SettingsTags.STREAM_TYPES).performClick()
        composeRule.onNodeWithTag(SettingsTags.STREAM_TYPES_FIELD).performTextClearance()
        composeRule.onNodeWithTag(SettingsTags.STREAM_TYPES_FIELD).performTextInput("psd, .BLEND")
        composeRule.onNodeWithTag(SettingsTags.STREAM_TYPES_SAVE).performClick()

        waitForText("2 file types")
        restart()
        assertEquals(setOf("psd", "blend"), runBlocking { repository().current().streamOnlyTypes })
    }

    @Test
    fun rememberedDefaultActionsAreListedAndCanBeForgotten() {
        runBlocking { DataStoreDefaultActionPrefs(store).set("pdf", FileAction.PREVIEW) }
        show()

        composeRule.waitUntil(5_000) {
            composeRule.onAllNodesWithText(".pdf").fetchSemanticsNodes().isNotEmpty()
        }
        scrollTo(SettingsTags.action("pdf")).assertIsDisplayed()
        composeRule.onNodeWithContentDescription(str(R.string.settings_remembered_reset, "pdf")).performClick()
        composeRule.waitUntil(5_000) { composeRule.onAllNodesWithText(".pdf").fetchSemanticsNodes().isEmpty() }
        assertTrue(runBlocking { DataStoreDefaultActionPrefs(store).get("pdf") } == null)
    }

    @Test
    fun appLockIsOnlySavedAfterTheDeviceCredentialSaysYes() {
        show()
        waitForText(str(R.string.settings_cellular_from, "50 MB"))

        authenticator.autoAnswer = AuthResult.Cancelled
        scrollTo(SettingsTags.LOCK).performClick()
        composeRule.waitForIdle()
        assertEquals(1, authenticator.prompts)
        assertEquals(str(R.string.settings_lock_prompt_enable), authenticator.lastTitle)
        assertFalse("cancelled prompt: the lock stays off", runBlocking { repository().current().appLockEnabled })

        authenticator.autoAnswer = AuthResult.Success
        scrollTo(SettingsTags.LOCK).performClick()
        composeRule.waitUntil(5_000) { runBlocking { repository().current().appLockEnabled } }

        // turning it off asks again
        scrollTo(SettingsTags.LOCK).performClick()
        composeRule.waitUntil(5_000) { !runBlocking { repository().current().appLockEnabled } }
        assertEquals(str(R.string.settings_lock_prompt_disable), authenticator.lastTitle)
    }

    @Test
    fun appLockCannotBeTurnedOnWithoutAScreenLockButCanAlwaysBeTurnedOff() {
        runBlocking { repository().setAppLockEnabled(true) }
        show()
        waitForText(str(R.string.settings_cellular_from, "50 MB"))

        // the phone lost its screen lock since: turning the lock off needs no prompt
        authenticator.availability = AuthAvailability.NOT_SET_UP
        scrollTo(SettingsTags.LOCK).performClick()
        composeRule.waitUntil(5_000) { !runBlocking { repository().current().appLockEnabled } }
        assertEquals(0, authenticator.prompts)

        // turning it on explains what is missing
        scrollTo(SettingsTags.LOCK).performClick()
        composeRule.onNodeWithTag(SettingsTags.LOCK_SETUP_DIALOG).assertIsDisplayed()
        assertFalse(runBlocking { repository().current().appLockEnabled })
    }

    @Test
    fun showingComputersInTheFilesAppIsOnByDefaultAndCanBeTurnedOff() {
        show()
        scrollTo(SettingsTags.FILES_APP).assertIsOn()
        assertTrue(runBlocking { repository().current().filesAppEnabled })

        scrollTo(SettingsTags.FILES_APP).performClick()
        composeRule.waitUntil(5_000) { !runBlocking { repository().current().filesAppEnabled } }
        scrollTo(SettingsTags.FILES_APP).assertIsOff()

        restart()
        assertFalse(runBlocking { repository().current().filesAppEnabled })
    }
}
