package fyi.arlo.gander.ui.browser

import androidx.lifecycle.SavedStateHandle
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import fyi.arlo.gander.core.data.UiError
import fyi.arlo.gander.core.data.UiErrorKind
import fyi.arlo.gander.core.data.browse.BrowseRepository
import fyi.arlo.gander.core.data.browse.BrowserPrefsRepository
import fyi.arlo.gander.core.data.browse.EntryTarget
import fyi.arlo.gander.core.data.browse.KindsFilter
import fyi.arlo.gander.core.data.browse.ListQuery
import fyi.arlo.gander.core.data.browse.PageResult
import fyi.arlo.gander.core.data.browse.SortField
import fyi.arlo.gander.core.data.browse.SortOrder
import fyi.arlo.gander.core.data.browse.SortSpec
import fyi.arlo.gander.core.data.browse.ViewMode
import fyi.arlo.gander.core.data.browse.fullPath
import fyi.arlo.gander.core.data.browse.parentPath
import fyi.arlo.gander.core.data.hosts.HostRepository
import fyi.arlo.gander.core.data.hosts.PairedHost
import fyi.arlo.gander.core.data.toUiError
import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.protocol.Entry
import fyi.arlo.gander.core.net.protocol.EntryKind
import fyi.arlo.gander.core.net.protocol.Permission
import fyi.arlo.gander.core.net.protocol.PreviewKind
import fyi.arlo.gander.core.net.protocol.SearchCapability
import fyi.arlo.gander.core.net.protocol.SearchVia
import fyi.arlo.gander.core.net.protocol.ServerEvent
import fyi.arlo.gander.core.net.protocol.ShareInfo
import fyi.arlo.gander.core.net.protocol.StreamDecision
import fyi.arlo.gander.core.net.protocol.ZipCapability
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.FlowPreview
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.debounce
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

enum class BrowserTab { FILES, RECENT }

/** One segment of the breadcrumb; [path] is share-relative (`""` is the share root). */
data class Crumb(val label: String, val path: String)

/** The folder shown on the Files tab. */
data class ListingState(
    val entries: List<Entry> = emptyList(),
    val nextCursor: String? = null,
    /** [entries] are meaningful (fresh, or a stored snapshot shown while refreshing). */
    val loaded: Boolean = false,
    /** First page being fetched (or revalidated). */
    val refreshing: Boolean = false,
    val loadingMore: Boolean = false,
    val truncated: Boolean = false,
    val unlistable: Int = 0,
    /** Set when [entries] are the stored snapshot because the computer cannot be reached. */
    val cachedAtMillis: Long? = null,
    /** The snapshot is shorter than the folder on the computer. */
    val cacheIncomplete: Boolean = false,
    /** The first page failed. Blocking when nothing is [loaded], a banner otherwise. */
    val error: UiError? = null,
    /** A later page failed; the entries so far stay and the footer offers a retry. */
    val appendError: UiError? = null,
)

data class SearchState(
    val active: Boolean = false,
    /** What is in the text field right now. */
    val text: String = "",
    /** The folder the search runs under (the folder that was open when search started). */
    val scopePath: String = "",
    /** The query [entries] answer; null while there are no results to show. */
    val searchedText: String? = null,
    val entries: List<Entry> = emptyList(),
    /** The host stopped early (time or count limit): results may be incomplete. */
    val partial: Boolean = false,
    val nextCursor: String? = null,
    val searching: Boolean = false,
    val loadingMore: Boolean = false,
    val error: UiError? = null,
    /** "Search inside files" is on. Only offered, and only honoured, for an indexed share. */
    val inFiles: Boolean = false,
    /** Whether [entries] answer an inside-files search (a cursor continues the mode it was made with). */
    val searchedInFiles: Boolean = false,
    /** What answered [entries]: the computer's index or its bounded walk; null for an older host. */
    val via: SearchVia? = null,
) {
    /** Inside-files was asked for and the computer could only search names (its index is not usable right now). */
    val namesOnly: Boolean get() = searchedInFiles && via == SearchVia.WALK
}

data class RecentState(
    val entries: List<Entry> = emptyList(),
    val partial: Boolean = false,
    val loaded: Boolean = false,
    val loading: Boolean = false,
    val error: UiError? = null,
)

