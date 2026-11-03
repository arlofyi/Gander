package fyi.arlo.gander

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.hasAnyAncestor
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollToIndex
import androidx.compose.ui.test.performTextInput
import androidx.compose.ui.test.performTouchInput
import androidx.compose.ui.test.longClick
import androidx.lifecycle.SavedStateHandle
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import fyi.arlo.gander.core.data.UiError
import fyi.arlo.gander.core.data.UiErrorKind
import fyi.arlo.gander.core.data.browse.BrowseRepository
import fyi.arlo.gander.core.data.browse.EntryTarget
import fyi.arlo.gander.core.data.browse.InMemoryBrowserPrefs
import fyi.arlo.gander.core.data.browse.SortField
import fyi.arlo.gander.core.data.browse.SortOrder
import fyi.arlo.gander.core.data.browse.ViewMode
import fyi.arlo.gander.core.data.hosts.InMemoryHostRepository
import fyi.arlo.gander.core.net.protocol.EntryFlag
import fyi.arlo.gander.core.net.protocol.SearchCapability
import fyi.arlo.gander.testing.FakeBrowseRepository
import fyi.arlo.gander.testing.SampleData
import fyi.arlo.gander.testing.SampleData.dir
import fyi.arlo.gander.testing.SampleData.file
import fyi.arlo.gander.ui.browser.BrowserActions
import fyi.arlo.gander.ui.browser.BrowserContent
import fyi.arlo.gander.ui.browser.BrowserScreen
import fyi.arlo.gander.ui.browser.BrowserTab
import fyi.arlo.gander.ui.browser.BrowserTags
import fyi.arlo.gander.ui.browser.BrowserUiState
import fyi.arlo.gander.ui.browser.BrowserViewModel
import fyi.arlo.gander.ui.browser.ListingState
import fyi.arlo.gander.ui.theme.GanderTheme
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/**
 * List rendering with a fake repository (WP-23 verify). These run on the emulator in WP-29; the
 * instrumented listing against the host's fixture tree needs the host Browse API (WP-14) and is
 * part of that work package, not this one.
 */
@RunWith(AndroidJUnit4::class)
class BrowserScreenTest {
    @get:Rule
    val composeRule = createComposeRule()

    private val host = SampleData.host()
    private val share = "shr_projects"
    private val fake = FakeBrowseRepository()
    private val prefs = InMemoryBrowserPrefs()
    private val opened = mutableListOf<EntryTarget>()
    private val fetched = mutableListOf<EntryTarget>()

    private fun str(id: Int, vararg args: Any) =
        InstrumentationRegistry.getInstrumentation().targetContext.getString(id, *args)

    private fun plural(id: Int, quantity: Int) =
        InstrumentationRegistry.getInstrumentation().targetContext.resources.getQuantityString(id, quantity, quantity)

    private fun seed() {
        fake.folders[""] = listOf(
            dir("Clients"),
            file("Deck.pptx", size = 48_213_001, flags = listOf(EntryFlag.CLOUD_ONLY)),
            file("notes.md", size = 2048),
            file("shortcut", flags = listOf(EntryFlag.LINK)),
        )
        fake.folders["Clients"] = listOf(dir("Acme"), file("contract.pdf"))
        fake.folders["Clients/Acme"] = listOf(file("Invoice 7.pdf"))
    }

    private fun launch(repository: BrowseRepository = fake): BrowserViewModel {
        val viewModel = BrowserViewModel(host.id, share, InMemoryHostRepository(listOf(host)), repository, prefs, SavedStateHandle())
        composeRule.setContent {
            GanderTheme(dynamicColor = false) {
                BrowserScreen(
                    viewModel = viewModel,
                    onNavigateUp = {},
                    onOpenFile = { opened += it },
                    onFetchSelected = { fetched += it },
                )
            }
        }
        return viewModel
    }

    private fun awaitText(text: String, substring: Boolean = false) {
        composeRule.waitUntil(timeoutMillis = 5_000) {
            composeRule.onAllNodes(hasText(text, substring = substring)).fetchSemanticsNodes().isNotEmpty()
        }
    }

