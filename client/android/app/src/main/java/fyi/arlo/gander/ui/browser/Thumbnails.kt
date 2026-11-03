package fyi.arlo.gander.ui.browser

import androidx.compose.foundation.background
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import coil3.ImageLoader
import coil3.SingletonImageLoader
import coil3.compose.AsyncImage
import coil3.compose.LocalPlatformContext
import coil3.request.Disposable
import coil3.request.ImageRequest
import coil3.request.crossfade
import fyi.arlo.gander.core.data.hosts.HostRepository
import fyi.arlo.gander.core.data.thumbs.ThumbRef
import fyi.arlo.gander.core.data.thumbs.ThumbSizes
import fyi.arlo.gander.core.data.thumbs.thumbPriority
import fyi.arlo.gander.core.data.thumbs.thumbVersion
import fyi.arlo.gander.core.data.thumbs.wantsThumbnail
import fyi.arlo.gander.core.net.ThumbPriority
import fyi.arlo.gander.core.net.protocol.Entry
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.filter
import kotlinx.coroutines.flow.map

/** Glyph sizes of the two layouts; thumbnails are requested for exactly these display sizes. */
internal val ROW_GLYPH_SIZE: Dp = 42.dp
internal val CELL_GLYPH_SIZE: Dp = 160.dp

/** Test tags of the thumbnail layer. */
object ThumbnailTags {
    /** Present on the thumbnail image of [path] only once it has loaded (the glyph is the fallback). */
    fun loaded(path: String) = "thumb-$path"
}

/**
 * Which host and share the entries on screen belong to, what thumbnail sizes the host renders and
 * which Coil loader draws them. Entries from other shares (the Recent tab) carry their own share.
 * When there is no scope (previews, tests that render rows alone) rows keep their type glyph.
 */
@Immutable
class ThumbnailScope(
    val hostId: String,
    val shareId: String,
    val sizes: List<Int> = ThumbSizes.DEFAULT,
    /** Null means Coil's singleton, which [fyi.arlo.gander.GanderApplication] provides. */
    val imageLoader: ImageLoader? = null,
)

val LocalThumbnailScope = staticCompositionLocalOf<ThumbnailScope?> { null }

/**
 * Provides a [ThumbnailScope] to [content]. The sizes follow `capabilities.thumb_sizes` of the
 * paired host (refreshed by every `/me`); until they are known the sizes every host renders are
 * assumed.
 */
@Composable
fun ThumbnailScopeProvider(
    hostId: String,
    shareId: String,
    hosts: HostRepository,
    imageLoader: ImageLoader? = null,
    content: @Composable () -> Unit,
) {
    val sizes by produceState(ThumbSizes.DEFAULT, hostId, hosts) {
        hosts.hosts
            .map { list -> list.firstOrNull { it.id == hostId }?.capabilities?.thumbSizes.orEmpty() }
            .filter { it.isNotEmpty() }
            .distinctUntilChanged()
            .collect { value = it }
    }
    val scope = remember(hostId, shareId, sizes, imageLoader) { ThumbnailScope(hostId, shareId, sizes, imageLoader) }
    CompositionLocalProvider(LocalThumbnailScope provides scope, content = content)
}

/** The `size` to request for a glyph [size] wide: density based, from what the host renders. */
@Composable
private fun rememberThumbSizePx(scope: ThumbnailScope, size: Dp): Int {
    val density = LocalDensity.current.density
    val tablet = LocalConfiguration.current.smallestScreenWidthDp >= ThumbSizes.TABLET_MIN_SMALLEST_WIDTH_DP
    return remember(scope.sizes, size, density, tablet) {
        ThumbSizes.pick(ThumbSizes.displayPx(size.value, density), scope.sizes, tablet)
    }
}

private fun thumbRefOf(scope: ThumbnailScope, display: EntryDisplay, sizePx: Int): ThumbRef? =
    display.entry.takeIf { it.wantsThumbnail() }?.let { thumbRefOf(scope, it, display.path, sizePx) }

private fun thumbRefOf(scope: ThumbnailScope, entry: Entry, path: String, sizePx: Int) =
    ThumbRef(scope.hostId, entry.share ?: scope.shareId, path, entry.thumbVersion(), sizePx)

