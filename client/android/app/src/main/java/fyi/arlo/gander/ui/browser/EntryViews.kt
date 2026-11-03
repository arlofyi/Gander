package fyi.arlo.gander.ui.browser

import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CheckboxDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import fyi.arlo.gander.R
import fyi.arlo.gander.core.data.browse.parentPath
import fyi.arlo.gander.core.net.protocol.Entry
import fyi.arlo.gander.core.net.protocol.EntryFlag
import fyi.arlo.gander.core.net.protocol.EntryKind
import fyi.arlo.gander.ui.theme.GanderIcons
import fyi.arlo.gander.ui.theme.GanderShape
import fyi.arlo.gander.ui.theme.GanderTheme

/** How one entry is drawn; the browser derives it from state, the instrumented tests pass it directly. */
data class EntryDisplay(
    val entry: Entry,
    /** Share-relative path; the stable key and test tag of the row. */
    val path: String,
    val selected: Boolean = false,
    /** Selection mode is on (rows show their selected state). */
    val selecting: Boolean = false,
    /** Search and recent rows show the folder they live in. */
    val showFolder: Boolean = false,
    /** The file shown in the tablet's detail pane: drawn as the selected row. */
    val highlighted: Boolean = false,
)

/** The type glyph of a [GlyphKind] (design document section 6, outlined icons). */
fun GlyphKind.icon(): ImageVector = when (this) {
    GlyphKind.FOLDER -> GanderIcons.Folder
    GlyphKind.IMAGE -> GanderIcons.Image
    GlyphKind.PDF -> GanderIcons.Pdf
    GlyphKind.TEXT -> GanderIcons.Doc
    GlyphKind.OFFICE -> GanderIcons.Slides
    GlyphKind.VIDEO -> GanderIcons.Video
    GlyphKind.AUDIO -> GanderIcons.Audio
    GlyphKind.ARCHIVE -> GanderIcons.Archive
    GlyphKind.FILE -> GanderIcons.File
}

/**
 * The neutral type tile (`surface-2`, folders `surface-3`), with the entry's thumbnail drawn over
 * it once loaded. With a [path] (share-relative) and a [LocalThumbnailScope], the thumbnail
 * appears (WP-24); until then, and whenever the host cannot make one, the glyph stays.
 */
@Composable
fun EntryGlyph(
    entry: Entry,
    modifier: Modifier = Modifier,
    size: Dp = ROW_GLYPH_SIZE,
    path: String? = null,
    shape: RoundedCornerShape = GanderShape.Tile,
    glyphSize: Dp = 24.dp,
) {
    val kind = remember(entry) { glyphKind(entry) }
    val c = GanderTheme.colors
    val folder = kind == GlyphKind.FOLDER
    Box(
        modifier = modifier
            .size(size)
            .clip(shape)
            .background(if (folder) c.surface3 else c.surface2),
        contentAlignment = Alignment.Center,
    ) {
        Icon(
            imageVector = kind.icon(),
            contentDescription = null,
            modifier = Modifier.size(glyphSize),
            tint = if (folder) c.text else c.text2,
        )
        if (path != null) EntryThumbnail(entry, path, size, Modifier.fillMaxSize())
    }
}

/** "48.2 MB · Oct 30, 2026" for a file, "Oct 30, 2026" for a folder. */
@Composable
internal fun entrySubtitle(entry: Entry): String {
    val date = remember(entry.mtime) { formatModified(entry.mtime) }
    return if (entry.kind == EntryKind.DIR) {
        date
    } else {
        listOf(formatFileSize(entry.size), date).filter { it.isNotEmpty() }.joinToString(" · ")
    }
}

/** The flag icons of an entry: cloud-only, link, hidden (client spec: flags render with a label). */
@Composable
private fun FlagIcons(entry: Entry) {
    val tint = GanderTheme.colors.text3
    Row(horizontalArrangement = Arrangement.spacedBy(10.dp), verticalAlignment = Alignment.CenterVertically) {
        if (EntryFlag.CLOUD_ONLY in entry.flags) {
            Icon(GanderIcons.Cloud, contentDescription = stringResource(R.string.entry_flag_cloud), modifier = Modifier.size(18.dp), tint = tint)
        }
        if (EntryFlag.LINK in entry.flags) {
            Icon(GanderIcons.Link, contentDescription = stringResource(R.string.entry_flag_link), modifier = Modifier.size(18.dp), tint = tint)
        }
        if (EntryFlag.HIDDEN in entry.flags) {
            Icon(GanderIcons.EyeOff, contentDescription = stringResource(R.string.entry_flag_hidden), modifier = Modifier.size(18.dp), tint = tint)
        }
    }
}

private fun Entry.hasFlagIcons() = flags.any { it == EntryFlag.CLOUD_ONLY || it == EntryFlag.LINK || it == EntryFlag.HIDDEN }

/**
 * A full-bleed file row (mockup 12): type tile, name, "size · date", flags. While selecting,
 * files carry a checkbox and folders are dimmed (they cannot be fetched as a whole yet).
 */
