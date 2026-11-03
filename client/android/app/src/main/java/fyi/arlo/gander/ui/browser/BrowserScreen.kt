package fyi.arlo.gander.ui.browser

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.GridItemSpan
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.foundation.lazy.grid.rememberLazyGridState
import androidx.compose.foundation.lazy.items as listItems
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.PrimaryTabRow
import androidx.compose.material3.RadioButton
import androidx.compose.material3.RadioButtonDefaults
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Tab
import androidx.compose.material3.TabRowDefaults
import androidx.compose.material3.TabRowDefaults.tabIndicatorOffset
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TextField
import androidx.compose.material3.TextFieldDefaults
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.repeatOnLifecycle
import fyi.arlo.gander.R
import fyi.arlo.gander.core.data.UiError
import fyi.arlo.gander.core.data.UiErrorKind
import fyi.arlo.gander.core.data.browse.EntryTarget
import fyi.arlo.gander.core.data.browse.KindsFilter
import fyi.arlo.gander.core.data.browse.SortField
import fyi.arlo.gander.core.data.browse.SortOrder
import fyi.arlo.gander.core.data.browse.SortSpec
import fyi.arlo.gander.core.data.browse.ViewMode
import fyi.arlo.gander.core.net.protocol.Entry
import fyi.arlo.gander.ui.common.CachedChip
import fyi.arlo.gander.ui.common.ErrorPanel
import fyi.arlo.gander.ui.common.text
import fyi.arlo.gander.ui.components.Banner
import fyi.arlo.gander.ui.components.BannerTone
import fyi.arlo.gander.ui.components.ButtonKind
import fyi.arlo.gander.ui.components.ChipStyle
import fyi.arlo.gander.ui.components.ChoiceChip
import fyi.arlo.gander.ui.components.GanderButton
import fyi.arlo.gander.ui.components.GanderChip
import fyi.arlo.gander.ui.components.GanderIconButton
import fyi.arlo.gander.ui.components.GanderProgress
import fyi.arlo.gander.ui.components.GanderTopBar
import fyi.arlo.gander.ui.components.SectionHeader
import fyi.arlo.gander.ui.components.Segmented
import fyi.arlo.gander.ui.components.SegmentOption
import fyi.arlo.gander.ui.theme.GanderIcons
import fyi.arlo.gander.ui.theme.GanderTheme

/** Test tags for the browser (instrumented tests, WP-29). */
object BrowserTags {
    const val LIST = "browser-list"
    const val GRID = "browser-grid"
    const val BREADCRUMB = "browser-breadcrumb"
    const val SEARCH_FIELD = "browser-search-field"
    const val CACHED = "browser-cached"
    const val PARTIAL = "browser-partial"
    const val SEARCH_INSIDE = "browser-search-inside"
    const val NAMES_ONLY = "browser-names-only"
    const val SORT_SHEET = "browser-sort-sheet"
    const val FETCH_SELECTED = "browser-fetch-selected"
    const val MORE = "browser-more"
    const val ZIP = "browser-zip"

    fun entry(path: String) = "entry-$path"
}

/** Everything the browser can be asked to do; the ViewModel provides it, tests pass their own. */
class BrowserActions(
    val onNavigateUp: () -> Unit = {},
    val onEntryClick: (Entry) -> Unit = {},
    val onEntryLongClick: (Entry) -> Unit = {},
    val onCrumbClick: (String) -> Unit = {},
    val onSelectTab: (BrowserTab) -> Unit = {},
    val onOpenSearch: () -> Unit = {},
    val onCloseSearch: () -> Unit = {},
    val onSearchText: (String) -> Unit = {},
    /** The "Search inside files" switch, offered when the share is indexed (WP-66). */
    val onSearchInFiles: (Boolean) -> Unit = {},
    val onSortField: (SortField) -> Unit = {},
    val onKinds: (KindsFilter) -> Unit = {},
    val onViewMode: (ViewMode) -> Unit = {},
    val onRefresh: () -> Unit = {},
    val onLoadMore: () -> Unit = {},
    val onRetryLoadMore: () -> Unit = {},
    val onClearSelection: () -> Unit = {},
    val onSelectAll: () -> Unit = {},
    /** Null while nothing consumes the fetch queue (WP-27): the Fetch action is then not offered. */
    val onFetchSelected: (() -> Unit)? = null,
    /** Null while nothing consumes the folder download (WP-68): the overflow item is then not offered. */
    val onDownloadZip: (() -> Unit)? = null,
)

/**
 * The browser of one share. [onOpenFile] receives a tapped file (the file sheet, WP-25);
 * [onFetchSelected] receives the multi-selection (the fetch queue, WP-27) and, while it is null,
 * the Fetch action is hidden. [highlightedPath] is the file shown in the tablet's detail pane.
 */
