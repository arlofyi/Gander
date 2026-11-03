package fyi.arlo.gander.ui.stream

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.size
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.input.pointer.changedToDown
import androidx.compose.ui.input.pointer.changedToUp
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.input.pointer.positionChanged
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import fyi.arlo.gander.core.data.stream.StreamVideoOutput
import org.webrtc.RendererCommon
import org.webrtc.SurfaceViewRenderer

/**
 * The streamed picture in a libwebrtc `SurfaceViewRenderer` (client spec: aspect-fit, with the
 * viewer's pinch zoom and pan handled locally and kept until reset).
 *
 * The renderer view is laid out at exactly the picture's aspect-fit rectangle ([StreamViewport.picture]),
 * so the letterbox bars are plain black Compose background and the touch mapping uses the very
 * rectangle that is drawn. The zoom is set on the View itself: a `SurfaceView` is composited
 * outside Compose's own layers, so `Modifier.graphicsLayer` would not move it. The picture is
 * centred, so scaling about the view's centre is scaling about the viewport's centre, which is
 * what [fyi.arlo.gander.ui.preview.ZoomTransform] describes.
 */
@Composable
fun StreamPicture(
    output: StreamVideoOutput,
    viewport: StreamViewport,
    modifier: Modifier = Modifier,
) {
    val density = LocalDensity.current
    val rect = viewport.picture()
    Box(modifier = modifier.fillMaxSize().background(Color.Black), contentAlignment = Alignment.Center) {
        if (rect.width > 0f && rect.height > 0f) {
            AndroidView(
                modifier = Modifier
                    .size(with(density) { rect.width.toDp() }, with(density) { rect.height.toDp() })
                    .testTag(StreamTags.SURFACE),
                factory = { context ->
                    SurfaceViewRenderer(context).apply {
                        init(output.eglBase.eglBaseContext, null)
                        setScalingType(RendererCommon.ScalingType.SCALE_ASPECT_FIT)
                        setEnableHardwareScaler(true)
                        setMirror(false)
                        output.addSink(this)
                    }
                },
                update = { view ->
                    view.scaleX = viewport.zoom.scale
                    view.scaleY = viewport.zoom.scale
                    view.translationX = viewport.zoom.offsetX
                    view.translationY = viewport.zoom.offsetY
                },
                onRelease = { view ->
                    output.removeSink(view)
                    view.release()
                },
            )
        }
    }
}

/** The viewer's own pointer (the host captures without a cursor): a ring with a dot. */
@Composable
fun PointerMarker(viewport: StreamViewport, position: Norm, modifier: Modifier = Modifier) {
    val (x, y) = viewport.toView(position)
    Canvas(modifier = modifier.fillMaxSize()) {
        val center = Offset(x, y)
        drawCircle(Color.White, radius = 12.dp.toPx(), center = center, style = Stroke(width = 2.dp.toPx()))
        drawCircle(Color.Black, radius = 12.dp.toPx() + 1.dp.toPx(), center = center, style = Stroke(width = 1.dp.toPx()))
        drawCircle(Color.White, radius = 2.5.dp.toPx(), center = center)
    }
}

/**
 * Feeds raw pointer events to [recognizer] and hands what it decides to [onOutputs]. Every change
 * is consumed: the stream owns the touches on the picture. A held touch is timed with the
 * recogniser's own timer (`msUntilTick`), so a long press fires without another event; the timer
 * runs on the events' own clock ([GestureTimer]), never on `SystemClock`.
 */
fun Modifier.streamGestures(
    recognizer: StreamGestureRecognizer,
    onOutputs: (List<GestureOutput>) -> Unit,
): Modifier = pointerInput(recognizer) {
    val timer = GestureTimer(recognizer)
    awaitPointerEventScope {
        while (true) {
            val wait = timer.waitMs()
            val event = if (wait == null) {
                awaitPointerEvent()
            } else {
                withTimeoutOrNull(wait) { awaitPointerEvent() }
            }
            if (event == null) {
                onOutputs(timer.onTimeout(wait ?: 0L))
                continue
            }
            event.changes.maxOfOrNull { it.uptimeMillis }?.let(timer::onEvent)
            // Fingers going down first, then every finger that moved as one batch (two fingers that
            // move together must be judged together), then fingers going up.
            for (change in event.changes) {
                if (change.changedToDown()) {
                    onOutputs(recognizer.onDown(change.id.value, change.position.x, change.position.y, change.uptimeMillis))
                }
            }
            val moved = event.changes.filter { it.pressed && it.previousPressed && it.positionChanged() }
            if (moved.isNotEmpty()) {
                val moves = moved.map { PointerMove(it.id.value, it.position.x, it.position.y) }
                onOutputs(recognizer.onMoved(moves, moved.first().uptimeMillis))
            }
            for (change in event.changes) {
                if (change.changedToUp()) {
                    onOutputs(recognizer.onUp(change.id.value, change.position.x, change.position.y, change.uptimeMillis))
                }
            }
            event.changes.forEach { it.consume() }
        }
    }
}