@OptIn(ExperimentalFoundationApi::class)
@Composable
fun EntryRow(
    display: EntryDisplay,
    onClick: () -> Unit,
    onLongClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val entry = display.entry
    val c = GanderTheme.colors
    val openable = entry.isOpenable()
    val dimFolder = display.selecting && !entry.isSelectable()
    val highlight = display.selected || display.highlighted
    Row(
        modifier = modifier
            .fillMaxWidth()
            .testTag(BrowserTags.entry(display.path))
            .then(
                if (display.highlighted) {
                    Modifier.padding(horizontal = 8.dp).clip(GanderShape.Panel)
                } else {
                    Modifier
                },
            )
            .background(if (highlight) c.accentContainer else Color.Transparent)
            .semantics { selected = display.selected || display.highlighted }
            .combinedClickable(onClick = onClick, onLongClick = onLongClick)
            .heightIn(min = 64.dp)
            .padding(start = if (display.highlighted) 12.dp else 20.dp, end = 16.dp, top = 10.dp, bottom = 10.dp)
            .alpha(if (!openable || dimFolder) 0.48f else 1f),
        horizontalArrangement = Arrangement.spacedBy(14.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        EntryGlyph(entry, path = display.path)
        Column(modifier = Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(1.dp)) {
            Text(
                entry.name,
                style = MaterialTheme.typography.bodyLarge.copy(fontSize = 15.5.sp, lineHeight = 22.sp),
                color = c.text,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
            if (display.showFolder) {
                val folder = parentPath(display.path)
                if (folder.isNotEmpty()) {
                    Text(folder, maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.bodySmall, color = c.text3)
                }
            }
            Text(
                entrySubtitle(entry),
                style = MaterialTheme.typography.bodyMedium.copy(fontSize = 13.sp, lineHeight = 18.sp),
                color = c.text3,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
        if (entry.hasFlagIcons()) FlagIcons(entry)
        if (display.selecting && entry.isSelectable()) {
            Checkbox(
                checked = display.selected,
                onCheckedChange = null,
                colors = CheckboxDefaults.colors(checkedColor = c.primary, checkmarkColor = c.onPrimary, uncheckedColor = c.outline),
            )
        }
    }
}

/**
 * A grid card (mockup 13): the thumbnail made on the computer over the type glyph, then the name
 * and size. Online-only files carry a small badge on the image.
 */
@OptIn(ExperimentalFoundationApi::class)
@Composable
fun EntryGridCell(
    display: EntryDisplay,
    onClick: () -> Unit,
    onLongClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val entry = display.entry
    val c = GanderTheme.colors
    val highlight = display.selected || display.highlighted
    Column(
        modifier = modifier
            .testTag(BrowserTags.entry(display.path))
            .alpha(if (entry.isOpenable()) 1f else 0.48f)
            .semantics(mergeDescendants = true) { selected = display.selected || display.highlighted }
            .clip(GanderShape.TileLarge)
            .background(if (highlight) c.accentContainer else c.surface2)
            .combinedClickable(onClick = onClick, onLongClick = onLongClick),
    ) {
        Box(
            modifier = Modifier
                .fillMaxWidth()
                .height(CELL_IMAGE_HEIGHT)
                .background(c.surface3),
        ) {
            EntryGlyph(
                entry,
                size = CELL_GLYPH_SIZE,
                path = display.path,
                shape = RoundedCornerShape(0.dp),
                glyphSize = 32.dp,
                modifier = Modifier.fillMaxSize(),
            )
            if (EntryFlag.CLOUD_ONLY in entry.flags) {
                Surface(
                    color = Color.Black.copy(alpha = 0.55f),
                    contentColor = Color.White,
                    shape = GanderShape.ChipSmall,
                    modifier = Modifier.align(Alignment.TopStart).padding(6.dp),
                ) {
                    Row(
                        modifier = Modifier.padding(horizontal = 8.dp, vertical = 4.dp),
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.spacedBy(4.dp),
                    ) {
                        Icon(GanderIcons.Cloud, contentDescription = stringResource(R.string.entry_flag_cloud), modifier = Modifier.size(14.dp))
                        Text(stringResource(R.string.sheet_chip_cloud_only), style = MaterialTheme.typography.labelMedium)
                    }
                }
            }
            if (display.selected) {
                Box(
                    modifier = Modifier
                        .align(Alignment.TopEnd)
                        .padding(6.dp)
                        .size(24.dp)
                        .clip(CircleShape)
                        .background(c.primary),
                    contentAlignment = Alignment.Center,
                ) {
                    Icon(
                        GanderIcons.Check,
                        contentDescription = stringResource(R.string.entry_selected),
                        tint = c.onPrimary,
                        modifier = Modifier.size(16.dp).offset(),
                    )
                }
            }
        }
        Column(modifier = Modifier.padding(start = 10.dp, end = 10.dp, top = 8.dp, bottom = 10.dp)) {
            Text(
                text = entry.name,
                style = MaterialTheme.typography.bodyMedium.copy(fontSize = 13.sp, fontWeight = FontWeight.Medium),
                color = c.text,
                maxLines = 1,
                overflow = TextOverflow.MiddleEllipsis,
            )
            Text(
                text = if (entry.kind == EntryKind.DIR) entrySubtitle(entry) else formatFileSize(entry.size),
                style = MaterialTheme.typography.bodySmall,
                color = c.text3,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
    }
}

/** Height of the image area of a grid card. */
internal val CELL_IMAGE_HEIGHT: Dp = 118.dp
