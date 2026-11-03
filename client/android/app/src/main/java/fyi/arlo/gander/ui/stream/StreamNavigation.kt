package fyi.arlo.gander.ui.stream

import android.net.Uri
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import androidx.navigation.NavController
import androidx.navigation.NavGraphBuilder
import androidx.navigation.NavType
import androidx.navigation.compose.composable
import androidx.navigation.navArgument
import fyi.arlo.gander.AppGraph
import fyi.arlo.gander.core.data.browse.EntryTarget
import fyi.arlo.gander.core.data.stream.H264Support
import fyi.arlo.gander.core.data.stream.StreamVideoOutput
import fyi.arlo.gander.core.data.stream.WebRtcStreamTransport
import fyi.arlo.gander.core.net.GanderJson
import fyi.arlo.gander.core.net.protocol.Entry
import fyi.arlo.gander.ui.filesheet.FileActionHandlers
import fyi.arlo.gander.ui.navigation.Routes

/**
 * The stream viewer's destination (WP-54). Like the preview route it carries the file sheet's
 * [EntryTarget]: host, share and path as ids, and the entry as its own JSON (its size and
 * modification time are what "the file changed during the session" is measured against).
 */
object StreamRoutes {
    const val ARG_PATH = "path"
    const val ARG_ENTRY = "entry"

    const val STREAM =
        "stream/{${Routes.ARG_HOST_ID}}/{${Routes.ARG_SHARE_ID}}?$ARG_PATH={$ARG_PATH}&$ARG_ENTRY={$ARG_ENTRY}"

    fun stream(target: EntryTarget): String =
        "stream/${Uri.encode(target.hostId)}/${Uri.encode(target.shareId)}" +
            "?$ARG_PATH=${Uri.encode(target.path)}" +
            "&$ARG_ENTRY=${Uri.encode(GanderJson.json.encodeToString(Entry.serializer(), target.entry))}"

    /** The target a route was built from, or null if the arguments are damaged (the viewer then closes). */
    fun target(hostId: String?, shareId: String?, path: String?, entryJson: String?): EntryTarget? {
        if (hostId == null || shareId == null || path == null || entryJson == null) return null
        val entry = runCatching { GanderJson.json.decodeFromString(Entry.serializer(), entryJson) }.getOrNull()
            ?: return null
        return EntryTarget(hostId, shareId, path, entry)
    }
}

/**
 * Registers the stream viewer. [fileActions] gives the end dialog's "Fetch the saved file" the
 * same destination the file sheet's Fetch button has.
 */
fun NavGraphBuilder.streamGraph(
    graph: AppGraph,
    navController: NavController,
    fileActions: FileActionHandlers,
) {
    composable(
        StreamRoutes.STREAM,
        arguments = listOf(
            navArgument(Routes.ARG_HOST_ID) { type = NavType.StringType },
            navArgument(Routes.ARG_SHARE_ID) { type = NavType.StringType },
            navArgument(StreamRoutes.ARG_PATH) {
                type = NavType.StringType
                nullable = true
            },
            navArgument(StreamRoutes.ARG_ENTRY) {
                type = NavType.StringType
                nullable = true
            },
        ),
    ) { entry ->
        val args = entry.arguments
        val target = StreamRoutes.target(
            hostId = args?.getString(Routes.ARG_HOST_ID),
            shareId = args?.getString(Routes.ARG_SHARE_ID),
            path = args?.getString(StreamRoutes.ARG_PATH),
            entryJson = args?.getString(StreamRoutes.ARG_ENTRY),
        )
        if (target == null) {
            LaunchedEffect(Unit) { navController.popBackStack() }
        } else {
            StreamDestination(
                target = target,
                graph = graph,
                onBack = { navController.popBackStack() },
                onFetchSaved = { fresh ->
                    fileActions.onFetch(fresh)
                    navController.popBackStack()
                },
            )
        }
    }
}

@Composable
private fun StreamDestination(
    target: EntryTarget,
    graph: AppGraph,
    onBack: () -> Unit,
    onFetchSaved: (EntryTarget) -> Unit,
) {
    val viewModel: StreamViewModel = viewModel(
        // One per file, so reopening another file never shows the last one's stream.
        key = "stream-${target.hostId}|${target.shareId}|${target.path}",
        factory = viewModelFactory {
            initializer {
                StreamViewModel(
                    target = target,
                    hosts = graph.hostRepository,
                    clientsFor = graph.hostClients,
                    prefs = graph.streamPrefs,
                    refusals = graph.actionRefusals,
                    output = StreamVideoOutput(),
                    transportFactory = { output -> WebRtcStreamTransport(graph.appContext, output) },
                    decoderAvailable = H264Support::decoderAvailable,
                    endScope = graph.processScope,
                    phoneClipboard = AndroidPhoneClipboard(graph.appContext),
                )
            }
        },
    )
    StreamScreen(
        viewModel = viewModel,
        onBack = onBack,
        onFetchSaved = onFetchSaved,
    )
}
