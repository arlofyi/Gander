package fyi.arlo.gander.ui.settings

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Close
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CenterAlignedTopAppBar
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import fyi.arlo.gander.BuildConfig
import fyi.arlo.gander.R
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.RadioButtonDefaults
import androidx.compose.ui.unit.sp
import fyi.arlo.gander.ui.components.GanderTopBar
import fyi.arlo.gander.ui.theme.GanderIcons
import fyi.arlo.gander.ui.theme.GanderTheme
import fyi.arlo.gander.core.data.actions.FileAction
import fyi.arlo.gander.core.data.fetch.VerifyPolicy
import fyi.arlo.gander.core.data.hosts.PairedHost
import fyi.arlo.gander.core.data.settings.AppSettings
import fyi.arlo.gander.core.data.settings.ExtensionList
import fyi.arlo.gander.ui.browser.formatFileSize
import fyi.arlo.gander.ui.common.relativeTimeText
import fyi.arlo.gander.ui.filesheet.label
import fyi.arlo.gander.ui.lock.AuthAvailability
import fyi.arlo.gander.ui.lock.AuthResult
import fyi.arlo.gander.ui.lock.LocalDeviceAuthenticator
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/** Test tags for the Settings screen (instrumented tests). */
object SettingsTags {
    const val SCREEN = "settings-screen"
    const val CELLULAR = "settings-cellular"
    const val VERIFY = "settings-verify"
    const val CACHE_CAP = "settings-cache-cap"
    const val STORAGE = "settings-storage"
    const val STREAM_TYPES = "settings-stream-types"
    const val STREAM_TYPES_FIELD = "settings-stream-types-field"
    const val STREAM_TYPES_SAVE = "settings-stream-types-save"
    const val STREAM_TYPES_RESET = "settings-stream-types-reset"
    const val LOCK = "settings-lock"
    const val FILES_APP = "settings-files-app"
    const val LOCK_MINUTES = "settings-lock-minutes"
    const val LOCK_SETUP_DIALOG = "settings-lock-setup-dialog"
    const val EXPORT = "settings-export"
    const val RESET_ALL = "settings-reset-actions"

    fun choice(index: Int) = "settings-choice-$index"

    fun action(ext: String) = "settings-action-$ext"

    fun host(hostId: String) = "settings-host-$hostId"
}

/**
 * Settings: the cellular rule, download verification, the saved-files limit, remembered default
 * actions and Stream-first types, the app lock, the diagnostics export, and the list of computers
 * this phone is paired with. Every change is saved as soon as it is made.
 */
@Composable
fun SettingsScreen(
    viewModel: SettingsViewModel,
    onBack: () -> Unit,
    onOpenStorage: () -> Unit,
    onOpenHost: (hostId: String) -> Unit,
    modifier: Modifier = Modifier,
) {
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    val context = LocalContext.current
    val authenticator = LocalDeviceAuthenticator.current
    val snackbar = remember { SnackbarHostState() }
    var lockSetupHelp by remember { mutableStateOf(false) }

    val enableTitle = stringResource(R.string.settings_lock_prompt_enable)
    val disableTitle = stringResource(R.string.settings_lock_prompt_disable)
    val promptSubtitle = stringResource(R.string.lock_prompt_subtitle)
    val failedMessage = stringResource(R.string.settings_diagnostics_failed)
    val shareTitle = stringResource(R.string.settings_diagnostics_share_title)
    val shareSubject = stringResource(R.string.settings_diagnostics_subject)

    LaunchedEffect(state.export) {
        when (val export = state.export) {
            is DiagnosticsExport.Ready -> {
                val file = withContext(Dispatchers.IO) { runCatching { DiagnosticsShare.write(context, export.text) }.getOrNull() }
                viewModel.exportHandled()
                if (file == null || !DiagnosticsShare.send(context, file, shareTitle, shareSubject)) {
                    snackbar.showSnackbar(failedMessage)
                }
            }
            DiagnosticsExport.Failed -> {
                viewModel.exportHandled()
                snackbar.showSnackbar(failedMessage)
            }
            else -> Unit
        }
    }

    SettingsContent(
        state = state,
        snackbar = snackbar,
        onBack = onBack,
        onOpenStorage = onOpenStorage,
        onOpenHost = onOpenHost,
        onVerify = viewModel::setVerify,
        onCellular = viewModel::setCellularWarnBytes,
        onCacheCap = viewModel::setCacheCapBytes,
        onStreamTypes = viewModel::setStreamOnlyTypes,
        onToggleLock = { enable ->
            // The device credential is asked for in both directions: turning the lock on proves it
            // works, and turning it off is not something whoever holds an unlocked phone may do.
            if (authenticator.availability() == AuthAvailability.AVAILABLE) {
                authenticator.authenticate(if (enable) enableTitle else disableTitle, promptSubtitle) { result ->
                    if (result == AuthResult.Success) viewModel.setAppLockEnabled(enable)
                }
            } else if (enable) {
                lockSetupHelp = true
            } else {
                viewModel.setAppLockEnabled(false)
            }
        },
        onLockMinutes = viewModel::setAppLockMinutes,
        onToggleFilesApp = viewModel::setFilesAppEnabled,
        onDefaultAction = viewModel::setDefaultAction,
        onResetDefaultAction = viewModel::resetDefaultAction,
        onResetAllDefaultActions = viewModel::resetAllDefaultActions,
        onExport = viewModel::exportDiagnostics,
        modifier = modifier,
    )

    if (lockSetupHelp) {
        AlertDialog(
            modifier = Modifier.testTag(SettingsTags.LOCK_SETUP_DIALOG),
            onDismissRequest = { lockSetupHelp = false },
            title = { Text(stringResource(R.string.settings_lock_setup_title)) },
            text = { Text(stringResource(R.string.settings_lock_setup_body)) },
            confirmButton = { TextButton(onClick = { lockSetupHelp = false }) { Text(stringResource(R.string.sheet_ok)) } },
        )
    }
}

