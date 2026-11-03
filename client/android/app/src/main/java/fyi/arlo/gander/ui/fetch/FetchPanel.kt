package fyi.arlo.gander.ui.fetch

import android.content.Intent
import androidx.activity.ComponentActivity
import androidx.activity.compose.LocalActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
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
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.core.util.Consumer
import fyi.arlo.gander.AppGraph
import fyi.arlo.gander.R
import fyi.arlo.gander.core.data.actions.FileAction
import fyi.arlo.gander.core.data.browse.EntryTarget
import fyi.arlo.gander.core.data.fetch.CachedFile
import fyi.arlo.gander.core.data.fetch.FetchFailure
import fyi.arlo.gander.core.data.fetch.FetchJob
import fyi.arlo.gander.core.data.fetch.FetchStatus
import fyi.arlo.gander.core.net.protocol.HashAlgo
import fyi.arlo.gander.core.net.protocol.PreviewKind
import fyi.arlo.gander.ui.browser.formatFileSize
import fyi.arlo.gander.ui.browser.glyphKindForName
import fyi.arlo.gander.ui.browser.icon
import fyi.arlo.gander.ui.components.ActionPanel
import fyi.arlo.gander.ui.components.ButtonKind
import fyi.arlo.gander.ui.components.EmptyState
import fyi.arlo.gander.ui.components.GanderButton
import fyi.arlo.gander.ui.components.GanderIconButton
import fyi.arlo.gander.ui.components.GanderProgress
import fyi.arlo.gander.ui.components.GanderTopBar
import fyi.arlo.gander.ui.components.ListRow
import fyi.arlo.gander.ui.components.SectionHeader
import fyi.arlo.gander.ui.components.TypeTile
import fyi.arlo.gander.ui.components.icon
import fyi.arlo.gander.ui.theme.GanderIcons
import fyi.arlo.gander.ui.theme.GanderShape
import fyi.arlo.gander.ui.theme.GanderTheme

object FetchTags {
    /** The Downloads screen (was the fetch panel sheet; the tag keeps its name). */
    const val PANEL = "fetch-panel"
    const val CELLULAR_DIALOG = "fetch-cellular-dialog"

    fun job(id: Long) = "fetch-job-$id"

    fun save(id: Long) = "fetch-save-$id"
}

/**
 * What the fetch engine draws over the navigation: the cellular dialog. Also turns the launch
 * intent of a fetch notification into "show Downloads" ([FetchController.showPanel]; the app
 * shell navigates there).
 *
 * [onPreviewInstead] and [onStreamInstead] are the cellular dialog's alternatives (client spec:
 * "offers Preview or Stream instead"); each is offered only when it is wired.
 */
@Composable
fun FetchOverlay(
    controller: FetchController,
    @Suppress("UNUSED_PARAMETER") graph: AppGraph,
    onPreviewInstead: ((EntryTarget) -> Unit)? = null,
    onStreamInstead: ((EntryTarget) -> Unit)? = null,
) {
    NotificationIntents(controller)
    controller.zipPrompt?.let { prompt ->
        ZipDialog(prompt, onConfirm = controller::confirmZip, onDismiss = controller::dismissZip, onRetry = controller::retryZip)
    }
    controller.cellularPrompt?.let { prompt ->
        CellularDialog(
            prompt = prompt,
            onConfirm = controller::confirmCellular,
            onDismiss = controller::dismissCellular,
            onPreviewInstead = onPreviewInstead?.takeIf {
                prompt.targets.size == 1 && prompt.targets[0].entry.preview != PreviewKind.NONE
            }?.let { preview -> { controller.dismissCellular(); preview(prompt.targets[0]) } },
            onStreamInstead = onStreamInstead?.takeIf { prompt.targets.size == 1 }
                ?.let { stream -> { controller.dismissCellular(); stream(prompt.targets[0]) } },
        )
    }
}

/** Shows Downloads for the launch intent and for every new intent that asks for it. */
@Composable
private fun NotificationIntents(controller: FetchController) {
    val activity = LocalActivity.current as? ComponentActivity ?: return
    LaunchedEffect(activity) {
        if (activity.intent?.getBooleanExtra(FetchNotifications.EXTRA_SHOW_FETCHES, false) == true) {
            activity.intent.removeExtra(FetchNotifications.EXTRA_SHOW_FETCHES)
            controller.showPanel()
        }
    }
    DisposableEffect(activity) {
        val listener = Consumer<Intent> { intent ->
            if (intent.getBooleanExtra(FetchNotifications.EXTRA_SHOW_FETCHES, false)) controller.showPanel()
        }
        activity.addOnNewIntentListener(listener)
        onDispose { activity.removeOnNewIntentListener(listener) }
    }
}

