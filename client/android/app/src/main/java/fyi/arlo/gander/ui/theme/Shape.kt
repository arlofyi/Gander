package fyi.arlo.gander.ui.theme

import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Shapes
import androidx.compose.ui.unit.dp

// Shapes and spacing from the design document, section 5.

/** M3 shape scale: extraSmall 6, small 8, medium 12, large 16, extraLarge 28. */
val GanderShapes = Shapes(
    extraSmall = RoundedCornerShape(6.dp),
    small = RoundedCornerShape(8.dp),
    medium = RoundedCornerShape(12.dp),
    large = RoundedCornerShape(16.dp),
    extraLarge = RoundedCornerShape(28.dp),
)

/** Named radii for the components that are not M3 roles. */
object GanderShape {
    val Chip = RoundedCornerShape(8.dp)
    val ChipSmall = RoundedCornerShape(6.dp)
    val Tag = RoundedCornerShape(5.dp)
    val Tile = RoundedCornerShape(10.dp)
    val TileLarge = RoundedCornerShape(14.dp)
    val Banner = RoundedCornerShape(12.dp)
    val Panel = RoundedCornerShape(16.dp)
    val Card = RoundedCornerShape(20.dp)
    val Sheet = RoundedCornerShape(topStart = 28.dp, topEnd = 28.dp)
    val Dialog = RoundedCornerShape(28.dp)
    val Key = RoundedCornerShape(8.dp)
}

/** The 4 dp grid. Screen gutters are 16 to 20 dp; touch targets at least 48 dp. */
object GanderSpacing {
    val XXS = 2.dp
    val XS = 4.dp
    val S = 8.dp
    val M = 12.dp
    val L = 16.dp
    val Gutter = 20.dp
    val XL = 24.dp
    val XXL = 32.dp
    val TouchTarget = 48.dp
    val RowMinHeight = 64.dp

    /** Content width of the phone layouts when they are shown on a tablet. */
    val ReadableMaxWidth = 600.dp

    /** Width of the folder pane in the tablet's two-pane browser. */
    val ListPaneWidth = 440.dp
}
