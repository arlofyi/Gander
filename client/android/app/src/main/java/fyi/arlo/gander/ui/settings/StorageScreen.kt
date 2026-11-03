package fyi.arlo.gander.ui.settings

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CenterAlignedTopAppBar
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import fyi.arlo.gander.R
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.IconToggleButton
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.sp
import fyi.arlo.gander.ui.browser.glyphKindForName
import fyi.arlo.gander.ui.browser.icon
import fyi.arlo.gander.ui.components.Banner
import fyi.arlo.gander.ui.components.BannerTone
import fyi.arlo.gander.ui.components.ButtonKind
import fyi.arlo.gander.ui.components.GanderButton
import fyi.arlo.gander.ui.components.GanderTopBar
import fyi.arlo.gander.ui.components.ListRow
import fyi.arlo.gander.ui.components.TypeTile
import fyi.arlo.gander.ui.theme.GanderIcons
import fyi.arlo.gander.ui.theme.GanderTheme
import fyi.arlo.gander.core.data.fetch.CachedFile
import fyi.arlo.gander.ui.browser.formatFileSize
import fyi.arlo.gander.ui.common.relativeTimeText

/** Test tags for the Storage screen (instrumented tests). */
object StorageTags {
    const val SCREEN = "storage-screen"
    const val SUMMARY = "storage-summary"
    const val DOWNLOADS = "storage-downloads"
    const val CLEAR_UNPINNED = "storage-clear-unpinned"
    const val CLEAR_UNPINNED_CONFIRM = "storage-clear-unpinned-confirm"
    const val CLEAR_DERIVED = "storage-clear-derived"
    const val EMPTY = "storage-empty"

    fun host(hostId: String) = "storage-host-$hostId"

    fun file(name: String) = "storage-file-$name"

    fun keep(name: String) = "storage-keep-$name"

    fun remove(name: String) = "storage-remove-$name"
}

/**
 * What is on this phone and how much room it takes (client spec: "the Storage screen shows usage
 * per host"): saved copies per computer, each openable without the computer, with Keep offline (not
 * deleted when the limit is reached) and Delete, plus the thumbnail and preview caches that can be
 * cleared any time. [onShowDownloads] opens the list of fetches in progress and finished.
 */
@Composable
fun StorageScreen(
    viewModel: StorageViewModel,
    onBack: () -> Unit,
    onOpenFile: (CachedFile) -> Unit,
    onShowDownloads: (() -> Unit)?,
    modifier: Modifier = Modifier,
) {
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    StorageContent(
        state = state,
        onBack = onBack,
        onOpenFile = onOpenFile,
        onShowDownloads = onShowDownloads,
        onKeepOffline = viewModel::setKeepOffline,
        onRemove = viewModel::remove,
        onClearUnpinned = viewModel::clearUnpinned,
        onClearDerived = viewModel::clearDerivedCaches,
        modifier = modifier,
    )
}

