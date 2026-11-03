package fyi.arlo.gander.ui.pairing

import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.ImageProxy
import androidx.camera.core.Preview
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.content.ContextCompat
import androidx.lifecycle.compose.LocalLifecycleOwner
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Camera preview that reports every QR code it reads to [onCode] (on the main thread). CameraX
 * binds to the current lifecycle, so the camera is only open while the screen is. The text is the
 * pairing URI with the window secret in it: [onCode] must not log it. [onUnavailable] is called
 * when the device has no usable camera or CameraX fails to open it (the caller offers manual
 * entry). The CAMERA permission must already be granted.
 */
@Composable
fun QrScanner(
    onCode: (String) -> Unit,
    onUnavailable: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val context = LocalContext.current
    val lifecycleOwner = LocalLifecycleOwner.current
    val currentOnCode by rememberUpdatedState(onCode)
    val currentOnUnavailable by rememberUpdatedState(onUnavailable)
    val previewView = remember(context) {
        PreviewView(context).apply {
            // COMPATIBLE (TextureView): drawn inside the window, so FLAG_SECURE covers the preview too.
            implementationMode = PreviewView.ImplementationMode.COMPATIBLE
            scaleType = PreviewView.ScaleType.FILL_CENTER
        }
    }

    DisposableEffect(lifecycleOwner, previewView) {
        val mainExecutor = ContextCompat.getMainExecutor(context)
        val analysisExecutor = Executors.newSingleThreadExecutor()
        val providerFuture = ProcessCameraProvider.getInstance(context)
        val disposed = AtomicBoolean(false)
        var provider: ProcessCameraProvider? = null

        providerFuture.addListener(
            {
                if (disposed.get()) return@addListener
                try {
                    val cameraProvider = providerFuture.get()
                    provider = cameraProvider
                    val selector = when {
                        cameraProvider.hasCamera(CameraSelector.DEFAULT_BACK_CAMERA) -> CameraSelector.DEFAULT_BACK_CAMERA
                        cameraProvider.hasCamera(CameraSelector.DEFAULT_FRONT_CAMERA) -> CameraSelector.DEFAULT_FRONT_CAMERA
                        else -> {
                            currentOnUnavailable()
                            return@addListener
                        }
                    }
                    val preview = Preview.Builder().build().also { it.surfaceProvider = previewView.surfaceProvider }
                    val analysis = ImageAnalysis.Builder()
                        .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
                        .build()
                        .also { it.setAnalyzer(analysisExecutor, QrAnalyzer { text -> mainExecutor.execute { if (!disposed.get()) currentOnCode(text) } }) }
                    cameraProvider.unbindAll()
                    cameraProvider.bindToLifecycle(lifecycleOwner, selector, preview, analysis)
                } catch (_: Exception) {
                    // ExecutionException, IllegalStateException, IllegalArgumentException, camera-in-use...
                    currentOnUnavailable()
                }
            },
            mainExecutor,
        )

        onDispose {
            disposed.set(true)
            provider?.unbindAll()
            analysisExecutor.shutdown()
        }
    }

    AndroidView(factory = { previewView }, modifier = modifier)
}

/** Feeds camera frames to a [QrFrameDecoder]; reports each code read. Always closes the frame. */
private class QrAnalyzer(private val onText: (String) -> Unit) : ImageAnalysis.Analyzer {
    private val decoder = QrFrameDecoder()

    override fun analyze(image: ImageProxy) {
        try {
            val plane = image.planes[0]
            val luma = QrFrameDecoder.tightLuma(plane.buffer, plane.rowStride, image.width, image.height)
            decoder.decode(luma, image.width, image.height)?.let(onText)
        } catch (_: RuntimeException) {
            // A bad frame is not worth losing the camera over; the next one is tried.
        } finally {
            image.close()
        }
    }
}
