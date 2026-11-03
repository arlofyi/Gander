package fyi.arlo.gander.ui.preview

import android.net.Uri
import androidx.compose.foundation.layout.padding
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.lifecycle.createSavedStateHandle
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
import fyi.arlo.gander.core.data.preview.CoilPagePrefetcher
import fyi.arlo.gander.core.data.preview.PreviewFailure
import fyi.arlo.gander.core.net.GanderJson
import fyi.arlo.gander.core.net.protocol.Entry
import fyi.arlo.gander.core.net.protocol.PreviewKind
import fyi.arlo.gander.ui.filesheet.FileActionHandlers
import fyi.arlo.gander.ui.navigation.Routes

/**
 * The preview viewers' destination. The route carries the file sheet's [EntryTarget]: host, share
 * and path as ids, and the entry (the fresh `stat?meta=1` answer, which says which kind of preview
 * to show) as its own JSON, so opening a viewer costs no second `stat`.
 */
object PreviewRoutes {
    const val ARG_PATH = "path"
    const val ARG_ENTRY = "entry"

    const val PREVIEW =
        "preview/{${Routes.ARG_HOST_ID}}/{${Routes.ARG_SHARE_ID}}?$ARG_PATH={$ARG_PATH}&$ARG_ENTRY={$ARG_ENTRY}"

    fun preview(target: EntryTarget): String =
        "preview/${Uri.encode(target.hostId)}/${Uri.encode(target.shareId)}" +
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
 * Registers the preview viewers (WP-26). [fileActions] gives the viewers' "Fetch" shortcut the same
 * destination the file sheet's Fetch button has.
 */
fun NavGraphBuilder.previewGraph(
    graph: AppGraph,
    navController: NavController,
    fileActions: FileActionHandlers,
) {
    composable(
        PreviewRoutes.PREVIEW,
        arguments = listOf(
            navArgument(Routes.ARG_HOST_ID) { type = NavType.StringType },
            navArgument(Routes.ARG_SHARE_ID) { type = NavType.StringType },
            navArgument(PreviewRoutes.ARG_PATH) {
                type = NavType.StringType
                nullable = true
            },
            navArgument(PreviewRoutes.ARG_ENTRY) {
                type = NavType.StringType
                nullable = true
            },
        ),
    ) { entry ->
        val args = entry.arguments
        val target = PreviewRoutes.target(
            hostId = args?.getString(Routes.ARG_HOST_ID),
            shareId = args?.getString(Routes.ARG_SHARE_ID),
            path = args?.getString(PreviewRoutes.ARG_PATH),
            entryJson = args?.getString(PreviewRoutes.ARG_ENTRY),
        )
        if (target == null) {
            LaunchedEffect(Unit) { navController.popBackStack() }
        } else {
            PreviewDestination(
                target = target,
                graph = graph,
                onBack = { navController.popBackStack() },
                onFetch = { fileActions.onFetch(target) },
            )
        }
    }
}

/** Picks the viewer for the entry's preview kind (`stat` says which one the host would produce). */
@Composable
fun PreviewDestination(
    target: EntryTarget,
    graph: AppGraph,
    onBack: () -> Unit,
    onFetch: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val title = target.entry.name
    // One ViewModel per file and viewer, so reopening another file never shows the last one's state.
    val key = "preview-${target.hostId}|${target.shareId}|${target.path}"
    when (target.entry.preview) {
        PreviewKind.PDF, PreviewKind.OFFICE -> {
            val viewModel: PagesViewModel = viewModel(
                key = "$key-pages",
                factory = viewModelFactory {
                    initializer {
                        PagesViewModel(
                            target = target,
                            hosts = graph.hostRepository,
                            source = graph.previewSource,
                            prefetcher = CoilPagePrefetcher(graph.appContext, graph.imageLoader),
                            refusalLog = graph.actionRefusals,
                            saved = createSavedStateHandle(),
                        )
                    }
                },
            )
            PagesViewerScreen(viewModel, title, graph.imageLoader, onBack, onFetch, modifier)
        }

        PreviewKind.IMAGE -> {
            val viewModel: SinglePageViewModel = viewModel(
                key = "$key-image",
                factory = viewModelFactory {
                    initializer {
                        SinglePageViewModel(
                            target, graph.hostRepository, graph.actionRefusals, tileInfo = graph.previewSource,
                        )
                    }
                },
            )
            ImageViewerScreen(viewModel, title, graph.imageLoader, onBack, onFetch, modifier)
        }

        PreviewKind.TEXT -> {
            val viewModel: TextViewModel = viewModel(
                key = "$key-text",
                factory = viewModelFactory {
                    initializer { TextViewModel(target, graph.previewSource, graph.actionRefusals) }
                },
            )
            val hostName by produceState("", target.hostId) { value = graph.hostRepository.get(target.hostId)?.name.orEmpty() }
            TextViewerScreen(viewModel, title, onBack, onFetch, modifier, hostName)
        }

        PreviewKind.VIDEO_POSTER -> {
            val viewModel: VideoViewModel = viewModel(
                key = "$key-video",
                factory = viewModelFactory {
                    initializer {
                        VideoViewModel(target, graph.hostRepository, graph.actionRefusals, createSavedStateHandle())
                    }
                },
            )
            VideoViewerScreen(
                viewModel, target, title, graph.imageLoader, graph.videoPlayerFactory, onBack, onFetch, modifier,
            )
        }

        PreviewKind.NONE, PreviewKind.UNKNOWN -> {
            val failure = remember { PreviewFailure.Unsupported }
            PreviewScaffold(title = title, onBack = onBack, onFetch = onFetch, modifier = modifier) { padding ->
                PreviewFailurePanel(
                    failure = failure,
                    onRetry = null,
                    onFetch = onFetch,
                    modifier = Modifier.padding(padding),
                )
            }
        }
    }
}
