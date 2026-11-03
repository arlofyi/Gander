package fyi.arlo.gander.ui.components

import android.provider.Settings
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.ProgressBarRangeInfo
import androidx.compose.ui.semantics.progressBarRangeInfo
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import fyi.arlo.gander.ui.theme.GanderTheme

/** True when the user turned animations off ("Remove animations"): indeterminate bars stop moving. */
@Composable
fun reducedMotion(): Boolean {
    val context = LocalContext.current
    return remember(context) {
        Settings.Global.getFloat(context.contentResolver, Settings.Global.ANIMATOR_DURATION_SCALE, 1f) == 0f
    }
}

/**
 * The 4 dp progress bar: `surface-3` track, ink indicator. [fraction] null is indeterminate, the
 * only looping animation in the app (design document section 7); with reduced motion it is a
 * static partial bar, and the text next to it says what is happening.
 */
@Composable
fun GanderProgress(fraction: Float?, modifier: Modifier = Modifier) {
    val c = GanderTheme.colors
    val bar = modifier
        .fillMaxWidth()
        .height(4.dp)
        .clip(RoundedCornerShape(2.dp))
    if (fraction != null) {
        LinearProgressIndicator(
            progress = { fraction.coerceIn(0f, 1f) },
            modifier = bar,
            color = c.primary,
            trackColor = c.surface3,
            strokeCap = StrokeCap.Round,
            gapSize = 0.dp,
            drawStopIndicator = {},
        )
    } else if (reducedMotion()) {
        Box(
            bar
                .background(c.surface3)
                .semantics { progressBarRangeInfo = ProgressBarRangeInfo.Indeterminate },
        ) {
            Box(Modifier.fillMaxWidth(STATIC_FRACTION).fillMaxHeight().background(c.primary))
        }
    } else {
        LinearProgressIndicator(
            modifier = bar,
            color = c.primary,
            trackColor = c.surface3,
            strokeCap = StrokeCap.Round,
            gapSize = 0.dp,
        )
    }
}

private const val STATIC_FRACTION = 0.35f