@Composable
fun BrowserScreen(
    viewModel: BrowserViewModel,
    onNavigateUp: () -> Unit,
    onOpenFile: (EntryTarget) -> Unit,
    onFetchSelected: ((List<EntryTarget>) -> Unit)?,
    modifier: Modifier = Modifier,
    highlightedPath: String? = null,
    onZipFolder: ((EntryTarget) -> Unit)? = null,
) {
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    val lifecycleOwner = LocalLifecycleOwner.current
    LaunchedEffect(viewModel, lifecycleOwner) {
        lifecycleOwner.lifecycle.repeatOnLifecycle(Lifecycle.State.STARTED) { viewModel.watchEvents() }
    }
    BackHandler(enabled = state.handlesBack) { viewModel.onBack() }

    val currentOpenFile by rememberUpdatedState(onOpenFile)
    val currentFetch by rememberUpdatedState(onFetchSelected)
    val currentZip by rememberUpdatedState(onZipFolder)
    val actions = remember(viewModel) {
        BrowserActions(
            onNavigateUp = onNavigateUp,
            onEntryClick = { entry -> viewModel.onEntryClick(entry)?.let { currentOpenFile(it) } },
            onEntryLongClick = viewModel::onEntryLongClick,
            onCrumbClick = viewModel::openPath,
            onSelectTab = viewModel::selectTab,
            onOpenSearch = viewModel::openSearch,
            onCloseSearch = viewModel::closeSearch,
            onSearchText = viewModel::onSearchTextChange,
            onSearchInFiles = viewModel::setSearchInFiles,
            onSortField = viewModel::selectSortField,
            onKinds = viewModel::setKinds,
            onViewMode = viewModel::setViewMode,
            onRefresh = viewModel::refresh,
            onLoadMore = viewModel::loadMore,
            onRetryLoadMore = viewModel::retryLoadMore,
            onClearSelection = viewModel::clearSelection,
            onSelectAll = viewModel::selectAll,
            onFetchSelected = if (onFetchSelected == null) {
                null
            } else {
                { currentFetch?.invoke(viewModel.selectedTargets()) }
            },
            onDownloadZip = if (onZipFolder == null) {
                null
            } else {
                { currentZip?.invoke(viewModel.currentFolderTarget()) }
            },
        )
    }
    BrowserContent(state, actions, modifier, highlightedPath)
}

/** Stateless browser (mockups 12 to 16); the Compose UI tests render it with a fixed [BrowserUiState]. */
@Composable
fun BrowserContent(
    state: BrowserUiState,
    actions: BrowserActions,
    modifier: Modifier = Modifier,
    highlightedPath: String? = null,
) {
    var sortSheet by remember { mutableStateOf(false) }
    Scaffold(
        modifier = modifier,
        containerColor = GanderTheme.colors.bg,
        topBar = { BrowserTopBar(state, actions) },
        bottomBar = { if (state.selecting && actions.onFetchSelected != null) SelectionBar(state, actions) },
    ) { innerPadding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(innerPadding),
        ) {
            if (state.ready && state.fatal == null) {
                if (state.tab == BrowserTab.FILES && !state.search.active) Breadcrumb(state, actions)
                if (state.showTabs && !state.selecting) BrowserTabs(state, actions)
                if (state.tab == BrowserTab.FILES && !state.search.active && !state.selecting) {
                    SortChips(state, onOpen = { sortSheet = true })
                }
            }
            Box(
                modifier = Modifier
                    .weight(1f)
                    .fillMaxWidth(),
            ) {
                when {
                    state.fatal != null -> ErrorPanel(
                        error = state.fatal,
                        onRetry = if (state.fatal.kind == UiErrorKind.REVOKED ||
                            state.fatal.kind == UiErrorKind.WRONG_COMPUTER ||
                            state.fatal.kind == UiErrorKind.NOT_PAIRED
                        ) {
                            null
                        } else {
                            actions.onRefresh
                        },
                        modifier = Modifier.fillMaxSize(),
                    )
                    !state.ready -> SkeletonRows()
                    state.search.active -> SearchBody(state, actions, highlightedPath)
                    state.tab == BrowserTab.RECENT -> RecentBody(state, actions, highlightedPath)
                    else -> FilesBody(state, actions, highlightedPath)
                }
            }
        }
    }
    if (sortSheet) SortSheet(state, actions, onDismiss = { sortSheet = false })
}

// Top bar -------------------------------------------------------------------------------------------

