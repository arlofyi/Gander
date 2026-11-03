package fyi.arlo.gander.ui.components

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import fyi.arlo.gander.ui.theme.GanderIcons
import fyi.arlo.gander.ui.theme.GanderShape
import fyi.arlo.gander.ui.theme.GanderTheme

// Structural pieces shared by the screens: the 64 dp top bar, the large screen title, section
// labels, list rows, the type tile, buttons and the segmented control.

/**
 * The 64 dp top bar of the mockups: an optional navigation icon, a title with an optional
 * one-line subtitle, and trailing [actions]. Not M3's TopAppBar: the title block wants a subtitle
 * line and the bar stays flat (tonal elevation only).
 */
@Composable
fun GanderTopBar(
    title: String,
    modifier: Modifier = Modifier,
    subtitle: String? = null,
    navigationIcon: ImageVector? = null,
    navigationDescription: String? = null,
    onNavigate: (() -> Unit)? = null,
    navigationModifier: Modifier = Modifier,
    containerColor: Color = Color.Transparent,
    contentColor: Color = GanderTheme.colors.text,
    subtitleColor: Color = GanderTheme.colors.text3,
    actions: @Composable RowScope.() -> Unit = {},
) {
    Surface(color = containerColor, contentColor = contentColor, modifier = modifier) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .height(64.dp)
                .padding(start = 4.dp, end = 6.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            if (navigationIcon != null && onNavigate != null) {
                IconButton(onClick = onNavigate, modifier = navigationModifier) {
                    Icon(navigationIcon, contentDescription = navigationDescription)
                }
            } else {
                Spacer(Modifier.width(12.dp))
            }
            Column(
                modifier = Modifier
                    .weight(1f)
                    .padding(start = 6.dp),
            ) {
                if (title.isNotEmpty()) {
                    Text(
                        text = title,
                        style = MaterialTheme.typography.titleLarge.copy(fontSize = 20.sp, lineHeight = 25.sp),
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
                if (subtitle != null) {
                    Text(
                        text = subtitle,
                        style = MaterialTheme.typography.bodySmall.copy(fontSize = 12.5.sp),
                        color = subtitleColor,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
            actions()
        }
    }
}

/** An icon button in a top bar or row, 44 dp drawn in a 48 dp target, with an optional orange dot. */
@Composable
fun GanderIconButton(
    icon: ImageVector,
    contentDescription: String?,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    tint: Color = GanderTheme.colors.text2,
    dot: Boolean = false,
    enabled: Boolean = true,
) {
    IconButton(onClick = onClick, modifier = modifier, enabled = enabled) {
        Box {
            Icon(icon, contentDescription = contentDescription, tint = tint)
            if (dot) {
                Box(
                    Modifier
                        .align(Alignment.TopEnd)
                        .offset(x = 2.dp, y = (-2).dp)
                        .size(8.dp)
                        .clip(CircleShape)
                        .background(GanderTheme.colors.accent),
                )
            }
        }
    }
}

/** The large screen title ("Your computers"), headlineMedium, 20 dp gutter. */
@Composable
fun LargeTitle(text: String, modifier: Modifier = Modifier) {
    Text(
        text = text,
        style = MaterialTheme.typography.headlineMedium.copy(fontSize = 30.sp, lineHeight = 36.sp, letterSpacing = (-0.2).sp),
        modifier = modifier.padding(start = 20.dp, end = 20.dp, top = 0.dp, bottom = 10.dp),
    )
}

/** A section label above a group of rows ("Shared with this phone"). */
@Composable
fun SectionHeader(
    text: String,
    modifier: Modifier = Modifier,
    trailing: String? = null,
    padding: PaddingValues = PaddingValues(start = 20.dp, end = 20.dp, top = 16.dp, bottom = 6.dp),
) {
    Row(modifier = modifier.fillMaxWidth().padding(padding), verticalAlignment = Alignment.CenterVertically) {
        Text(text, style = GanderTheme.type.section, color = GanderTheme.colors.text2)
        if (trailing != null) {
            Text(
                " · $trailing",
                style = GanderTheme.type.section.copy(fontWeight = FontWeight.Normal),
                color = GanderTheme.colors.text3,
            )
        }
    }
}

/** A hairline between rows, inset by the gutter. */
@Composable
fun RowDivider(modifier: Modifier = Modifier, inset: Dp = 20.dp) {
    HorizontalDivider(modifier = modifier.padding(horizontal = inset), thickness = 1.dp, color = GanderTheme.colors.line)
}

/**
 * The neutral type tile of a row (42 dp, radius 10): `surface-2` for files, `surface-3` for
 * folders, with the small green "saved on this phone" check at the corner when [saved].
 */
@Composable
fun TypeTile(
    icon: ImageVector,
    modifier: Modifier = Modifier,
    folder: Boolean = false,
    saved: Boolean = false,
    size: Dp = 42.dp,
    iconSize: Dp = 24.dp,
    tint: Color? = null,
    container: Color? = null,
) {
    val c = GanderTheme.colors
    Box(modifier = modifier.size(size)) {
        Box(
            modifier = Modifier
                .size(size)
                .clip(if (size > 44.dp) GanderShape.TileLarge else GanderShape.Tile)
                .background(container ?: if (folder) c.surface3 else c.surface2),
            contentAlignment = Alignment.Center,
        ) {
            Icon(icon, contentDescription = null, tint = tint ?: if (folder) c.text else c.text2, modifier = Modifier.size(iconSize))
        }
        if (saved) {
            Box(
                modifier = Modifier
                    .align(Alignment.BottomEnd)
                    .offset(x = 2.dp, y = 2.dp)
                    .size(18.dp)
                    .clip(CircleShape)
                    .background(c.surface),
                contentAlignment = Alignment.Center,
            ) {
                Icon(GanderIcons.Check, contentDescription = null, tint = c.ok, modifier = Modifier.size(14.dp))
            }
        }
    }
}

/**
 * A full-bleed list row (not a card): [leading], a primary line, optional secondary lines and a
 * [trailing] slot, at least 64 dp tall. [selected] uses `accent-container`, [dimmed] draws the
 * row at 48 % (unavailable offline).
 */
@Composable
fun ListRow(
    headline: String,
    modifier: Modifier = Modifier,
    supporting: String? = null,
    supportingColor: Color? = null,
    supportingContent: (@Composable () -> Unit)? = null,
    leading: (@Composable () -> Unit)? = null,
    trailing: (@Composable () -> Unit)? = null,
    selected: Boolean = false,
    dimmed: Boolean = false,
    onClick: (() -> Unit)? = null,
    onClickLabel: String? = null,
    headlineMaxLines: Int = 1,
    supportingMaxLines: Int = 1,
    alignTop: Boolean = false,
    minHeight: Dp = 64.dp,
    contentPadding: PaddingValues = PaddingValues(start = 20.dp, end = 16.dp, top = 10.dp, bottom = 10.dp),
) {
    val c = GanderTheme.colors
    val click = if (onClick != null) Modifier.clickable(onClickLabel = onClickLabel, role = Role.Button, onClick = onClick) else Modifier
    Row(
        modifier = modifier
            .fillMaxWidth()
            .then(if (selected) Modifier.background(c.accentContainer) else Modifier)
            .then(click)
            .semantics { if (selected) this.selected = true }
            .heightIn(min = minHeight)
            .padding(contentPadding)
            .alpha(if (dimmed) 0.48f else 1f),
        horizontalArrangement = Arrangement.spacedBy(14.dp),
        verticalAlignment = if (alignTop) Alignment.Top else Alignment.CenterVertically,
    ) {
        leading?.invoke()
        Column(modifier = Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(1.dp)) {
            Text(
                text = headline,
                style = MaterialTheme.typography.bodyLarge.copy(fontSize = 15.5.sp, lineHeight = 22.sp),
                color = c.text,
                maxLines = headlineMaxLines,
                overflow = TextOverflow.MiddleEllipsis,
            )
            if (supporting != null) {
                Text(
                    text = supporting,
                    style = MaterialTheme.typography.bodyMedium.copy(fontSize = 13.sp, lineHeight = 18.sp),
                    color = supportingColor ?: c.text3,
                    maxLines = supportingMaxLines,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            supportingContent?.invoke()
        }
        trailing?.invoke()
    }
}

/** What a [GanderButton] looks like. Commands are ink; destructive actions are red; orange is never a command colour. */
enum class ButtonKind { PRIMARY, TONAL, OUTLINED, TEXT, DANGER }

/** A fully rounded 44 dp button with an optional leading icon. */
@Composable
fun GanderButton(
    text: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    kind: ButtonKind = ButtonKind.PRIMARY,
    icon: ImageVector? = null,
    enabled: Boolean = true,
    small: Boolean = false,
) {
    val c = GanderTheme.colors
    val height = if (small) 38.dp else 44.dp
    val padding = PaddingValues(horizontal = if (kind == ButtonKind.TEXT) 12.dp else if (small) 16.dp else 22.dp)
    val content: @Composable RowScope.() -> Unit = {
        if (icon != null) {
            Icon(icon, contentDescription = null, modifier = Modifier.size(18.dp))
            Spacer(Modifier.width(8.dp))
        }
        Text(text, style = MaterialTheme.typography.labelLarge.copy(fontSize = 14.5.sp), maxLines = 1, overflow = TextOverflow.Ellipsis)
    }
    val sized = modifier.heightIn(min = height)
    when (kind) {
        ButtonKind.PRIMARY -> Button(onClick, sized, enabled, contentPadding = padding, content = content)
        ButtonKind.TONAL -> Button(
            onClick, sized, enabled,
            colors = ButtonDefaults.buttonColors(containerColor = c.surface3, contentColor = c.text),
            contentPadding = padding, content = content,
        )
        ButtonKind.DANGER -> Button(
            onClick, sized, enabled,
            colors = ButtonDefaults.buttonColors(containerColor = c.danger, contentColor = c.onDanger),
            contentPadding = padding, content = content,
        )
        ButtonKind.OUTLINED -> OutlinedButton(
            onClick, sized, enabled,
            colors = ButtonDefaults.outlinedButtonColors(contentColor = c.text),
            border = BorderStroke(1.dp, c.outline),
            contentPadding = padding, content = content,
        )
        ButtonKind.TEXT -> TextButton(
            onClick, sized, enabled,
            colors = ButtonDefaults.textButtonColors(contentColor = c.text),
            contentPadding = padding, content = content,
        )
    }
}

/** One option of a [Segmented] control. */
data class SegmentOption(val label: String, val icon: ImageVector? = null, val modifier: Modifier = Modifier)

/**
 * The outlined two- or three-way segmented control (Direct / Trackpad, A to Z / Z to A). The
 * selected segment is `accent-container` (selection colour). Each segment is a labelled button.
 */
@Composable
fun Segmented(
    options: List<SegmentOption>,
    selectedIndex: Int,
    onSelect: (Int) -> Unit,
    modifier: Modifier = Modifier,
) {
    val c = GanderTheme.colors
    Surface(
        modifier = modifier.height(36.dp),
        shape = RoundedCornerShape(18.dp),
        color = Color.Transparent,
        border = BorderStroke(1.dp, c.outline),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            options.forEachIndexed { index, option ->
                if (index > 0) Box(Modifier.width(1.dp).height(36.dp).background(c.outline))
                val on = index == selectedIndex
                Row(
                    modifier = option.modifier
                        .height(36.dp)
                        .background(if (on) c.accentContainer else Color.Transparent)
                        .clickable(role = Role.Tab) { onSelect(index) }
                        .semantics {
                            selected = on
                            role = Role.Tab
                        }
                        .padding(horizontal = 12.dp),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(6.dp),
                ) {
                    val tint = if (on) c.onAccentContainer else c.text2
                    if (option.icon != null) Icon(option.icon, contentDescription = null, tint = tint, modifier = Modifier.size(14.dp))
                    Text(
                        option.label,
                        style = MaterialTheme.typography.labelMedium.copy(
                            fontSize = 12.5.sp,
                            fontWeight = if (on) FontWeight.SemiBold else FontWeight.Medium,
                        ),
                        color = tint,
                        maxLines = 1,
                    )
                }
            }
        }
    }
}
