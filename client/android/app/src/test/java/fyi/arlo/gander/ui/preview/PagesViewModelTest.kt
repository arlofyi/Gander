package fyi.arlo.gander.ui.preview

import androidx.lifecycle.SavedStateHandle
import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.ViewModelStore
import fyi.arlo.gander.core.data.UiErrorKind
import fyi.arlo.gander.core.data.actions.ActionReason
import fyi.arlo.gander.core.data.actions.FileAction
import fyi.arlo.gander.core.data.actions.InMemoryActionRefusals
import fyi.arlo.gander.core.data.browse.EntryTarget
import fyi.arlo.gander.core.data.hosts.InMemoryHostRepository
import fyi.arlo.gander.core.data.hosts.PairedHost
import fyi.arlo.gander.core.data.preview.Cancelable
import fyi.arlo.gander.core.data.preview.PagePrefetcher
import fyi.arlo.gander.core.data.preview.PreviewFailure
import fyi.arlo.gander.core.data.preview.PreviewPageRef
import fyi.arlo.gander.core.data.preview.PreviewRetryPolicy
import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.protocol.Entry
import fyi.arlo.gander.core.net.protocol.PreviewKind
import fyi.arlo.gander.core.net.protocol.TilePage
import fyi.arlo.gander.testing.FakePreviewSource
import fyi.arlo.gander.testing.MainDispatcherRule
import fyi.arlo.gander.testing.SampleData
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

@OptIn(ExperimentalCoroutinesApi::class)
class PagesViewModelTest {
    @get:Rule
    val main = MainDispatcherRule()

    private val host = SampleData.host()
    private val source = FakePreviewSource()
    private val refusals = InMemoryActionRefusals()
    private val prefetcher = RecordingPrefetcher()

    private val pdf = SampleData.file("Report.pdf", size = 2_000_000, preview = PreviewKind.PDF)
    private val word = SampleData.file("Contract.docx", size = 40_000, preview = PreviewKind.OFFICE)

    private var clock = 0L
    private val waits = mutableListOf<Long>()
    private val sleep: suspend (Long) -> Unit = {
        waits += it
        clock += it
    }

    /** Records every prefetch with its page and whether it was cancelled since. */
    private class RecordingPrefetcher : PagePrefetcher {
        class Started(val ref: PreviewPageRef) {
            var cancelled = false
        }

        val started = mutableListOf<Started>()

        /** 1-based pages currently being prefetched (started and not cancelled). */
        val live: List<Int> get() = started.filter { !it.cancelled }.map { it.ref.page }

        override fun prefetch(ref: PreviewPageRef): Cancelable {
            val s = Started(ref)
            started += s
            return Cancelable { s.cancelled = true }
        }
    }

    private fun target(entry: Entry = pdf) = EntryTarget(host.id, "shr_projects", "Clients/${entry.name}", entry)

    private fun viewModel(
        entry: Entry = pdf,
        paired: PairedHost? = host,
        saved: SavedStateHandle = SavedStateHandle(),
        retry: PreviewRetryPolicy = PreviewRetryPolicy(),
    ) = PagesViewModel(
        target = target(entry),
        hosts = InMemoryHostRepository(listOfNotNull(paired)),
        source = source,
        prefetcher = prefetcher,
        refusalLog = refusals,
        saved = saved,
        retry = retry,
        sleep = sleep,
        now = { clock },
    )

    /** A ViewModel with its info loaded (5 pages) and a 1080 px viewport, page 1 on screen and loaded. */
    private fun readyViewModel(pages: Int = 5, entry: Entry = pdf): PagesViewModel {
        source.infoHandler = { FakePreviewSource.pdfInfo(pages) }
        val vm = viewModel(entry)
        vm.onViewport(1080)
        return vm
    }

    private suspend fun refusalsOf(entry: Entry = pdf): Map<FileAction, ActionReason> =
        refusals.observe(target(entry)).first()

    // Info ----------------------------------------------------------------------------------------

    @Test
    fun theHostIsAskedForTheInfoAndTheCountAndKindAreKept() {
        source.infoHandler = { FakePreviewSource.pdfInfo(pages = 42, estimatedMs = 400) }
        val vm = viewModel()
        val state = vm.uiState.value
        assertEquals(PagesInfoState.Ready(PreviewKind.PDF, 400), state.info)
        assertEquals(42, state.pageCount)
        assertEquals(42, state.shownPageCount)
        assertEquals("studio-pc", state.hostName)
        assertEquals(listOf("Clients/Report.pdf"), source.infoCalls)
    }

