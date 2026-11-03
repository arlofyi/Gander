package fyi.arlo.gander.ui.preview

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.testTag
import coil3.ImageLoader
import coil3.SingletonImageLoader
import coil3.compose.AsyncImage
import coil3.compose.LocalPlatformContext
import coil3.request.ImageRequest
import fyi.arlo.gander.core.data.preview.PreviewPageRef
import fyi.arlo.gander.core.data.preview.previewPage
import fyi.arlo.gander.core.data.preview.toPreviewError
import fyi.arlo.gander.core.net.GanderError

/**
 * One preview picture drawn by Coil from [ref] at its decoded size (see [previewPage]), fitted
 * into its bounds. [zoom], when given, makes it pinch-zoomable. [attempt] is part of the request's
 * identity: bumping it after a failure builds the request again, which is what Retry does.
 * [loadedTag] is on the node only once the picture has loaded.
 *
 * [onBitmapSize] is told the decoded size of the picture once it has loaded (deep zoom places its
 * tiles by it). [overlay], when given together with [zoom], is drawn over the picture inside the
 * same zoomed layer, so whatever it draws pans and zooms exactly with the picture (deep-zoom
 * tiles, WP-67).
 */
@Composable
internal fun PreviewPageImage(
    ref: PreviewPageRef,
    attempt: Int,
    imageLoader: ImageLoader?,
    contentDescription: String?,
    loadedTag: String,
    onLoaded: () -> Unit,
    onFailed: (GanderError) -> Unit,
    modifier: Modifier = Modifier,
    zoom: ZoomState? = null,
    onBitmapSize: ((width: Int, height: Int) -> Unit)? = null,
    overlay: (@Composable BoxScope.() -> Unit)? = null,
) {
    val context = LocalPlatformContext.current
    val request = remember(ref, attempt, context) { ImageRequest.Builder(context).previewPage(ref).build() }
    var loaded by remember(request) { mutableStateOf(false) }
    val picture: @Composable (Modifier) -> Unit = { pictureModifier ->
        AsyncImage(
            model = request,
            contentDescription = contentDescription,
            imageLoader = imageLoader ?: SingletonImageLoader.get(context),
            contentScale = ContentScale.Fit,
            modifier = pictureModifier.then(if (loaded) Modifier.testTag(loadedTag) else Modifier),
            onSuccess = {
                loaded = true
                onBitmapSize?.invoke(it.result.image.width, it.result.image.height)
                onLoaded()
            },
            onError = {
                loaded = false
                onFailed(it.result.throwable.toPreviewError())
            },
        )
    }
    if (zoom != null && overlay != null) {
        Box(modifier = modifier.zoomable(zoom)) {
            picture(Modifier.fillMaxSize())
            overlay()
        }
    } else {
        picture(modifier.then(if (zoom != null) Modifier.zoomable(zoom) else Modifier))
    }
}
