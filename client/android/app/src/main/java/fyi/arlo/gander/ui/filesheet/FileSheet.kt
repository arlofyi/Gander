package fyi.arlo.gander.ui.filesheet

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CheckboxDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.repeatOnLifecycle
import fyi.arlo.gander.R
import fyi.arlo.gander.core.data.UiErrorKind
import fyi.arlo.gander.core.data.actions.ActionOption
import fyi.arlo.gander.core.data.actions.ActionReason
import fyi.arlo.gander.core.data.actions.ActionWarning
import fyi.arlo.gander.core.data.actions.FileAction
import fyi.arlo.gander.core.data.browse.parentPath
import fyi.arlo.gander.core.net.protocol.Entry
import fyi.arlo.gander.core.net.protocol.EntryFlag
import fyi.arlo.gander.core.net.protocol.EntryMeta
import fyi.arlo.gander.ui.browser.CELL_GLYPH_SIZE
import fyi.arlo.gander.ui.browser.EntryGlyph
import fyi.arlo.gander.ui.browser.formatFileSize
import fyi.arlo.gander.ui.browser.glyphKind
import fyi.arlo.gander.ui.browser.icon
import fyi.arlo.gander.ui.common.text
import fyi.arlo.gander.ui.components.ActionNote
import fyi.arlo.gander.ui.components.ActionPanel
import fyi.arlo.gander.ui.components.ActionPanelState
import fyi.arlo.gander.ui.components.Banner
import fyi.arlo.gander.ui.components.BannerTone
import fyi.arlo.gander.ui.components.ChipStyle
import fyi.arlo.gander.ui.components.GanderChip
import fyi.arlo.gander.ui.components.GanderProgress
import fyi.arlo.gander.ui.components.GanderTopBar
import fyi.arlo.gander.ui.components.TypeTile
import fyi.arlo.gander.ui.components.icon
import fyi.arlo.gander.ui.shell.isExpandedWidth
import fyi.arlo.gander.ui.theme.GanderIcons
import fyi.arlo.gander.ui.theme.GanderShape
import fyi.arlo.gander.ui.theme.GanderTheme

/** Test tags for the file sheet (instrumented tests, WP-29). */
object FileSheetTags {
    const val SHEET = "file-sheet"
    const val PANE = "file-detail-pane"
    const val DETAILS_LOADING = "file-sheet-details-loading"
    const val DETAILS_FAILED = "file-sheet-details-failed"
    const val RETRY = "file-sheet-retry"
    const val REMEMBER = "file-sheet-remember"
    const val RESET = "file-sheet-reset"

    fun action(action: FileAction) = "file-sheet-action-${action.wire}"
}

/** What the sheet can be asked to do; the screen wires the ViewModel in, tests pass their own. */
class FileSheetActions(
    val onAction: (FileAction) -> Unit = {},
    val onRememberChange: (Boolean) -> Unit = {},
    val onResetDefault: () -> Unit = {},
    val onRetry: () -> Unit = {},
)

@Composable
private fun rememberSheetActions(viewModel: FileSheetViewModel, handlers: FileActionHandlers): FileSheetActions {
    val lifecycleOwner = LocalLifecycleOwner.current
    LaunchedEffect(viewModel, lifecycleOwner) {
        lifecycleOwner.lifecycle.repeatOnLifecycle(Lifecycle.State.STARTED) { viewModel.watchEvents() }
    }
    val currentHandlers by rememberUpdatedState(handlers)
    return remember(viewModel) {
        FileSheetActions(
            onAction = { action -> viewModel.onActionClick(action)?.let { currentHandlers.run(action, it) } },
            onRememberChange = viewModel::setRememberChoice,
            onResetDefault = viewModel::resetDefault,
            onRetry = viewModel::reload,
        )
    }
}