    @Test
    fun aNotYetReadyInfoIsRetriedAfterTheHostsHintUntilItAnswers() {
        var calls = 0
        val seenWhileWaiting = mutableListOf<PagesInfoState>()
        val constructed = CompletableDeferred<Unit>()
        lateinit var vm: PagesViewModel
        source.infoHandler = {
            constructed.await() // the ViewModel starts asking inside its constructor; let the test hold it
            if (++calls <= 2) throw GanderError.Host.Generating(503, "queued", 400)
            FakePreviewSource.pdfInfo(7)
        }
        val recording: suspend (Long) -> Unit = {
            seenWhileWaiting += vm.uiState.value.info
            sleep(it)
        }
        vm = PagesViewModel(
            target = target(), hosts = InMemoryHostRepository(listOf(host)), source = source,
            prefetcher = prefetcher, refusalLog = refusals, saved = SavedStateHandle(), sleep = recording, now = { clock },
        )
        constructed.complete(Unit)

        assertEquals(3, source.infoCalls.size)
        assertEquals(listOf(400L, 400L), waits)
        assertEquals(
            "the screen says the computer is still preparing it",
            PagesInfoState.Loading(waiting = true),
            seenWhileWaiting.first(),
        )
        assertEquals(PagesInfoState.Ready(PreviewKind.PDF, 400), vm.uiState.value.info)
        assertEquals(7, vm.uiState.value.pageCount)
    }

    @Test
    fun whenTheBudgetIsUsedUpTheViewerOffersRetryAndTheSheetIsNotTold() = runTest {
        source.infoHandler = { throw GanderError.Host.Generating(503, "queued", 500) }
        val vm = viewModel(retry = PreviewRetryPolicy(budgetMs = 2_000))
        assertEquals(PagesInfoState.Failed(PreviewFailure.StillGenerating), vm.uiState.value.info)
        assertEquals(5, source.infoCalls.size)
        assertTrue("a busy renderer is not a reason to switch Preview off", refusalsOf().isEmpty())

        // the user taps Retry once the computer is free
        source.infoHandler = { FakePreviewSource.pdfInfo(3) }
        vm.retryInfo()
        assertEquals(PagesInfoState.Ready(PreviewKind.PDF, 400), vm.uiState.value.info)
        assertEquals(3, vm.uiState.value.pageCount)
    }

    @Test
    fun aFileTheHostCannotRenderIsAnExplanationAndTheSheetLearnsWhy() = runTest {
        source.infoHandler = { throw GanderError.Host.UnsupportedType(415, "no renderer") }
        val vm = viewModel()
        assertEquals(PagesInfoState.Failed(PreviewFailure.Unsupported), vm.uiState.value.info)
        assertEquals(ActionReason.NoRenderer("pdf"), refusalsOf()[FileAction.PREVIEW])
    }

    @Test
    fun anInfoOfKindNoneIsUnsupportedToo() {
        source.infoHandler = { FakePreviewSource.pdfInfo(kind = PreviewKind.NONE) }
        assertEquals(PagesInfoState.Failed(PreviewFailure.Unsupported), viewModel().uiState.value.info)
    }

    @Test
    fun aForgottenHostFailsWithoutAskingAnyone() {
        val vm = viewModel(paired = null)
        val failed = vm.uiState.value.info as PagesInfoState.Failed
        assertEquals(UiErrorKind.NOT_PAIRED, (failed.failure as PreviewFailure.Host).error.kind)
        assertTrue(source.infoCalls.isEmpty())
    }

    @Test
    fun anOfflinePhoneIsToldSoAndItsRefusalIsNotLeftOnTheSheet() = runTest {
        source.infoHandler = { throw GanderError.Unreachable(listOf("100.101.102.103"), null) }
        val vm = viewModel()
        val failed = vm.uiState.value.info as PagesInfoState.Failed
        assertEquals(UiErrorKind.OFFLINE, (failed.failure as PreviewFailure.Host).error.kind)
        assertTrue(refusalsOf().isEmpty())
    }

    // Page count of Word and Excel ---------------------------------------------------------------