private fun ImageRequest.Builder.forThumb(ref: ThumbRef, priority: ThumbPriority): ImageRequest.Builder =
    data(ref).thumbPriority(priority)

/**
 * The thumbnail of [entry], drawn over the type glyph that [EntryGlyph] keeps underneath: it
 * shows only once loaded, so a slow, refused or unsupported thumbnail leaves the glyph as is. The
 * request is `priority=visible`, and Compose disposing the row (it scrolled out of the lazy list)
 * cancels it. Draws nothing when there is no [LocalThumbnailScope] or the host predicts no thumbnail.
 */
@Composable
internal fun EntryThumbnail(entry: Entry, path: String, size: Dp, modifier: Modifier = Modifier) {
    val scope = LocalThumbnailScope.current ?: return
    if (!entry.wantsThumbnail()) return
    val sizePx = rememberThumbSizePx(scope, size)
    val context = LocalPlatformContext.current
    val request = remember(scope.hostId, scope.shareId, entry.share, path, entry.mtime, entry.size, sizePx, context) {
        ImageRequest.Builder(context)
            .forThumb(thumbRefOf(scope, entry, path, sizePx), ThumbPriority.VISIBLE)
            .crossfade(true)
            .build()
    }
    var loaded by remember(request) { mutableStateOf(false) }
    val surface = MaterialTheme.colorScheme.surfaceVariant
    AsyncImage(
        model = request,
        contentDescription = null,
        imageLoader = scope.imageLoader ?: SingletonImageLoader.get(context),
        modifier = modifier
            .clip(RoundedCornerShape(8.dp))
            // Opaque once loaded, so a transparent PNG does not show the glyph through it.
            .then(if (loaded) Modifier.background(surface).testTag(ThumbnailTags.loaded(path)) else Modifier),
        contentScale = ContentScale.Crop,
        onLoading = { loaded = false },
        onSuccess = { loaded = true },
        onError = { loaded = false },
    )
}

/** How many rows beyond the last visible one are prefetched. */
private const val PREFETCH_AHEAD = 12

/**
 * Asks for the thumbnails of the [PREFETCH_AHEAD] rows after the last visible one with
 * `priority=prefetch` (the host and [fyi.arlo.gander.core.data.thumbs.ThumbScheduler] serve
 * visible rows first), so they are cached by the time the user scrolls to them. Requests for
 * rows that fall out of the window are cancelled as the list scrolls, and all of them when the
 * list leaves the screen. [glyphSize] must be the size the rows use ([ROW_GLYPH_SIZE] or
 * [CELL_GLYPH_SIZE]); [lastVisibleIndex] is -1 until the list has been laid out.
 */
@Composable
internal fun ThumbnailPrefetchEffect(
    displays: List<EntryDisplay>,
    glyphSize: Dp,
    lastVisibleIndex: () -> Int,
) {
    val scope = LocalThumbnailScope.current ?: return
    val context = LocalPlatformContext.current
    val sizePx = rememberThumbSizePx(scope, glyphSize)
    val current by rememberUpdatedState(displays)
    val currentLastVisible by rememberUpdatedState(lastVisibleIndex)
    LaunchedEffect(scope, sizePx, context) {
        val loader = scope.imageLoader ?: SingletonImageLoader.get(context)
        val requested = HashMap<String, Disposable>()
        try {
            snapshotFlow { currentLastVisible() to current.size }.distinctUntilChanged().collect { (last, _) ->
                if (last < 0) return@collect
                val wanted = HashMap<String, ThumbRef>()
                for (index in (last + 1)..(last + PREFETCH_AHEAD)) {
                    val ref = current.getOrNull(index)?.let { thumbRefOf(scope, it, sizePx) } ?: continue
                    wanted[ref.cacheKey()] = ref
                }
                requested.keys.filter { it !in wanted }.forEach { requested.remove(it)?.dispose() }
                for ((key, ref) in wanted) {
                    if (key !in requested) {
                        requested[key] = loader.enqueue(
                            ImageRequest.Builder(context).forThumb(ref, ThumbPriority.PREFETCH).build(),
                        )
                    }
                }
            }
        } finally {
            requested.values.forEach { it.dispose() }
        }
    }
}
