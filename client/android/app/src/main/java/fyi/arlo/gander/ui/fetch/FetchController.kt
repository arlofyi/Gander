package fyi.arlo.gander.ui.fetch

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.os.Build
import android.widget.Toast
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.platform.LocalContext
import androidx.core.content.ContextCompat
import fyi.arlo.gander.AppGraph
import fyi.arlo.gander.R
import fyi.arlo.gander.core.data.UiError
import fyi.arlo.gander.core.data.toUiError
import fyi.arlo.gander.core.data.fetch.FolderZip
import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.protocol.ZipPlan
import fyi.arlo.gander.core.data.fetch.FetchDecision
import fyi.arlo.gander.core.data.fetch.FetchGate
import fyi.arlo.gander.core.data.fetch.FetchJob
import fyi.arlo.gander.core.data.fetch.fileKey
import fyi.arlo.gander.core.data.browse.EntryTarget
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch
import java.time.Instant

/** A fetch waiting for the user's answer to the cellular dialog. */
data class CellularPrompt(val targets: List<EntryTarget>, val totalBytes: Long)

/**
 * The folder download waiting for the user (WP-68): the zip is only ever started from [Ready],
 * after the computer has measured the folder and the user has seen the count and the size.
 */
sealed interface ZipPrompt {
    val folder: EntryTarget

    /** The dry run is in flight. */
    data class Measuring(override val folder: EntryTarget) : ZipPrompt

    /** What the zip would hold; Download queues it. */
    data class Ready(override val folder: EntryTarget, val plan: ZipPlan) : ZipPrompt

    /** `413 too_large`: over the computer's limits ([message] is its own sentence). Nothing to download. */
    data class TooLarge(override val folder: EntryTarget, val message: String) : ZipPrompt

    /** The dry run failed (offline, no permission, folder gone); Retry measures again. */
    data class Failed(override val folder: EntryTarget, val error: UiError) : ZipPrompt
}

/**
 * What the file sheet's Fetch and the browser's multi-select Fetch call (`GanderNavHost`'s
 * `onFetch` / `onFetchSelected`), and what the fetch panel's buttons do. Holds the UI state of the
 * cellular dialog and the panel; the work is the engine's.
 */
