package fyi.arlo.gander.ui.components

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.layout.Arrangement
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
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.text
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import fyi.arlo.gander.ui.theme.GanderShape
import fyi.arlo.gander.ui.theme.GanderTheme

/** How a chip is drawn (design document section 8, permission and relay chips). */
enum class ChipStyle {
    /** 1 dp outline, secondary text: a control (sort, filter). */
    OUTLINED,

    /** Filled `surface-3`: an allowed permission, a fact about a file. */
    FILLED,

    /** `accent-container`: selected filter, cached state. */
    SELECTED,

    /** Outlined in `warn`: relayed, metered. */
    WARN,

    /** `ok-c` fill: verified, saved. */
    OK,

    /** Crossed out at 45 %: a mode the share does not allow. */
    NOT_ALLOWED,
}

private data class ChipColors(val container: Color, val content: Color, val border: Color?)

@Composable
private fun ChipStyle.colors(): ChipColors {
    val c = GanderTheme.colors
    return when (this) {
        ChipStyle.OUTLINED -> ChipColors(Color.Transparent, c.text2, c.outline)
        ChipStyle.FILLED -> ChipColors(c.surface3, c.text, null)
        ChipStyle.SELECTED -> ChipColors(c.accentContainer, c.onAccentContainer, null)
        ChipStyle.WARN -> ChipColors(Color.Transparent, c.warn, c.warn)
        ChipStyle.OK -> ChipColors(c.okContainer, c.ok, null)
        ChipStyle.NOT_ALLOWED -> ChipColors(Color.Transparent, c.text2, c.outline)
    }
}

/**
 * A Gander chip: 30 dp (or 24 dp when [small]) with an optional leading icon. Interactive when
 * [onClick] is set (the touch target then grows to 48 dp); [selected] is announced.
 */
@Composable
fun GanderChip(
    text: String,
    modifier: Modifier = Modifier,
    icon: ImageVector? = null,
    style: ChipStyle = ChipStyle.FILLED,
    small: Boolean = false,
    selected: Boolean = style == ChipStyle.SELECTED,
    onClick: (() -> Unit)? = null,
) {
    val colors = style.colors()
    val shape = if (small) GanderShape.ChipSmall else GanderShape.Chip
    val border = colors.border?.let { BorderStroke(1.dp, it) }
    val crossed = style == ChipStyle.NOT_ALLOWED
    val body: @Composable () -> Unit = {
        Row(
            modifier = Modifier
                .height(if (small) 24.dp else 30.dp)
                .padding(horizontal = if (small) 8.dp else 12.dp)
                .alpha(if (crossed) 0.45f else 1f),
            horizontalArrangement = Arrangement.spacedBy(if (small) 4.dp else 6.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            if (icon != null) Icon(icon, contentDescription = null, modifier = Modifier.size(if (small) 14.dp else 18.dp))
            Text(
                text = text,
                style = (if (small) MaterialTheme.typography.labelMedium else MaterialTheme.typography.bodyMedium)
                    .copy(textDecoration = if (crossed) TextDecoration.LineThrough else null),
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
    }
    if (onClick != null) {
        Surface(
            onClick = onClick,
            modifier = modifier.semantics { this.selected = selected },
            shape = shape,
            color = colors.container,
            contentColor = colors.content,
            border = border,
        ) { body() }
    } else {
        Surface(
            modifier = modifier.semantics(mergeDescendants = true) {},
            shape = shape,
            color = colors.container,
            contentColor = colors.content,
            border = border,
        ) { body() }
    }
}

/** A chip in a single-choice group (Show: files and folders / files only / folders only). */
@Composable
fun ChoiceChip(
    text: String,
    selected: Boolean,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    icon: ImageVector? = null,
) {
    GanderChip(
        text = text,
        icon = icon ?: if (selected) fyi.arlo.gander.ui.theme.GanderIcons.Check else null,
        style = if (selected) ChipStyle.SELECTED else ChipStyle.OUTLINED,
        selected = selected,
        onClick = onClick,
        modifier = modifier.semantics { role = Role.RadioButton },
    )
}

/** The small uppercase tag ("DEFAULT"): orange fill, on-accent text. [neutral] draws it in `surface-3`. */
@Composable
fun Tag(text: String, modifier: Modifier = Modifier, neutral: Boolean = false) {
    val c = GanderTheme.colors
    Surface(
        // Drawn in capitals, announced and found by tests as written.
        modifier = modifier.clearAndSetSemantics { this.text = AnnotatedString(text) },
        shape = GanderShape.Tag,
        color = if (neutral) c.surface3 else c.accent,
        contentColor = if (neutral) c.text2 else c.onAccent,
    ) {
        Text(
            text = text.uppercase(),
            style = GanderTheme.type.tag,
            modifier = Modifier.padding(horizontal = 7.dp, vertical = 2.dp),
        )
    }
}
