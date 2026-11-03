package fyi.arlo.gander.ui.theme

import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.size
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.PathFillType
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.graphics.vector.addPathNodes
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp

/**
 * The Lens brand mark (design document section 6): a goose head inside a magnifying-glass ring.
 * The path data is `brand/gander-lens.svg` (100 x 100), so the in-app mark matches the launcher
 * icon. Ring, neck, handle and head take [ink]; the eye is a hole (even-odd), so whatever is
 * behind the mark shows through; the beak is always the greylag orange, in both themes.
 */
fun lensMarkVector(ink: Color): ImageVector =
    ImageVector.Builder(
        name = "GanderLens",
        defaultWidth = 100.dp,
        defaultHeight = 100.dp,
        viewportWidth = 100f,
        viewportHeight = 100f,
    ).apply {
        // The ring: circle (44, 44), r 31, stroke 8.
        addPath(
            pathData = addPathNodes("M13,44a31,31 0 1,0 62,0a31,31 0 1,0 -62,0Z"),
            stroke = SolidColor(ink),
            strokeLineWidth = 8f,
            strokeLineCap = StrokeCap.Round,
        )
        // The neck.
        addPath(
            pathData = addPathNodes("M37.5,73.5C31,60 44,55 46.5,38"),
            stroke = SolidColor(ink),
            strokeLineWidth = 9f,
            strokeLineCap = StrokeCap.Round,
        )
        // The handle, butt-capped, with a round end.
        addPath(
            pathData = addPathNodes("M65,65L85,85"),
            stroke = SolidColor(ink),
            strokeLineWidth = 11f,
            strokeLineCap = StrokeCap.Butt,
        )
        addPath(
            pathData = addPathNodes("M79.5,85a5.5,5.5 0 1,0 11,0a5.5,5.5 0 1,0 -11,0Z"),
            fill = SolidColor(ink),
        )
        // The head with the eye cut out.
        addPath(
            pathData = addPathNodes("M56.5,33.5a9.5,9.5 0 1,0 -19,0a9.5,9.5 0 1,0 19,0ZM53.4,30.7a2.2,2.2 0 1,0 -4.4,0a2.2,2.2 0 1,0 4.4,0Z"),
            fill = SolidColor(ink),
            pathFillType = PathFillType.EvenOdd,
        )
        // The beak.
        addPath(
            pathData = addPathNodes("M54,28.5L73.5,34.5L54,39.5Z"),
            fill = SolidColor(Brand.GreylagBeak),
            stroke = SolidColor(Brand.GreylagBeak),
            strokeLineWidth = 1.6f,
            strokeLineJoin = StrokeJoin.Round,
        )
    }.build()

/**
 * The Lens mark at [size]. Decorative by default ([contentDescription] null): it always appears
 * next to the app's name or a title that says the same thing.
 */
@Composable
fun LensMark(
    modifier: Modifier = Modifier,
    size: Dp = 48.dp,
    ink: Color = GanderTheme.colors.text,
    contentDescription: String? = null,
) {
    val vector = remember(ink) { lensMarkVector(ink) }
    Image(imageVector = vector, contentDescription = contentDescription, modifier = modifier.size(size))
}