/**
 * The file sheet as a modal bottom sheet (mockups 17 and 18). It stays open when an action is
 * run: the preview, fetch and stream screens are entered through [handlers] and Back returns
 * here. The ViewModel collects `/events` while the sheet is visible (computer locked, share
 * permissions changed, file changed).
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun FileSheet(
    viewModel: FileSheetViewModel,
    onDismiss: () -> Unit,
    handlers: FileActionHandlers,
    modifier: Modifier = Modifier,
) {
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    val actions = rememberSheetActions(viewModel, handlers)
    ModalBottomSheet(
        onDismissRequest = onDismiss,
        modifier = modifier,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
        containerColor = GanderTheme.colors.surface,
        scrimColor = GanderTheme.colors.scrim,
    ) {
        FileSheetContent(state, actions)
    }
}

/** The same file details and actions inline, for the tablet's detail pane (mockup 35). */
@Composable
fun FileDetailPane(viewModel: FileSheetViewModel, handlers: FileActionHandlers, modifier: Modifier = Modifier) {
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    val actions = rememberSheetActions(viewModel, handlers)
    FileDetailContent(state, actions, modifier)
}

/** Stateless sheet body; the Compose UI tests render it with a fixed [FileSheetUiState]. */
@Composable
fun FileSheetContent(state: FileSheetUiState, actions: FileSheetActions, modifier: Modifier = Modifier) {
    Column(
        modifier = modifier
            .fillMaxWidth()
            .verticalScroll(rememberScrollState())
            .navigationBarsPadding()
            .padding(bottom = 22.dp)
            .testTag(FileSheetTags.SHEET),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Header(state, Modifier.padding(horizontal = 20.dp))
        FlagChips(state, Modifier.padding(horizontal = 20.dp))
        DetailsBanner(state, actions, Modifier.padding(horizontal = 16.dp))
        DetailRows(state.entry, Modifier.padding(horizontal = 20.dp))
        RememberedUnavailableBanner(state, Modifier.padding(horizontal = 16.dp))
        ActionsSection(state, actions, wide = false)
        RememberSection(state, actions, Modifier.padding(horizontal = 20.dp))
    }
}

/**
 * The tablet detail pane body: title bar, first-page preview beside the facts, the three actions
 * side by side, and the remember row.
 */
@Composable
fun FileDetailContent(state: FileSheetUiState, actions: FileSheetActions, modifier: Modifier = Modifier) {
    Column(
        modifier = modifier
            .fillMaxWidth()
            .verticalScroll(rememberScrollState())
            .padding(bottom = 24.dp)
            .testTag(FileSheetTags.PANE),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        GanderTopBar(
            title = state.entry.name,
            subtitle = locationLine(state),
            modifier = Modifier.statusBarsPadding().padding(start = 8.dp),
        )
        Row(modifier = Modifier.padding(horizontal = 24.dp), horizontalArrangement = Arrangement.spacedBy(24.dp)) {
            EntryGlyph(
                entry = state.entry,
                path = state.target.path,
                size = CELL_GLYPH_SIZE,
                shape = GanderShape.Banner,
                glyphSize = 48.dp,
                modifier = Modifier
                    .width(380.dp)
                    .height(214.dp),
            )
            Column(modifier = Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                FlagChips(state)
                DetailsBanner(state, actions)
                DetailRows(state.entry)
            }
        }
        RememberedUnavailableBanner(state, Modifier.padding(horizontal = 24.dp))
        ActionsSection(state, actions, wide = true)
        RememberSection(state, actions, Modifier.padding(horizontal = 24.dp))
    }
}

// Header, details, metadata -------------------------------------------------------------------------

/** "studio-pc › Projects › Clients › Kestrel house". */
@Composable
private fun locationLine(state: FileSheetUiState): String {
    val share = state.shareName.ifEmpty { state.target.shareId }
    val folder = parentPath(state.target.path)
    val location = (listOf(share) + folder.split('/').filter { it.isNotEmpty() }).joinToString(" › ")
    return if (state.hostName.isEmpty()) location else stringResource(R.string.sheet_where, state.hostName, location)
}

