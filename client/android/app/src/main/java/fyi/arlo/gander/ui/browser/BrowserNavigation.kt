package fyi.arlo.gander.ui.browser

import androidx.compose.runtime.getValue
import androidx.lifecycle.compose.collectAsStateWithLifecycle
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
import fyi.arlo.gander.ui.filesheet.FileActionHandlers
import fyi.arlo.gander.ui.filesheet.FileDetailPaneHost
import fyi.arlo.gander.ui.filesheet.FileSheetHost
import fyi.arlo.gander.ui.filesheet.rememberFileSheetOpener
import fyi.arlo.gander.ui.navigation.Routes
import fyi.arlo.gander.ui.shell.ListDetailLayout
import fyi.arlo.gander.ui.shell.isExpandedWidth

/**
 * Shares of a host and the browser of a share. The folder is browser state, not a destination:
 * going into a folder keeps the same screen (and ViewModel), back goes up one folder.
 *
 * A tapped file opens the file sheet (WP-25) over the browser; the sheet hands the chosen action to
 * [fileActions].
 */
fun NavGraphBuilder.browserGraph(
    graph: AppGraph,
    navController: NavController,
    fileActions: FileActionHandlers,
    onFetchSelected: ((List<EntryTarget>) -> Unit)?,
    onZipFolder: ((EntryTarget) -> Unit)? = null,
) {
    val args = listOf(
        navArgument(Routes.ARG_HOST_ID) { type = NavType.StringType },
        navArgument(Routes.ARG_SHARE_ID) { type = NavType.StringType },
    )

    composable(Routes.SHARES, arguments = args.take(1)) { entry ->
        val hostId = requireNotNull(entry.arguments?.getString(Routes.ARG_HOST_ID))
        val viewModel: SharesViewModel = viewModel(
            factory = viewModelFactory {
                initializer { SharesViewModel(hostId, graph.hostRepository, graph.browseRepository) }
            },
        )
        SharesScreen(
            viewModel = viewModel,
            onBack = { navController.popBackStack() },
            onOpenShare = { shareId -> navController.navigate(Routes.browser(hostId, shareId)) },
        )
    }

    composable(Routes.BROWSER, arguments = args) { entry ->
        val hostId = requireNotNull(entry.arguments?.getString(Routes.ARG_HOST_ID))
        val shareId = requireNotNull(entry.arguments?.getString(Routes.ARG_SHARE_ID))
        val viewModel: BrowserViewModel = viewModel(
            factory = viewModelFactory {
                initializer {
                    BrowserViewModel(
                        hostId = hostId,
                        shareId = shareId,
                        hosts = graph.hostRepository,
                        repository = graph.browseRepository,
                        prefs = graph.browserPrefs,
                        saved = createSavedStateHandle(),
                    )
                }
            },
        )
        val fileSheet = rememberFileSheetOpener()
        ThumbnailScopeProvider(hostId, shareId, graph.hostRepository, graph.imageLoader) {
            if (isExpandedWidth()) {
                // Tablet (mockup 35): the folder pane and an always-visible detail pane.
                ListDetailLayout(
                    list = {
                        BrowserScreen(
                            viewModel = viewModel,
                            onNavigateUp = { navController.popBackStack() },
                            onOpenFile = fileSheet::show,
                            onFetchSelected = onFetchSelected,
                            highlightedPath = fileSheet.opened?.target?.path,
                            onZipFolder = onZipFolder,
                        )
                    },
                    detail = {
                        FileDetailPaneHost(opener = fileSheet, graph = graph, handlers = fileActions) {
                            val state by viewModel.uiState.collectAsStateWithLifecycle()
                            FolderSummaryPane(state)
                        }
                    },
                )
            } else {
                BrowserScreen(
                    viewModel = viewModel,
                    onNavigateUp = { navController.popBackStack() },
                    onOpenFile = fileSheet::show,
                    onFetchSelected = onFetchSelected,
                    onZipFolder = onZipFolder,
                )
            }
        }
        if (!isExpandedWidth()) FileSheetHost(opener = fileSheet, graph = graph, handlers = fileActions)
    }
}