@Composable
private fun BrowserTopBar(state: BrowserUiState, actions: BrowserActions) {
    val c = GanderTheme.colors
    when {
        state.selecting -> {
            val bytes = state.visibleEntries.filter { state.pathOf(it) in state.selection }.sumOf { it.size }
            GanderTopBar(
                title = pluralStringResource(R.plurals.selection_count, state.selection.size, state.selection.size),
                subtitle = formatFileSize(bytes),
                navigationIcon = GanderIcons.Close,
                navigationDescription = stringResource(R.string.selection_clear),
                onNavigate = actions.onClearSelection,
                containerColor = c.accentContainer,
                contentColor = c.onAccentContainer,
                subtitleColor = c.onAccentContainer,
                modifier = Modifier.background(c.accentContainer).statusBarsPadding(),
            ) {
                GanderIconButton(
                    icon = GanderIcons.Check,
                    contentDescription = stringResource(R.string.selection_all),
                    onClick = actions.onSelectAll,
                    tint = c.onAccentContainer,
                )
            }
        }

        state.search.active -> Row(
            modifier = Modifier
                .statusBarsPadding()
                .fillMaxWidth()
                .heightIn(min = 64.dp)
                .padding(horizontal = 4.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            GanderIconButton(
                icon = GanderIcons.Back,
                contentDescription = stringResource(R.string.browser_search_close),
                onClick = actions.onCloseSearch,
            )
            SearchField(state, actions, Modifier.weight(1f))
        }

        else -> GanderTopBar(
            title = state.crumbs.lastOrNull()?.label ?: state.shareName,
            subtitle = if (state.hostName.isEmpty()) {
                null
            } else if (state.path.isEmpty()) {
                state.hostName
            } else {
                stringResource(R.string.browser_where, state.hostName, state.shareName)
            },
            navigationIcon = GanderIcons.Back,
            navigationDescription = stringResource(R.string.action_back),
            onNavigate = actions.onNavigateUp,
            modifier = Modifier.statusBarsPadding(),
        ) {
            if (state.ready && state.fatal == null) {
                GanderIconButton(GanderIcons.Search, stringResource(R.string.browser_search), actions.onOpenSearch)
                if (state.tab == BrowserTab.FILES) {
                    val toGrid = state.viewMode == ViewMode.LIST
                    GanderIconButton(
                        icon = if (toGrid) GanderIcons.Grid else GanderIcons.List,
                        contentDescription = stringResource(if (toGrid) R.string.browser_view_grid else R.string.browser_view_list),
                        onClick = { actions.onViewMode(if (toGrid) ViewMode.GRID else ViewMode.LIST) },
                    )
                    val zip = actions.onDownloadZip
                    if (zip != null && state.canZip) FolderMenu(onDownloadZip = zip)
                }
            }
        }
    }
}

/** The open folder's overflow menu (WP-68): "Download folder as zip", explicit and confirmed next. */
@Composable
private fun FolderMenu(onDownloadZip: () -> Unit) {
    var open by remember { mutableStateOf(false) }
    Box {
        GanderIconButton(
            GanderIcons.More,
            stringResource(R.string.action_more),
            { open = true },
            modifier = Modifier.testTag(BrowserTags.MORE),
        )
        DropdownMenu(expanded = open, onDismissRequest = { open = false }, containerColor = GanderTheme.colors.surface2) {
            DropdownMenuItem(
                text = { Text(stringResource(R.string.zip_menu)) },
                leadingIcon = { Icon(GanderIcons.Archive, contentDescription = null) },
                onClick = {
                    open = false
                    onDownloadZip()
                },
                modifier = Modifier.testTag(BrowserTags.ZIP),
            )
        }
    }
}

@Composable
private fun SearchField(state: BrowserUiState, actions: BrowserActions, modifier: Modifier = Modifier) {
    val focus = remember { FocusRequester() }
    LaunchedEffect(Unit) { focus.requestFocus() }
    val scopeLabel = state.crumbs.lastOrNull { it.path == state.search.scopePath }?.label ?: state.shareName
    val c = GanderTheme.colors
    TextField(
        value = state.search.text,
        onValueChange = actions.onSearchText,
        modifier = modifier
            .focusRequester(focus)
            .testTag(BrowserTags.SEARCH_FIELD),
        placeholder = { Text(stringResource(R.string.browser_search_hint, scopeLabel), color = c.text3) },
        singleLine = true,
        keyboardOptions = KeyboardOptions(imeAction = ImeAction.Search),
        trailingIcon = {
            if (state.search.text.isNotEmpty()) {
                IconButton(onClick = { actions.onSearchText("") }) {
                    Icon(GanderIcons.Close, contentDescription = stringResource(R.string.browser_search_clear))
                }
            }
        },
        shape = MaterialTheme.shapes.extraLarge,
        colors = TextFieldDefaults.colors(
            focusedContainerColor = c.surface2,
            unfocusedContainerColor = c.surface2,
            focusedIndicatorColor = c.surface2,
            unfocusedIndicatorColor = c.surface2,
            cursorColor = c.text,
        ),
    )
}

// Tabs, breadcrumb, sort chips -----------------------------------------------------------------------

@Composable
private fun BrowserTabs(state: BrowserUiState, actions: BrowserActions) {
    val c = GanderTheme.colors
    PrimaryTabRow(
        selectedTabIndex = state.tab.ordinal,
        containerColor = c.bg,
        contentColor = c.text,
        indicator = {
            TabRowDefaults.PrimaryIndicator(
                modifier = Modifier.tabIndicatorOffset(state.tab.ordinal, matchContentSize = true),
                color = c.accent,
                width = androidx.compose.ui.unit.Dp.Unspecified,
                height = 3.dp,
            )
        },
        divider = { HorizontalDivider(color = c.line) },
    ) {
        listOf(BrowserTab.FILES to R.string.tab_files, BrowserTab.RECENT to R.string.tab_recent).forEach { (tab, label) ->
            val on = state.tab == tab
            Tab(
                selected = on,
                onClick = { actions.onSelectTab(tab) },
                selectedContentColor = c.text,
                unselectedContentColor = c.text2,
                text = { Text(stringResource(label), fontWeight = if (on) FontWeight.SemiBold else FontWeight.Normal) },
            )
        }
    }
}

@Composable
private fun Breadcrumb(state: BrowserUiState, actions: BrowserActions) {
    val crumbs = state.crumbs
    val listState = rememberLazyListState()
    val c = GanderTheme.colors
    LaunchedEffect(crumbs.size) { listState.scrollToItem(crumbs.lastIndex) }
    LazyRow(
        state = listState,
        modifier = Modifier
            .fillMaxWidth()
            .testTag(BrowserTags.BREADCRUMB),
        contentPadding = PaddingValues(start = 10.dp, end = 16.dp, bottom = 2.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        itemsIndexed(crumbs, key = { _, crumb -> crumb.path }) { index, crumb ->
            val last = index == crumbs.lastIndex
            Row(verticalAlignment = Alignment.CenterVertically) {
                if (index > 0) {
                    Icon(GanderIcons.Chevron, contentDescription = null, tint = c.text3, modifier = Modifier.size(14.dp))
                }
                TextButton(
                    onClick = { actions.onCrumbClick(crumb.path) },
                    enabled = !last,
                    contentPadding = PaddingValues(horizontal = 6.dp),
                ) {
                    Text(
                        text = crumb.label,
                        style = MaterialTheme.typography.bodyMedium.copy(fontSize = 13.sp),
                        fontWeight = if (last) FontWeight.SemiBold else FontWeight.Normal,
                        color = if (last) c.text else c.text3,
                        maxLines = 1,
                    )
                }
            }
        }
    }
}

@Composable
private fun sortOrderLabel(sort: SortSpec): String {
    val asc = sort.order == SortOrder.ASC
    return stringResource(
        when (sort.field) {
            SortField.NAME, SortField.KIND -> if (asc) R.string.sort_order_az else R.string.sort_order_za
            SortField.MTIME -> if (asc) R.string.sort_order_oldest else R.string.sort_order_newest
            SortField.SIZE -> if (asc) R.string.sort_order_smallest else R.string.sort_order_largest
        },
    )
}

private fun sortLabel(field: SortField): Int = when (field) {
    SortField.NAME -> R.string.sort_name
    SortField.MTIME -> R.string.sort_modified
    SortField.SIZE -> R.string.sort_size
    SortField.KIND -> R.string.sort_kind
}

private fun kindsLabel(kinds: KindsFilter): Int = when (kinds) {
    KindsFilter.ALL -> R.string.filter_all
    KindsFilter.FILES -> R.string.filter_files
    KindsFilter.FOLDERS -> R.string.filter_folders
}

/** "Name, A to Z" and the kinds filter, both opening the sort and filter sheet (mockup 14). */
@Composable
private fun SortChips(state: BrowserUiState, onOpen: () -> Unit) {
    val sortText = stringResource(R.string.browser_sort_chip, stringResource(sortLabel(state.sort.field)), sortOrderLabel(state.sort))
    val sortDescription = stringResource(R.string.browser_sort)
    val filterText = stringResource(kindsLabel(state.kinds))
    val filterDescription = stringResource(R.string.browser_filter)
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .horizontalScroll(rememberScrollState())
            .padding(start = 20.dp, end = 20.dp, top = 6.dp, bottom = 4.dp),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        GanderChip(
            text = sortText,
            icon = GanderIcons.Sort,
            style = ChipStyle.OUTLINED,
            selected = false,
            onClick = onOpen,
            modifier = Modifier.semantics {
                contentDescription = sortDescription
                stateDescription = sortText
            },
        )
        GanderChip(
            text = filterText,
            icon = GanderIcons.Filter,
            style = if (state.kinds == KindsFilter.ALL) ChipStyle.OUTLINED else ChipStyle.SELECTED,
            selected = state.kinds != KindsFilter.ALL,
            onClick = onOpen,
            modifier = Modifier.semantics {
                contentDescription = filterDescription
                stateDescription = filterText
            },
        )
        state.listing.cachedAtMillis?.let { CachedChip(it) }
    }
}

/** Sort and filter (mockup 14): sort field radios, the order, files and folders. Remembered per share. */
@OptIn(ExperimentalMaterial3Api::class, ExperimentalLayoutApi::class)
@Composable
private fun SortSheet(state: BrowserUiState, actions: BrowserActions, onDismiss: () -> Unit) {
    val c = GanderTheme.colors
    ModalBottomSheet(
        onDismissRequest = onDismiss,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
        containerColor = c.surface,
        scrimColor = c.scrim,
        modifier = Modifier.testTag(BrowserTags.SORT_SHEET),
    ) {
        Column(Modifier.navigationBarsPadding().padding(bottom = 16.dp)) {
            Text(
                stringResource(R.string.browser_sort_title),
                style = MaterialTheme.typography.titleLarge.copy(fontSize = 18.sp),
                modifier = Modifier.padding(horizontal = 24.dp, vertical = 4.dp),
            )
            SectionHeader(stringResource(R.string.browser_sort_by), padding = PaddingValues(start = 20.dp, end = 20.dp, top = 8.dp, bottom = 4.dp))
            SortField.entries.forEach { field ->
                val on = field == state.sort.field
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .selectable(selected = on, role = Role.RadioButton) { actions.onSortField(field) }
                        .heightIn(min = 48.dp)
                        .padding(horizontal = 20.dp),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(14.dp),
                ) {
                    RadioButton(
                        selected = on,
                        onClick = null,
                        colors = RadioButtonDefaults.colors(selectedColor = c.accent, unselectedColor = c.outline),
                    )
                    Text(stringResource(sortLabel(field)), style = MaterialTheme.typography.bodyLarge.copy(fontSize = 15.sp))
                }
            }
            val asc = state.sort.order == SortOrder.ASC
            val ascLabel = sortOrderLabel(state.sort.copy(order = SortOrder.ASC))
            val descLabel = sortOrderLabel(state.sort.copy(order = SortOrder.DESC))
            Segmented(
                options = listOf(SegmentOption(ascLabel, if (asc) GanderIcons.Check else null), SegmentOption(descLabel, if (!asc) GanderIcons.Check else null)),
                selectedIndex = if (asc) 0 else 1,
                // Choosing the current field again flips the order (the ViewModel's rule).
                onSelect = { index -> if ((index == 0) != asc) actions.onSortField(state.sort.field) },
                modifier = Modifier.padding(start = 20.dp, top = 6.dp, bottom = 4.dp),
            )
            SectionHeader(stringResource(R.string.browser_show))
            FlowRow(
                modifier = Modifier.padding(horizontal = 20.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                KindsFilter.entries.forEach { kinds ->
                    ChoiceChip(
                        text = stringResource(kindsLabel(kinds)),
                        selected = kinds == state.kinds,
                        onClick = { actions.onKinds(kinds) },
                    )
                }
            }
            Text(
                stringResource(R.string.browser_sort_note),
                style = MaterialTheme.typography.bodySmall.copy(fontSize = 12.5.sp),
                color = c.text3,
                modifier = Modifier.padding(start = 20.dp, end = 20.dp, top = 12.dp),
            )
        }
    }
}

/** The multi-select bar (mockup 15): what cannot be selected, and Fetch with the count. */
@Composable
private fun SelectionBar(state: BrowserUiState, actions: BrowserActions) {
    val c = GanderTheme.colors
    val fetch = actions.onFetchSelected ?: return
    Surface(color = c.surface2) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .navigationBarsPadding()
                .padding(horizontal = 16.dp, vertical = 12.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text(
                stringResource(if (state.canFetch) R.string.selection_folders_note else R.string.selection_fetch_denied),
                style = MaterialTheme.typography.bodySmall.copy(fontSize = 12.5.sp),
                color = if (state.canFetch) c.text2 else c.bad,
                modifier = Modifier.weight(1f),
            )
            GanderButton(
                text = pluralStringResource(R.plurals.selection_fetch_count, state.selection.size, state.selection.size),
                icon = GanderIcons.Fetch,
                onClick = fetch,
                enabled = state.canFetch,
                kind = ButtonKind.PRIMARY,
                modifier = Modifier.testTag(BrowserTags.FETCH_SELECTED),
            )
        }
    }
}

