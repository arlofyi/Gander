package fyi.arlo.gander.ui.components

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
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
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import fyi.arlo.gander.core.data.actions.FileAction
import fyi.arlo.gander.ui.theme.GanderIcons
import fyi.arlo.gander.ui.theme.GanderShape
import fyi.arlo.gander.ui.theme.GanderTheme

/** The three looks of an action panel (design document section 8). */
enum class ActionPanelState {
    /** `surface-2` panel. */
    AVAILABLE,

    /** The suggested action: `accent-container` fill, 2 dp accent outline and a "Default" tag. */
    DEFAULT,

    /** Outline only, content at 50 %, the reason line at full contrast. */
    UNAVAILABLE,
}

/** One "why" line under the hint: an icon and a sentence, always at full contrast. */
data class ActionNote(val text: String, val icon: ImageVector = GanderIcons.Info)

/** The mode glyph used for [FileAction] everywhere (eye, arrow into a tray, screen with play). */
fun FileAction.icon(): ImageVector = when (this) {
    FileAction.PREVIEW -> GanderIcons.Preview
    FileAction.FETCH -> GanderIcons.Fetch
    FileAction.STREAM -> GanderIcons.Stream
}

/**
 * An action panel: icon tile, title (with the "Default" tag when [state] is DEFAULT), one-line
 * cost [hint], [notes] (reasons and warnings) and, for a file mode, the [WorkRail] with
 * [railCaption]. Used by the file sheet, the tablet detail pane, the add-computer chooser and the
 * cellular dialog. An unavailable panel is not clickable but stays in the accessibility tree, so
 * TalkBack reads why.
 */
@Composable
fun ActionPanel(
    title: String,
    icon: ImageVector,
    modifier: Modifier = Modifier,
    state: ActionPanelState = ActionPanelState.AVAILABLE,
    hint: String? = null,
    notes: List<ActionNote> = emptyList(),
    mode: FileAction? = null,
    railCaption: String? = null,
    defaultTag: String? = null,
    compact: Boolean = false,
    trailing: (@Composable () -> Unit)? = null,
    onClick: () -> Unit,
) {
    val c = GanderTheme.colors
    val container: Color
    val border: BorderStroke?
    when (state) {
        ActionPanelState.AVAILABLE -> {
            container = c.surface2
            border = null
        }
        ActionPanelState.DEFAULT -> {
            container = c.accentContainer
            border = BorderStroke(2.dp, c.accent)
        }
        ActionPanelState.UNAVAILABLE -> {
            container = Color.Transparent
            border = BorderStroke(1.dp, c.line)
        }
    }
    val dim = if (state == ActionPanelState.UNAVAILABLE) 0.5f else 1f
    Surface(
        onClick = onClick,
        enabled = state != ActionPanelState.UNAVAILABLE,
        shape = GanderShape.Panel,
        color = container,
        contentColor = c.text,
        border = border,
        modifier = modifier.fillMaxWidth(),
    ) {
        Row(
            modifier = Modifier.padding(
                start = if (compact) 12.dp else 14.dp,
                end = if (compact) 12.dp else 14.dp,
                top = if (compact) 10.dp else 14.dp,
                bottom = if (compact) 10.dp else 12.dp,
            ),
            horizontalArrangement = Arrangement.spacedBy(14.dp),
            verticalAlignment = if (trailing != null && notes.isEmpty() && mode == null) Alignment.CenterVertically else Alignment.Top,
        ) {
            Box(
                modifier = Modifier
                    .alpha(dim)
                    .size(if (compact) 32.dp else 40.dp),
                contentAlignment = Alignment.Center,
            ) {
                Surface(shape = MaterialTheme.shapes.medium, color = c.surface, contentColor = c.text) {
                    Box(Modifier.size(if (compact) 32.dp else 40.dp), contentAlignment = Alignment.Center) {
                        Icon(icon, contentDescription = null, modifier = Modifier.size(if (compact) 18.dp else 24.dp))
                    }
                }
            }
            Column(modifier = Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        text = title,
                        style = MaterialTheme.typography.titleMedium.copy(fontSize = if (compact) 14.sp else 16.sp),
                        color = if (state == ActionPanelState.DEFAULT) c.onAccentContainer else c.text,
                        modifier = Modifier.alpha(dim),
                    )
                    if (state == ActionPanelState.DEFAULT && defaultTag != null) Tag(defaultTag)
                }
                if (hint != null) {
                    Text(
                        text = hint,
                        style = MaterialTheme.typography.bodyMedium.copy(fontSize = 13.sp, lineHeight = 18.sp),
                        color = c.text2,
                        modifier = Modifier.alpha(dim),
                    )
                }
                notes.forEach { note ->
                    Row(
                        modifier = Modifier.padding(top = 4.dp),
                        horizontalArrangement = Arrangement.spacedBy(6.dp),
                        verticalAlignment = Alignment.Top,
                    ) {
                        Icon(note.icon, contentDescription = null, tint = c.text, modifier = Modifier.padding(top = 1.dp).size(16.dp))
                        Text(
                            text = note.text,
                            style = MaterialTheme.typography.bodyMedium.copy(fontSize = 13.sp, lineHeight = 18.sp),
                            color = c.text,
                        )
                    }
                }
                if (mode != null && railCaption != null) {
                    WorkRail(
                        mode = mode,
                        caption = railCaption,
                        singleLine = false,
                        modifier = Modifier.padding(top = 8.dp).alpha(if (state == ActionPanelState.UNAVAILABLE) 0.7f else 1f),
                    )
                }
            }
            if (trailing != null) {
                Box(Modifier.alpha(dim)) { trailing() }
            }
        }
    }
}