/**
 * "Fetch over mobile data?" (mockup 24): the size first, the cheaper modes offered as panels, then
 * Cancel or Fetch.
 */
@Composable
fun CellularDialog(
    prompt: CellularPrompt,
    onConfirm: () -> Unit,
    onDismiss: () -> Unit,
    onPreviewInstead: (() -> Unit)? = null,
    onStreamInstead: (() -> Unit)? = null,
) {
    val size = formatFileSize(prompt.totalBytes)
    val c = GanderTheme.colors
    AlertDialog(
        modifier = Modifier.testTag(FetchTags.CELLULAR_DIALOG),
        onDismissRequest = onDismiss,
        containerColor = c.surface2,
        icon = { Icon(GanderIcons.Cellular, contentDescription = null, tint = c.warn, modifier = Modifier.size(32.dp)) },
        title = { Text(stringResource(R.string.fetch_cellular_title)) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(
                    if (prompt.targets.size == 1) {
                        stringResource(R.string.fetch_cellular_body_one, prompt.targets[0].entry.name, size)
                    } else {
                        pluralStringResource(R.plurals.fetch_cellular_body_many, prompt.targets.size, prompt.targets.size, size)
                    },
                    color = c.text2,
                )
                onPreviewInstead?.let {
                    ActionPanel(
                        title = stringResource(R.string.fetch_cellular_preview),
                        icon = FileAction.PREVIEW.icon(),
                        compact = true,
                        onClick = it,
                    )
                }
                onStreamInstead?.let {
                    ActionPanel(
                        title = stringResource(R.string.fetch_cellular_stream),
                        icon = FileAction.STREAM.icon(),
                        compact = true,
                        onClick = it,
                    )
                }
            }
        },
        confirmButton = {
            GanderButton(stringResource(R.string.fetch_cellular_confirm, size), onClick = onConfirm)
        },
        dismissButton = {
            GanderButton(stringResource(R.string.fetch_cancel), onClick = onDismiss, kind = ButtonKind.TEXT)
        },
    )
}

/**
 * Downloads (mockup 23, a bottom-navigation destination): every fetch, in progress then finished,
 * with Open with / Share / Save to device / Keep offline on a verified copy and the reason plus
 * Retry on a failed one.
 */
@Composable
fun DownloadsScreen(controller: FetchController, graph: AppGraph, onOpenStorage: () -> Unit, modifier: Modifier = Modifier) {
    val jobs by graph.fetchEngine.jobs.collectAsState(initial = emptyList())
    val cached by graph.offlineFiles.all.collectAsState(initial = emptyList())
    val cachedByKey = remember(cached) { cached.associateBy { it.key } }
    // Android 8 and 9: "Save to device" asks where (newer versions save straight to Downloads).
    val saveAs = rememberLauncherForActivityResult(ActivityResultContracts.CreateDocument("*/*")) { uri -> controller.saveAs(uri) }
    val pending = controller.pendingSaveAs
    LaunchedEffect(pending) {
        if (pending != null) saveAs.launch(DeviceSave.displayName(pending.name))
    }
    var menu by remember { mutableStateOf(false) }
    val c = GanderTheme.colors
    val inProgress = jobs.filter { it.status.active }
    val finished = jobs.filter { !it.status.active }
    Scaffold(
        modifier = modifier.testTag(FetchTags.PANEL),
        containerColor = c.bg,
        topBar = {
            GanderTopBar(title = stringResource(R.string.nav_downloads), modifier = Modifier.statusBarsPadding()) {
                GanderIconButton(GanderIcons.Storage, stringResource(R.string.storage_title), onOpenStorage)
                if (jobs.any { it.status in FetchStatus.FINISHED }) {
                    Box {
                        GanderIconButton(GanderIcons.More, stringResource(R.string.action_more), { menu = true })
                        DropdownMenu(expanded = menu, onDismissRequest = { menu = false }, containerColor = c.surface2) {
                            DropdownMenuItem(
                                text = { Text(stringResource(R.string.fetch_clear_finished)) },
                                onClick = {
                                    menu = false
                                    controller.clearFinished()
                                },
                            )
                        }
                    }
                }
            }
        },
    ) { padding ->
        if (jobs.isEmpty()) {
            Column(Modifier.fillMaxSize().padding(padding), verticalArrangement = Arrangement.Center) {
                EmptyState(
                    title = stringResource(R.string.fetch_empty_title),
                    body = stringResource(R.string.fetch_panel_empty),
                    visual = { TypeTile(GanderIcons.Fetch, size = 64.dp, iconSize = 32.dp) },
                )
            }
            return@Scaffold
        }
        LazyColumn(Modifier.fillMaxSize().padding(padding), contentPadding = PaddingValues(bottom = 16.dp)) {
            if (inProgress.isNotEmpty()) {
                item { SectionHeader(stringResource(R.string.fetch_in_progress), padding = PaddingValues(start = 20.dp, end = 20.dp, top = 2.dp, bottom = 6.dp)) }
                items(inProgress, key = { it.id }) { job -> ActiveRow(job, controller) }
            }
            if (finished.isNotEmpty()) {
                item { SectionHeader(stringResource(R.string.fetch_finished)) }
                items(finished, key = { it.id }) { job ->
                    val copy = cachedByKey[job.key]
                    if (job.status == FetchStatus.DONE && copy != null) DoneCard(job, copy, controller) else EndedRow(job, controller)
                }
            }
        }
    }
}

