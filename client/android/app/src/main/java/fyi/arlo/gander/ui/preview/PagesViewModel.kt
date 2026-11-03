package fyi.arlo.gander.ui.preview

import androidx.lifecycle.SavedStateHandle
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import fyi.arlo.gander.core.data.UiError
import fyi.arlo.gander.core.data.UiErrorKind
import fyi.arlo.gander.core.data.actions.ActionRefusals
import fyi.arlo.gander.core.data.actions.FileAction
import fyi.arlo.gander.core.data.browse.EntryTarget
import fyi.arlo.gander.core.data.hosts.HostRepository
import fyi.arlo.gander.core.data.preview.Cancelable
import fyi.arlo.gander.core.data.preview.PagePrefetcher
import fyi.arlo.gander.core.data.preview.PreviewFailure
import fyi.arlo.gander.core.data.preview.PreviewPageRef
import fyi.arlo.gander.core.data.preview.PreviewRetryPolicy
import fyi.arlo.gander.core.data.preview.PreviewTileRef
import fyi.arlo.gander.core.data.preview.PreviewSource
import fyi.arlo.gander.core.data.preview.PreviewWidths
import fyi.arlo.gander.core.data.preview.TILE_SIZE
import fyi.arlo.gander.core.data.preview.shouldReportRefusal
import fyi.arlo.gander.core.data.preview.toPreviewFailure
import fyi.arlo.gander.core.data.thumbs.thumbVersion
import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.protocol.PreviewInfo
import fyi.arlo.gander.core.net.protocol.PreviewKind
import fyi.arlo.gander.core.net.protocol.TilePage
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/** Progress of `GET /preview/info`. */
sealed interface PagesInfoState {
    /** The host is being asked. [waiting] turns true once it answered "still generating". */
    data class Loading(val waiting: Boolean = false) : PagesInfoState

    /** [estimatedMs] is the host's guess for rendering one page cold (`estimated_ms`). */
    data class Ready(val kind: PreviewKind, val estimatedMs: Long) : PagesInfoState

    data class Failed(val failure: PreviewFailure) : PagesInfoState
}

data class PagesUiState(
    val info: PagesInfoState = PagesInfoState.Loading(),
    /**
     * Page count. Null while the host does not know it: Word and Excel documents report it only
     * once the first page has been rendered (preview-renderers spec). The pager then shows page 1.
     */
    val pageCount: Int? = null,
    /** 0-based; what the pager has settled on. */
    val currentPage: Int = 0,
    /** The `width` pages are requested at; null until the viewport is measured. */
    val widthPx: Int? = null,
    /**
     * Deep-zoom geometry of each page, index 0 being page 1 (`preview/info` `tile_pages`); null
     * while the host offers no tiles for this file, and for Word and Excel documents until the
     * first page has rendered (the host exports them to PDF on demand). Pages past the end of the
     * list have no deep zoom.
     */
    val tilePages: List<TilePage>? = null,
    /** Pages that could not be loaded, by 0-based index; the pager shows the reason with Retry. */
    val failedPages: Map<Int, PreviewFailure> = emptyMap(),
    /** Bumped by [PagesViewModel.retryPage] so the page's image request is built again. */
    val attempts: Map<Int, Int> = emptyMap(),
    val hostName: String = "",
) {
    /** What the pager has room for: the known count, else the one page that can be shown. */
    val shownPageCount: Int get() = pageCount ?: 1
}

/** Which neighbours of the page on screen are fetched ahead of time. */
object PageWindow {
    /** Client spec: "prefetch n+1"; the previous page is the other likely swipe. */
    const val DEFAULT_RADIUS = 1

    /**
     * 0-based pages to prefetch around [current], nearest first and the next page before the
     * previous one (people read forward), limited to `0 until count`.
     */
    fun prefetch(current: Int, count: Int, radius: Int = DEFAULT_RADIUS): List<Int> =
        (1..radius).flatMap { distance -> listOf(current + distance, current - distance) }
            .filter { it in 0 until count }
}

