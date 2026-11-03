package fyi.arlo.gander.ui.components

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.size
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import fyi.arlo.gander.ui.theme.GanderTheme

/**
 * What a status dot says. Status is never colour alone (design document 3.4): each kind is drawn
 * with its own shape (filled dot, hollow ring, dot with a halo) and always sits next to a word.
 */
enum class StatusKind {
    /** Filled `ok` dot: online, verified. */
    ONLINE,

    /** Hollow ring in muted text: offline, not reached. */
    OFFLINE,

    /** Filled `warn` dot: relayed, metered, needs attention. */
    WARN,

    /** Filled `bad` dot: refused, revoked. */
    BAD,

    /** Hollow ring, muted: checking. */
    CHECKING,

    /** Orange dot with a halo: something is looking right now. Never pulses. */
    LIVE,
}

@Composable
fun StatusKind.color(): Color {
    val c = GanderTheme.colors
    return when (this) {
        StatusKind.ONLINE -> c.ok
        StatusKind.OFFLINE, StatusKind.CHECKING -> c.text3
        StatusKind.WARN -> c.warn
        StatusKind.BAD -> c.bad
        StatusKind.LIVE -> c.accentText
    }
}

/** The 8 dp dot of a status line; decorative (the word next to it carries the meaning). */
@Composable
fun StatusDot(kind: StatusKind, modifier: Modifier = Modifier, size: Dp = 8.dp) {
    val c = GanderTheme.colors
    val color = if (kind == StatusKind.LIVE) c.accent else kind.color()
    val halo = size + 6.dp
    Canvas(modifier = modifier.size(if (kind == StatusKind.LIVE) halo else size)) {
        val r = size.toPx() / 2f
        val center = Offset(this.size.width / 2f, this.size.height / 2f)
        when (kind) {
            StatusKind.OFFLINE, StatusKind.CHECKING -> {
                val w = 2.dp.toPx()
                drawCircle(color, r - w / 2f, center, style = Stroke(width = w))
            }
            StatusKind.LIVE -> {
                drawCircle(color.copy(alpha = 0.3f), r + 3.dp.toPx(), center)
                drawCircle(color, r, center)
            }
            else -> drawCircle(color, r, center)
        }
    }
}

/** A dot plus a word in the status colour: "Online", "Offline", "Relayed", "Private network: connected". */
@Composable
fun StatusLine(
    kind: StatusKind,
    text: String,
    modifier: Modifier = Modifier,
    style: TextStyle = MaterialTheme.typography.bodyMedium,
) {
    Row(
        modifier = modifier,
        horizontalArrangement = Arrangement.spacedBy(6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        StatusDot(kind)
        Text(text, style = style, color = kind.color(), maxLines = 1, overflow = TextOverflow.Ellipsis)
    }
}

/** The LIVE label: orange dot with a halo and bold `accent-text` ("LIVE · 02:41"). */
@Composable
fun LiveIndicator(text: String, modifier: Modifier = Modifier) {
    Row(
        modifier = modifier,
        horizontalArrangement = Arrangement.spacedBy(4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        StatusDot(StatusKind.LIVE)
        Text(
            text = text,
            style = MaterialTheme.typography.labelMedium.copy(fontWeight = FontWeight.SemiBold),
            color = GanderTheme.colors.accentText,
            maxLines = 1,
        )
    }
}