/** Stateless Storage screen; the instrumented tests render it with fixed state. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun StorageContent(
    state: StorageUiState,
    modifier: Modifier = Modifier,
    onBack: () -> Unit = {},
    onOpenFile: (CachedFile) -> Unit = {},
    onShowDownloads: (() -> Unit)? = null,
    onKeepOffline: (CachedFile, Boolean) -> Unit = { _, _ -> },
    onRemove: (CachedFile) -> Unit = {},
    onClearUnpinned: (hostId: String?) -> Unit = {},
    onClearDerived: () -> Unit = {},
) {
    // The computer whose unpinned files the open confirmation is about; the empty string means all.
    var confirmClear by remember { mutableStateOf<String?>(null) }

    Scaffold(
        modifier = modifier.testTag(StorageTags.SCREEN),
        containerColor = GanderTheme.colors.bg,
        topBar = {
            GanderTopBar(
                title = stringResource(R.string.storage_title),
                navigationIcon = GanderIcons.Back,
                navigationDescription = stringResource(R.string.action_back),
                onNavigate = onBack,
                modifier = Modifier.statusBarsPadding(),
            )
        },
    ) { innerPadding ->
        if (!state.loaded) return@Scaffold
        LazyColumn(Modifier.fillMaxSize().padding(innerPadding)) {
            item { Summary(state) }
            if (onShowDownloads != null) {
                item {
                    ListRow(
                        headline = stringResource(R.string.storage_downloads_title),
                        supporting = stringResource(R.string.storage_downloads_summary),
                        supportingMaxLines = 2,
                        leading = { TypeTile(GanderIcons.Fetch) },
                        trailing = { Icon(GanderIcons.Chevron, contentDescription = null, tint = GanderTheme.colors.text3) },
                        onClick = onShowDownloads,
                        modifier = Modifier.testTag(StorageTags.DOWNLOADS),
                    )
                }
            }
            if (state.hosts.isEmpty()) {
                item {
                    Column(
                        modifier = Modifier
                            .fillMaxWidth()
                            .padding(32.dp)
                            .testTag(StorageTags.EMPTY),
                        verticalArrangement = Arrangement.spacedBy(8.dp),
                        horizontalAlignment = Alignment.CenterHorizontally,
                    ) {
                        Text(
                            stringResource(R.string.storage_empty_title),
                            style = MaterialTheme.typography.titleMedium,
                            textAlign = TextAlign.Center,
                        )
                        Text(
                            stringResource(R.string.storage_empty_body),
                            style = MaterialTheme.typography.bodyMedium,
                            textAlign = TextAlign.Center,
                        )
                    }
                }
            }
            for (host in state.hosts) {
                item(key = "host-${host.hostId}") { HostHeader(host, onClear = { confirmClear = host.hostId }) }
                items(host.files, key = { "${it.key.hostId}|${it.key.shareId}|${it.key.path}" }) { file ->
                    FileRow(
                        file = file,
                        onOpen = { onOpenFile(file) },
                        onKeepOffline = { onKeepOffline(file, it) },
                        onRemove = { onRemove(file) },
                    )
                }
            }
            item { DerivedCachesRow(state.derivedBytes, onClearDerived) }
            item { ClearUnpinned(state, onClearAll = { confirmClear = ALL }) }
        }
    }

    confirmClear?.let { target ->
        val hostId = target.takeIf { it != ALL }
        val files = state.hosts.filter { hostId == null || it.hostId == hostId }.flatMap { it.files }.filter { !it.pinned }
        AlertDialog(
            onDismissRequest = { confirmClear = null },
            containerColor = GanderTheme.colors.surface2,
            title = { Text(stringResource(R.string.storage_clear_title)) },
            text = {
                Text(
                    pluralStringResource(
                        R.plurals.storage_clear_body,
                        files.size,
                        files.size,
                        formatFileSize(files.sumOf { it.size }),
                    ),
                )
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        confirmClear = null
                        onClearUnpinned(hostId)
                    },
                    modifier = Modifier.testTag(StorageTags.CLEAR_UNPINNED_CONFIRM),
                ) { Text(stringResource(R.string.storage_clear_confirm)) }
            },
            dismissButton = { TextButton(onClick = { confirmClear = null }) { Text(stringResource(R.string.action_cancel)) } },
        )
    }
}

private const val ALL = ""

/** Used of the limit, then a meter split into kept offline (orange), other saved files (ink) and caches (outline). */
@Composable
private fun Summary(state: StorageUiState) {
    val c = GanderTheme.colors
    val cap = state.capBytes.coerceAtLeast(1).toFloat()
    val otherBytes = (state.usedBytes - state.pinnedBytes).coerceAtLeast(0)
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(start = 20.dp, end = 20.dp, top = 6.dp, bottom = 8.dp)
            .testTag(StorageTags.SUMMARY),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Text(
            stringResource(R.string.storage_used, formatFileSize(state.usedBytes), formatFileSize(state.capBytes)),
            style = MaterialTheme.typography.headlineMedium.copy(fontSize = 26.sp, lineHeight = 32.sp),
        )
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .height(10.dp)
                .clip(RoundedCornerShape(5.dp))
                .background(c.surface3),
        ) {
            val parts = listOf(state.pinnedBytes to c.accent, otherBytes to c.primary, state.derivedBytes to c.outline)
            parts.forEach { (bytes, color) ->
                val fraction = (bytes / cap).coerceIn(0f, 1f)
                if (fraction > 0f) Box(Modifier.fillMaxHeight().fillMaxWidth(fraction).background(color))
            }
        }
        LegendItem(c.accent, stringResource(R.string.storage_kept, formatFileSize(state.pinnedBytes)))
        LegendItem(c.primary, stringResource(R.string.storage_legend_other, formatFileSize(otherBytes)))
        LegendItem(c.outline, stringResource(R.string.storage_legend_derived, formatFileSize(state.derivedBytes)))
        if (state.pinnedOverCap) {
            Banner(tone = BannerTone.WARN, text = stringResource(R.string.storage_kept_over_cap))
        }
    }
}