/** Queued, running, verifying or waiting: the progress and Cancel. */
@Composable
private fun ActiveRow(job: FetchJob, controller: FetchController) {
    val c = GanderTheme.colors
    ListRow(
        headline = job.name,
        alignTop = true,
        modifier = Modifier.testTag(FetchTags.job(job.id)),
        leading = { TypeTile(glyphKindForName(job.name).icon()) },
        supportingContent = {
            Text(statusLine(job), style = MaterialTheme.typography.bodyMedium.copy(fontSize = 13.sp), color = c.text3)
            if (job.changedOnHost) {
                Text(stringResource(R.string.fetch_changed_on_host), style = MaterialTheme.typography.bodySmall, color = c.warn)
            }
            if (job.status != FetchStatus.QUEUED) {
                val fraction = job.fraction
                GanderProgress(
                    fraction = if (job.status == FetchStatus.VERIFYING) null else fraction,
                    modifier = Modifier.padding(top = 10.dp, bottom = 4.dp),
                )
            }
        },
        trailing = {
            GanderIconButton(GanderIcons.Close, stringResource(R.string.fetch_cancel), { controller.cancel(job) })
        },
    )
}

/** A verified copy (mockup 23, "done"): Open with, Share, Save to device, Keep offline. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun DoneCard(job: FetchJob, cached: CachedFile, controller: FetchController) {
    val c = GanderTheme.colors
    Surface(
        shape = GanderShape.Panel,
        color = c.surface,
        border = BorderStroke(1.dp, c.line),
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 4.dp)
            .testTag(FetchTags.job(job.id)),
    ) {
        Column(Modifier.padding(14.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Row(horizontalArrangement = Arrangement.spacedBy(14.dp), verticalAlignment = Alignment.CenterVertically) {
                TypeTile(glyphKindForName(job.name).icon(), saved = true)
                Column(Modifier.weight(1f)) {
                    Text(
                        job.name,
                        style = MaterialTheme.typography.bodyLarge.copy(fontSize = 15.5.sp),
                        maxLines = 1,
                        overflow = TextOverflow.MiddleEllipsis,
                    )
                    Text(statusLine(job), style = MaterialTheme.typography.bodyMedium.copy(fontSize = 13.sp), color = c.ok)
                }
                GanderIconButton(GanderIcons.Close, stringResource(R.string.fetch_remove), { controller.remove(job) })
            }
            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                GanderButton(stringResource(R.string.fetch_open_with), onClick = { controller.open(job) }, icon = GanderIcons.Open, small = true)
                GanderButton(
                    stringResource(R.string.fetch_share),
                    onClick = { controller.share(job) },
                    icon = GanderIcons.Share,
                    kind = ButtonKind.OUTLINED,
                    small = true,
                )
                GanderButton(
                    stringResource(R.string.fetch_save_to_device),
                    onClick = { controller.saveToDevice(job) },
                    icon = GanderIcons.Fetch,
                    kind = ButtonKind.OUTLINED,
                    small = true,
                    modifier = Modifier.testTag(FetchTags.save(job.id)),
                )
            }
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    stringResource(R.string.fetch_keep_offline),
                    style = MaterialTheme.typography.bodyMedium.copy(fontSize = 13.sp),
                    color = c.text2,
                    modifier = Modifier.weight(1f),
                )
                Switch(checked = cached.pinned, onCheckedChange = { controller.setKeepOffline(job, it) })
            }
        }
    }
}

/** Failed, cancelled, paused, or done without a copy: the reason, Retry and Remove. */
@Composable
private fun EndedRow(job: FetchJob, controller: FetchController) {
    val c = GanderTheme.colors
    val failed = job.status == FetchStatus.FAILED
    val paused = job.status == FetchStatus.PAUSED
    ListRow(
        headline = job.name,
        alignTop = true,
        modifier = Modifier.testTag(FetchTags.job(job.id)),
        leading = {
            TypeTile(
                icon = when {
                    failed -> GanderIcons.Error
                    paused -> GanderIcons.Pause
                    else -> glyphKindForName(job.name).icon()
                },
                tint = if (failed) c.bad else null,
            )
        },
        supportingContent = {
            Text(
                statusLine(job),
                style = MaterialTheme.typography.bodyMedium.copy(fontSize = 13.sp),
                color = if (failed) c.text else c.text3,
            )
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.padding(top = 8.dp)) {
                if (job.status == FetchStatus.FAILED || job.status == FetchStatus.CANCELLED || paused) {
                    GanderButton(
                        stringResource(R.string.fetch_retry),
                        onClick = { controller.retry(job) },
                        icon = if (paused) GanderIcons.Play else GanderIcons.Refresh,
                        kind = ButtonKind.TONAL,
                        small = true,
                    )
                }
                GanderButton(stringResource(R.string.fetch_remove), onClick = { controller.remove(job) }, kind = ButtonKind.TEXT, small = true)
            }
        },
    )
}