/** Stateless Settings screen; the instrumented tests render it with fixed state. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsContent(
    state: SettingsUiState,
    modifier: Modifier = Modifier,
    onBack: () -> Unit = {},
    onOpenStorage: () -> Unit = {},
    onOpenHost: (hostId: String) -> Unit = {},
    onVerify: (VerifyPolicy) -> Unit = {},
    onCellular: (Long) -> Unit = {},
    onCacheCap: (Long) -> Unit = {},
    onStreamTypes: (String?) -> Unit = {},
    onToggleLock: (Boolean) -> Unit = {},
    onLockMinutes: (Int) -> Unit = {},
    onToggleFilesApp: (Boolean) -> Unit = {},
    onDefaultAction: (ext: String, FileAction) -> Unit = { _, _ -> },
    onResetDefaultAction: (ext: String) -> Unit = {},
    onResetAllDefaultActions: () -> Unit = {},
    onExport: () -> Unit = {},
    snackbar: SnackbarHostState = remember { SnackbarHostState() },
) {
    val settings = state.settings
    var dialog by remember { mutableStateOf<SettingsDialog?>(null) }

    Scaffold(
        modifier = modifier.testTag(SettingsTags.SCREEN),
        containerColor = GanderTheme.colors.bg,
        topBar = {
            // A top-level destination (bottom navigation): no back arrow (mockup 26).
            GanderTopBar(title = stringResource(R.string.settings_title), modifier = Modifier.statusBarsPadding())
        },
        snackbarHost = { SnackbarHost(snackbar) },
    ) { innerPadding ->
        if (!state.loaded) return@Scaffold
        LazyColumn(Modifier.fillMaxSize().padding(innerPadding)) {
            item { SectionHeader(R.string.settings_section_downloads) }
            item {
                SettingRow(
                    title = stringResource(R.string.settings_cellular_title),
                    value = cellularLabel(settings.cellularWarnBytes),
                    tag = SettingsTags.CELLULAR,
                    onClick = { dialog = SettingsDialog.Cellular },
                )
            }
            item {
                SettingRow(
                    title = stringResource(R.string.settings_verify_title),
                    value = verifyLabel(settings.verify),
                    tag = SettingsTags.VERIFY,
                    onClick = { dialog = SettingsDialog.Verify },
                )
            }

            item { SectionHeader(R.string.settings_section_storage) }
            item {
                SettingRow(
                    title = stringResource(R.string.settings_cache_title),
                    value = formatFileSize(settings.cacheCapBytes),
                    tag = SettingsTags.CACHE_CAP,
                    onClick = { dialog = SettingsDialog.CacheCap },
                )
            }
            item {
                SettingRow(
                    title = stringResource(R.string.settings_storage_open),
                    value = stringResource(R.string.settings_storage_open_summary),
                    tag = SettingsTags.STORAGE,
                    onClick = onOpenStorage,
                )
            }

            item { SectionHeader(R.string.settings_section_opening) }
            item {
                ListItem(
                    headlineContent = { Text(stringResource(R.string.settings_remembered_title)) },
                    supportingContent = if (state.defaultActions.isEmpty()) {
                        { Text(stringResource(R.string.settings_remembered_empty)) }
                    } else {
                        null
                    },
                    trailingContent = if (state.defaultActions.isNotEmpty()) {
                        {
                            TextButton(onClick = onResetAllDefaultActions, modifier = Modifier.testTag(SettingsTags.RESET_ALL)) {
                                Text(stringResource(R.string.settings_remembered_reset_all))
                            }
                        }
                    } else {
                        null
                    },
                )
            }
            items(state.defaultActions.size) { index ->
                val row = state.defaultActions[index]
                DefaultActionItem(
                    row = row,
                    onChange = { onDefaultAction(row.ext, it) },
                    onReset = { onResetDefaultAction(row.ext) },
                )
            }
            item {
                SettingRow(
                    title = stringResource(R.string.settings_stream_types_title),
                    value = pluralStringResource(
                        R.plurals.settings_stream_types_count,
                        settings.streamOnlyTypes.size,
                        settings.streamOnlyTypes.size,
                    ),
                    tag = SettingsTags.STREAM_TYPES,
                    onClick = { dialog = SettingsDialog.StreamTypes },
                )
            }
            item {
                SwitchRow(
                    title = stringResource(R.string.settings_files_app_title),
                    summary = stringResource(R.string.settings_files_app_summary),
                    checked = settings.filesAppEnabled,
                    tag = SettingsTags.FILES_APP,
                    onChange = onToggleFilesApp,
                )
            }

            item { SectionHeader(R.string.settings_section_security) }
            item {
                ListItem(
                    headlineContent = { Text(stringResource(R.string.settings_lock_title)) },
                    supportingContent = { Text(stringResource(R.string.settings_lock_summary)) },
                    trailingContent = { Switch(checked = settings.appLockEnabled, onCheckedChange = null) },
                    modifier = Modifier
                        .toggleable(
                            value = settings.appLockEnabled,
                            role = Role.Switch,
                            onValueChange = onToggleLock,
                        )
                        .testTag(SettingsTags.LOCK),
                )
            }
            item {
                SettingRow(
                    title = stringResource(R.string.settings_lock_after_title),
                    value = lockMinutesLabel(settings.appLockMinutes),
                    tag = SettingsTags.LOCK_MINUTES,
                    enabled = settings.appLockEnabled,
                    onClick = { dialog = SettingsDialog.LockMinutes },
                )
            }
            item {
                SettingRow(
                    title = stringResource(R.string.settings_diagnostics_title),
                    value = stringResource(
                        if (state.export == DiagnosticsExport.Working) {
                            R.string.settings_diagnostics_working
                        } else {
                            R.string.settings_diagnostics_summary
                        },
                    ),
                    tag = SettingsTags.EXPORT,
                    enabled = state.export != DiagnosticsExport.Working,
                    onClick = onExport,
                )
            }

            item { SectionHeader(R.string.settings_section_computers) }
            if (state.hosts.isEmpty()) {
                item {
                    ListItem(headlineContent = { Text(stringResource(R.string.settings_computers_empty)) })
                }
            }
            items(state.hosts.size) { index ->
                HostRow(state.hosts[index], onClick = { onOpenHost(state.hosts[index].id) })
            }

            item { SectionHeader(R.string.settings_section_about) }
            item {
                ListItem(headlineContent = { Text(stringResource(R.string.settings_version, BuildConfig.VERSION_NAME)) })
            }
        }
    }

    when (dialog) {
        SettingsDialog.Cellular -> ChoiceDialog(
            title = stringResource(R.string.settings_cellular_title),
            body = stringResource(R.string.settings_cellular_dialog_body),
            choices = AppSettings.CELLULAR_CHOICES.map { it to cellularLabel(it) },
            selected = settings.cellularWarnBytes,
            onSelect = onCellular,
            onDismiss = { dialog = null },
        )
        SettingsDialog.Verify -> ChoiceDialog(
            title = stringResource(R.string.settings_verify_title),
            body = stringResource(R.string.settings_verify_dialog_body),
            choices = VerifyPolicy.entries.map { it to verifyLabel(it) },
            selected = settings.verify,
            onSelect = onVerify,
            onDismiss = { dialog = null },
        )
        SettingsDialog.CacheCap -> ChoiceDialog(
            title = stringResource(R.string.settings_cache_title),
            body = stringResource(R.string.settings_cache_dialog_body),
            choices = AppSettings.CACHE_CAP_CHOICES.map { it to formatFileSize(it) },
            selected = settings.cacheCapBytes,
            onSelect = onCacheCap,
            onDismiss = { dialog = null },
        )
        SettingsDialog.LockMinutes -> ChoiceDialog(
            title = stringResource(R.string.settings_lock_after_title),
            body = null,
            choices = AppSettings.LOCK_MINUTES_CHOICES.map { it to lockMinutesLabel(it) },
            selected = settings.appLockMinutes,
            onSelect = onLockMinutes,
            onDismiss = { dialog = null },
        )
        SettingsDialog.StreamTypes -> StreamTypesDialog(
            types = settings.streamOnlyTypes,
            onSave = { text ->
                onStreamTypes(text)
                dialog = null
            },
            onReset = {
                onStreamTypes(null)
                dialog = null
            },
            onDismiss = { dialog = null },
        )
        null -> Unit
    }
}

private enum class SettingsDialog { Cellular, Verify, CacheCap, LockMinutes, StreamTypes }

@Composable
private fun SectionHeader(title: Int) {
    fyi.arlo.gander.ui.components.SectionHeader(stringResource(title))
}

/** A settings row (M3 list item with supporting text): title, its current value in muted text. */
@Composable
private fun SettingRow(title: String, value: String?, tag: String, enabled: Boolean = true, onClick: () -> Unit) {
    ListItem(
        headlineContent = { Text(title, style = MaterialTheme.typography.bodyLarge.copy(fontSize = 15.5.sp)) },
        supportingContent = value?.let { text -> { Text(text, color = GanderTheme.colors.text3) } },
        trailingContent = { Icon(GanderIcons.Chevron, contentDescription = null, tint = GanderTheme.colors.text3) },
        colors = ListItemDefaults.colors(containerColor = GanderTheme.colors.bg),
        modifier = Modifier
            .alpha(if (enabled) 1f else 0.5f)
            .clickable(enabled = enabled, onClick = onClick)
            .testTag(tag),
    )
}

