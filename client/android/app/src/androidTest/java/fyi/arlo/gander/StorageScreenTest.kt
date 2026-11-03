package fyi.arlo.gander

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import fyi.arlo.gander.core.data.fetch.CachedFile
import fyi.arlo.gander.core.data.fetch.FileKey
import fyi.arlo.gander.testing.SampleData
import fyi.arlo.gander.ui.settings.HostStorage
import fyi.arlo.gander.ui.settings.StorageContent
import fyi.arlo.gander.ui.settings.StorageTags
import fyi.arlo.gander.ui.settings.StorageUiState
import fyi.arlo.gander.ui.theme.GanderTheme
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/** The Storage screen with fixed state: usage, per-computer files, Keep offline, delete, clear (run by WP-29). */
@RunWith(AndroidJUnit4::class)
class StorageScreenTest {
    @get:Rule
    val composeRule = createComposeRule()

    private fun str(id: Int, vararg args: Any) =
        InstrumentationRegistry.getInstrumentation().targetContext.getString(id, *args)

    private val mib = 1024L * 1024
    private val calls = mutableListOf<String>()

    private fun file(name: String, size: Long, pinned: Boolean = false) = CachedFile(
        key = FileKey(SampleData.HOST_ID, "shr_projects", name),
        name = name,
        mime = null,
        localPath = "fetched/j1/$name",
        size = size,
        mtime = "2026-11-01T00:00:00Z",
        etag = null,
        pinned = pinned,
        lastUsed = System.currentTimeMillis() - 3_600_000,
        fetchedAt = System.currentTimeMillis() - 7_200_000,
    )

    private val state = StorageUiState(
        loaded = true,
        capBytes = 2048 * mib,
        usedBytes = 300 * mib,
        pinnedBytes = 100 * mib,
        derivedBytes = 5 * mib,
        hosts = listOf(
            HostStorage(
                hostId = SampleData.HOST_ID,
                hostName = "studio-pc",
                files = listOf(file("render.mp4", 200 * mib), file("brief.pdf", 100 * mib, pinned = true)),
            ),
        ),
    )

    private fun show(state: StorageUiState, withDownloads: Boolean = true) {
        composeRule.setContent {
            GanderTheme(dynamicColor = false) {
                StorageContent(
                    state = state,
                    onBack = { calls += "back" },
                    onOpenFile = { calls += "open:${it.name}" },
                    onShowDownloads = if (withDownloads) ({ calls += "downloads" }) else null,
                    onKeepOffline = { file, keep -> calls += "keep:${file.name}:$keep" },
                    onRemove = { calls += "remove:${it.name}" },
                    onClearUnpinned = { calls += "clear:${it ?: "all"}" },
                    onClearDerived = { calls += "clear-derived" },
                )
            }
        }
    }

    @Test
    fun theSummaryShowsUsageAgainstTheCapAndWhatIsKeptOffline() {
        show(state)
        composeRule.onNodeWithText(str(R.string.storage_used, "300 MB", "2 GB")).assertIsDisplayed()
        composeRule.onNodeWithText(str(R.string.storage_kept, "100 MB")).assertIsDisplayed()
        composeRule.onNodeWithText("studio-pc").assertIsDisplayed()
    }

    @Test
    fun filesOpenAndKeepOfflineAndDeleteReportTheRightFile() {
        show(state)
        composeRule.onNodeWithTag(StorageTags.file("render.mp4")).performClick()
        composeRule.onNodeWithTag(StorageTags.keep("render.mp4")).performClick()
        composeRule.onNodeWithTag(StorageTags.remove("brief.pdf")).performClick()
        assertEquals(listOf("open:render.mp4", "keep:render.mp4:true", "remove:brief.pdf"), calls)
    }

    @Test
    fun clearingUnkeptFilesAsksFirstAndSaysHowMuchGoes() {
        show(state)
        composeRule.onNodeWithTag(StorageTags.CLEAR_UNPINNED).performClick()
        // one unpinned file of 200 MB: render.mp4
        composeRule.onNodeWithText("This deletes 1 file (200 MB) that you have not chosen to keep offline.").assertIsDisplayed()
        assertTrue("nothing is deleted before the answer", calls.isEmpty())
        composeRule.onNodeWithTag(StorageTags.CLEAR_UNPINNED_CONFIRM).performClick()
        assertEquals(listOf("clear:all"), calls)
    }

    @Test
    fun theDownloadsRowIsThePermanentWayToTheFetchList() {
        show(state)
        composeRule.onNodeWithTag(StorageTags.DOWNLOADS).performClick()
        assertEquals(listOf("downloads"), calls)
    }

    @Test
    fun theDownloadsRowIsHiddenWhenNothingHandlesIt() {
        show(state, withDownloads = false)
        composeRule.onNodeWithTag(StorageTags.DOWNLOADS).assertDoesNotExist()
    }

    @Test
    fun thumbnailsAndPreviewsCanBeCleared() {
        show(state)
        composeRule.onNodeWithTag(StorageTags.CLEAR_DERIVED).performClick()
        assertEquals(listOf("clear-derived"), calls)
    }

    @Test
    fun anEmptyCacheExplainsWhatSavedFilesAre() {
        show(StorageUiState(loaded = true))
        composeRule.onNodeWithTag(StorageTags.EMPTY).assertIsDisplayed()
        composeRule.onNodeWithTag(StorageTags.CLEAR_UNPINNED).assertIsNotEnabled()
        composeRule.onNodeWithTag(StorageTags.CLEAR_DERIVED).assertIsNotEnabled()
    }

    @Test
    fun keptFilesThatAloneExceedTheLimitAreFlagged() {
        show(state.copy(capBytes = 50 * mib))
        composeRule.onNodeWithText(str(R.string.storage_kept_over_cap)).assertIsDisplayed()
    }
}
