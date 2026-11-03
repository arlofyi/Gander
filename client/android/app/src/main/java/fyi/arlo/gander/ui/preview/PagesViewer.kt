package fyi.arlo.gander.ui.preview

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.pager.HorizontalPager
import androidx.compose.foundation.pager.rememberPagerState
import androidx.compose.foundation.BorderStroke
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import coil3.ImageLoader
import fyi.arlo.gander.R
import fyi.arlo.gander.ui.theme.GanderShape
import fyi.arlo.gander.ui.theme.Immersive
import kotlin.math.ceil

/**
 * The pages viewer for PDF and Office previews (client spec, "Preview: pages"): a horizontal
 * pager of page images with pinch zoom, a page counter and the "Fetch this file" shortcut in the
 * top bar. [PagesViewModel] decides which pages are requested and prefetched.
 */
@Composable
fun PagesViewerScreen(
    viewModel: PagesViewModel,
    title: String,
    imageLoader: ImageLoader?,
    onBack: () -> Unit,
    onFetch: (() -> Unit)?,
    modifier: Modifier = Modifier,
) {
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    val total = state.pageCount
    PreviewScaffold(
        title = title,
        onBack = onBack,
        onFetch = onFetch,
        modifier = modifier,
        subtitle = total?.let { stringResource(R.string.preview_page_description, state.currentPage + 1, it) },
        footer = {
            PreviewRail(
                caption = stringResource(R.string.rail_pages, state.hostName),
                trailing = stringResource(R.string.preview_pinch_hint),
            )
        },
    ) { padding ->
        Box(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding),
            contentAlignment = Alignment.Center,
        ) {
            when (val info = state.info) {
                is PagesInfoState.Loading -> PreviewProgress(
                    stringResource(
                        if (info.waiting) R.string.preview_loading_info_busy else R.string.preview_loading_info,
                        state.hostName,
                    ),
                )
                is PagesInfoState.Failed -> PreviewFailurePanel(
                    failure = info.failure,
                    onRetry = viewModel::retryInfo,
                    onFetch = onFetch,
                )
                is PagesInfoState.Ready -> PagesPager(state, info, viewModel, imageLoader, onFetch)
            }
        }
    }
}

@Composable
private fun PagesPager(
    state: PagesUiState,
    info: PagesInfoState.Ready,
    viewModel: PagesViewModel,
    imageLoader: ImageLoader?,
    onFetch: (() -> Unit)?,
) {
    val count = state.shownPageCount
    val pagerState = rememberPagerState(initialPage = state.currentPage.coerceIn(0, count - 1)) { count }
    // Zoomed in, a drag pans the page; the pager only swipes at 1x.
    var zoomed by remember { mutableStateOf(false) }
    LaunchedEffect(pagerState, viewModel) {
        snapshotFlow { pagerState.settledPage }.collect { page ->
            zoomed = false
            viewModel.onPageSettled(page)
        }
    }
    BoxWithConstraints(modifier = Modifier.fillMaxSize()) {
        val widthPx = constraints.maxWidth
        LaunchedEffect(widthPx, viewModel) { viewModel.onViewport(widthPx) }
        HorizontalPager(
            state = pagerState,
            modifier = Modifier
                .fillMaxSize()
                .testTag(PreviewTags.PAGER),
            userScrollEnabled = !zoomed,
            key = { it },
        ) { index ->
            PagesPage(
                index = index,
                state = state,
                info = info,
                viewModel = viewModel,
                imageLoader = imageLoader,
                onFetch = onFetch,
                onZoomChanged = { if (index == pagerState.settledPage) zoomed = it },
            )
        }
        val total = state.pageCount
        if (total != null && total > 1) {
            Surface(
                modifier = Modifier
                    .align(Alignment.BottomCenter)
                    .padding(bottom = 12.dp),
                shape = GanderShape.ChipSmall,
                color = Immersive.Background.copy(alpha = 0.7f),
                contentColor = Immersive.Content,
                border = BorderStroke(1.dp, Immersive.Content.copy(alpha = 0.3f)),
            ) {
                Text(
                    text = stringResource(R.string.preview_page_counter, pagerState.currentPage + 1, total),
                    style = MaterialTheme.typography.labelMedium,
                    modifier = Modifier
                        .padding(horizontal = 10.dp, vertical = 4.dp)
                        .testTag(PreviewTags.PAGE_COUNTER),
                )
            }
        }
    }
}

@Composable
private fun PagesPage(
    index: Int,
    state: PagesUiState,
    info: PagesInfoState.Ready,
    viewModel: PagesViewModel,
    imageLoader: ImageLoader?,
    onFetch: (() -> Unit)?,
    onZoomChanged: (Boolean) -> Unit,
) {
    val ref = remember(index, state.widthPx) { viewModel.pageRef(index) }
    val attempt = state.attempts[index] ?: 0
    val failure = state.failedPages[index]
    val zoom = remember { ZoomState() }
    LaunchedEffect(zoom) { snapshotFlow { zoom.isZoomed }.collect { onZoomChanged(it) } }
    var loaded by remember(ref, attempt) { mutableStateOf(false) }
    // Deep zoom (WP-67): the base page's decoded size places the tiles; the pyramid comes from the host.
    var bitmap by remember(ref, attempt) { mutableStateOf(IntSize.Zero) }
    val tilePage = state.tilePages?.getOrNull(index)
    val pyramid = remember(tilePage) { tilePage?.let { tilePyramidOf(it) } }
    val overlay: (@Composable BoxScope.() -> Unit)? =
        if (pyramid != null && bitmap != IntSize.Zero) {
            { TileOverlay(zoom, pyramid, bitmap.width, bitmap.height, { viewModel.tileRef(index, it) }, imageLoader) }
        } else {
            null
        }

    Box(modifier = Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
        if (ref != null && failure == null) {
            val count = state.pageCount
            PreviewPageImage(
                ref = ref,
                attempt = attempt,
                imageLoader = imageLoader,
                contentDescription = if (count != null) {
                    stringResource(R.string.preview_page_description, index + 1, count)
                } else {
                    stringResource(R.string.preview_page_description_unknown, index + 1)
                },
                loadedTag = PreviewTags.pageLoaded(index + 1),
                onLoaded = {
                    loaded = true
                    viewModel.onPageLoaded(index)
                },
                onFailed = { error ->
                    loaded = false
                    viewModel.onPageFailed(index, error)
                },
                modifier = Modifier.fillMaxSize(),
                zoom = zoom,
                onBitmapSize = { w, h -> bitmap = IntSize(w, h) },
                overlay = overlay,
            )
        }
        when {
            failure != null -> PreviewFailurePanel(
                failure = failure,
                onRetry = { viewModel.retryPage(index) },
                onFetch = onFetch,
            )
            !loaded -> PreviewProgress(renderingText(state.hostName, info.estimatedMs))
        }
    }
}

/** "studio-pc is rendering this page, about 12 seconds..." once the host says it takes a while. */
@Composable
private fun renderingText(hostName: String, estimatedMs: Long): String =
    if (estimatedMs >= SHOW_ESTIMATE_FROM_MS) {
        ceil(estimatedMs / 1000.0).toInt().let { seconds ->
            pluralStringResource(R.plurals.preview_rendering_estimate, seconds, hostName, seconds)
        }
    } else {
        stringResource(R.string.preview_rendering, hostName)
    }

private const val SHOW_ESTIMATE_FROM_MS = 2_000L
