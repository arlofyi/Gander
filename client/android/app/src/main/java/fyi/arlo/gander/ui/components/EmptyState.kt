package fyi.arlo.gander.ui.components

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import fyi.arlo.gander.ui.theme.GanderTheme

/**
 * A centred empty or result state: a visual (the Lens mark, or a [ResultBadge]), a title, a body
 * sentence, an optional muted [note] and an optional [actions] slot (a button).
 */
@Composable
fun EmptyState(
    title: String,
    modifier: Modifier = Modifier,
    body: String? = null,
    note: String? = null,
    visual: (@Composable () -> Unit)? = null,
    bodyColor: Color = GanderTheme.colors.text2,
    actions: (@Composable ColumnScope.() -> Unit)? = null,
) {
    Column(
        modifier = modifier
            .fillMaxWidth()
            .padding(horizontal = 32.dp, vertical = 32.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        visual?.invoke()
        Text(
            title,
            style = MaterialTheme.typography.titleLarge.copy(fontSize = 22.sp, fontWeight = androidx.compose.ui.text.font.FontWeight.Normal),
            textAlign = TextAlign.Center,
            modifier = Modifier.padding(top = 8.dp),
        )
        if (body != null) {
            Text(body, style = MaterialTheme.typography.bodyMedium, color = bodyColor, textAlign = TextAlign.Center)
        }
        if (note != null) {
            Text(
                note,
                style = MaterialTheme.typography.bodySmall.copy(fontSize = 13.sp, lineHeight = 18.sp),
                color = GanderTheme.colors.text3,
                textAlign = TextAlign.Center,
                modifier = Modifier.padding(top = 6.dp),
            )
        }
        if (actions != null) {
            Column(
                modifier = Modifier.padding(top = 18.dp),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(8.dp),
                content = actions,
            )
        }
    }
}

/** The 96 dp round badge of a result screen: a check on `ok-c` for success, a shield on `bad-c` for failure. */
@Composable
fun ResultBadge(icon: ImageVector, tone: BannerTone, modifier: Modifier = Modifier) {
    val colors = tone.colors()
    Box(
        modifier = modifier
            .size(96.dp)
            .clip(CircleShape)
            .background(colors.container),
        contentAlignment = Alignment.Center,
    ) {
        Icon(icon, contentDescription = null, tint = colors.icon, modifier = Modifier.size(48.dp))
    }
}