    // List rendering ----------------------------------------------------------------------------

    @Test
    fun theListShowsNamesSizesDatesAndFlags() {
        seed()
        launch()
        awaitText("Deck.pptx")
        composeRule.onNodeWithText("Clients").assertIsDisplayed()
        composeRule.onNodeWithText("notes.md").assertIsDisplayed()
        composeRule.onNodeWithText("46 MB", substring = true).assertIsDisplayed()
        composeRule.onNodeWithText("2 KB", substring = true).assertIsDisplayed()
        // the cloud-only entry carries its flag, the link is shown too (greyed)
        composeRule.onNodeWithContentDescription(str(R.string.entry_flag_cloud)).assertIsDisplayed()
        composeRule.onNodeWithContentDescription(str(R.string.entry_flag_link)).assertIsDisplayed()
        composeRule.onNodeWithTag(BrowserTags.LIST).assertIsDisplayed()
    }

    @Test
    fun theGridShowsTheSameEntriesAndTheChoiceIsRemembered() {
        seed()
        val viewModel = launch()
        awaitText("Deck.pptx")
        composeRule.onNodeWithContentDescription(str(R.string.browser_view_grid)).performClick()
        composeRule.onNodeWithTag(BrowserTags.GRID).assertIsDisplayed()
        composeRule.onNodeWithTag(BrowserTags.LIST).assertDoesNotExist()
        composeRule.onNodeWithText("notes.md").assertIsDisplayed()
        composeRule.waitUntil(5_000) { runBlocking { prefs.get(host.id, share).viewMode } == ViewMode.GRID }
        assertEquals(ViewMode.GRID, viewModel.uiState.value.viewMode)
        // and the button now offers the way back
        composeRule.onNodeWithContentDescription(str(R.string.browser_view_list)).assertIsDisplayed()
    }

    @Test
    fun anEmptyFolderSaysSo() {
        fake.folders[""] = emptyList()
        launch()
        awaitText(str(R.string.browser_empty_folder))
    }

    // Navigation --------------------------------------------------------------------------------

    @Test
    fun foldersOpenInPlaceWithABreadcrumbBackToTheRoot() {
        seed()
        launch()
        awaitText("Clients")
        composeRule.onNodeWithText("Clients").performClick()
        awaitText("contract.pdf")
        composeRule.onNodeWithText("Deck.pptx").assertDoesNotExist()
        composeRule.onNode(hasText("Clients") and hasAnyAncestor(hasTestTag(BrowserTags.BREADCRUMB))).assertIsDisplayed()

        composeRule.onNodeWithText("Acme").performClick()
        awaitText("Invoice 7.pdf")

        // a tap on the root crumb jumps straight back
        composeRule.onNode(hasText("Projects") and hasAnyAncestor(hasTestTag(BrowserTags.BREADCRUMB))).performClick()
        awaitText("Deck.pptx")
    }

    @Test
    fun tappingAFileHandsItOverWithItsFullPath() {
        seed()
        launch()
        awaitText("Clients")
        composeRule.onNodeWithText("Clients").performClick()
        awaitText("contract.pdf")
        composeRule.onNodeWithText("contract.pdf").performClick()
        composeRule.runOnIdle {
            assertEquals(listOf("Clients/contract.pdf"), opened.map { it.path })
            assertEquals(share, opened.single().shareId)
        }
    }

    // Sort, paging, cached ----------------------------------------------------------------------

    @Test
    fun theSortMenuListsTheFourOrdersAndAppliesTheChoice() {
        seed()
        val viewModel = launch()
        awaitText("Deck.pptx")
        composeRule.onNodeWithContentDescription(str(R.string.browser_sort)).performClick()
        listOf(R.string.sort_name, R.string.sort_modified, R.string.sort_size, R.string.sort_kind).forEach {
            composeRule.onNodeWithText(str(it)).assertIsDisplayed()
        }
        composeRule.onNodeWithText(str(R.string.sort_size)).performClick()
        composeRule.waitUntil(5_000) { viewModel.uiState.value.sort.field == SortField.SIZE }
        assertEquals(SortOrder.DESC, viewModel.uiState.value.sort.order)
    }