// Bodies --------------------------------------------------------------------------------------------

/** Loading: skeleton rows rather than a spinner (mockup 12, loading state). */
@Composable
private fun SkeletonRows() {
    val c = GanderTheme.colors
    Column(Modifier.fillMaxSize()) {
        GanderProgress(fraction = null, modifier = Modifier.padding(horizontal = 20.dp, vertical = 8.dp))
        repeat(6) {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .heightIn(min = 64.dp)
                    .padding(start = 20.dp, end = 16.dp, top = 10.dp, bottom = 10.dp),
                horizontalArrangement = Arrangement.spacedBy(14.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Box(Modifier.size(42.dp).background(c.surface2, MaterialTheme.shapes.small))
                Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    Box(Modifier.size(width = 180.dp, height = 12.dp).background(c.surface2, MaterialTheme.shapes.extraSmall))
                    Box(Modifier.size(width = 110.dp, height = 10.dp).background(c.surface2, MaterialTheme.shapes.extraSmall))
                }
            }
        }
    }
}

/** A banner line inside the browser: muted info, or an error with Retry. */
@Composable
private fun InlineNote(text: String, modifier: Modifier = Modifier, isError: Boolean = false, retry: (() -> Unit)? = null) {
    Banner(
        tone = if (isError) BannerTone.BAD else BannerTone.INFO,
        modifier = modifier.padding(horizontal = 16.dp, vertical = 4.dp),
    ) {
        Text(text, style = MaterialTheme.typography.bodyMedium)
        if (retry != null) {
            TextButton(onClick = retry, contentPadding = PaddingValues(horizontal = 0.dp)) {
                Text(stringResource(R.string.action_retry), color = GanderTheme.colors.text, fontWeight = FontWeight.SemiBold)
            }
        }
    }
}

