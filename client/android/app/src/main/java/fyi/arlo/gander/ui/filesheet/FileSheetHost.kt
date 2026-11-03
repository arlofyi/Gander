package fyi.arlo.gander.ui.filesheet

import androidx.compose.runtime.Composable
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.saveable.Saver
import androidx.compose.runtime.saveable.listSaver
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import fyi.arlo.gander.AppGraph
import fyi.arlo.gander.core.data.actions.ActionRules
import fyi.arlo.gander.core.data.actions.FileAction
import fyi.arlo.gander.core.data.actions.RuleConfig
import fyi.arlo.gander.core.data.browse.EntryTarget
import fyi.arlo.gander.core.data.settings.toRuleConfig
import fyi.arlo.gander.core.net.GanderJson
import fyi.arlo.gander.core.net.protocol.Entry

/**
 * Where the three actions go. The file sheet hands the chosen file over through one of these and
 * stays open underneath, so Back from the destination returns to the sheet:
 * - [onPreview]: the preview viewers (WP-26),
 * - [onFetch]: the fetch engine, which also owns the cellular dialog (WP-27),
 * - [onStream]: the stream viewer (WP-54).
 *
 * A destination that fails tells the sheet why through `AppGraph.actionRefusals`
 * ([fyi.arlo.gander.core.data.actions.ActionRefusals.report]) with the `GanderError` it got.
 */
class FileActionHandlers(
    val onPreview: (EntryTarget) -> Unit,
    val onFetch: (EntryTarget) -> Unit,
    val onStream: (EntryTarget) -> Unit,
) {
    fun run(action: FileAction, target: EntryTarget) = when (action) {
        FileAction.PREVIEW -> onPreview(target)
        FileAction.FETCH -> onFetch(target)
        FileAction.STREAM -> onStream(target)
    }
}

/** The file currently shown in the sheet. [id] is new for every opening: a fresh ViewModel per tap. */
class OpenedFile(val id: Int, val target: EntryTarget)

/**
 * Holds which file's sheet is open. It survives recomposition, rotation and process death (the
 * target is saved as strings and the entry's own JSON), and it is what makes the sheet reappear
 * when Back returns from a preview, fetch or stream screen that was opened from it.
 */
@Stable
class FileSheetOpener internal constructor(private var lastId: Int, opened: OpenedFile?) {
    var opened: OpenedFile? by mutableStateOf(opened)
        private set

    fun show(target: EntryTarget) {
        lastId += 1
        opened = OpenedFile(lastId, target)
    }

    fun dismiss() {
        opened = null
    }

    internal companion object {
        val Saver: Saver<FileSheetOpener, Any> = listSaver(
            save = { opener ->
                val open = opener.opened
                if (open == null) {
                    listOf(opener.lastId)
                } else {
                    listOf(
                        opener.lastId,
                        open.id,
                        open.target.hostId,
                        open.target.shareId,
                        open.target.path,
                        GanderJson.json.encodeToString(Entry.serializer(), open.target.entry),
                    )
                }
            },
            restore = { saved ->
                val lastId = saved[0] as Int
                val opened = if (saved.size == SAVED_OPEN_SIZE) {
                    runCatching {
                        OpenedFile(
                            id = saved[1] as Int,
                            target = EntryTarget(
                                hostId = saved[2] as String,
                                shareId = saved[3] as String,
                                path = saved[4] as String,
                                entry = GanderJson.json.decodeFromString(Entry.serializer(), saved[5] as String),
                            ),
                        )
                    }.getOrNull()
                } else {
                    null
                }
                FileSheetOpener(lastId, opened)
            },
        )

        private const val SAVED_OPEN_SIZE = 6
    }
}

@Composable
fun rememberFileSheetOpener(): FileSheetOpener =
    rememberSaveable(saver = FileSheetOpener.Saver) { FileSheetOpener(0, null) }

/** Draws the sheet of [opener]'s file, if one is open, with a ViewModel built from [graph]. */
@Composable
fun FileSheetHost(
    opener: FileSheetOpener,
    graph: AppGraph,
    handlers: FileActionHandlers,
) {
    val open = opener.opened ?: return
    val viewModel = rememberFileSheetViewModel(open, graph) ?: return
    FileSheet(viewModel = viewModel, onDismiss = opener::dismiss, handlers = handlers)
}

/**
 * The tablet's detail pane (mockup 35): [opener]'s file inline, with the same ViewModel the sheet
 * uses, or [nothingSelected] (the folder summary) while no file is chosen. The pane always shows
 * (owner decision).
 */
@Composable
fun FileDetailPaneHost(
    opener: FileSheetOpener,
    graph: AppGraph,
    handlers: FileActionHandlers,
    nothingSelected: @Composable () -> Unit,
) {
    val open = opener.opened
    if (open == null) {
        nothingSelected()
        return
    }
    val viewModel = rememberFileSheetViewModel(open, graph) ?: return
    FileDetailPane(viewModel = viewModel, handlers = handlers)
}

/**
 * The ViewModel of [open]'s sheet. The user's cellular threshold and Stream-first types
 * (Settings, WP-28) feed the rule table: one DataStore read, so this is null for a moment.
 */
@Composable
private fun rememberFileSheetViewModel(open: OpenedFile, graph: AppGraph): FileSheetViewModel? {
    val ruleConfig by produceState<RuleConfig?>(initialValue = null, open.id) {
        value = graph.settingsRepository.current().toRuleConfig()
    }
    val config = ruleConfig ?: return null
    return viewModel(
        key = "file-sheet-${open.id}",
        factory = viewModelFactory {
            initializer {
                FileSheetViewModel(
                    target = open.target,
                    hosts = graph.hostRepository,
                    repository = graph.browseRepository,
                    stat = graph.entryStatSource,
                    prefs = graph.defaultActionPrefs,
                    network = graph.networkStateMonitor,
                    handlers = graph.phoneHandlerLookup,
                    cachedFiles = graph.cachedFileLookup,
                    prober = graph.hostProber,
                    refusalLog = graph.actionRefusals,
                    rules = ActionRules(config),
                )
            }
        },
    )
}
