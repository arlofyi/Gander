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
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.PathEffect
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import fyi.arlo.gander.core.data.actions.FileAction
import fyi.arlo.gander.ui.theme.GanderTheme

/**
 * The work rail (design document section 6): computer, link, phone, with the device that does the
 * work filled orange. Preview shows a page travelling to the phone, Fetch shows bytes and an arrow
 * into the filled phone, Stream shows a dashed mirror link and a window outline on the phone.
 * Always paired with [caption] ("Work on studio-pc"), which is also its accessibility label; the
 * glyph itself is decorative.
 */
@Composable
fun WorkRail(
    mode: FileAction,
    caption: String,
    modifier: Modifier = Modifier,
    captionColor: Color = GanderTheme.colors.text3,
    strokeColor: Color = GanderTheme.colors.text2,
    singleLine: Boolean = true,
) {
    Row(
        modifier = modifier.clearAndSetSemantics { contentDescription = caption },
        horizontalArrangement = Arrangement.spacedBy(6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        WorkRailGlyph(mode, strokeColor = strokeColor)
        Text(
            text = caption,
            style = MaterialTheme.typography.bodySmall,
            color = captionColor,
            maxLines = if (singleLine) 1 else 3,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

/** The glyph alone, 58 x 18 dp, drawn from the mockup's 58 x 18 viewBox. */
@Composable
fun WorkRailGlyph(
    mode: FileAction,
    modifier: Modifier = Modifier,
    strokeColor: Color = GanderTheme.colors.text2,
    accent: Color = GanderTheme.colors.accent,
    pageFill: Color = GanderTheme.colors.surface,
) {
    Canvas(modifier = modifier.size(width = 58.dp, height = 18.dp)) {
        val u = size.width / 58f
        drawRail(mode, u, strokeColor, accent, pageFill)
    }
}

private fun DrawScope.drawRail(mode: FileAction, u: Float, s: Color, a: Color, page: Color) {
    fun stroke(w: Float, dash: FloatArray? = null) = Stroke(
        width = w * u,
        cap = StrokeCap.Round,
        join = StrokeJoin.Round,
        pathEffect = dash?.let { PathEffect.dashPathEffect(it.map { d -> d * u }.toFloatArray()) },
    )
    fun line(x1: Float, y1: Float, x2: Float, y2: Float, w: Float, dash: FloatArray? = null) {
        val st = stroke(w, dash)
        drawLine(
            color = s,
            start = Offset(x1 * u, y1 * u),
            end = Offset(x2 * u, y2 * u),
            strokeWidth = st.width,
            cap = if (dash == null) StrokeCap.Round else StrokeCap.Butt,
            pathEffect = st.pathEffect,
        )
    }
    fun box(x: Float, y: Float, w: Float, h: Float, r: Float, fill: Color?, edge: Color, edgeWidth: Float) {
        val topLeft = Offset(x * u, y * u)
        val sz = Size(w * u, h * u)
        val radius = CornerRadius(r * u, r * u)
        if (fill != null) drawRoundRect(fill, topLeft, sz, radius)
        drawRoundRect(edge, topLeft, sz, radius, style = stroke(edgeWidth))
    }
    fun chevron(x: Float, y: Float, dx: Float, dy: Float, w: Float) {
        val p = Path().apply {
            moveTo(x * u, (y - dy) * u)
            lineTo((x + dx) * u, y * u)
            lineTo(x * u, (y + dy) * u)
        }
        drawPath(p, s, style = stroke(w))
    }

    val computerFill = if (mode == FileAction.FETCH) null else a
    val phoneFill = if (mode == FileAction.FETCH) a else null
    // Computer, its stand, the phone.
    box(1f, 2f, 18f, 11f, 2f, computerFill, s, 1.4f)
    line(7f, 16f, 13f, 16f, 1.4f)
    box(47f, 1f, 10f, 16f, 2f, phoneFill, s, 1.4f)
    when (mode) {
        FileAction.PREVIEW -> {
            line(22f, 8f, 41f, 8f, 1.3f, floatArrayOf(2f, 2f))
            box(28.5f, 4.5f, 6f, 7f, 1f, page, s, 1.3f)
            chevron(40.5f, 8f, 3f, 2.5f, 1.3f)
        }
        FileAction.FETCH -> {
            line(22f, 8f, 42f, 8f, 1.6f)
            chevron(40f, 8f, 3.2f, 3f, 1.6f)
            drawCircle(s, 1.4f * u, Offset(27f * u, 8f * u))
            drawCircle(s, 1.4f * u, Offset(32f * u, 8f * u))
        }
        FileAction.STREAM -> {
            line(22f, 6.5f, 43f, 6.5f, 1.2f, floatArrayOf(1.5f, 2f))
            line(22f, 10f, 43f, 10f, 1.2f, floatArrayOf(1.5f, 2f))
            drawRoundRect(
                color = a,
                topLeft = Offset(49f * u, 5f * u),
                size = Size(6f * u, 5f * u),
                cornerRadius = CornerRadius(0.8f * u, 0.8f * u),
                style = stroke(1.4f),
            )
        }
    }
}