/** The Files tab: the open folder with its snapshot, error and paging states. */
@Composable
private fun FilesBody(state: BrowserUiState, actions: BrowserActions, highlightedPath: String?) {
    val listing = state.listing
    Column(modifier = Modifier.fillMaxSize()) {
        listing.cachedAtMillis?.let {
            Banner(
                tone = BannerTone.WARN,
                icon = GanderIcons.Cloud,
                title = stringResource(R.string.browser_offline_title, state.hostName.ifEmpty { state.shareName }),
                modifier = Modifier
                    .padding(horizontal = 16.dp, vertical = 4.dp)
                    .testTag(BrowserTags.CACHED),
            ) {
                Text(stringResource(R.string.browser_offline_body), style = MaterialTheme.typography.bodyMedium)
                Text(
                    stringResource(R.string.browser_offline_fix),
                    style = MaterialTheme.typography.bodyMedium.copy(fontWeight = FontWeight.SemiBold),
                    modifier = Modifier.padding(top = 6.dp),
                )
            }
        }
        // A failed refresh over entries that are already on screen: keep them, say why.
        if (listing.error != null && listing.loaded && listing.cachedAtMillis == null) {
            InlineNote(
                text = stringResource(R.string.browser_refresh_failed) + " " + listing.error.text(),
                isError = true,
                retry = actions.onRefresh,
            )
        }
        if (listing.refreshing) GanderProgress(fraction = null)
        Box(modifier = Modifier.weight(1f)) {
            when {
                listing.error != null && !listing.loaded ->
                    ErrorPanel(listing.error, onRetry = actions.onRefresh, modifier = Modifier.fillMaxSize())
                !listing.loaded -> SkeletonRows()
                listing.entries.isEmpty() -> EmptyMessage(
                    stringResource(
                        if (state.kinds == KindsFilter.ALL) R.string.browser_empty_folder else R.string.browser_empty_filtered,
                    ),
                    onRefresh = actions.onRefresh,
                )
                else -> EntryCollection(
                    state = state,
                    entries = listing.entries,
                    showFolder = false,
                    canLoadMore = listing.nextCursor != null && listing.appendError == null,
                    actions = actions,
                    highlightedPath = highlightedPath,
                    footer = { ListingFooter(state, actions) },
                )
            }
        }
    }
}

