package fyi.arlo.gander.ui.components

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.selection.toggleable
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import fyi.arlo.gander.ui.theme.GanderShape
import fyi.arlo.gander.ui.theme.GanderTheme

/**
 * A key of the stream keyboard bar: 38 dp tall, `surface` on the `surface-2` bar with a 1 dp
 * bottom edge. A sticky modifier is a toggle: when [latched] is true it is drawn orange (selection
 * colour) and announced with [latchedDescription] ("sticky, on"). Give it a [modifier] with a
 * weight to share a row.
 */
@Composable
fun KeyCap(
    label: String?,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    icon: ImageVector? = null,
    contentDescription: String? = null,
    latched: Boolean? = null,
    latchedDescription: String? = null,
) {
    val c = GanderTheme.colors
    val on = latched == true
    val container = if (on) c.accent else c.surface
    val content = if (on) c.onAccent else c.text
    val edge = c.line
    val interaction = if (latched != null) {
        Modifier.toggleable(value = on, role = Role.Switch, onValueChange = { onClick() })
    } else {
        Modifier.clickable(role = Role.Button, onClick = onClick)
    }
    Row(
        modifier = modifier
            .height(38.dp)
            .widthIn(min = 38.dp)
            .drawBehind {
                if (!on) drawLine(edge, Offset(8.dp.toPx(), size.height), Offset(size.width - 8.dp.toPx(), size.height), 1.dp.toPx())
            }
            .clip(GanderShape.Key)
            .background(container)
            .then(interaction)
            .semantics {
                if (contentDescription != null) this.contentDescription = contentDescription
                if (latched != null && latchedDescription != null) stateDescription = latchedDescription
            }
            .padding(horizontal = 6.dp),
        horizontalArrangement = Arrangement.spacedBy(6.dp, Alignment.CenterHorizontally),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (icon != null) Icon(icon, contentDescription = null, tint = content, modifier = Modifier.size(18.dp))
        if (label != null) {
            Text(
                text = label,
                style = MaterialTheme.typography.labelLarge,
                color = content,
                maxLines = 1,
                overflow = TextOverflow.Clip,
                softWrap = false,
            )
        }
    }
}