@Composable
private fun LegendItem(color: Color, text: String) {
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
        Box(Modifier.size(10.dp).clip(RoundedCornerShape(3.dp)).background(color))
        Text(text, style = MaterialTheme.typography.bodySmall, color = GanderTheme.colors.text3)
    }
}

@Composable
private fun ClearUnpinned(state: StorageUiState, onClearAll: () -> Unit) {
    Column(Modifier.padding(horizontal = 20.dp, vertical = 10.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
        GanderButton(
            text = stringResource(R.string.storage_clear_all, formatFileSize(state.unpinnedBytes)),
            onClick = onClearAll,
            kind = ButtonKind.OUTLINED,
            icon = GanderIcons.Trash,
            enabled = state.unpinnedBytes > 0,
            modifier = Modifier
                .fillMaxWidth()
                .testTag(StorageTags.CLEAR_UNPINNED),
        )
        Text(
            stringResource(R.string.storage_cap_note, formatFileSize(state.capBytes)),
            style = MaterialTheme.typography.bodySmall.copy(fontSize = 12.5.sp),
            color = GanderTheme.colors.text3,
        )
    }
}

@Composable
private fun HostHeader(host: HostStorage, onClear: () -> Unit) {
    val c = GanderTheme.colors
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(start = 20.dp, end = 8.dp, top = 12.dp)
            .testTag(StorageTags.host(host.hostId)),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f)) {
            Text(
                host.hostName ?: stringResource(R.string.storage_host_removed),
                style = GanderTheme.type.section,
                color = c.text2,
            )
            Text(
                pluralStringResource(R.plurals.storage_host_total, host.files.size, formatFileSize(host.bytes), host.files.size),
                style = MaterialTheme.typography.bodySmall,
                color = c.text3,
            )
        }
        TextButton(onClick = onClear, enabled = host.bytes > host.pinnedBytes) {
            Text(stringResource(R.string.storage_host_clear), color = c.text, fontWeight = FontWeight.SemiBold)
        }
    }
}

@Composable
private fun FileRow(file: CachedFile, onOpen: () -> Unit, onKeepOffline: (Boolean) -> Unit, onRemove: () -> Unit) {
    val c = GanderTheme.colors
    ListRow(
        headline = file.name,
        supporting = stringResource(R.string.storage_file_line, formatFileSize(file.size), relativeTimeText(file.lastUsed)),
        leading = { TypeTile(glyphKindForName(file.name).icon(), saved = true) },
        trailing = {
            Row(verticalAlignment = Alignment.CenterVertically) {
                // Keep offline: a pin, orange (selection colour) when on.
                IconToggleButton(
                    checked = file.pinned,
                    onCheckedChange = onKeepOffline,
                    modifier = Modifier.testTag(StorageTags.keep(file.name)),
                ) {
                    Icon(
                        GanderIcons.Pin,
                        contentDescription = stringResource(R.string.fetch_keep_offline),
                        tint = if (file.pinned) c.accentText else c.text3,
                    )
                }
                IconButton(onClick = onRemove, modifier = Modifier.testTag(StorageTags.remove(file.name))) {
                    Icon(GanderIcons.Trash, contentDescription = stringResource(R.string.storage_delete, file.name), tint = c.text2)
                }
            }
        },
        onClick = onOpen,
        modifier = Modifier.testTag(StorageTags.file(file.name)),
    )
}

@Composable
private fun DerivedCachesRow(bytes: Long, onClear: () -> Unit) {
    fyi.arlo.gander.ui.components.SectionHeader(stringResource(R.string.storage_derived_title))
    ListRow(
        headline = formatFileSize(bytes),
        supporting = stringResource(R.string.storage_derived_note),
        supportingMaxLines = 2,
        leading = { TypeTile(GanderIcons.Image) },
        trailing = {
            TextButton(onClick = onClear, enabled = bytes > 0, modifier = Modifier.testTag(StorageTags.CLEAR_DERIVED)) {
                Text(stringResource(R.string.storage_derived_clear), color = GanderTheme.colors.text, fontWeight = FontWeight.SemiBold)
            }
        },
    )
}