@Composable
private fun ListingFooter(state: BrowserUiState, actions: BrowserActions) {
    val listing = state.listing
    Column(modifier = Modifier.fillMaxWidth(), horizontalAlignment = Alignment.CenterHorizontally) {
        if (listing.loadingMore) {
            GanderProgress(fraction = null, modifier = Modifier.padding(horizontal = 20.dp, vertical = 16.dp))
        }
        listing.appendError?.let {
            InlineNote(
                text = stringResource(R.string.browser_page_failed) + " " + it.text(),
                isError = true,
                retry = actions.onRetryLoadMore,
            )
        }
        if (listing.truncated) {
            InlineNote(pluralStringResource(R.plurals.browser_truncated, listing.entries.size, listing.entries.size))
        }
        if (listing.unlistable > 0) {
            InlineNote(pluralStringResource(R.plurals.browser_unlistable, listing.unlistable, listing.unlistable))
        }
        if (listing.cacheIncomplete && listing.cachedAtMillis != null) {
            Text(
                stringResource(R.string.browser_cache_incomplete),
                style = MaterialTheme.typography.bodySmall.copy(fontSize = 12.5.sp),
                color = GanderTheme.colors.text3,
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 20.dp, vertical = 12.dp),
            )
        }
    }
}

@Composable
private fun RecentBody(state: BrowserUiState, actions: BrowserActions, highlightedPath: String?) {
    val recent = state.recent
    Column(modifier = Modifier.fillMaxSize()) {
        if (recent.loading) GanderProgress(fraction = null)
        if (recent.partial) {
            InlineNote(stringResource(R.string.browser_recent_partial), modifier = Modifier.testTag(BrowserTags.PARTIAL))
        }
        Box(modifier = Modifier.weight(1f)) {
            when {
                recent.error != null && !recent.loaded ->
                    ErrorPanel(recent.error, onRetry = actions.onRefresh, modifier = Modifier.fillMaxSize())
                !recent.loaded -> SkeletonRows()
                recent.entries.isEmpty() ->
                    EmptyMessage(stringResource(R.string.browser_recent_empty), onRefresh = actions.onRefresh)
                else -> EntryCollection(
                    state = state,
                    entries = recent.entries,
                    showFolder = true,
                    canLoadMore = false,
                    actions = actions,
                    highlightedPath = highlightedPath,
                    footer = {},
                )
            }
        }
    }
}