data class BrowserUiState(
    val hostName: String = "",
    val shareName: String = "",
    val share: ShareInfo? = null,
    /** Host, share and stored preferences are resolved; the first listing may still be loading. */
    val ready: Boolean = false,
    /** The browser cannot work at all (host forgotten, access revoked mid-session). */
    val fatal: UiError? = null,
    val tab: BrowserTab = BrowserTab.FILES,
    val path: String = "",
    val viewMode: ViewMode = ViewMode.LIST,
    val sort: SortSpec = SortSpec(),
    val kinds: KindsFilter = KindsFilter.ALL,
    val listing: ListingState = ListingState(),
    val recent: RecentState = RecentState(),
    val search: SearchState = SearchState(),
    /** Share-relative paths of the selected files. */
    val selection: Set<String> = emptySet(),
    /** The computer's folder-download limits (`capabilities.fetch.zip`); null when it has none (an older host). */
    val zip: ZipCapability? = null,
) {
    val selecting: Boolean get() = selection.isNotEmpty()

    /** True while search results (not just an empty search field) replace the folder. */
    val showingSearchResults: Boolean get() = search.active && search.searchedText != null

    /** Fetch needs the `fetch` permission; unknown permissions (share not loaded) do not block. */
    val canFetch: Boolean get() = share?.permissions?.let { Permission.FETCH in it } ?: true

    /** "Search inside files" is offered: the share's folder is in the computer's Windows Search index (WP-66). */
    val canSearchInside: Boolean get() = share?.search == SearchCapability.INDEXED

    /** "Download folder as zip" is offered: the host can do it and this share allows fetching. */
    val canZip: Boolean get() = zip != null && canFetch

    /** Back is consumed by the screen (selection, search, tab, parent folder) before leaving it. */
    val handlesBack: Boolean get() = selecting || search.active || tab != BrowserTab.FILES || path.isNotEmpty()

    /** Tabs are offered at the share root only (client spec). */
    val showTabs: Boolean get() = path.isEmpty() && !search.active

    /** Root first, then one crumb per folder segment. */
    val crumbs: List<Crumb>
        get() {
            val root = Crumb(shareName.ifEmpty { "/" }, "")
            if (path.isEmpty()) return listOf(root)
            var acc = ""
            return listOf(root) + path.split('/').filter { it.isNotEmpty() }.map { segment ->
                acc = if (acc.isEmpty()) segment else "$acc/$segment"
                Crumb(segment, acc)
            }
        }

    /** The entries on screen now, whichever mode is showing. */
    val visibleEntries: List<Entry>
        get() = when {
            showingSearchResults -> search.entries
            tab == BrowserTab.RECENT -> recent.entries
            else -> listing.entries
        }

    /** The share-relative path of [entry] as shown now. */
    fun pathOf(entry: Entry): String = entry.fullPath(path)

    fun query(): ListQuery = ListQuery(sort, kinds)
}

/**
 * Browser logic for one share (client spec, "Browser behaviour details"): the current folder
 * with cursor paging, sort and kinds filter, breadcrumb navigation, debounced search, the Recent
 * tab and multi-select.
 *
 * Plain constructor arguments (no Android types besides [SavedStateHandle], which only keeps the
 * folder and tab across process death) so the JVM unit tests drive it with a scripted host.
 */
