package fyi.arlo.gander.ui.browser

import androidx.lifecycle.SavedStateHandle
import fyi.arlo.gander.core.data.UiErrorKind
import fyi.arlo.gander.core.data.browse.BrowserPrefs
import fyi.arlo.gander.core.data.browse.DefaultBrowseRepository
import fyi.arlo.gander.core.data.browse.InMemoryBrowserPrefs
import fyi.arlo.gander.core.data.browse.InMemorySnapshotStore
import fyi.arlo.gander.core.data.browse.KindsFilter
import fyi.arlo.gander.core.data.browse.SortField
import fyi.arlo.gander.core.data.browse.SortOrder
import fyi.arlo.gander.core.data.browse.SortSpec
import fyi.arlo.gander.core.data.hosts.InMemoryHostRepository
import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.protocol.EntryFlag
import fyi.arlo.gander.core.net.protocol.RecentResponse
import fyi.arlo.gander.core.net.protocol.SearchCapability
import fyi.arlo.gander.core.net.protocol.SearchResponse
import fyi.arlo.gander.core.net.protocol.SearchVia
import fyi.arlo.gander.core.net.protocol.ServerEvent
import fyi.arlo.gander.testing.FakeHostConnector
import fyi.arlo.gander.testing.MainDispatcherRule
import fyi.arlo.gander.testing.SampleData
import fyi.arlo.gander.testing.SampleData.dir
import fyi.arlo.gander.testing.SampleData.file
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.launch
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