@Composable
private fun SearchBody(state: BrowserUiState, actions: BrowserActions, highlightedPath: String?) {
    val search = state.search
    Column(modifier = Modifier.fillMaxSize()) {
        if (state.canSearchInside) SearchInsideChip(search.inFiles, actions.onSearchInFiles)
        if (search.searching || search.loadingMore) GanderProgress(fraction = null)
        if (search.searchedText != null && search.namesOnly) {
            InlineNote(stringResource(R.string.browser_search_names_only), modifier = Modifier.testTag(BrowserTags.NAMES_ONLY))
        }
        if (search.searchedText != null && search.partial) {
            InlineNote(stringResource(R.string.browser_search_partial), modifier = Modifier.testTag(BrowserTags.PARTIAL))
        }
        Box(modifier = Modifier.weight(1f)) {
            when {
                search.error != null && search.searchedText == null ->
                    ErrorPanel(search.error, onRetry = actions.onRefresh, modifier = Modifier.fillMaxSize())
                search.searchedText == null && search.text.isBlank() ->
                    EmptyMessage(
                        stringResource(if (search.inFiles) R.string.browser_search_prompt_inside else R.string.browser_search_prompt),
                        onRefresh = null,
                    )
                search.searchedText == null -> EmptyMessage(stringResource(R.string.browser_searching), onRefresh = null)
                search.entries.isEmpty() ->
                    EmptyMessage(stringResource(R.string.browser_search_empty, search.searchedText), onRefresh = null)
                else -> EntryCollection(
                    state = state,
                    entries = search.entries,
                    showFolder = true,
                    canLoadMore = search.nextCursor != null && search.error == null,
                    actions = actions,
                    highlightedPath = highlightedPath,
                    footer = { SearchFooter(search.error, actions) },
                )
            }
        }
    }
}

/** "Search inside files" (WP-66): a chip under the search field, selected while it is on. */
@Composable
private fun SearchInsideChip(on: Boolean, onChange: (Boolean) -> Unit) {
    val text = stringResource(R.string.browser_search_inside)
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(start = 20.dp, end = 20.dp, top = 6.dp, bottom = 4.dp),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        GanderChip(
            text = text,
            icon = GanderIcons.File,
            style = if (on) ChipStyle.SELECTED else ChipStyle.OUTLINED,
            selected = on,
            onClick = { onChange(!on) },
            modifier = Modifier.testTag(BrowserTags.SEARCH_INSIDE),
        )
    }
}