@Stable
class FetchController internal constructor(
    private val graph: AppGraph,
    private val context: Context,
    private val scope: CoroutineScope,
    private val askNotificationPermission: () -> Unit,
) {
    var cellularPrompt: CellularPrompt? by mutableStateOf(null)
        private set

    var panelVisible: Boolean by mutableStateOf(false)
        private set

    /**
     * Android 8 and 9 only: a "Save to device" waiting for the user to pick where
     * (`ACTION_CREATE_DOCUMENT`, launched by the panel). Newer versions save straight to Downloads.
     */
    var pendingSaveAs: FetchJob? by mutableStateOf(null)
        private set

    /** The folder download being confirmed; [FetchOverlay] draws the dialog. */
    var zipPrompt: ZipPrompt? by mutableStateOf(null)
        private set

    private var zipJob: Job? = null

    private val engine get() = graph.fetchEngine
    private val offline get() = graph.offlineFiles

    fun request(target: EntryTarget) = requestAll(listOf(target))

    fun requestAll(targets: List<EntryTarget>) {
        scope.launch {
            val current = targets.associateWith { graph.cachedFileLookup.isCached(it) }
            val decision = FetchGate.decide(
                targets = targets,
                network = graph.networkStateMonitor.current(),
                cellularWarnBytes = graph.fetchSettings.current().cellularWarnBytes,
                isCachedCurrent = { current[it] == true },
            )
            when (decision) {
                FetchDecision.NothingToFetch -> if (targets.any { current[it] == true }) showPanel()
                is FetchDecision.OpenCached -> openCached(decision.target)
                is FetchDecision.ConfirmCellular -> cellularPrompt = CellularPrompt(decision.targets, decision.totalBytes)
                is FetchDecision.Start -> start(decision.targets)
            }
        }
    }

    /**
     * "Download folder as zip" (WP-68): asks the computer how big [folder] is, shows the count and
     * the size, and starts nothing until [confirmZip].
     */
    fun requestZip(folder: EntryTarget) {
        zipJob?.cancel()
        zipPrompt = ZipPrompt.Measuring(folder)
        zipJob = scope.launch {
            zipPrompt = try {
                ZipPrompt.Ready(folder, engine.planZip(folder))
            } catch (e: CancellationException) {
                throw e
            } catch (e: GanderError.Host.TooLarge) {
                ZipPrompt.TooLarge(folder, e.hostMessage)
            } catch (e: GanderError) {
                ZipPrompt.Failed(folder, e.toUiError())
            }
        }
    }

    fun retryZip() {
        zipPrompt?.folder?.let(::requestZip)
    }

    /** The user saw the numbers and said Download: queue the zip like a file (cellular rule included). */
    fun confirmZip() {
        val prompt = zipPrompt as? ZipPrompt.Ready ?: return
        zipPrompt = null
        requestAll(listOf(FolderZip.target(prompt.folder, prompt.plan, mtime = Instant.now().toString())))
    }

    fun dismissZip() {
        zipJob?.cancel()
        zipPrompt = null
    }

    fun confirmCellular() {
        val prompt = cellularPrompt ?: return
        cellularPrompt = null
        scope.launch { start(prompt.targets) }
    }

    fun dismissCellular() {
        cellularPrompt = null
    }

    private suspend fun start(targets: List<EntryTarget>) {
        askNotificationPermission()
        engine.enqueueAll(targets)
        panelVisible = true
    }

    fun showPanel() {
        panelVisible = true
    }

    fun hidePanel() {
        panelVisible = false
    }

    private suspend fun openCached(target: EntryTarget) {
        val (cached, file) = offline.open(target.fileKey) ?: return toast(R.string.fetch_copy_gone)
        if (!FileIntents.view(context, cached, file)) toast(R.string.fetch_no_app)
    }

    fun open(job: FetchJob) = scope.launch {
        val (cached, file) = offline.open(job.key) ?: return@launch toast(R.string.fetch_copy_gone)
        if (!FileIntents.view(context, cached, file)) toast(R.string.fetch_no_app)
    }

    fun share(job: FetchJob) = scope.launch {
        val (cached, file) = offline.open(job.key) ?: return@launch toast(R.string.fetch_copy_gone)
        if (!FileIntents.share(context, cached, file, context.getString(R.string.fetch_share_title, cached.name))) {
            toast(R.string.fetch_no_app)
        }
    }

    /** "Save to device": a copy in the phone's Downloads/Gander, visible to every app. */
    fun saveToDevice(job: FetchJob) {
        if (!DeviceSave.canSaveDirectly) {
            pendingSaveAs = job
            return
        }
        scope.launch {
            val (cached, file) = offline.open(job.key) ?: return@launch toast(R.string.fetch_copy_gone)
            try {
                DeviceSave.toDownloads(context.contentResolver, file, cached.name, FileIntents.mimeOf(cached.name, cached.mime))
                toast(R.string.fetch_saved_to_downloads)
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (e: Exception) {
                fyi.arlo.gander.core.DebugLog.failure("save to device", e)
                toast(R.string.fetch_save_failed)
            }
        }
    }

    /** The answer to [pendingSaveAs]: [target] is where the user chose to save, null if cancelled. */
    fun saveAs(target: android.net.Uri?) {
        val job = pendingSaveAs ?: return
        pendingSaveAs = null
        if (target == null) return
        scope.launch {
            val (_, file) = offline.open(job.key) ?: return@launch toast(R.string.fetch_copy_gone)
            try {
                DeviceSave.copyTo(context.contentResolver, file, target)
                toast(R.string.fetch_saved)
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (e: Exception) {
                fyi.arlo.gander.core.DebugLog.failure("save to device", e)
                toast(R.string.fetch_save_failed)
            }
        }
    }

    fun setKeepOffline(job: FetchJob, keep: Boolean) = scope.launch { offline.setPinned(job.key, keep) }

    fun cancel(job: FetchJob) = scope.launch { engine.cancel(job.id) }

    fun retry(job: FetchJob) = scope.launch { engine.retry(job.id) }

    fun remove(job: FetchJob) = scope.launch { engine.remove(job.id) }

    fun clearFinished() = scope.launch { engine.clearFinished() }

    private fun toast(message: Int) {
        Toast.makeText(context, message, Toast.LENGTH_SHORT).show()
    }
}

/**
 * The app's [FetchController]. Also resumes, once per process, jobs that the system paused
 * (foreground-service time limit) or that a dead process left behind (client spec: "resuming on
 * next launch"), and asks for the notification permission (Android 13+) on the first fetch only.
 */
@Composable
fun rememberFetchController(graph: AppGraph): FetchController {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var asked by rememberSaveable { mutableStateOf(false) }
    val permission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { }
    val controller = remember(graph) {
        FetchController(graph, context, scope) {
            val needed = Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
                ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) !=
                PackageManager.PERMISSION_GRANTED
            if (needed && !asked) {
                asked = true
                permission.launch(Manifest.permission.POST_NOTIFICATIONS)
            }
        }
    }
    LaunchedEffect(graph) { FetchLaunch.resume(graph) }
    return controller
}

/** Picks up persisted work when the app comes to the foreground. */
object FetchLaunch {
    suspend fun resume(graph: AppGraph) {
        val engine = graph.fetchEngine
        engine.start()
        engine.resumePaused()
        if (engine.hasPendingWork()) FetchService.start(graph.appContext)
    }
}
