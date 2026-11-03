package fyi.arlo.gander.ui.preview

import android.text.format.Formatter
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import fyi.arlo.gander.R
import fyi.arlo.gander.ui.common.InfoChip
import fyi.arlo.gander.ui.components.ButtonKind
import fyi.arlo.gander.ui.components.GanderButton
import fyi.arlo.gander.ui.components.GanderProgress
import fyi.arlo.gander.ui.theme.GanderIcons
import fyi.arlo.gander.ui.theme.GanderTheme
import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.rememberScrollState

/**
 * The text viewer (client spec, "Preview: text"): monospace text, the detected encoding in a bar
 * above it, and the next slice loaded when the list reaches its end (or with "Load more").
 * Highlighting is not part of v1.
 */
@Composable
fun TextViewerScreen(
    viewModel: TextViewModel,
    title: String,
    onBack: () -> Unit,
    onFetch: (() -> Unit)?,
    modifier: Modifier = Modifier,
    hostName: String = "",
) {
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    PreviewScaffold(
        title = title,
        onBack = onBack,
        onFetch = onFetch,
        modifier = modifier,
        immersive = false,
        footer = {
            PreviewRail(
                caption = stringResource(R.string.rail_text, hostName.ifEmpty { stringResource(R.string.host_os_unknown) }),
                trailing = stringResource(R.string.preview_text_mono),
            )
        },
    ) { padding ->
        Box(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding),
            contentAlignment = Alignment.Center,
        ) {
            when (val phase = state.phase) {
                TextPhase.Loading -> PreviewProgress(null)
                is TextPhase.Failed -> PreviewFailurePanel(phase.failure, onRetry = viewModel::retry, onFetch = onFetch)
                TextPhase.Ready -> TextContent(state, viewModel, onFetch)
            }
        }
    }
}

@Composable
private fun TextContent(state: TextUiState, viewModel: TextViewModel, onFetch: (() -> Unit)?) {
    val listState = rememberLazyListState()
    // The footer is the last item: whenever it is on screen (it scrolled into view, or a short slice
    // left it there) the next slice is read. A new slice changes the item count, so this runs again.
    LaunchedEffect(listState, viewModel) {
        snapshotFlow {
            val info = listState.layoutInfo
            (info.visibleItemsInfo.lastOrNull()?.index ?: -1) to info.totalItemsCount
        }.collect { (last, total) -> if (last >= 0 && last >= total - 1) viewModel.loadMore() }
    }

    Column(modifier = Modifier.fillMaxSize()) {
        EncodingBar(state)
        HorizontalDivider(color = GanderTheme.colors.line)
        SelectionContainer(
            modifier = Modifier
                .weight(1f)
                .background(GanderTheme.colors.surface),
        ) {
            LazyColumn(
                state = listState,
                modifier = Modifier
                    .fillMaxSize()
                    .testTag(PreviewTags.TEXT_LIST),
                contentPadding = PaddingValues(horizontal = 12.dp, vertical = 8.dp),
            ) {
                items(state.blocks, key = { it.index }) { block ->
                    Text(
                        text = block.text,
                        modifier = Modifier.fillMaxWidth(),
                        style = GanderTheme.type.mono,
                        color = GanderTheme.colors.text,
                    )
                }
                item(key = "footer") { TextFooter(state, viewModel, onFetch) }
            }
        }
    }
}

@Composable
private fun EncodingBar(state: TextUiState) {
    val context = LocalContext.current
    val encoding = state.encoding
    val read = Formatter.formatShortFileSize(context, state.loadedBytes)
    val progress = state.totalBytes?.let {
        stringResource(R.string.preview_text_progress, read, Formatter.formatShortFileSize(context, it))
    } ?: stringResource(R.string.preview_text_progress_unknown, read)
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .horizontalScroll(rememberScrollState())
            .padding(start = 20.dp, end = 20.dp, bottom = 10.dp),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (encoding != null) {
            InfoChip(
                text = if (encoding.converted) {
                    stringResource(R.string.preview_text_encoding_converted, encoding.name)
                } else {
                    stringResource(R.string.preview_text_encoding, encoding.name)
                },
                modifier = Modifier.testTag(PreviewTags.TEXT_ENCODING),
            )
        }
        InfoChip(text = progress)
    }
}

@Composable
private fun TextFooter(state: TextUiState, viewModel: TextViewModel, onFetch: (() -> Unit)?) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 16.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        val failure = state.loadMoreFailure
        when {
            state.capped -> {
                val limit = Formatter.formatShortFileSize(LocalContext.current, state.loadedBytes)
                Text(
                    text = stringResource(R.string.preview_text_capped, limit),
                    style = MaterialTheme.typography.bodyMedium,
                    textAlign = TextAlign.Center,
                )
                if (onFetch != null) {
                    GanderButton(stringResource(R.string.preview_fetch), onClick = onFetch, kind = ButtonKind.TONAL, icon = GanderIcons.Fetch)
                }
            }
            failure != null -> {
                Text(failure.message(), style = MaterialTheme.typography.bodyMedium, textAlign = TextAlign.Center)
                GanderButton(stringResource(R.string.action_retry), onClick = viewModel::retry, kind = ButtonKind.TONAL, icon = GanderIcons.Refresh)
            }
            state.loadingMore -> {
                GanderProgress(fraction = null, modifier = Modifier.padding(horizontal = 40.dp))
                Text(stringResource(R.string.preview_text_loading_more), style = MaterialTheme.typography.bodySmall)
            }
            state.hasMore -> GanderButton(
                text = stringResource(R.string.preview_text_load_more),
                onClick = viewModel::loadMore,
                kind = ButtonKind.OUTLINED,
                modifier = Modifier.testTag(PreviewTags.TEXT_LOAD_MORE),
            )
            state.blocks.all { it.text.isEmpty() } -> Text(
                text = stringResource(R.string.preview_text_empty),
                style = MaterialTheme.typography.bodyMedium,
            )
        }
    }
}