@Composable
private fun SearchFooter(error: UiError?, actions: BrowserActions) {
    if (error != null) {
        InlineNote(
            text = stringResource(R.string.browser_page_failed) + " " + error.text(),
            isError = true,
            retry = actions.onLoadMore,
        )
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun EmptyMessage(text: String, onRefresh: (() -> Unit)?) {
    val content: @Composable () -> Unit = {
        Column(
            modifier = Modifier
                .fillMaxSize()
                .verticalScroll(rememberScrollState())
                .padding(32.dp),
            verticalArrangement = Arrangement.Center,
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            Icon(GanderIcons.Folder, contentDescription = null, tint = GanderTheme.colors.text3, modifier = Modifier.size(40.dp))
            Text(
                text,
                style = MaterialTheme.typography.bodyLarge,
                color = GanderTheme.colors.text2,
                textAlign = TextAlign.Center,
                modifier = Modifier.padding(top = 12.dp),
            )
        }
    }
    if (onRefresh == null) {
        content()
    } else {
        PullToRefreshBox(isRefreshing = false, onRefresh = onRefresh, modifier = Modifier.fillMaxSize()) { content() }
    }
}

// The list and the grid -----------------------------------------------------------------------------

private const val PREFETCH_DISTANCE = 10

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun EntryCollection(
    state: BrowserUiState,
    entries: List<Entry>,
    showFolder: Boolean,
    canLoadMore: Boolean,
    actions: BrowserActions,
    highlightedPath: String?,
    footer: @Composable () -> Unit,
) {
    var pulled by remember { mutableStateOf(false) }
    val busy = state.listing.refreshing || state.recent.loading || state.search.searching
    LaunchedEffect(busy) { if (!busy) pulled = false }

    val displays = remember(entries, state.path, state.selection, showFolder, highlightedPath) {
        entries.map { entry ->
            val path = state.pathOf(entry)
            EntryDisplay(
                entry,
                path,
                selected = path in state.selection,
                selecting = state.selecting,
                showFolder = showFolder,
                highlighted = path == highlightedPath,
            )
        }
    }
    val grid = state.viewMode == ViewMode.GRID

    PullToRefreshBox(
        isRefreshing = pulled && busy,
        onRefresh = {
            pulled = true
            actions.onRefresh()
        },
        modifier = Modifier.fillMaxSize(),
    ) {
        if (grid) {
            val gridState = rememberLazyGridState()
            LoadMoreEffect(
                key = displays.size,
                enabled = canLoadMore,
                threshold = displays.size - PREFETCH_DISTANCE,
                lastVisibleIndex = { gridState.layoutInfo.visibleItemsInfo.lastOrNull()?.index ?: -1 },
                onLoadMore = actions.onLoadMore,
            )
            ThumbnailPrefetchEffect(displays, CELL_GLYPH_SIZE) {
                gridState.layoutInfo.visibleItemsInfo.lastOrNull()?.index ?: -1
            }
            LazyVerticalGrid(
                columns = GridCells.Adaptive(minSize = 156.dp),
                state = gridState,
                modifier = Modifier
                    .fillMaxSize()
                    .testTag(BrowserTags.GRID),
                contentPadding = PaddingValues(horizontal = 16.dp, vertical = 4.dp),
                horizontalArrangement = Arrangement.spacedBy(10.dp),
                verticalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                items(displays, key = { it.path }) { display ->
                    EntryGridCell(
                        display = display,
                        onClick = { actions.onEntryClick(display.entry) },
                        onLongClick = { actions.onEntryLongClick(display.entry) },
                    )
                }
                item(span = { GridItemSpan(maxLineSpan) }) { footer() }
            }
        } else {
            val listState = rememberLazyListState()
            LoadMoreEffect(
                key = displays.size,
                enabled = canLoadMore,
                threshold = displays.size - PREFETCH_DISTANCE,
                lastVisibleIndex = { listState.layoutInfo.visibleItemsInfo.lastOrNull()?.index ?: -1 },
                onLoadMore = actions.onLoadMore,
            )
            ThumbnailPrefetchEffect(displays, ROW_GLYPH_SIZE) {
                listState.layoutInfo.visibleItemsInfo.lastOrNull()?.index ?: -1
            }
            LazyColumn(
                state = listState,
                modifier = Modifier
                    .fillMaxSize()
                    .testTag(BrowserTags.LIST),
            ) {
                listItems(displays, key = { it.path }) { display ->
                    EntryRow(
                        display = display,
                        onClick = { actions.onEntryClick(display.entry) },
                        onLongClick = { actions.onEntryLongClick(display.entry) },
                    )
                }
                item { footer() }
            }
        }
    }
}

/**
 * Calls [onLoadMore] when the last visible item is within [PREFETCH_DISTANCE] of the end (cursor
 * paging). Re-evaluates when [key] (the item count) changes, so a page that leaves the end still
 * in view asks for the next one.
 */
@Composable
private fun LoadMoreEffect(
    key: Any?,
    enabled: Boolean,
    threshold: Int,
    lastVisibleIndex: () -> Int,
    onLoadMore: () -> Unit,
) {
    val currentLoadMore by rememberUpdatedState(onLoadMore)
    LaunchedEffect(key, enabled, threshold) {
        if (!enabled) return@LaunchedEffect
        snapshotFlow(lastVisibleIndex).collect { last ->
            if (last >= threshold.coerceAtLeast(0)) currentLoadMore()
        }
    }
}