@OptIn(FlowPreview::class)
class BrowserViewModel(
    private val hostId: String,
    private val shareId: String,
    private val hosts: HostRepository,
    private val repository: BrowseRepository,
    private val prefs: BrowserPrefsRepository,
    private val saved: SavedStateHandle,
    private val searchDebounceMs: Long = SEARCH_DEBOUNCE_MS,
) : ViewModel() {

    private val _state = MutableStateFlow(
        BrowserUiState(
            shareName = shareId,
            path = saved.get<String>(KEY_PATH).orEmpty(),
            tab = saved.get<String>(KEY_TAB)?.let { name -> BrowserTab.entries.firstOrNull { it.name == name } }
                ?: BrowserTab.FILES,
        ),
    )
    val uiState: StateFlow<BrowserUiState> = _state.asStateFlow()

    private val hostReady = CompletableDeferred<PairedHost?>()
    private var host: PairedHost? = null
    private var loadJob: Job? = null
    private var moreJob: Job? = null
    private var recentJob: Job? = null
    private var searchMoreJob: Job? = null
    private var searchNowJob: Job? = null

    /** Last first pages by folder and query, for instant return to a folder already visited. */
    private val memory = object : LinkedHashMap<String, ListingState>(16, 0.75f, true) {
        override fun removeEldestEntry(eldest: MutableMap.MutableEntry<String, ListingState>?) =
            size > MEMORY_FOLDERS
    }

    private val searchText = MutableStateFlow("")

    init {
        viewModelScope.launch { bootstrap() }
        viewModelScope.launch {
            searchText
                .map { it.trim() }
                .debounce { if (it.isEmpty()) 0L else searchDebounceMs }
                .distinctUntilChanged()
                .collectLatest { runSearch(it) }
        }
    }

    // Setup -------------------------------------------------------------------------------------

    private suspend fun bootstrap() {
        val paired = hosts.get(hostId)
        host = paired
        hostReady.complete(paired)
        if (paired == null) {
            _state.update { it.copy(ready = true, fatal = UiError(UiErrorKind.NOT_PAIRED)) }
            return
        }
        val stored = prefs.get(hostId, shareId)
        val cachedShare = repository.cachedShares(paired)?.value?.firstOrNull { it.id == shareId }
        _state.update {
            it.copy(
                hostName = paired.name,
                sort = stored.sort,
                viewMode = stored.viewMode,
                ready = true,
                zip = paired.capabilities?.fetch?.zip,
            ).withShare(cachedShare)
        }
        if (cachedShare == null) {
            viewModelScope.launch {
                try {
                    repository.shares(paired).value.firstOrNull { it.id == shareId }
                        ?.let { share -> _state.update { it.withShare(share) } }
                } catch (_: GanderError) {
                    // The listing call reports the problem; the share name just stays the id.
                }
            }
        }
        if (_state.value.tab == BrowserTab.RECENT) loadRecent() else reload()
    }

    private fun BrowserUiState.withShare(share: ShareInfo?) =
        if (share == null) this else copy(share = share, shareName = share.name)

    // Folder navigation ---------------------------------------------------------------------------

    /** Opens the folder at [target] (share-relative, `""` for the root) on the Files tab. */
    fun openPath(target: String) {
        saved[KEY_PATH] = target
        saved[KEY_TAB] = BrowserTab.FILES.name
        closeSearchState()
        moreJob?.cancel()
        _state.update {
            it.copy(
                tab = BrowserTab.FILES,
                path = target,
                selection = emptySet(),
                listing = memory[memoryKey(target, it.query())] ?: ListingState(refreshing = true),
            )
        }
        reload()
    }

    /** Pull-to-refresh, and what a `share.changed` for this folder triggers. */
    fun refresh() {
        when {
            _state.value.tab == BrowserTab.RECENT -> loadRecent()
            _state.value.showingSearchResults -> runSearchNow()
            else -> reload()
        }
    }

    private fun reload() {
        val paired = host ?: return
        loadJob?.cancel()
        moreJob?.cancel()
        val at = _state.value
        val path = at.path
        val query = at.query()
        loadJob = viewModelScope.launch {
            _state.update {
                it.copy(listing = it.listing.copy(refreshing = true, loadingMore = false, error = null, appendError = null))
            }
            if (!_state.value.listing.loaded) {
                repository.cachedFirstPage(paired, shareId, path, query)?.let { cached ->
                    _state.update { s -> s.copy(listing = cached.toListing(loaded = true, cachedAt = null, refreshing = true)) }
                }
            }
            try {
                val page = repository.firstPage(paired, shareId, path, query)
                val listing = page.toListing(loaded = true, cachedAt = page.cachedAtMillis, refreshing = false)
                memory[memoryKey(path, query)] = listing
                _state.update { it.copy(listing = listing, selection = it.selection.intersect(listing.paths(path))) }
            } catch (e: CancellationException) {
                throw e
            } catch (e: GanderError) {
                handleFolderFailure(e)
            }
        }
    }

    private fun handleFolderFailure(e: GanderError) {
        val error = e.toUiError()
        _state.update {
            val listing = it.listing.copy(refreshing = false, loadingMore = false, error = error)
            when (error.kind) {
                UiErrorKind.REVOKED, UiErrorKind.WRONG_COMPUTER -> it.copy(listing = listing, fatal = error)
                else -> it.copy(listing = listing)
            }
        }
    }

    /** Next page by cursor; called when the list scrolls near its end. No-op while one is running. */
    fun loadMore() {
        val s = _state.value
        when {
            s.showingSearchResults -> loadMoreResults()
            s.tab == BrowserTab.FILES -> loadMoreFiles(s)
        }
    }

    /** The footer's Retry after a failed page. */
    fun retryLoadMore() {
        _state.update { it.copy(listing = it.listing.copy(appendError = null)) }
        loadMore()
    }

    private fun loadMoreFiles(s: BrowserUiState) {
        val paired = host ?: return
        val listing = s.listing
        val cursor = listing.nextCursor ?: return
        if (listing.refreshing || listing.loadingMore || listing.appendError != null || listing.cachedAtMillis != null) return
        if (moreJob?.isActive == true) return
        val path = s.path
        val query = s.query()
        moreJob = viewModelScope.launch {
            _state.update { it.copy(listing = it.listing.copy(loadingMore = true, appendError = null)) }
            try {
                val page = repository.nextPage(paired, shareId, path, query, cursor)
                _state.update {
                    val merged = it.listing.entries.mergedWith(page.entries)
                    val listing = it.listing.copy(
                        entries = merged,
                        nextCursor = page.nextCursor,
                        loadingMore = false,
                        truncated = page.truncated,
                        unlistable = page.unlistable,
                    )
                    it.copy(listing = listing)
                }
                memory[memoryKey(path, query)] = _state.value.listing
            } catch (e: CancellationException) {
                throw e
            } catch (e: GanderError) {
                _state.update { it.copy(listing = it.listing.copy(loadingMore = false, appendError = e.toUiError())) }
            }
        }
    }

    // Sort, filter, view mode ---------------------------------------------------------------------------

    /**
     * Picks a sort field: choosing the current field flips the order; a new field starts ascending
     * for names and kinds and descending (newest, largest first) for dates and sizes.
     */
    fun selectSortField(field: SortField) {
        val current = _state.value.sort
        val next = if (field == current.field) {
            current.copy(order = current.order.toggled())
        } else {
            SortSpec(field, if (field == SortField.MTIME || field == SortField.SIZE) SortOrder.DESC else SortOrder.ASC)
        }
        setSort(next)
    }

    fun setSort(sort: SortSpec) {
        if (sort == _state.value.sort) return
        _state.update { it.copy(sort = sort, listing = ListingState(refreshing = true), selection = emptySet()) }
        viewModelScope.launch { prefs.setSort(hostId, shareId, sort) }
        if (_state.value.tab == BrowserTab.FILES) reload()
    }

    fun setKinds(kinds: KindsFilter) {
        if (kinds == _state.value.kinds) return
        _state.update { it.copy(kinds = kinds, listing = ListingState(refreshing = true), selection = emptySet()) }
        if (_state.value.tab == BrowserTab.FILES) reload()
    }

    fun setViewMode(mode: ViewMode) {
        if (mode == _state.value.viewMode) return
        _state.update { it.copy(viewMode = mode) }
        viewModelScope.launch { prefs.setViewMode(hostId, shareId, mode) }
    }

    // Tabs ----------------------------------------------------------------------------------------

    fun selectTab(tab: BrowserTab) {
        if (tab == _state.value.tab) return
        saved[KEY_TAB] = tab.name
        _state.update { it.copy(tab = tab, selection = emptySet()) }
        if (tab == BrowserTab.RECENT && !_state.value.recent.loaded) loadRecent()
    }

    private fun loadRecent() {
        val paired = host ?: return
        recentJob?.cancel()
        recentJob = viewModelScope.launch {
            _state.update { it.copy(recent = it.recent.copy(loading = true, error = null)) }
            try {
                val page = repository.recent(paired)
                // /recent is host-wide; this tab belongs to one share.
                val mine = page.entries.filter { it.share == null || it.share == shareId }
                _state.update {
                    it.copy(recent = RecentState(entries = mine, partial = page.partial, loaded = true))
                }
            } catch (e: CancellationException) {
                throw e
            } catch (e: GanderError) {
                _state.update { it.copy(recent = it.recent.copy(loading = false, error = e.toUiError())) }
            }
        }
    }

    // Search ------------------------------------------------------------------------------------

    fun openSearch() {
        if (_state.value.search.active) return
        _state.update {
            it.copy(
                search = SearchState(active = true, scopePath = if (it.tab == BrowserTab.FILES) it.path else ""),
                selection = emptySet(),
            )
        }
    }

    fun closeSearch() {
        closeSearchState()
        _state.update { it.copy(selection = emptySet()) }
    }

    private fun closeSearchState() {
        searchText.value = ""
        searchMoreJob?.cancel()
        _state.update { it.copy(search = SearchState()) }
    }

    /** Called on every keystroke; the host is asked once typing pauses for 300 ms. */
    fun onSearchTextChange(text: String) {
        if (!_state.value.search.active) return
        _state.update { it.copy(search = it.search.copy(text = text)) }
        searchText.value = text
    }

    /** The "Search inside files" switch: re-runs the search in the new mode when there is text. */
    fun setSearchInFiles(on: Boolean) {
        val state = _state.value
        if (!state.search.active || state.search.inFiles == on || !state.canSearchInside) return
        searchMoreJob?.cancel()
        _state.update { it.copy(search = it.search.copy(inFiles = on)) }
        if (_state.value.search.text.isNotBlank()) runSearchNow()
    }

    private fun runSearchNow() {
        val text = _state.value.search.text.trim()
        searchNowJob?.cancel()
        searchNowJob = viewModelScope.launch { runSearch(text) }
    }

    private suspend fun runSearch(text: String) {
        val paired = host ?: hostReady.await() ?: return
        val scope = _state.value.search.scopePath
        if (!_state.value.search.active) return
        if (text.isEmpty()) {
            searchMoreJob?.cancel()
            _state.update { it.copy(search = it.search.copy(searchedText = null, entries = emptyList(), partial = false, nextCursor = null, searching = false, loadingMore = false, error = null, searchedInFiles = false, via = null)) }
            return
        }
        val inside = _state.value.let { it.search.inFiles && it.canSearchInside }
        _state.update { it.copy(search = it.search.copy(searching = true, error = null)) }
        try {
            val page = repository.search(paired, shareId, scope, text, inside = inside)
            _state.update {
                it.copy(
                    search = it.search.copy(
                        searchedText = text,
                        entries = page.entries,
                        partial = page.partial,
                        nextCursor = page.nextCursor,
                        searching = false,
                        loadingMore = false,
                        searchedInFiles = inside,
                        via = page.via,
                    ),
                    selection = emptySet(),
                )
            }
        } catch (e: CancellationException) {
            throw e
        } catch (e: GanderError) {
            _state.update { it.copy(search = it.search.copy(searching = false, error = e.toUiError())) }
        }
    }

    private fun loadMoreResults() {
        val paired = host ?: return
        val search = _state.value.search
        val cursor = search.nextCursor ?: return
        val text = search.searchedText ?: return
        if (search.searching || search.loadingMore || searchMoreJob?.isActive == true) return
        searchMoreJob = viewModelScope.launch {
            _state.update { it.copy(search = it.search.copy(loadingMore = true, error = null)) }
            try {
                val page = repository.search(paired, shareId, search.scopePath, text, cursor, search.searchedInFiles)
                _state.update {
                    it.copy(
                        search = it.search.copy(
                            entries = it.search.entries.mergedWith(page.entries) { e -> e.path ?: e.name },
                            partial = it.search.partial || page.partial,
                            nextCursor = page.nextCursor,
                            loadingMore = false,
                        ),
                    )
                }
            } catch (e: CancellationException) {
                throw e
            } catch (e: GanderError) {
                _state.update { it.copy(search = it.search.copy(loadingMore = false, error = e.toUiError())) }
            }
        }
    }

    // Entries and selection ---------------------------------------------------------------------------


    /**
     * Tap on [entry]. In selection mode it toggles; a folder opens in place; a file returns its
     * [EntryTarget] for the caller to open (file sheet, WP-25); a link does nothing.
     */
    fun onEntryClick(entry: Entry): EntryTarget? {
        val state = _state.value
        if (state.selecting) {
            toggleSelection(entry)
            return null
        }
        if (!entry.isOpenable()) return null
        val path = state.pathOf(entry)
        if (entry.kind == EntryKind.DIR) {
            openPath(path)
            return null
        }
        return EntryTarget(hostId, shareId, path, entry)
    }

    fun onEntryLongClick(entry: Entry) {
        if (entry.isSelectable()) toggleSelection(entry)
    }

    fun toggleSelection(entry: Entry) {
        if (!entry.isSelectable()) return
        val key = _state.value.pathOf(entry)
        _state.update { it.copy(selection = if (key in it.selection) it.selection - key else it.selection + key) }
    }

    fun clearSelection() {
        _state.update { it.copy(selection = emptySet()) }
    }

    fun selectAll() {
        _state.update { s -> s.copy(selection = s.visibleEntries.filter { it.isSelectable() }.mapTo(LinkedHashSet()) { s.pathOf(it) }) }
    }

    /** The open folder as a target for the folder download (WP-68); the share root is `""`. */
    fun currentFolderTarget(): EntryTarget {
        val s = _state.value
        val name = if (s.path.isEmpty()) s.shareName else s.path.substringAfterLast('/')
        val folder = Entry(
            name = name,
            kind = EntryKind.DIR,
            size = 0,
            mtime = "",
            ctime = null,
            flags = emptyList(),
            thumb = false,
            preview = PreviewKind.NONE,
            stream = StreamDecision.REFUSED,
        )
        return EntryTarget(hostId, shareId, s.path, folder)
    }

    /** The selected files, for the fetch queue (WP-27). Empty when nothing is selected. */
    fun selectedTargets(): List<EntryTarget> {
        val s = _state.value
        return s.visibleEntries.filter { s.pathOf(it) in s.selection }.map { EntryTarget(hostId, shareId, s.pathOf(it), it) }
    }

    /**
     * System back. Returns true when it was consumed here (selection, search, Recent tab, parent
     * folder); false means the screen should be left.
     */
    fun onBack(): Boolean {
        val s = _state.value
        when {
            s.selecting -> clearSelection()
            s.search.active -> closeSearch()
            s.tab != BrowserTab.FILES -> selectTab(BrowserTab.FILES)
            s.path.isNotEmpty() -> openPath(parentPath(s.path))
            else -> return false
        }
        return true
    }

    // Events ------------------------------------------------------------------------------------

    /** Collects `/events` until cancelled (run while the screen is visible): a change in the open folder reloads it. */
    suspend fun watchEvents() {
        val paired = hostReady.await() ?: return
        try {
            repository.events(paired).collect { event ->
                when (event) {
                    is ServerEvent.ShareChanged -> {
                        val s = _state.value
                        if (event.share == shareId && s.tab == BrowserTab.FILES && !s.search.active && event.path == s.path) {
                            reload()
                        }
                    }
                    is ServerEvent.DeviceRevoked ->
                        _state.update { it.copy(fatal = UiError(UiErrorKind.REVOKED)) }
                    else -> Unit
                }
            }
        } catch (e: GanderError) {
            val error = e.toUiError()
            if (error.kind == UiErrorKind.REVOKED || error.kind == UiErrorKind.WRONG_COMPUTER) {
                _state.update { it.copy(fatal = error) }
            }
        }
    }

    // Helpers -----------------------------------------------------------------------------------

    private fun memoryKey(path: String, query: ListQuery) =
        "$path|${query.sort.field.wire}|${query.sort.order.wire}|${query.kinds.wire.orEmpty()}"

    private fun PageResult.toListing(loaded: Boolean, cachedAt: Long?, refreshing: Boolean) = ListingState(
        entries = entries,
        nextCursor = nextCursor,
        loaded = loaded,
        refreshing = refreshing,
        truncated = truncated,
        unlistable = unlistable,
        cachedAtMillis = cachedAt,
        cacheIncomplete = cacheIncomplete,
    )

    private fun ListingState.paths(parent: String): Set<String> = entries.mapTo(HashSet()) { it.fullPath(parent) }

    /** [this] followed by the entries of [more] it does not have yet (a refresh can repeat one). */
    private fun List<Entry>.mergedWith(more: List<Entry>, key: (Entry) -> String = { it.name }): List<Entry> {
        val known = mapTo(HashSet(), key)
        return this + more.filter { known.add(key(it)) }
    }

    companion object {
        const val SEARCH_DEBOUNCE_MS = 300L
        private const val MEMORY_FOLDERS = 24
        private const val KEY_PATH = "path"
        private const val KEY_TAB = "tab"
    }
}
