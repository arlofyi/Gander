package fyi.arlo.gander.ui.common

import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import fyi.arlo.gander.R
import fyi.arlo.gander.ui.components.ChipStyle
import fyi.arlo.gander.ui.components.GanderChip
import fyi.arlo.gander.ui.theme.GanderIcons
import fyi.arlo.gander.ui.theme.GanderShape
import fyi.arlo.gander.ui.theme.GanderTheme

/**
 * A small non-interactive label (a fact about a file, a permission): the filled small chip of the
 * design system. [container] and [content] override the colours for the rare chip that needs it.
 */
@Composable
fun InfoChip(
    text: String,
    modifier: Modifier = Modifier,
    icon: ImageVector? = null,
    container: Color = GanderTheme.colors.surface3,
    content: Color = GanderTheme.colors.text,
) {
    // One node with the chip's text, so a tag on the chip finds its words.
    Surface(modifier = modifier.semantics(mergeDescendants = true) {}, shape = GanderShape.ChipSmall, color = container, contentColor = content) {
        Row(
            modifier = Modifier
                .height(24.dp)
                .padding(horizontal = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            if (icon != null) {
                Icon(icon, contentDescription = null, modifier = Modifier.size(14.dp))
            }
            Text(
                text = text,
                style = MaterialTheme.typography.labelMedium,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.padding(start = if (icon != null) 4.dp else 0.dp),
            )
        }
    }
}

/** The "Cached <relative time>" chip shown on listings and share lists that are stored snapshots. */
@Composable
fun CachedChip(cachedAtMillis: Long, modifier: Modifier = Modifier) {
    GanderChip(
        text = stringResource(R.string.cached_chip, relativeTimeText(cachedAtMillis)),
        icon = GanderIcons.Clock,
        style = ChipStyle.SELECTED,
        selected = false,
        modifier = modifier,
    )
}