@Composable
private fun Header(state: FileSheetUiState, modifier: Modifier = Modifier) {
    val entry = state.entry
    val c = GanderTheme.colors
    Row(modifier = modifier, horizontalArrangement = Arrangement.spacedBy(14.dp), verticalAlignment = Alignment.CenterVertically) {
        TypeTile(glyphKind(entry).icon(), size = 52.dp, iconSize = 32.dp, saved = state.cachedLocally)
        Column(Modifier.weight(1f)) {
            Text(
                entry.name,
                style = MaterialTheme.typography.titleMedium.copy(fontSize = 18.sp, lineHeight = 24.sp),
                maxLines = 3,
                overflow = TextOverflow.Ellipsis,
            )
            Text(
                text = locationLine(state),
                style = MaterialTheme.typography.bodyMedium.copy(fontSize = 13.sp),
                color = c.text3,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun FlagChips(state: FileSheetUiState, modifier: Modifier = Modifier) {
    val chips = flagChips(state)
    if (chips.isEmpty()) return
    FlowRow(modifier = modifier, horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        chips.forEach { chip -> GanderChip(text = chip.label, icon = chip.icon, style = chip.style, small = true) }
    }
}

@Composable
private fun DetailsBanner(state: FileSheetUiState, actions: FileSheetActions, modifier: Modifier = Modifier) {
    val host = state.hostName.ifEmpty { stringResource(R.string.host_os_unknown) }
    when (val details = state.details) {
        DetailsState.Loaded -> Unit
        DetailsState.Loading -> Column(
            modifier = modifier.testTag(FileSheetTags.DETAILS_LOADING),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            GanderProgress(fraction = null)
            Text(
                stringResource(R.string.sheet_loading_details, host),
                style = MaterialTheme.typography.bodySmall.copy(fontSize = 13.sp),
                color = GanderTheme.colors.text3,
            )
        }
        is DetailsState.Failed -> Banner(
            tone = BannerTone.BAD,
            modifier = modifier.testTag(FileSheetTags.DETAILS_FAILED),
        ) {
            Text(
                text = if (details.error.kind == UiErrorKind.NOT_FOUND) {
                    stringResource(R.string.reason_file_gone, host)
                } else {
                    details.error.text()
                },
                style = MaterialTheme.typography.bodyMedium,
            )
            Text(stringResource(R.string.sheet_details_failed), style = MaterialTheme.typography.bodySmall)
            if (details.error.kind.canRetry()) {
                TextButton(onClick = actions.onRetry, modifier = Modifier.testTag(FileSheetTags.RETRY)) {
                    Text(stringResource(R.string.action_retry), color = GanderTheme.colors.text, fontWeight = FontWeight.SemiBold)
                }
            }
        }
    }
}

/** Revoked access, a wrong certificate and a forgotten host do not get better by asking again. */
private fun UiErrorKind.canRetry() = this != UiErrorKind.REVOKED &&
    this != UiErrorKind.WRONG_COMPUTER &&
    this != UiErrorKind.NOT_PAIRED &&
    this != UiErrorKind.NOT_FOUND

private data class SheetChip(val label: String, val icon: ImageVector?, val style: ChipStyle = ChipStyle.FILLED)

@Composable
private fun flagChips(state: FileSheetUiState): List<SheetChip> = buildList {
    state.entry.flags.forEach { flag ->
        when (flag) {
            EntryFlag.CLOUD_ONLY -> add(SheetChip(stringResource(R.string.sheet_chip_cloud_only), GanderIcons.Cloud))
            EntryFlag.READONLY -> add(SheetChip(stringResource(R.string.sheet_chip_readonly), null))
            EntryFlag.SYSTEM -> add(SheetChip(stringResource(R.string.sheet_chip_system), null))
            EntryFlag.ENCRYPTED -> add(SheetChip(stringResource(R.string.sheet_chip_encrypted), GanderIcons.Lock))
            EntryFlag.LINK -> add(SheetChip(stringResource(R.string.sheet_chip_link), GanderIcons.Link))
            EntryFlag.HIDDEN -> add(SheetChip(stringResource(R.string.entry_flag_hidden), GanderIcons.EyeOff))
            EntryFlag.UNKNOWN -> Unit
        }
    }
    if (state.cachedLocally) add(SheetChip(stringResource(R.string.sheet_chip_saved), GanderIcons.Check, ChipStyle.OK))
    if (state.network.metered) add(SheetChip(stringResource(R.string.sheet_chip_metered), GanderIcons.Cellular, ChipStyle.WARN))
}

@Composable
private fun DetailRows(entry: Entry, modifier: Modifier = Modifier) {
    Column(modifier = modifier, verticalArrangement = Arrangement.spacedBy(4.dp)) {
        DetailRow(stringResource(R.string.sheet_label_size), formatFileSize(entry.size))
        DetailRow(stringResource(R.string.sheet_label_modified), formatDateTime(entry.mtime))
        DetailRow(stringResource(R.string.sheet_label_created), formatDateTime(entry.ctime))
        DetailRow(
            stringResource(R.string.sheet_label_type),
            entry.mime ?: entry.ext?.let { stringResource(R.string.sheet_type_ext, ".$it") }.orEmpty(),
        )
        entry.meta?.let { MetaRows(it) }
    }
}

@Composable
private fun MetaRows(meta: EntryMeta) {
    meta.image?.let { image ->
        DetailRow(
            stringResource(R.string.sheet_meta_dimensions),
            stringResource(R.string.sheet_meta_dimensions_value, image.width, image.height),
        )
        DetailRow(stringResource(R.string.sheet_meta_taken), formatExifTime(image.takenAt))
        DetailRow(stringResource(R.string.sheet_meta_camera), image.camera.orEmpty())
    }
    meta.video?.let { video ->
        DetailRow(
            stringResource(R.string.sheet_meta_dimensions),
            stringResource(R.string.sheet_meta_dimensions_value, video.width, video.height),
        )
        DetailRow(stringResource(R.string.sheet_meta_duration), formatDuration(video.durationMs))
        DetailRow(stringResource(R.string.sheet_meta_codec), video.codec.orEmpty())
    }
    meta.pdf?.let { DetailRow(stringResource(R.string.sheet_meta_pages), it.pages.toString()) }
    meta.audio?.let { audio ->
        DetailRow(stringResource(R.string.sheet_meta_duration), formatDuration(audio.durationMs))
        DetailRow(stringResource(R.string.sheet_meta_song_title), audio.title.orEmpty())
        DetailRow(stringResource(R.string.sheet_meta_artist), audio.artist.orEmpty())
    }
    meta.office?.pages?.let { DetailRow(stringResource(R.string.sheet_meta_pages), it.toString()) }
}

/** A label and its value (the `kv` list of the mockups); a blank value draws nothing (the host did not know it). */
@Composable
private fun DetailRow(label: String, value: String) {
    if (value.isBlank()) return
    val c = GanderTheme.colors
    Row(horizontalArrangement = Arrangement.spacedBy(14.dp)) {
        Text(
            label,
            style = MaterialTheme.typography.bodyMedium.copy(fontSize = 13.sp),
            color = c.text3,
            modifier = Modifier.width(96.dp),
        )
        Text(value, style = MaterialTheme.typography.bodyMedium.copy(fontSize = 13.sp), color = c.text, modifier = Modifier.weight(1f))
    }
}

// Actions ---------------------------------------------------------------------------------------------

/** "Your usual choice for .blend files (Stream) is not available now, so Fetch is suggested instead." */
@Composable
private fun RememberedUnavailableBanner(state: FileSheetUiState, modifier: Modifier = Modifier) {
    val ext = state.extension ?: return
    val remembered = state.remembered ?: return
    val fallback = state.plan.default ?: return
    if (state.plan.rememberedUnavailable == null) return
    Banner(
        tone = BannerTone.INFO,
        text = stringResource(R.string.sheet_remembered_unavailable, ".$ext", remembered.label(), fallback.label()),
        modifier = modifier,
    )
}

@Composable
private fun ActionsSection(state: FileSheetUiState, actions: FileSheetActions, wide: Boolean) {
    val host = state.hostName.ifEmpty { stringResource(R.string.host_os_unknown) }
    val pad = if (wide) 24.dp else 16.dp
    Text(
        stringResource(R.string.sheet_actions_title),
        style = GanderTheme.type.section,
        color = GanderTheme.colors.text2,
        modifier = Modifier.padding(start = pad + 4.dp, top = 6.dp),
    )
    if (wide) {
        Row(
            modifier = Modifier.padding(horizontal = pad),
            horizontalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            state.plan.options.forEach { option ->
                Box(Modifier.weight(1f)) { SheetAction(option, host, wide = true) { actions.onAction(option.action) } }
            }
        }
    } else {
        Column(modifier = Modifier.padding(horizontal = pad), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            state.plan.options.forEach { option ->
                SheetAction(option, host, wide = false) { actions.onAction(option.action) }
            }
        }
    }
}

/** The rail caption of an action: where the work happens. */
@Composable
fun FileAction.railCaption(host: String): String = when (this) {
    FileAction.PREVIEW -> stringResource(R.string.rail_preview, host)
    FileAction.FETCH -> stringResource(if (isExpandedWidth()) R.string.rail_fetch_tablet else R.string.rail_fetch_phone)
    FileAction.STREAM -> stringResource(R.string.rail_stream, host)
}

private fun ActionWarning.icon(): ImageVector = when (this) {
    ActionWarning.OwnerApproval -> GanderIcons.User
    ActionWarning.RelayedConnection -> GanderIcons.Relay
    ActionWarning.CloudOnly -> GanderIcons.Cloud
    else -> GanderIcons.Warn
}

private fun ActionReason.icon(): ImageVector = when (this) {
    ActionReason.HostLocked -> GanderIcons.Lock
    is ActionReason.NoPermission, ActionReason.StreamRefusedByPolicy, is ActionReason.StreamRefused -> GanderIcons.Shield
    ActionReason.HostUnreachable, ActionReason.PhoneOffline -> GanderIcons.Cloud
    else -> GanderIcons.Info
}

@Composable
private fun SheetAction(option: ActionOption, host: String, wide: Boolean, onClick: () -> Unit) {
    val notes = buildList {
        val reason = option.reason
        if (reason == null) {
            option.warnings.forEach { add(ActionNote(it.text(host), it.icon())) }
        } else {
            add(ActionNote(reason.text(host), reason.icon()))
            option.suggestion?.let { add(ActionNote(stringResource(R.string.sheet_try_instead, it.label()), GanderIcons.Info)) }
        }
    }
    ActionPanel(
        title = option.action.label(),
        icon = option.action.icon(),
        state = when {
            !option.enabled -> ActionPanelState.UNAVAILABLE
            option.isDefault -> ActionPanelState.DEFAULT
            else -> ActionPanelState.AVAILABLE
        },
        hint = if (option.reason == null) option.hint?.text(host) else null,
        notes = notes,
        mode = option.action,
        railCaption = option.action.railCaption(host),
        defaultTag = stringResource(R.string.sheet_default_tag),
        modifier = Modifier
            .then(if (wide) Modifier else Modifier.fillMaxWidth())
            .testTag(FileSheetTags.action(option.action)),
        onClick = onClick,
    )
}

@Composable
private fun RememberSection(state: FileSheetUiState, actions: FileSheetActions, modifier: Modifier = Modifier) {
    val ext = state.extension ?: return
    val shownExt = ".$ext"
    val c = GanderTheme.colors
    Column(modifier = modifier) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .toggleable(
                    value = state.rememberChoice,
                    role = Role.Checkbox,
                    onValueChange = actions.onRememberChange,
                )
                .testTag(FileSheetTags.REMEMBER),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Checkbox(
                checked = state.rememberChoice,
                onCheckedChange = null,
                colors = CheckboxDefaults.colors(checkedColor = c.primary, checkmarkColor = c.onPrimary, uncheckedColor = c.outline),
                modifier = Modifier.size(48.dp),
            )
            Text(
                stringResource(R.string.sheet_remember, shownExt),
                style = MaterialTheme.typography.bodyMedium.copy(fontSize = 13.5.sp),
                color = c.text2,
            )
        }
        val remembered = state.remembered ?: return@Column
        Row(verticalAlignment = Alignment.CenterVertically) {
            // When the remembered action is off, the banner above the actions already says so.
            Text(
                text = if (state.plan.rememberedUnavailable == null) {
                    stringResource(R.string.sheet_remembered, shownExt, remembered.label())
                } else {
                    ""
                },
                style = MaterialTheme.typography.bodySmall,
                color = c.text3,
                modifier = Modifier.weight(1f),
            )
            TextButton(onClick = actions.onResetDefault, modifier = Modifier.testTag(FileSheetTags.RESET)) {
                Text(stringResource(R.string.sheet_remembered_reset), color = c.text, fontWeight = FontWeight.SemiBold)
            }
        }
    }
}
