package fyi.arlo.gander.ui.preview

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import coil3.ImageLoader
import fyi.arlo.gander.R
import fyi.arlo.gander.ui.components.GanderButton
import fyi.arlo.gander.ui.theme.GanderIcons

/**
 * The image viewer (client spec, "Preview: image"): the host's resized copy at the width of the
 * screen, pinch to zoom and double tap to toggle, and "View original (Fetch)" for the full file.
 */
@Composable
fun ImageViewerScreen(
    viewModel: SinglePageViewModel,
    title: String,
    imageLoader: ImageLoader?,
    onBack: () -> Unit,
    onFetch: (() -> Unit)?,
    modifier: Modifier = Modifier,
) {
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    PreviewScaffold(
        title = title,
        onBack = onBack,
        onFetch = onFetch,
        modifier = modifier,
        footer = {
            Column(
                modifier = Modifier.padding(start = 0.dp, end = 0.dp, bottom = 8.dp),
                verticalArrangement = Arrangement.spacedBy(4.dp),
            ) {
                PreviewRail(caption = stringResource(R.string.rail_image, state.hostName))
                if (onFetch != null && state.loaded) {
                    // "View original (Fetch)": the full file costs a Fetch (mockup 20).
                    GanderButton(
                        text = stringResource(R.string.preview_fetch_original),
                        onClick = onFetch,
                        icon = GanderIcons.Fetch,
                        modifier = Modifier
                            .fillMaxWidth()
                            .padding(horizontal = 16.dp),
                    )
                }
            }
        },
    ) { padding ->
        BoxWithConstraints(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding),
            contentAlignment = Alignment.Center,
        ) {
            val widthPx = constraints.maxWidth
            LaunchedEffect(widthPx, viewModel) { viewModel.onViewport(widthPx) }
            val ref = remember(state.widthPx) { viewModel.pageRef() }
            val failure = state.failure
            val zoom = remember { ZoomState() }
            // Deep zoom (WP-67): the base picture's decoded size places the tiles; the pyramid comes from the host.
            var bitmap by remember(ref, state.attempt) { mutableStateOf(IntSize.Zero) }
            val pyramid = remember(state.tilePage) { state.tilePage?.let { tilePyramidOf(it) } }
            val overlay: (@Composable BoxScope.() -> Unit)? =
                if (pyramid != null && bitmap != IntSize.Zero) {
                    { TileOverlay(zoom, pyramid, bitmap.width, bitmap.height, viewModel::tileRef, imageLoader) }
                } else {
                    null
                }
            if (ref != null && failure == null) {
                PreviewPageImage(
                    ref = ref,
                    attempt = state.attempt,
                    imageLoader = imageLoader,
                    contentDescription = stringResource(R.string.preview_image_description, title),
                    loadedTag = PreviewTags.IMAGE_LOADED,
                    onLoaded = viewModel::onLoaded,
                    onFailed = viewModel::onFailed,
                    modifier = Modifier.fillMaxSize(),
                    zoom = zoom,
                    onBitmapSize = { w, h -> bitmap = IntSize(w, h) },
                    overlay = overlay,
                )
            }
            when {
                failure != null -> PreviewFailurePanel(failure, onRetry = viewModel::retry, onFetch = onFetch)
                !state.loaded -> PreviewProgress(stringResource(R.string.preview_rendering, state.hostName))
            }
        }
    }
}