/** A settings row with a switch: title, a muted sentence, the whole row toggles (one TalkBack stop). */
@Composable
private fun SwitchRow(title: String, summary: String, checked: Boolean, tag: String, onChange: (Boolean) -> Unit) {
    ListItem(
        headlineContent = { Text(title, style = MaterialTheme.typography.bodyLarge.copy(fontSize = 15.5.sp)) },
        supportingContent = { Text(summary, color = GanderTheme.colors.text3) },
        trailingContent = { Switch(checked = checked, onCheckedChange = null) },
        colors = ListItemDefaults.colors(containerColor = GanderTheme.colors.bg),
        modifier = Modifier
            .toggleable(value = checked, role = Role.Switch, onValueChange = onChange)
            .testTag(tag),
    )
}

@Composable
private fun <T> ChoiceDialog(
    title: String,
    body: String?,
    choices: List<Pair<T, String>>,
    selected: T,
    onSelect: (T) -> Unit,
    onDismiss: () -> Unit,
) {
    AlertDialog(
        onDismissRequest = onDismiss,
        containerColor = GanderTheme.colors.surface2,
        title = { Text(title) },
        text = {
            Column(Modifier.verticalScroll(rememberScrollState()).selectableGroup()) {
                if (body != null) Text(body, style = MaterialTheme.typography.bodyMedium, modifier = Modifier.padding(bottom = 8.dp))
                choices.forEachIndexed { index, (value, label) ->
                    Row(
                        modifier = Modifier
                            .fillMaxWidth()
                            .selectable(
                                selected = value == selected,
                                role = Role.RadioButton,
                                onClick = {
                                    onSelect(value)
                                    onDismiss()
                                },
                            )
                            .padding(vertical = 12.dp)
                            .testTag(SettingsTags.choice(index)),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        RadioButton(
                            selected = value == selected,
                            onClick = null,
                            colors = RadioButtonDefaults.colors(selectedColor = GanderTheme.colors.accent, unselectedColor = GanderTheme.colors.outline),
                        )
                        Spacer(Modifier.width(16.dp))
                        Text(label, style = MaterialTheme.typography.bodyLarge)
                    }
                }
            }
        },
        confirmButton = {},
        dismissButton = { TextButton(onClick = onDismiss) { Text(stringResource(R.string.action_cancel)) } },
    )
}