    @Test
    fun aWordDocumentShowsOnePageUntilTheFirstRenderTellsHowManyThereAre() {
        var pages: Int? = null
        source.infoHandler = { FakePreviewSource.pdfInfo(pages, estimatedMs = 15_000, kind = PreviewKind.OFFICE) }
        val vm = viewModel(word)
        vm.onViewport(1080)
        assertNull(vm.uiState.value.pageCount)
        assertEquals(1, vm.uiState.value.shownPageCount)
        assertEquals("nothing to prefetch while the count is unknown", emptyList<Int>(), prefetcher.live)

        // page 1 renders on the computer, which now knows the count
        pages = 9
        vm.onPageLoaded(0)
        assertEquals(9, vm.uiState.value.pageCount)
        assertEquals(2, source.infoCalls.size)
        assertEquals("and the next page is prefetched", listOf(2), prefetcher.live)
    }

    @Test
    fun ifTheCountIsStillUnknownAfterPageOneTheViewerKeepsShowingPageOne() {
        source.infoHandler = { FakePreviewSource.pdfInfo(null, kind = PreviewKind.OFFICE) }
        val vm = viewModel(word)
        vm.onViewport(1080)
        vm.onPageLoaded(0)
        assertNull(vm.uiState.value.pageCount)
        assertEquals(1, vm.uiState.value.shownPageCount)
    }

    // Deep zoom (WP-67) --------------------------------------------------------------------------

    private val a1 = TilePage(7016, 9933, 6)
    private val a4 = TilePage(2480, 3508, 4)

    @Test
    fun theTilePagesOfThePdfComeWithTheInfo() {
        source.infoHandler = { FakePreviewSource.pdfInfo(3, tilePages = listOf(a1, a1, a4)) }
        val vm = viewModel()
        assertEquals(listOf(a1, a1, a4), vm.uiState.value.tilePages)
    }

    @Test
    fun aFileWithoutTilesHasNoTilePages() {
        val vm = readyViewModel()
        assertNull(vm.uiState.value.tilePages)
    }

    @Test
    fun aTileSizeThisClientDoesNotSpeakMeansNoTiles() {
        source.infoHandler = { FakePreviewSource.pdfInfo(3, tilePages = listOf(a1)).copy(tileSize = 256) }
        assertNull(viewModel().uiState.value.tilePages)
        source.infoHandler = { FakePreviewSource.pdfInfo(3, tilePages = emptyList()) }
        assertNull(viewModel().uiState.value.tilePages)
    }

    @Test
    fun aWordDocumentsTilePagesArriveWithTheCountAfterThePageOneRender() {
        var tiles: List<TilePage>? = null
        var pages: Int? = null
        source.infoHandler = { FakePreviewSource.pdfInfo(pages, kind = PreviewKind.OFFICE, tilePages = tiles) }
        val vm = viewModel(word)
        vm.onViewport(1080)
        assertNull(vm.uiState.value.tilePages)

        // the host exported the document to PDF for page 1 and now knows every page's size
        pages = 2
        tiles = listOf(a4, a4)
        vm.onPageLoaded(0)
        assertEquals(2, vm.uiState.value.pageCount)
        assertEquals(listOf(a4, a4), vm.uiState.value.tilePages)
    }

    @Test
    fun theRefreshKeepsTheTilePagesWhenItOnlyLearnsTheCount() {
        var pages: Int? = null
        source.infoHandler = { FakePreviewSource.pdfInfo(pages, kind = PreviewKind.OFFICE, tilePages = listOf(a4)) }
        val vm = viewModel(word)
        vm.onViewport(1080)
        assertEquals(listOf(a4), vm.uiState.value.tilePages)
        pages = 4
        vm.onPageLoaded(0)
        assertEquals(4, vm.uiState.value.pageCount)
        assertEquals(listOf(a4), vm.uiState.value.tilePages)
    }

    @Test
    fun aTileRequestNamesTheFileVersionPageAndTile() {
        val vm = readyViewModel()
        vm.onViewport(1080)
        val ref = vm.tileRef(2, TileKey(level = 3, x = 1, y = 4))
        assertEquals(3, ref.page)
        assertEquals(3, ref.level)
        assertEquals(1, ref.x)
        assertEquals(4, ref.y)
        assertEquals("Clients/Report.pdf", ref.path)
        assertEquals(host.id, ref.hostId)
        assertEquals("shr_projects", ref.shareId)
        // the same version the page requests carry, so a file edit invalidates pages and tiles alike
        assertEquals(vm.pageRef(2)!!.version, ref.version)
    }