/**
 * Browser logic against a scripted host through the real [DefaultBrowseRepository]: cursor paging,
 * sort persistence, folder navigation, offline snapshots, search, Recent and multi-select.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class BrowserViewModelTest {
    @get:Rule
    val main = MainDispatcherRule()

    private val host = SampleData.host()
    private val share = "shr_projects"
    private val connector = FakeHostConnector()
    private val api get() = connector.api
    private val snapshots = InMemorySnapshotStore()
    private var now = 10_000L
    private val repository = DefaultBrowseRepository(
        connector = connector,
        listings = snapshots,
        sharesStore = snapshots,
        clock = { now },
        reconnectBaseMs = 1_000,
    )
    private val hosts = InMemoryHostRepository(listOf(host))
    private val prefs = InMemoryBrowserPrefs()

    private fun viewModel(
        saved: SavedStateHandle = SavedStateHandle(),
        hostRepository: InMemoryHostRepository = hosts,
    ) = BrowserViewModel(host.id, share, hostRepository, repository, prefs, saved)

    private fun names(vm: BrowserViewModel) = vm.uiState.value.listing.entries.map { it.name }

    private fun hostError(status: Int, code: String) =
        GanderError.fromResponse(status, """{"error":{"code":"$code","message":"m","retry_after_ms":0}}""")

    private fun seedTree() {
        api.folders[""] = listOf(dir("Clients"), file("b.txt"), file("a.txt"))
        api.folders["Clients"] = listOf(dir("Acme"), file("notes.md"))
        api.folders["Clients/Acme"] = listOf(file("Deck.pptx"))
    }

    // Loading -----------------------------------------------------------------------------------

    @Test
    fun showsTheRootFolderWithTheDefaultSort() = runTest {
        seedTree()
        val vm = viewModel()
        val state = vm.uiState.value
        assertTrue(state.ready)
        assertEquals("Projects", state.shareName)
        assertEquals("studio-pc", state.hostName)
        assertEquals(listOf("Clients", "a.txt", "b.txt"), names(vm))
        assertTrue(state.listing.loaded)
        assertFalse(state.listing.refreshing)
        assertNull(state.listing.cachedAtMillis)
        val call = api.listCalls.single()
        assertEquals(listOf("", null, "name", "asc", null), listOf(call.path, call.cursor, call.sort, call.order, call.kinds))
    }

    @Test
    fun aHostThatIsNoLongerPairedIsFatal() = runTest {
        val vm = viewModel(hostRepository = InMemoryHostRepository())
        assertEquals(UiErrorKind.NOT_PAIRED, vm.uiState.value.fatal?.kind)
        assertEquals(0, api.listCalls.size)
    }

    // Paging ------------------------------------------------------------------------------------

    @Test
    fun loadMoreFollowsTheCursorUntilTheFolderEnds() = runTest {
        api.folders[""] = (1..450).map { file("f%03d.txt".format(it)) }
        val vm = viewModel()
        assertEquals(200, vm.uiState.value.listing.entries.size)
        assertEquals("o200", vm.uiState.value.listing.nextCursor)

        vm.loadMore()
        assertEquals(400, vm.uiState.value.listing.entries.size)
        vm.loadMore()
        assertEquals(450, vm.uiState.value.listing.entries.size)
        assertNull(vm.uiState.value.listing.nextCursor)

        vm.loadMore() // nothing left: no request
        assertEquals(3, api.listCalls.size)
        assertEquals(450, names(vm).toSet().size)
        assertEquals(listOf(null, "o200", "o400"), api.listCalls.map { it.cursor })
    }

    @Test
    fun onlyOnePageIsRequestedAtATime() = runTest {
        api.folders[""] = (1..450).map { file("f%03d.txt".format(it)) }
        val vm = viewModel()
        val gate = CompletableDeferred<Unit>()
        api.listGate = gate
        vm.loadMore()
        vm.loadMore()
        vm.loadMore()
        assertTrue(vm.uiState.value.listing.loadingMore)
        assertEquals(2, api.listCalls.size) // first page + one page in flight
        gate.complete(Unit)
        assertFalse(vm.uiState.value.listing.loadingMore)
        assertEquals(400, vm.uiState.value.listing.entries.size)
    }

    @Test
    fun aFailedPageKeepsTheEntriesAndOffersARetry() = runTest {
        api.folders[""] = (1..450).map { file("f%03d.txt".format(it)) }
        val vm = viewModel()
        api.failure = GanderError.Unreachable(host.addresses, null)
        vm.loadMore()
        var listing = vm.uiState.value.listing
        assertEquals(200, listing.entries.size)
        assertEquals(UiErrorKind.OFFLINE, listing.appendError?.kind)
        assertFalse(listing.loadingMore)

        val calls = api.listCalls.size
        vm.loadMore() // scrolling again must not hammer a failing host
        assertEquals(calls, api.listCalls.size)

        api.failure = null
        vm.retryLoadMore()
        listing = vm.uiState.value.listing
        assertEquals(400, listing.entries.size)
        assertNull(listing.appendError)
    }

    // Navigation --------------------------------------------------------------------------------

    @Test
    fun foldersOpenInPlaceAndBackWalksUpOneFolderAtATime() = runTest {
        seedTree()
        val saved = SavedStateHandle()
        val vm = viewModel(saved)
        assertNull(vm.onEntryClick(dir("Clients")))
        assertEquals("Clients", vm.uiState.value.path)
        assertEquals(listOf("Acme", "notes.md"), names(vm))
        assertEquals("Clients", saved.get<String>("path"))

        vm.onEntryClick(dir("Acme"))
        assertEquals("Clients/Acme", vm.uiState.value.path)
        assertEquals(listOf("Projects", "Clients", "Acme"), vm.uiState.value.crumbs.map { it.label })
        assertEquals(listOf("", "Clients", "Clients/Acme"), vm.uiState.value.crumbs.map { it.path })

        assertTrue(vm.onBack())
        assertEquals("Clients", vm.uiState.value.path)
        assertTrue(vm.onBack())
        assertEquals("", vm.uiState.value.path)
        assertFalse(vm.onBack()) // at the root: leave the screen
    }

    @Test
    fun aBreadcrumbTapJumpsStraightToThatFolder() = runTest {
        seedTree()
        val vm = viewModel()
        vm.openPath("Clients/Acme")
        assertEquals(listOf("Deck.pptx"), names(vm))
        vm.openPath("")
        assertEquals("", vm.uiState.value.path)
        assertEquals(listOf("Clients", "a.txt", "b.txt"), names(vm))
    }

    @Test
    fun aFolderAlreadyVisitedComesBackInstantlyWhileItRevalidates() = runTest {
        seedTree()
        val vm = viewModel()
        vm.onEntryClick(dir("Clients"))
        vm.onBack()
        val gate = CompletableDeferred<Unit>()
        api.listGate = gate // the host is slow now
        vm.onEntryClick(dir("Clients"))
        val listing = vm.uiState.value.listing
        assertEquals(listOf("Acme", "notes.md"), listing.entries.map { it.name })
        assertTrue(listing.refreshing)
        gate.complete(Unit)
        assertFalse(vm.uiState.value.listing.refreshing)
    }

    @Test
    fun tappingAFileHandsItsTargetToTheCaller() = runTest {
        seedTree()
        val vm = viewModel()
        vm.openPath("Clients")
        val entry = vm.uiState.value.listing.entries.first { it.name == "notes.md" }
        val target = vm.onEntryClick(entry)!!
        assertEquals(host.id, target.hostId)
        assertEquals(share, target.shareId)
        assertEquals("Clients/notes.md", target.path)
        assertEquals(entry, target.entry)
    }

    @Test
    fun aLinkIsNotOpenable() = runTest {
        api.folders[""] = listOf(file("shortcut", flags = listOf(EntryFlag.LINK)))
        val vm = viewModel()
        assertNull(vm.onEntryClick(vm.uiState.value.listing.entries.single()))
        vm.onEntryLongClick(vm.uiState.value.listing.entries.single())
        assertTrue(vm.uiState.value.selection.isEmpty())
    }

    @Test
    fun theFolderIsRestoredAfterProcessDeath() = runTest {
        seedTree()
        val vm = viewModel(SavedStateHandle(mapOf("path" to "Clients")))
        assertEquals("Clients", vm.uiState.value.path)
        assertEquals("Clients", api.listCalls.first().path)
        assertEquals(listOf("Acme", "notes.md"), names(vm))
    }

    // Sort, filter, view mode -----------------------------------------------------------------

    @Test
    fun sortChoicesReloadFromTheHostAndAreRememberedPerShare() = runTest {
        api.folders[""] = listOf(
            file("small", size = 1, mtime = "2026-10-03T00:00:00Z"),
            file("big", size = 99, mtime = "2026-10-01T00:00:00Z"),
        )
        val vm = viewModel()

        vm.selectSortField(SortField.MTIME)
        assertEquals(SortSpec(SortField.MTIME, SortOrder.DESC), vm.uiState.value.sort) // newest first
        var call = api.listCalls.last()
        assertEquals(listOf("mtime", "desc"), listOf(call.sort, call.order))
        assertEquals(listOf("small", "big"), names(vm))

        vm.selectSortField(SortField.MTIME) // same field flips the order
        assertEquals(SortOrder.ASC, vm.uiState.value.sort.order)
        assertEquals(listOf("big", "small"), names(vm))

        vm.selectSortField(SortField.SIZE)
        assertEquals(SortSpec(SortField.SIZE, SortOrder.DESC), vm.uiState.value.sort)
        vm.selectSortField(SortField.NAME)
        assertEquals(SortSpec(SortField.NAME, SortOrder.ASC), vm.uiState.value.sort)

        vm.selectSortField(SortField.MTIME) // leave it on newest first
        assertEquals(BrowserPrefs(SortSpec(SortField.MTIME, SortOrder.DESC)), prefs.get(host.id, share))

        // a later session of the same share starts with it, and asks the host for it
        val again = viewModel()
        assertEquals(SortSpec(SortField.MTIME, SortOrder.DESC), again.uiState.value.sort)
        call = api.listCalls.last()
        assertEquals(listOf("mtime", "desc"), listOf(call.sort, call.order))
        // another share is untouched
        assertEquals(BrowserPrefs(), prefs.get(host.id, "shr_other"))
    }

    @Test
    fun theKindsFilterIsSentToTheHost() = runTest {
        seedTree()
        val vm = viewModel()
        vm.setKinds(KindsFilter.FILES)
        assertEquals("file", api.listCalls.last().kinds)
        assertEquals(listOf("a.txt", "b.txt"), names(vm))
        vm.setKinds(KindsFilter.FOLDERS)
        assertEquals("dir", api.listCalls.last().kinds)
        assertEquals(listOf("Clients"), names(vm))
        vm.setKinds(KindsFilter.ALL)
        assertNull(api.listCalls.last().kinds)
    }

    @Test
    fun theViewModeIsPersistedPerShare() = runTest {
        seedTree()
        val vm = viewModel()
        vm.setViewMode(fyi.arlo.gander.core.data.browse.ViewMode.GRID)
        assertEquals(fyi.arlo.gander.core.data.browse.ViewMode.GRID, viewModel().uiState.value.viewMode)
    }

    // ETag, offline -----------------------------------------------------------------------------

    @Test
    fun refreshRevalidatesWithTheEtag() = runTest {
        seedTree()
        val vm = viewModel()
        assertNull(api.listCalls.last().ifNoneMatch)
        vm.refresh()
        assertNotNull(api.listCalls.last().ifNoneMatch)
        assertEquals(listOf("Clients", "a.txt", "b.txt"), names(vm))
        assertNull(vm.uiState.value.listing.cachedAtMillis)
    }

    @Test
    fun anUnreachableHostShowsTheSnapshotWithACachedChip() = runTest {
        seedTree()
        viewModel() // an earlier visit stores the snapshot at now = 10_000
        now = 99_000
        api.failure = GanderError.Unreachable(host.addresses, null)

        val vm = viewModel()
        val listing = vm.uiState.value.listing
        assertEquals(listOf("Clients", "a.txt", "b.txt"), listing.entries.map { it.name })
        assertEquals(10_000L, listing.cachedAtMillis)
        assertNull(listing.error)
        assertTrue(listing.loaded)
    }

    @Test
    fun offlineWithoutASnapshotIsABlockingError() = runTest {
        seedTree()
        api.failure = GanderError.Unreachable(host.addresses, null)
        val vm = viewModel()
        val listing = vm.uiState.value.listing
        assertFalse(listing.loaded)
        assertEquals(UiErrorKind.OFFLINE, listing.error?.kind)
        assertFalse(listing.refreshing)
    }

    @Test
    fun aFailedRefreshKeepsWhatIsOnScreen() = runTest {
        seedTree()
        val vm = viewModel()
        api.failure = hostError(500, "internal")
        vm.refresh()
        val listing = vm.uiState.value.listing
        assertEquals(UiErrorKind.OTHER, listing.error?.kind)
        assertTrue(listing.loaded)
        assertEquals(3, listing.entries.size)
    }

    @Test
    fun aRevokedTokenMakesTheWholeBrowserFatal() = runTest {
        seedTree()
        val vm = viewModel()
        api.failure = hostError(401, "token_revoked")
        vm.refresh()
        assertEquals(UiErrorKind.REVOKED, vm.uiState.value.fatal?.kind)
    }

    @Test
    fun aMissingFolderIsAnErrorNotAnEmptyListing() = runTest {
        seedTree()
        val vm = viewModel()
        vm.openPath("Gone")
        assertEquals(UiErrorKind.NOT_FOUND, vm.uiState.value.listing.error?.kind)
        assertFalse(vm.uiState.value.listing.loaded)
    }

    // Search ------------------------------------------------------------------------------------

    @Test
    fun searchWaitsForTheTypingToPauseFor300ms() = runTest {
        seedTree()
        val vm = viewModel()
        vm.openSearch()
        vm.onSearchTextChange("d")
        advanceTimeBy(100)
        vm.onSearchTextChange("de")
        advanceTimeBy(100)
        vm.onSearchTextChange("dec")
        advanceTimeBy(299)
        runCurrent()
        assertEquals(0, api.searchCalls.size)
        advanceTimeBy(2)
        runCurrent()
        assertEquals(listOf("dec"), api.searchCalls.map { it.q })
    }

    @Test
    fun searchShowsResultsWithPathsAndThePartialIndicator() = runTest {
        seedTree()
        api.searchHandler = {
            SearchResponse(
                listOf(file("Deck.pptx", path = "Clients/Acme/Deck.pptx"), dir("Acme", path = "Clients/Acme")),
                partial = true,
                nextCursor = null,
            )
        }
        val vm = viewModel()
        vm.openSearch()
        vm.onSearchTextChange("a")
        advanceTimeBy(301)
        runCurrent()

        val search = vm.uiState.value.search
        assertEquals("a", search.searchedText)
        assertTrue(search.partial)
        assertTrue(vm.uiState.value.showingSearchResults)
        assertEquals(listOf("Clients/Acme/Deck.pptx", "Clients/Acme"), vm.uiState.value.visibleEntries.map { vm.uiState.value.pathOf(it) })

        // a file result opens as a target with its own full path
        val target = vm.onEntryClick(search.entries.first())!!
        assertEquals("Clients/Acme/Deck.pptx", target.path)
        // a folder result jumps into that folder and ends the search
        assertNull(vm.onEntryClick(search.entries.last()))
        assertEquals("Clients/Acme", vm.uiState.value.path)
        assertFalse(vm.uiState.value.search.active)
    }

    // Search inside files (WP-66) ---------------------------------------------------------------------

    private fun indexedShare() {
        api.shares = listOf(SampleData.share(search = SearchCapability.INDEXED))
    }

    @Test
    fun insideFilesIsOfferedForAnIndexedShare() = runTest {
        seedTree()
        indexedShare()
        val vm = viewModel()
        assertTrue(vm.uiState.value.canSearchInside)
        assertFalse(vm.uiState.value.search.inFiles) // off until the user turns it on
    }

    @Test
    fun insideFilesIsNotOfferedWhenTheHostSaysNothingAboutSearch() = runTest {
        seedTree()
        assertFalse(viewModel().uiState.value.canSearchInside) // an older host: the walk
    }

    @Test
    fun insideFilesIsNotOfferedForAShareSearchedByTheWalk() = runTest {
        seedTree()
        api.shares = listOf(SampleData.share(search = SearchCapability.BOUNDED))
        assertFalse(viewModel().uiState.value.canSearchInside)
    }

    @Test
    fun theSwitchAsksTheHostToSearchContentAndRerunsTheSearch() = runTest {
        seedTree()
        indexedShare()
        api.searchHandler = { call ->
            SearchResponse(
                listOf(file(if (call.scope == "content") "inside.txt" else "name.txt", path = "x")),
                partial = false,
                nextCursor = null,
                via = SearchVia.INDEX,
            )
        }
        val vm = viewModel()
        vm.openSearch()
        vm.onSearchTextChange("budget")
        advanceTimeBy(301)
        runCurrent()
        assertEquals(listOf<String?>(null), api.searchCalls.map { it.scope })
        assertEquals("name.txt", vm.uiState.value.search.entries.single().name)
        assertEquals(SearchVia.INDEX, vm.uiState.value.search.via)

        vm.setSearchInFiles(true)
        runCurrent()
        assertTrue(vm.uiState.value.search.inFiles)
        assertEquals(listOf<String?>(null, "content"), api.searchCalls.map { it.scope })
        assertEquals("inside.txt", vm.uiState.value.search.entries.single().name)
        assertTrue(vm.uiState.value.search.searchedInFiles)
        assertFalse(vm.uiState.value.search.namesOnly)

        // typing again keeps the mode
        vm.onSearchTextChange("budget plan")
        advanceTimeBy(301)
        runCurrent()
        assertEquals("content", api.searchCalls.last().scope)

        vm.setSearchInFiles(false)
        runCurrent()
        assertNull(api.searchCalls.last().scope)
    }

    @Test
    fun aCursorContinuesTheModeItWasMadeIn() = runTest {
        seedTree()
        indexedShare()
        api.searchHandler = { call ->
            if (call.cursor == null) {
                SearchResponse(listOf(file("one", path = "one")), partial = false, nextCursor = "i1.x.1", via = SearchVia.INDEX)
            } else {
                SearchResponse(listOf(file("two", path = "two")), partial = false, nextCursor = null, via = SearchVia.INDEX)
            }
        }
        val vm = viewModel()
        vm.openSearch()
        vm.setSearchInFiles(true)
        vm.onSearchTextChange("o")
        advanceTimeBy(301)
        runCurrent()
        vm.loadMore()
        assertEquals(listOf<String?>("content", "content"), api.searchCalls.map { it.scope })
        assertEquals(listOf(null, "i1.x.1"), api.searchCalls.map { it.cursor })
        assertEquals(listOf("one", "two"), vm.uiState.value.search.entries.map { it.name })
    }

    @Test
    fun aWalkThatAnsweredAnInsideFilesSearchIsSaidToBeNamesOnly() = runTest {
        seedTree()
        indexedShare()
        // The index was unusable for this request: the host answered with its walk.
        api.searchHandler = { SearchResponse(listOf(file("a", path = "a")), partial = false, nextCursor = null, via = SearchVia.WALK) }
        val vm = viewModel()
        vm.openSearch()
        vm.setSearchInFiles(true)
        vm.onSearchTextChange("a")
        advanceTimeBy(301)
        runCurrent()
        assertTrue(vm.uiState.value.search.namesOnly)

        // Names mode is not a downgrade, whoever answered.
        vm.setSearchInFiles(false)
        runCurrent()
        assertFalse(vm.uiState.value.search.namesOnly)
    }

    @Test
    fun theSwitchDoesNothingOnAShareThatIsNotIndexed() = runTest {
        seedTree()
        val vm = viewModel()
        vm.openSearch()
        vm.setSearchInFiles(true)
        assertFalse(vm.uiState.value.search.inFiles)
        vm.onSearchTextChange("a")
        advanceTimeBy(301)
        runCurrent()
        assertNull(api.searchCalls.single().scope)
    }

    @Test
    fun closingSearchTurnsInsideFilesOff() = runTest {
        seedTree()
        indexedShare()
        val vm = viewModel()
        vm.openSearch()
        vm.setSearchInFiles(true)
        assertTrue(vm.uiState.value.search.inFiles)
        vm.closeSearch()
        vm.openSearch()
        assertFalse(vm.uiState.value.search.inFiles)
    }

    @Test
    fun searchRunsUnderTheFolderThatWasOpen() = runTest {
        seedTree()
        val vm = viewModel()
        vm.openPath("Clients")
        vm.openSearch()
        vm.onSearchTextChange("x")
        advanceTimeBy(301)
        runCurrent()
        assertEquals("Clients", api.searchCalls.single().path)
        assertEquals("Clients", vm.uiState.value.search.scopePath)
    }

    @Test
    fun clearingTheFieldClearsResultsWithoutAskingTheHost() = runTest {
        seedTree()
        api.searchHandler = { SearchResponse(listOf(file("a", path = "a")), partial = false, nextCursor = null) }
        val vm = viewModel()
        vm.openSearch()
        vm.onSearchTextChange("a")
        advanceTimeBy(301)
        runCurrent()
        assertEquals(1, vm.uiState.value.search.entries.size)

        vm.onSearchTextChange("")
        runCurrent()
        assertNull(vm.uiState.value.search.searchedText)
        assertTrue(vm.uiState.value.search.entries.isEmpty())
        assertEquals(1, api.searchCalls.size)
        assertFalse(vm.uiState.value.showingSearchResults)
    }

    @Test
    fun searchResultsPageByCursor() = runTest {
        seedTree()
        api.searchHandler = { call ->
            if (call.cursor == null) {
                SearchResponse(listOf(file("one", path = "one")), partial = false, nextCursor = "c1")
            } else {
                SearchResponse(listOf(file("two", path = "two")), partial = true, nextCursor = null)
            }
        }
        val vm = viewModel()
        vm.openSearch()
        vm.onSearchTextChange("o")
        advanceTimeBy(301)
        runCurrent()
        vm.loadMore()
        val search = vm.uiState.value.search
        assertEquals(listOf("one", "two"), search.entries.map { it.name })
        assertNull(search.nextCursor)
        assertTrue(search.partial)
        assertEquals(listOf(null, "c1"), api.searchCalls.map { it.cursor })
    }

    @Test
    fun aFailedSearchIsExplainedAndBackClosesSearch() = runTest {
        seedTree()
        val vm = viewModel()
        vm.openSearch()
        api.failure = GanderError.Unreachable(host.addresses, null)
        vm.onSearchTextChange("x")
        advanceTimeBy(301)
        runCurrent()
        assertEquals(UiErrorKind.OFFLINE, vm.uiState.value.search.error?.kind)

        assertTrue(vm.onBack())
        assertFalse(vm.uiState.value.search.active)
    }

    // Recent ------------------------------------------------------------------------------------

    @Test
    fun theRecentTabListsThisSharesRecentFilesOnce() = runTest {
        seedTree()
        api.recentResponse = RecentResponse(
            listOf(
                file("mine.txt", share = share, path = "Clients/mine.txt"),
                file("theirs.txt", share = "shr_other", path = "theirs.txt"),
            ),
            partial = true,
        )
        val vm = viewModel()
        assertTrue(vm.uiState.value.showTabs)
        vm.selectTab(BrowserTab.RECENT)
        val recent = vm.uiState.value.recent
        assertEquals(listOf("mine.txt"), recent.entries.map { it.name })
        assertTrue(recent.partial)
        assertEquals(listOf("mine.txt"), vm.uiState.value.visibleEntries.map { it.name })

        val target = vm.onEntryClick(recent.entries.single())!!
        assertEquals("Clients/mine.txt", target.path)

        vm.selectTab(BrowserTab.FILES)
        vm.selectTab(BrowserTab.RECENT)
        assertTrue(vm.uiState.value.recent.loaded) // not fetched again
        assertTrue(vm.onBack())
        assertEquals(BrowserTab.FILES, vm.uiState.value.tab)
    }

    @Test
    fun tabsAreOfferedAtTheShareRootOnly() = runTest {
        seedTree()
        val vm = viewModel()
        assertTrue(vm.uiState.value.showTabs)
        vm.openPath("Clients")
        assertFalse(vm.uiState.value.showTabs)
        vm.openPath("")
        vm.openSearch()
        assertFalse(vm.uiState.value.showTabs)
    }

    // Selection ---------------------------------------------------------------------------------

    @Test
    fun longPressStartsSelectionAndTapsToggleIt() = runTest {
        seedTree()
        val vm = viewModel()
        val entries = vm.uiState.value.listing.entries
        val a = entries.first { it.name == "a.txt" }
        val b = entries.first { it.name == "b.txt" }
        val folder = entries.first { it.name == "Clients" }

        vm.onEntryLongClick(folder) // folders cannot be fetched in v1
        assertFalse(vm.uiState.value.selecting)

        vm.onEntryLongClick(a)
        assertEquals(setOf("a.txt"), vm.uiState.value.selection)
        assertNull(vm.onEntryClick(b)) // in selection mode a tap toggles, it does not open
        assertEquals(setOf("a.txt", "b.txt"), vm.uiState.value.selection)
        vm.onEntryClick(a)
        assertEquals(setOf("b.txt"), vm.uiState.value.selection)
        vm.onEntryClick(folder) // still selecting: ignored, does not navigate
        assertEquals("", vm.uiState.value.path)
    }

    @Test
    fun selectAllTakesFilesOnlyAndTargetsCarryFullPaths() = runTest {
        seedTree()
        val vm = viewModel()
        vm.openPath("Clients")
        vm.selectAll()
        assertEquals(setOf("Clients/notes.md"), vm.uiState.value.selection)
        val targets = vm.selectedTargets()
        assertEquals(listOf("Clients/notes.md"), targets.map { it.path })
        assertEquals(listOf(share), targets.map { it.shareId })
    }

    @Test
    fun backClearsSelectionFirstAndNavigatingDropsIt() = runTest {
        seedTree()
        val vm = viewModel()
        vm.openPath("Clients")
        vm.onEntryLongClick(vm.uiState.value.listing.entries.first { it.name == "notes.md" })
        assertTrue(vm.uiState.value.handlesBack)
        assertTrue(vm.onBack())
        assertFalse(vm.uiState.value.selecting)
        assertEquals("Clients", vm.uiState.value.path) // the first back only cleared the selection

        vm.onEntryLongClick(vm.uiState.value.listing.entries.first { it.name == "notes.md" })
        vm.openPath("")
        assertFalse(vm.uiState.value.selecting)
    }

    @Test
    fun fetchNeedsThePermission() = runTest {
        seedTree()
        api.shares = listOf(SampleData.share(permissions = listOf(fyi.arlo.gander.core.net.protocol.Permission.LIST)))
        val vm = viewModel()
        assertFalse(vm.uiState.value.canFetch)
    }

    // Events ------------------------------------------------------------------------------------

    @Test
    fun aChangeInTheOpenFolderReloadsIt() = runTest {
        seedTree()
        val vm = viewModel()
        backgroundScope.launch { vm.watchEvents() }
        runCurrent()
        val before = api.listCalls.size

        connector.events.emit(ServerEvent.ShareChanged(share, "Clients")) // another folder
        connector.events.emit(ServerEvent.ShareChanged("shr_other", ""))
        runCurrent()
        assertEquals(before, api.listCalls.size)

        api.folders[""] = api.folders.getValue("") + file("new.txt")
        connector.events.emit(ServerEvent.ShareChanged(share, ""))
        runCurrent()
        assertEquals(before + 1, api.listCalls.size)
        assertTrue("new.txt" in names(vm))
    }

    @Test
    fun aRevokedDeviceEventIsFatal() = runTest {
        seedTree()
        val vm = viewModel()
        backgroundScope.launch { vm.watchEvents() }
        runCurrent()
        connector.events.emit(ServerEvent.DeviceRevoked)
        runCurrent()
        assertEquals(UiErrorKind.REVOKED, vm.uiState.value.fatal?.kind)
    }
}