    @Test
    fun scrollingToTheEndLoadsTheNextPage() {
        fake.folders[""] = (1..450).map { file("f%03d.txt".format(it)) }
        val viewModel = launch()
        awaitText("f001.txt")
        assertEquals(200, viewModel.uiState.value.listing.entries.size)
        composeRule.onNodeWithTag(BrowserTags.LIST).performScrollToIndex(195)
        composeRule.waitUntil(5_000) { viewModel.uiState.value.listing.entries.size >= 400 }
        composeRule.onNodeWithTag(BrowserTags.LIST).performScrollToIndex(395)
        composeRule.waitUntil(5_000) { viewModel.uiState.value.listing.entries.size == 450 }
    }

    @Test
    fun aSnapshotShownBecauseTheHostIsUnreachableCarriesTheCachedChip() {
        seed()
        fake.offlineCachedAtMillis = System.currentTimeMillis() - 3 * 3_600_000L
        launch()
        awaitText("Deck.pptx")
        composeRule.onNodeWithTag(BrowserTags.CACHED).assertIsDisplayed()
        composeRule.onNodeWithText("Cached", substring = true).assertIsDisplayed()
    }

    @Test
    fun aLiveListingHasNoCachedChip() {
        seed()
        launch()
        awaitText("Deck.pptx")
        composeRule.onNodeWithTag(BrowserTags.CACHED).assertDoesNotExist()
    }

    // Search and Recent -------------------------------------------------------------------------

    @Test
    fun searchShowsResultsWithTheirFolderAndThePartialIndicator() {
        seed()
        fake.searchPartial = true
        launch()
        awaitText("Deck.pptx")
        composeRule.onNodeWithContentDescription(str(R.string.browser_search)).performClick()
        composeRule.onNodeWithTag(BrowserTags.SEARCH_FIELD).performTextInput("invoice")
        awaitText("Invoice 7.pdf")
        composeRule.onNodeWithText("Clients/Acme").assertIsDisplayed() // results show where they are
        composeRule.onNodeWithTag(BrowserTags.PARTIAL).assertIsDisplayed()
        composeRule.onNodeWithText(str(R.string.browser_search_partial)).assertIsDisplayed()
    }

    @Test
    fun insideFilesIsOfferedOnlyForAnIndexedShareAndAsksForContent() {
        seed()
        fake.shares = listOf(SampleData.share(search = SearchCapability.INDEXED))
        launch()
        awaitText("Deck.pptx")
        composeRule.onNodeWithContentDescription(str(R.string.browser_search)).performClick()
        composeRule.onNodeWithTag(BrowserTags.SEARCH_INSIDE).assertIsDisplayed()
        composeRule.onNodeWithText(str(R.string.browser_search_inside)).assertIsDisplayed()
        composeRule.onNodeWithTag(BrowserTags.SEARCH_INSIDE).performClick()
        composeRule.onNodeWithTag(BrowserTags.SEARCH_FIELD).performTextInput("invoice")
        awaitText("Invoice 7.pdf")
        composeRule.waitUntil(5_000) { fake.searchInside.lastOrNull() == true }
    }

    @Test
    fun theWalkOffersNoInsideFilesSwitch() {
        seed()
        launch()
        awaitText("Deck.pptx")
        composeRule.onNodeWithContentDescription(str(R.string.browser_search)).performClick()
        composeRule.onNodeWithTag(BrowserTags.SEARCH_INSIDE).assertDoesNotExist()
    }

    @Test
    fun searchWithoutMatchesSaysSo() {
        seed()
        launch()
        awaitText("Deck.pptx")
        composeRule.onNodeWithContentDescription(str(R.string.browser_search)).performClick()
        composeRule.onNodeWithTag(BrowserTags.SEARCH_FIELD).performTextInput("zzz")
        awaitText(str(R.string.browser_search_empty, "zzz"))
    }