    @Test
    fun officePagesAreNotKeptOnDisk() {
        source.infoHandler = { FakePreviewSource.pdfInfo(3, kind = PreviewKind.OFFICE) }
        val vm = viewModel(word)
        vm.onViewport(1080)
        assertFalse(vm.pageRef(0)!!.persistent)
        val pdfVm = readyViewModel()
        assertTrue(pdfVm.pageRef(0)!!.persistent)
    }

    // Width -------------------------------------------------------------------------------------

    @Test
    fun pagesAreRequestedAtTheViewportWidthClampedToTheHostsMaximum() {
        source.infoHandler = { FakePreviewSource.pdfInfo(5) }
        val vm = viewModel()
        assertNull("not measured yet", vm.pageRef(0))

        vm.onViewport(1080)
        assertEquals(1080, vm.uiState.value.widthPx)
        val ref = vm.pageRef(2)!!
        assertEquals(3, ref.page)
        assertEquals(1080, ref.widthPx)
        assertEquals("Clients/Report.pdf", ref.path)
        assertEquals("2026-10-30T09:12:44Z#2000000", ref.version)

        vm.onViewport(3000)
        assertEquals("the host renders at most 2048", 2048, vm.uiState.value.widthPx)
    }

    @Test
    fun theHostsOwnMaximumWinsWhenItIsLower() {
        val capped = host.copy(capabilities = SampleData.me().capabilities.copy(previewMaxWidth = 1000))
        source.infoHandler = { FakePreviewSource.pdfInfo(3) }
        val vm = viewModel(paired = capped)
        vm.onViewport(1440)
        assertEquals(1000, vm.uiState.value.widthPx)
    }

    @Test
    fun aNewWidthDropsWhatWasLoadedAndPrefetchedAtTheOldOne() {
        val vm = readyViewModel()
        vm.onPageLoaded(0)
        assertEquals(listOf(2), prefetcher.live)

        vm.onViewport(720) // rotated
        assertEquals("the old prefetch was cancelled", emptyList<Int>(), prefetcher.live)
        vm.onPageLoaded(0)
        assertEquals("and starts again at the new width", listOf(2), prefetcher.live)
        assertEquals(720, prefetcher.started.last().ref.widthPx)
    }

    // Prefetch window -----------------------------------------------------------------------------

    @Test
    fun theWindowIsTheNextPageThenThePreviousOne() {
        assertEquals(listOf(5, 3), PageWindow.prefetch(current = 4, count = 10))
        assertEquals("first page: only the next", listOf(1), PageWindow.prefetch(0, 10))
        assertEquals("last page: only the previous", listOf(8), PageWindow.prefetch(9, 10))
        assertEquals("a single page has no neighbours", emptyList<Int>(), PageWindow.prefetch(0, 1))
        assertEquals("a wider window goes forward first", listOf(5, 3, 6, 2), PageWindow.prefetch(4, 10, radius = 2))
    }

    @Test
    fun theNextPageIsPrefetchedOnlyAfterTheOneOnScreenHasLoaded() {
        val vm = readyViewModel()
        assertEquals("page 1 still loading: the host's slot is for it", emptyList<Int>(), prefetcher.live)
        vm.onPageLoaded(0)
        assertEquals(listOf(2), prefetcher.live)
        assertEquals(1080, prefetcher.started.single().ref.widthPx)
    }

    @Test
    fun swipingMovesTheWindowAndCancelsWhatLeftIt() {
        val vm = readyViewModel(pages = 6)
        vm.onPageLoaded(0)
        assertEquals(listOf(2), prefetcher.live)

        vm.onPageSettled(2) // page 3
        assertEquals("not loaded yet: no new requests", listOf(2), prefetcher.live)
        vm.onPageLoaded(2)
        assertEquals("pages 2 and 4 around page 3", listOf(2, 4), prefetcher.live.sorted())
        assertEquals("page 2 was already running and is not asked twice", 2, prefetcher.started.size)

        vm.onPageSettled(4) // page 5
        vm.onPageLoaded(4)
        assertEquals("page 2 left the window, 4 stays, 6 joins", listOf(4, 6), prefetcher.live.sorted())
        assertTrue(prefetcher.started.first { it.ref.page == 2 }.cancelled)
    }

    @Test
    fun aPageThatIsAlreadyLoadedIsNotPrefetched() {
        val vm = readyViewModel(pages = 4)
        vm.onPageLoaded(0)
        vm.onPageSettled(1)
        vm.onPageLoaded(1)
        // page 1 is loaded (and in Coil's memory cache): only page 3 is new
        assertEquals(listOf(2, 3), prefetcher.started.map { it.ref.page })
    }