@Composable
private fun StreamTypesDialog(types: Set<String>, onSave: (String) -> Unit, onReset: () -> Unit, onDismiss: () -> Unit) {
    var text by remember(types) { mutableStateOf(ExtensionList.format(types)) }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.settings_stream_types_dialog_title)) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(stringResource(R.string.settings_stream_types_dialog_body), style = MaterialTheme.typography.bodyMedium)
                OutlinedTextField(
                    value = text,
                    onValueChange = { text = it },
                    label = { Text(stringResource(R.string.settings_stream_types_label)) },
                    minLines = 3,
                    maxLines = 6,
                    modifier = Modifier
                        .fillMaxWidth()
                        .testTag(SettingsTags.STREAM_TYPES_FIELD),
                )
                TextButton(onClick = onReset, modifier = Modifier.testTag(SettingsTags.STREAM_TYPES_RESET)) {
                    Text(stringResource(R.string.settings_stream_types_reset))
                }
            }
        },
        confirmButton = {
            TextButton(onClick = { onSave(text) }, modifier = Modifier.testTag(SettingsTags.STREAM_TYPES_SAVE)) {
                Text(stringResource(R.string.action_save))
            }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text(stringResource(R.string.action_cancel)) } },
    )
}

@Composable
private fun DefaultActionItem(row: DefaultActionRow, onChange: (FileAction) -> Unit, onReset: () -> Unit) {
    var menuOpen by remember { mutableStateOf(false) }
    ListItem(
        headlineContent = { Text(".${row.ext}") },
        supportingContent = { Text(row.action.label()) },
        trailingContent = {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Box {
                    TextButton(onClick = { menuOpen = true }) { Text(stringResource(R.string.settings_remembered_change)) }
                    DropdownMenu(expanded = menuOpen, onDismissRequest = { menuOpen = false }) {
                        for (action in FileAction.entries) {
                            DropdownMenuItem(
                                text = { Text(action.label()) },
                                onClick = {
                                    menuOpen = false
                                    onChange(action)
                                },
                            )
                        }
                    }
                }
                IconButton(onClick = onReset) {
                    Icon(Icons.Filled.Close, contentDescription = stringResource(R.string.settings_remembered_reset, row.ext))
                }
            }
        },
        modifier = Modifier.testTag(SettingsTags.action(row.ext)),
    )
}

