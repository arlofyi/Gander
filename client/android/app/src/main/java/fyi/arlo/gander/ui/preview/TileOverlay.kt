package fyi.arlo.gander.ui.preview

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.offset
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Modifier
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.layout.layout
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.unit.Constraints
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.IntSize
import coil3.ImageLoader
import coil3.SingletonImageLoader
import coil3.compose.AsyncImage
import coil3.compose.LocalPlatformContext
import coil3.request.ImageRequest
import fyi.arlo.gander.core.data.preview.PreviewTileRef
import fyi.arlo.gander.core.data.preview.TilePyramid
import fyi.arlo.gander.core.data.preview.previewTile
import fyi.arlo.gander.core.data.preview.toPreviewError
import fyi.arlo.gander.core.net.GanderError
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.collectLatest
import kotlin.math.roundToInt

/**
 * Deep-zoom tiles (WP-67) drawn over the base page, inside the same zoomed layer: once the zoom
 * passes [DeepZoom.ENGAGE_ABOVE] the tiles of the pyramid level that is sharp enough and that
 * intersect the viewport are composed, each one a Coil request. Only visible tiles are composed,
 * so a tile that pans off screen leaves the composition, which disposes its request and cancels
 * the load and the HTTP call. The base page stays underneath as the placeholder: a tile that has
 * not arrived (or never will) shows the base.
 *
 * The visible set follows the zoom state with a short debounce ([DEBOUNCE_MS]), so a pinch does
 * not start a request per frame. When the level changes, the tiles of the previous level that
 * already loaded stay under the new ones until those have loaded (or [KEEP_PREVIOUS_MS] passed),
 * so zooming does not blink back to the blurry base.
 *
 * A tile failure is silent. A refusal that will not change (the host has no tiles for this file,
 * or this tile does not exist) switches the overlay off for this picture; anything else (busy,
 * offline) just leaves that tile to the base.
 */
@Composable
internal fun BoxScope.TileOverlay(
    zoom: ZoomState,
    pyramid: TilePyramid,
    bitmapWidth: Int,
    bitmapHeight: Int,
    tileRef: (TileKey) -> PreviewTileRef,
    imageLoader: ImageLoader?,
) {
    var viewport by remember { mutableStateOf(IntSize.Zero) }
    var unavailable by remember(pyramid) { mutableStateOf(false) }
    var plan by remember(pyramid, bitmapWidth, bitmapHeight) { mutableStateOf<TilePlan?>(null) }
    var previous by remember(pyramid, bitmapWidth, bitmapHeight) { mutableStateOf<TilePlan?>(null) }
    val loaded = remember(pyramid, bitmapWidth, bitmapHeight) { mutableStateMapOf<TileKey, Boolean>() }

    // With tiles to show, the pinch may go further than the base page alone would be worth.
    DisposableEffect(zoom, pyramid, bitmapWidth, bitmapHeight, viewport, unavailable) {
        zoom.maxScale = if (unavailable) {
            ZoomMath.MAX_SCALE
        } else {
            DeepZoom.maxZoomFor(pyramid, viewport.width.toFloat(), viewport.height.toFloat(), bitmapWidth, bitmapHeight)
        }
        onDispose { zoom.maxScale = ZoomMath.MAX_SCALE }
    }

    LaunchedEffect(pyramid, bitmapWidth, bitmapHeight, viewport, unavailable) {
        if (unavailable || viewport.width <= 0 || viewport.height <= 0) {
            plan = null
            previous = null
            return@LaunchedEffect
        }
        snapshotFlow { ZoomTransform(zoom.scale, zoom.offsetX, zoom.offsetY) }.collectLatest { transform ->
            delay(DEBOUNCE_MS)
            val next = DeepZoom.plan(
                pyramid, viewport.width.toFloat(), viewport.height.toFloat(), bitmapWidth, bitmapHeight, transform,
            )
            val old = plan
            if (next != old) {
                previous = if (old != null && next != null && old.level != next.level) old else null
                plan = next
            }
        }
    }

    val current = plan
    val currentReady = current == null || current.tiles.all { it in loaded }
    LaunchedEffect(currentReady, previous) {
        if (previous != null) {
            if (!currentReady) delay(KEEP_PREVIOUS_MS)
            previous = null
        }
    }

    Box(
        modifier = Modifier
            .fillMaxSize()
            .onSizeChanged { viewport = it },
    ) {
        if (viewport.width > 0 && viewport.height > 0) {
            val page = DeepZoom.pageRect(viewport.width.toFloat(), viewport.height.toFloat(), bitmapWidth, bitmapHeight)
            val below = previous
            if (below != null) {
                // Already loaded tiles of the level we just left: Coil serves them from memory.
                for (tile in below.tiles) {
                    if (tile !in loaded) continue
                    key(tile) { TileImage(pyramid, tile, page, tileRef, imageLoader, onLoaded = {}, onFailed = {}) }
                }
            }
            if (current != null) {
                for (tile in current.tiles) {
                    key(tile) {
                        TileImage(
                            pyramid, tile, page, tileRef, imageLoader,
                            onLoaded = { loaded[tile] = true },
                            onFailed = { error -> if (isLasting(error)) unavailable = true },
                        )
                    }
                }
            }
        }
    }
}

/** A refusal that asking again cannot change: the file has no tiles, or this tile does not exist. */
internal fun isLasting(error: GanderError): Boolean =
    error is GanderError.Host.UnsupportedType ||
        error is GanderError.Host.BadRequest ||
        error is GanderError.Host.NotFound ||
        error is GanderError.Host.PermissionDenied

@Composable
private fun TileImage(
    pyramid: TilePyramid,
    tile: TileKey,
    page: PageRect,
    tileRef: (TileKey) -> PreviewTileRef,
    imageLoader: ImageLoader?,
    onLoaded: () -> Unit,
    onFailed: (GanderError) -> Unit,
) {
    val placement = DeepZoom.placement(pyramid, tile, page) ?: return
    // Rounded edges so neighbouring tiles meet without a hairline gap.
    val left = placement[0].roundToInt()
    val top = placement[1].roundToInt()
    val width = (placement[2].roundToInt() - left).coerceAtLeast(1)
    val height = (placement[3].roundToInt() - top).coerceAtLeast(1)
    val context = LocalPlatformContext.current
    val ref = remember(tile) { tileRef(tile) }
    val request = remember(ref, context) { ImageRequest.Builder(context).previewTile(ref).build() }
    AsyncImage(
        model = request,
        contentDescription = null,
        imageLoader = imageLoader ?: SingletonImageLoader.get(context),
        contentScale = ContentScale.FillBounds,
        modifier = Modifier
            .offset { IntOffset(left, top) }
            .layout { measurable, _ ->
                val placeable = measurable.measure(Constraints.fixed(width, height))
                layout(width, height) { placeable.place(0, 0) }
            },
        onSuccess = { onLoaded() },
        onError = { onFailed(it.result.throwable.toPreviewError()) },
    )
}

/** How long the zoom state must hold still before the visible tiles are recomputed. */
internal const val DEBOUNCE_MS = 120L

/** How long the previous level's tiles stay under a new level whose tiles are slow to arrive. */
internal const val KEEP_PREVIOUS_MS = 4_000L