/**
 * Logic of the page viewer for PDF and Office previews (client spec, "Preview: pages"): asks the
 * host for `preview/info` (retrying `503 generating` for as long as the retry budget allows),
 * works out the page count and the request width, and keeps the pages next to the one on screen
 * warm.
 *
 * The page images themselves are drawn by Coil from [pageRef]; the UI tells this class what
 * happened to them ([onPageLoaded], [onPageFailed]) and which page the pager settled on
 * ([onPageSettled]). A neighbour is prefetched only after the page on screen has loaded, so the
 * host's single preview slot goes to the visible page first.
 *
 * Plain constructor arguments, no Android types, so the JVM unit tests drive it with a scripted host.
 */
class PagesViewModel(
    private val target: EntryTarget,
    private val hosts: HostRepository,
    private val source: PreviewSource,
    private val prefetcher: PagePrefetcher,
    private val refusalLog: ActionRefusals,
    private val saved: SavedStateHandle,
    private val retry: PreviewRetryPolicy = PreviewRetryPolicy(),
    private val sleep: (suspend (millis: Long) -> Unit)? = null,
    private val now: () -> Long = { System.nanoTime() / 1_000_000 },
    private val prefetchRadius: Int = PageWindow.DEFAULT_RADIUS,
) : ViewModel() {

    private val _state = MutableStateFlow(PagesUiState(currentPage = saved.get<Int>(KEY_PAGE) ?: 0))
    val uiState: StateFlow<PagesUiState> = _state.asStateFlow()

    private var hostMaxWidth: Int? = null
    private var viewportPx: Int? = null
    private val loaded = HashSet<Int>()
    private val prefetching = LinkedHashMap<Int, Cancelable>()
    private var refusalCleared = false
    private var infoJob: Job? = null

    init {
        infoJob = viewModelScope.launch {
            val host = hosts.get(target.hostId)
            if (host == null) {
                _state.update { it.copy(info = PagesInfoState.Failed(PreviewFailure.Host(UiError(UiErrorKind.NOT_PAIRED)))) }
                return@launch
            }
            hostMaxWidth = host.capabilities?.previewMaxWidth
            _state.update { it.copy(hostName = host.name) }
            recomputeWidth()
            loadInfo()
        }
    }

    // Info ---------------------------------------------------------------------------------------

    private suspend fun loadInfo() {
        _state.update { it.copy(info = PagesInfoState.Loading()) }
        try {
            val info = askInfo { _state.update { s -> if (s.info is PagesInfoState.Loading) s.copy(info = PagesInfoState.Loading(waiting = true)) else s } }
            applyInfo(info)
        } catch (e: CancellationException) {
            throw e
        } catch (e: GanderError) {
            _state.update { it.copy(info = PagesInfoState.Failed(e.toPreviewFailure())) }
            report(e)
        }
    }

    private suspend fun askInfo(onWait: () -> Unit = {}): PreviewInfo =
        retry.run(sleep, now, onWait = { _, _ -> onWait() }) {
            source.info(target.hostId, target.shareId, target.path)
        }

    private fun applyInfo(info: PreviewInfo) {
        if (info.kind == PreviewKind.NONE || info.kind == PreviewKind.UNKNOWN) {
            _state.update { it.copy(info = PagesInfoState.Failed(PreviewFailure.Unsupported)) }
            return
        }
        val count = info.pages?.coerceAtLeast(1)
        _state.update { state ->
            state.copy(
                info = PagesInfoState.Ready(info.kind, info.estimatedMs),
                pageCount = count,
                currentPage = state.currentPage.coerceIn(0, (count ?: 1) - 1),
                tilePages = info.usableTilePages(),
            )
        }
        updatePrefetch()
    }

    /** Word and Excel report their page count after the first render: ask again once page 1 is there. */
    private suspend fun refreshCount() {
        try {
            val info = askInfo()
            val count = info.pages?.coerceAtLeast(1)
            val tiles = info.usableTilePages()
            if (count != null || tiles != null) {
                _state.update {
                    it.copy(
                        pageCount = count ?: it.pageCount,
                        currentPage = if (count != null) it.currentPage.coerceIn(0, count - 1) else it.currentPage,
                        tilePages = tiles ?: it.tilePages,
                    )
                }
                updatePrefetch()
            }
        } catch (e: CancellationException) {
            throw e
        } catch (_: GanderError) {
            // The pager keeps showing page 1; the next open asks again.
        }
    }

    /** Retry after [PagesInfoState.Failed]. */
    fun retryInfo() {
        if (_state.value.info !is PagesInfoState.Failed) return
        infoJob?.cancel()
        infoJob = viewModelScope.launch { loadInfo() }
    }

    // Pages --------------------------------------------------------------------------------------

    /** The viewport (the pager) is [px] wide; pages are requested at that width, clamped to the host's maximum. */
    fun onViewport(px: Int) {
        if (px <= 0 || viewportPx == px) return
        viewportPx = px
        recomputeWidth()
    }

    private fun recomputeWidth() {
        val px = viewportPx ?: return
        val width = PreviewWidths.pick(px, hostMaxWidth)
        if (_state.value.widthPx == width) return
        // Other width, other images: what was loaded or prefetched does not count any more.
        cancelPrefetch()
        loaded.clear()
        _state.update { it.copy(widthPx = width, failedPages = emptyMap()) }
    }

    /** The request for 0-based page [index], or null while the viewport is not measured. */
    fun pageRef(index: Int): PreviewPageRef? {
        val width = _state.value.widthPx ?: return null
        return PreviewPageRef(
            hostId = target.hostId,
            shareId = target.shareId,
            path = target.path,
            version = target.entry.thumbVersion(),
            page = index + 1,
            widthPx = width,
            persistent = target.entry.preview != PreviewKind.OFFICE,
        )
    }

    /** The request for tile [tile] of 0-based page [index] of this file (deep zoom, WP-67). */
    fun tileRef(index: Int, tile: TileKey): PreviewTileRef = PreviewTileRef(
        hostId = target.hostId,
        shareId = target.shareId,
        path = target.path,
        version = target.entry.thumbVersion(),
        page = index + 1,
        level = tile.level,
        x = tile.x,
        y = tile.y,
    )

    fun onPageSettled(index: Int) {
        val clamped = index.coerceIn(0, _state.value.shownPageCount - 1)
        saved[KEY_PAGE] = clamped
        _state.update { it.copy(currentPage = clamped) }
        updatePrefetch()
    }

    fun onPageLoaded(index: Int) {
        loaded += index
        _state.update { it.copy(failedPages = it.failedPages - index) }
        if (!refusalCleared) {
            refusalCleared = true
            refusalLog.clear(target, FileAction.PREVIEW)
        }
        if (index == 0 && _state.value.pageCount == null && _state.value.info is PagesInfoState.Ready) {
            viewModelScope.launch { refreshCount() }
        }
        updatePrefetch()
    }

    fun onPageFailed(index: Int, error: GanderError) {
        loaded -= index
        _state.update { it.copy(failedPages = it.failedPages + (index to error.toPreviewFailure())) }
        if (index == _state.value.currentPage) report(error)
    }

    fun retryPage(index: Int) {
        _state.update {
            it.copy(
                failedPages = it.failedPages - index,
                attempts = it.attempts + (index to ((it.attempts[index] ?: 0) + 1)),
            )
        }
    }

    // Prefetch -----------------------------------------------------------------------------------

    private fun updatePrefetch() {
        val state = _state.value
        val ready = state.info as? PagesInfoState.Ready
        if (ready == null || state.widthPx == null || state.currentPage !in loaded) return
        val wanted = PageWindow.prefetch(state.currentPage, state.shownPageCount, prefetchRadius)
        prefetching.keys.filter { it !in wanted }.forEach { prefetching.remove(it)?.cancel() }
        for (index in wanted) {
            if (index in prefetching || index in loaded) continue
            val ref = pageRef(index) ?: continue
            prefetching[index] = prefetcher.prefetch(ref)
        }
    }

    private fun cancelPrefetch() {
        prefetching.values.forEach { it.cancel() }
        prefetching.clear()
    }

    private fun report(error: GanderError) {
        if (shouldReportRefusal(target, error)) refusalLog.report(target, FileAction.PREVIEW, error)
    }

    override fun onCleared() {
        cancelPrefetch()
    }

    private companion object {
        const val KEY_PAGE = "page"
    }
}

/**
 * The deep-zoom page list of this `preview/info`, or null when the host offers none or speaks a
 * tile size this client does not (tiles are 512 px, `tile_size`).
 */
internal fun PreviewInfo.usableTilePages(): List<TilePage>? =
    tilePages?.takeIf { it.isNotEmpty() && (tileSize ?: TILE_SIZE) == TILE_SIZE }