    @Test
    fun theRecentTabListsRecentFilesOfThisShare() {
        seed()
        fake.recentEntries = listOf(file("today.txt", share = share, path = "Clients/today.txt"))
        launch()
        awaitText("Deck.pptx")
        composeRule.onNodeWithText(str(R.string.tab_recent)).performClick()
        awaitText("today.txt")
        composeRule.onNodeWithText("Clients", substring = true).assertIsDisplayed() // its folder
        composeRule.onNodeWithText("Deck.pptx").assertDoesNotExist()
    }

    // Multi-select ------------------------------------------------------------------------------

    @Test
    fun longPressSelectsAndTheFetchActionReceivesTheSelection() {
        seed()
        launch()
        awaitText("notes.md")
        composeRule.onNodeWithTag(BrowserTags.entry("notes.md")).performTouchInput { longClick() }
        awaitText(plural(R.plurals.selection_count, 1))
        composeRule.onNodeWithTag(BrowserTags.entry("Deck.pptx")).performClick() // toggles while selecting
        awaitText(plural(R.plurals.selection_count, 2))
        composeRule.onNodeWithTag(BrowserTags.FETCH_SELECTED).performClick()
        composeRule.runOnIdle {
            assertEquals(setOf("notes.md", "Deck.pptx"), fetched.map { it.path }.toSet())
            assertTrue(opened.isEmpty()) // taps while selecting never opened a file
        }
    }

    @Test
    fun cancelSelectionReturnsToTheNormalBar() {
        seed()
        launch()
        awaitText("notes.md")
        composeRule.onNodeWithTag(BrowserTags.entry("notes.md")).performTouchInput { longClick() }
        awaitText(plural(R.plurals.selection_count, 1))
        composeRule.onNodeWithContentDescription(str(R.string.selection_clear)).performClick()
        composeRule.onNodeWithContentDescription(str(R.string.browser_search)).assertIsDisplayed()
    }

    // Stateless rendering of failure states -------------------------------------------------------

    @Test
    fun anOfflineErrorWithoutASnapshotExplainsAndOffersRetry() {
        var retried = 0
        composeRule.setContent {
            GanderTheme(dynamicColor = false) {
                BrowserContent(
                    state = BrowserUiState(
                        shareName = "Projects",
                        ready = true,
                        listing = ListingState(error = UiError(UiErrorKind.OFFLINE)),
                    ),
                    actions = BrowserActions(onRefresh = { retried++ }),
                )
            }
        }
        composeRule.onNodeWithText(str(R.string.error_offline)).assertIsDisplayed()
        composeRule.onNodeWithText(str(R.string.action_retry)).performClick()
        assertEquals(1, retried)
    }

    @Test
    fun aRevokedHostIsExplainedWithoutARetry() {
        composeRule.setContent {
            GanderTheme(dynamicColor = false) {
                BrowserContent(
                    state = BrowserUiState(ready = true, fatal = UiError(UiErrorKind.REVOKED), tab = BrowserTab.FILES),
                    actions = BrowserActions(),
                )
            }
        }
        composeRule.onNodeWithText(str(R.string.error_revoked)).assertIsDisplayed()
        composeRule.onNodeWithText(str(R.string.action_retry)).assertDoesNotExist()
    }

    @Test
    fun aFailedRefreshOverEntriesKeepsThemAndSaysWhy() {
        composeRule.setContent {
            GanderTheme(dynamicColor = false) {
                BrowserContent(
                    state = BrowserUiState(
                        shareName = "Projects",
                        ready = true,
                        listing = ListingState(
                            entries = listOf(file("kept.txt")),
                            loaded = true,
                            error = UiError(UiErrorKind.BUSY),
                        ),
                    ),
                    actions = BrowserActions(),
                )
            }
        }
        composeRule.onNodeWithText("kept.txt").assertIsDisplayed()
        composeRule.onNodeWithText(str(R.string.browser_refresh_failed), substring = true).assertIsDisplayed()
    }
}