    @Test
    fun theLastPageHasNoNextToPrefetch() {
        val vm = readyViewModel(pages = 3)
        vm.onPageSettled(2)
        vm.onPageLoaded(2)
        assertEquals(listOf(2), prefetcher.live)
    }

    @Test
    fun leavingTheViewerCancelsThePrefetch() {
        val vm = readyViewModel()
        vm.onPageLoaded(0)
        assertEquals(listOf(2), prefetcher.live)
        // the screen is gone: the owner clears the ViewModel
        val store = ViewModelStore()
        ViewModelProvider(store, object : ViewModelProvider.Factory {
            @Suppress("UNCHECKED_CAST")
            override fun <T : ViewModel> create(modelClass: Class<T>): T = vm as T
        })[PagesViewModel::class.java]
        store.clear()
        assertEquals(emptyList<Int>(), prefetcher.live)
    }

    @Test
    fun theWindowIsNotUsedBeforeTheInfoIsKnown() {
        val gate = CompletableDeferred<Unit>()
        source.infoHandler = {
            gate.await()
            FakePreviewSource.pdfInfo(5)
        }
        val vm = viewModel()
        vm.onViewport(1080)
        vm.onPageLoaded(0)
        assertTrue(prefetcher.started.isEmpty())
        gate.complete(Unit)
        // info arrived: page 1 had already loaded, so the next page starts now
        assertEquals(listOf(2), prefetcher.live)
    }

    // Pager state ----------------------------------------------------------------------------------

    @Test
    fun theCurrentPageSurvivesProcessDeathAndIsClampedToTheCount() {
        val saved = SavedStateHandle()
        val vm = readyViewModelWith(saved)
        vm.onPageSettled(3)
        assertEquals(3, saved.get<Int>("page"))

        // process death: a new ViewModel with the same saved state, but the file now has 2 pages
        source.infoHandler = { FakePreviewSource.pdfInfo(2) }
        val restored = viewModel(saved = saved)
        assertEquals("clamped into the shorter document", 1, restored.uiState.value.currentPage)
    }

    private fun readyViewModelWith(saved: SavedStateHandle): PagesViewModel {
        source.infoHandler = { FakePreviewSource.pdfInfo(5) }
        return viewModel(saved = saved).also { it.onViewport(1080) }
    }

    // Page failures ---------------------------------------------------------------------------------

    @Test
    fun aFailedPageShowsItsReasonAndRetryBuildsTheRequestAgain() {
        val vm = readyViewModel()
        vm.onPageFailed(0, GanderError.Host.Generating(503, "queued", 100))
        assertEquals(PreviewFailure.StillGenerating, vm.uiState.value.failedPages[0])
        assertEquals(0, vm.uiState.value.attempts[0] ?: 0)

        vm.retryPage(0)
        assertNull(vm.uiState.value.failedPages[0])
        assertEquals(1, vm.uiState.value.attempts[0])

        vm.onPageLoaded(0)
        assertTrue(vm.uiState.value.failedPages.isEmpty())
    }

    @Test
    fun aRefusedCurrentPageReachesTheSheetButANeighbourDoesNot() = runTest {
        val vm = readyViewModel()
        vm.onPageFailed(1, GanderError.Host.UnsupportedType(415, ""))
        assertTrue("page 2 is not on screen", refusalsOf().isEmpty())
        vm.onPageFailed(0, GanderError.Host.UnsupportedType(415, ""))
        assertEquals(ActionReason.NoRenderer("pdf"), refusalsOf()[FileAction.PREVIEW])
    }

    @Test
    fun aPageThatLoadsClearsAnEarlierRefusalOfThisFile() = runTest {
        source.infoHandler = { FakePreviewSource.pdfInfo(5) }
        refusals.report(target(), FileAction.PREVIEW, GanderError.Host.UnsupportedType(415, ""))
        assertNotNull(refusalsOf()[FileAction.PREVIEW])
        val vm = viewModel()
        vm.onViewport(1080)
        vm.onPageLoaded(0)
        assertTrue(refusalsOf().isEmpty())
    }

    @Test
    fun aDecoderFailureIsReportedAsAnUnusableAnswer() {
        val vm = readyViewModel()
        vm.onPageFailed(0, IllegalStateException("cannot decode").let { GanderError.MalformedResponse(it) })
        val failure = vm.uiState.value.failedPages[0] as PreviewFailure.Host
        assertEquals(UiErrorKind.INCOMPATIBLE, failure.error.kind)
    }
}