@Composable
private fun HostRow(host: PairedHost, onClick: () -> Unit) {
    val seen = host.lastSeenAtMillis
    ListItem(
        headlineContent = { Text(host.name) },
        supportingContent = {
            Column {
                host.pairedAs?.let { Text(stringResource(R.string.host_paired_as, it)) }
                Text(
                    if (seen != null) {
                        stringResource(R.string.host_last_seen, relativeTimeText(seen))
                    } else {
                        stringResource(R.string.host_never_seen)
                    },
                )
            }
        },
        modifier = Modifier
            .clickable(onClick = onClick)
            .testTag(SettingsTags.host(host.id)),
    )
}

@Composable
private fun cellularLabel(bytes: Long): String = when (bytes) {
    0L -> stringResource(R.string.settings_cellular_always)
    AppSettings.CELLULAR_NEVER -> stringResource(R.string.settings_cellular_never)
    else -> stringResource(R.string.settings_cellular_from, formatFileSize(bytes))
}

@Composable
private fun verifyLabel(policy: VerifyPolicy): String = when (policy) {
    VerifyPolicy.NEVER -> stringResource(R.string.settings_verify_never)
    VerifyPolicy.LARGE_FILES -> stringResource(
        R.string.settings_verify_large,
        formatFileSize(fyi.arlo.gander.core.data.fetch.FetchSettings().verifyFromBytes),
    )
    VerifyPolicy.ALWAYS -> stringResource(R.string.settings_verify_always)
}

@Composable
private fun lockMinutesLabel(minutes: Int): String =
    if (minutes == 0) {
        stringResource(R.string.settings_lock_after_now)
    } else {
        pluralStringResource(R.plurals.settings_lock_after_minutes, minutes, minutes)
    }
