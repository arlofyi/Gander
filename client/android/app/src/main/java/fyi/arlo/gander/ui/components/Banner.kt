package fyi.arlo.gander.ui.components

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material3.Icon
import androidx.compose.material3.LocalContentColor
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import fyi.arlo.gander.ui.theme.GanderIcons
import fyi.arlo.gander.ui.theme.GanderShape
import fyi.arlo.gander.ui.theme.GanderTheme

/** The banner tones (design document section 8): neutral info, warn, bad, ok and live. */
enum class BannerTone { INFO, WARN, BAD, OK, LIVE }

/** Container, rule and icon colours of a [BannerTone]. Text is always `text` (or on-accent when live). */
data class BannerColors(val container: Color, val content: Color, val icon: Color, val rule: Color?)

@Composable
fun BannerTone.colors(): BannerColors {
    val c = GanderTheme.colors
    return when (this) {
        BannerTone.INFO -> BannerColors(c.surface2, c.text, c.text2, null)
        BannerTone.WARN -> BannerColors(c.warnContainer, c.text, c.warn, c.warn)
        BannerTone.BAD -> BannerColors(c.badContainer, c.text, c.bad, c.bad)
        BannerTone.OK -> BannerColors(c.okContainer, c.text, c.ok, null)
        BannerTone.LIVE -> BannerColors(c.accent, c.onAccent, c.onAccent, null)
    }
}

/** The tone's default icon; banners always pair colour with an icon and a sentence. */
fun BannerTone.defaultIcon(): ImageVector = when (this) {
    BannerTone.INFO -> GanderIcons.Info
    BannerTone.WARN -> GanderIcons.Warn
    BannerTone.BAD -> GanderIcons.Error
    BannerTone.OK -> GanderIcons.Ok
    BannerTone.LIVE -> GanderIcons.Stream
}

/**
 * A tinted container with a 3 dp left rule in the status colour (warn and bad), an icon, and a
 * sentence. [content] is the body below the optional bold [title]; actions (a text button) go there too.
 */
@Composable
fun Banner(
    tone: BannerTone,
    modifier: Modifier = Modifier,
    icon: ImageVector = tone.defaultIcon(),
    title: String? = null,
    containerColor: Color? = null,
    content: @Composable ColumnScope.() -> Unit,
) {
    val colors = tone.colors()
    val rule = colors.rule
    Row(
        modifier = modifier
            .fillMaxWidth()
            .clip(GanderShape.Banner)
            .background(containerColor ?: colors.container)
            .drawBehind {
                if (rule != null) {
                    val w = 3.dp.toPx()
                    drawLine(rule, Offset(w / 2f, 0f), Offset(w / 2f, size.height), strokeWidth = w)
                }
            }
            .padding(horizontal = 16.dp, vertical = 12.dp),
        horizontalArrangement = Arrangement.spacedBy(12.dp),
        verticalAlignment = Alignment.Top,
    ) {
        Icon(icon, contentDescription = null, tint = colors.icon, modifier = Modifier.padding(top = 1.dp).size(22.dp))
        CompositionLocalProvider(LocalContentColor provides colors.content) {
            Column(modifier = Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                if (title != null) {
                    Text(title, style = MaterialTheme.typography.bodyMedium.copy(fontWeight = FontWeight.SemiBold))
                }
                content()
            }
        }
    }
}

/** A banner with a plain sentence. */
@Composable
fun Banner(
    tone: BannerTone,
    text: String,
    modifier: Modifier = Modifier,
    icon: ImageVector = tone.defaultIcon(),
    title: String? = null,
    containerColor: Color? = null,
) {
    Banner(tone = tone, modifier = modifier, icon = icon, title = title, containerColor = containerColor) {
        Text(text, style = MaterialTheme.typography.bodyMedium)
    }
}
