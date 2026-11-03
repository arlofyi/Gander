package fyi.arlo.gander.ui.settings

import android.widget.Toast
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.platform.LocalContext
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import androidx.navigation.NavController
import androidx.navigation.NavGraphBuilder
import androidx.navigation.NavType
import androidx.navigation.compose.composable
import androidx.navigation.navArgument
import fyi.arlo.gander.AppGraph
import fyi.arlo.gander.R
import fyi.arlo.gander.core.data.fetch.CachedFile
import fyi.arlo.gander.ui.fetch.FileIntents
import fyi.arlo.gander.ui.navigation.Routes
import kotlinx.coroutines.launch

/**
 * Settings, Storage and Host details (WP-28). [onShowDownloads] opens the fetch panel (the list of
 * fetches in progress and finished, owned by `FetchOverlay`); the Storage screen is its permanent
 * entry point, and the row is hidden while it is null.
 */
fun NavGraphBuilder.settingsGraph(
    graph: AppGraph,
    navController: NavController,
    onShowDownloads: (() -> Unit)?,
) {
    composable(Routes.SETTINGS) {
        val viewModel: SettingsViewModel = viewModel(
            factory = viewModelFactory {
                initializer {
                    SettingsViewModel(
                        settings = graph.settingsRepository,
                        defaultActions = graph.defaultActionPrefs,
                        hosts = graph.hostRepository,
                        diagnostics = graph.diagnosticsCollector,
                    )
                }
            },
        )
        SettingsScreen(
            viewModel = viewModel,
            onBack = { navController.popBackStack() },
            onOpenStorage = { navController.navigate(Routes.STORAGE) },
            onOpenHost = { hostId -> navController.navigate(Routes.hostDetails(hostId)) },
        )
    }

    composable(Routes.STORAGE) {
        val viewModel: StorageViewModel = viewModel(
            factory = viewModelFactory {
                initializer {
                    StorageViewModel(
                        offline = graph.offlineFiles,
                        hosts = graph.hostRepository,
                        settings = graph.settingsRepository,
                        derived = graph.derivedCaches,
                    )
                }
            },
        )
        val context = LocalContext.current
        val scope = rememberCoroutineScope()
        StorageScreen(
            viewModel = viewModel,
            onBack = { navController.popBackStack() },
            onOpenFile = { cached -> scope.launch { openSavedCopy(graph, context, cached) } },
            onShowDownloads = onShowDownloads,
        )
    }

    composable(
        Routes.HOST_DETAILS,
        arguments = listOf(navArgument(Routes.ARG_HOST_ID) { type = NavType.StringType }),
    ) { entry ->
        val hostId = requireNotNull(entry.arguments?.getString(Routes.ARG_HOST_ID))
        val viewModel: HostDetailsViewModel = viewModel(
            factory = viewModelFactory {
                initializer {
                    HostDetailsViewModel(
                        hostId = hostId,
                        hosts = graph.hostRepository,
                        rotator = graph.tokenRotator,
                        forgetter = graph.hostForgetter,
                    )
                }
            },
        )
        HostDetailsScreen(
            viewModel = viewModel,
            onBack = { navController.popBackStack() },
            // The computer is gone: back to the list of computers, whatever was between.
            onClosed = { navController.popBackStack(Routes.HOSTS, inclusive = false) },
        )
    }
}

/** Opens a saved copy with the system resolver, with no network involved: the "offline" in offline files. */
private suspend fun openSavedCopy(graph: AppGraph, context: android.content.Context, cached: CachedFile) {
    val opened = graph.offlineFiles.open(cached.key)
    val message = when {
        opened == null -> R.string.fetch_copy_gone
        !FileIntents.view(context, opened.first, opened.second) -> R.string.fetch_no_app
        else -> return
    }
    Toast.makeText(context, message, Toast.LENGTH_SHORT).show()
}