@Composable
private fun statusLine(job: FetchJob): String {
    val done = formatFileSize(job.bytesDone)
    val total = formatFileSize(job.size)
    return when (job.status) {
        FetchStatus.QUEUED -> stringResource(R.string.fetch_status_queued)
        FetchStatus.RUNNING -> stringResource(R.string.fetch_progress_of, done, total)
        FetchStatus.VERIFYING -> stringResource(R.string.fetch_status_verifying)
        FetchStatus.WAITING -> stringResource(R.string.fetch_status_waiting, done, total)
        FetchStatus.PAUSED -> stringResource(R.string.fetch_status_paused, done, total)
        FetchStatus.CANCELLED -> stringResource(R.string.fetch_status_cancelled)
        FetchStatus.DONE -> when (job.verifiedWith) {
            HashAlgo.BLAKE3 -> stringResource(R.string.fetch_status_done_verified, total, "BLAKE3")
            HashAlgo.SHA256 -> stringResource(R.string.fetch_status_done_verified, total, "SHA-256")
            else -> stringResource(R.string.fetch_status_done, total)
        }
        FetchStatus.FAILED -> failureText(job.failure)
    }
}

@Composable
private fun failureText(failure: FetchFailure?): String = stringResource(
    when (failure) {
        FetchFailure.NETWORK, null -> R.string.fetch_failed_network
        FetchFailure.HOST_REFUSED -> R.string.fetch_failed_refused
        FetchFailure.NOT_FOUND -> R.string.fetch_failed_not_found
        FetchFailure.NOT_AUTHORISED -> R.string.fetch_failed_not_authorised
        FetchFailure.WRONG_COMPUTER -> R.string.fetch_failed_wrong_computer
        FetchFailure.INCOMPATIBLE -> R.string.fetch_failed_incompatible
        FetchFailure.FILE_CHANGED -> R.string.fetch_failed_file_changed
        FetchFailure.VERIFY_MISMATCH -> R.string.fetch_failed_verify
        FetchFailure.NO_SPACE -> R.string.fetch_failed_no_space
        FetchFailure.STORAGE -> R.string.fetch_failed_storage
        FetchFailure.HOST_FORGOTTEN -> R.string.fetch_failed_host_forgotten
    },
)
