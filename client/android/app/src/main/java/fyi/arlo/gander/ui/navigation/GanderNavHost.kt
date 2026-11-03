package fyi.arlo.gander.ui.navigation

import android.net.Uri
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.navigation.NavHostController
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.rememberNavController
import fyi.arlo.gander.AppGraph
import fyi.arlo.gander.R
import fyi.arlo.gander.core.data.actions.FileAction
import fyi.arlo.gander.core.data.browse.EntryTarget
import fyi.arlo.gander.ui.browser.browserGraph
import fyi.arlo.gander.ui.filesheet.FileActionHandlers
import fyi.arlo.gander.ui.filesheet.label
import fyi.arlo.gander.ui.hosts.hostsGraph
import fyi.arlo.gander.ui.pairing.pairingGraph
import fyi.arlo.gander.ui.preview.PreviewRoutes
import fyi.arlo.gander.ui.preview.previewGraph
import fyi.arlo.gander.ui.settings.settingsGraph
import fyi.arlo.gander.ui.stream.StreamRoutes
import fyi.arlo.gander.ui.stream.streamGraph

/**
 * Route names. Each feature owns its entries and registers them from its own
 * `NavGraphBuilder.xxxGraph(...)` extension, so work packages add a line here and a file next to
 * their screens instead of editing one big graph.
 */
object Routes {
    const val HOSTS = "hosts"

    /** Add host: scan or type, compare the code, wait for approval (registered by `pairingGraph`). */
    const val ADD_HOST = "hosts/add"

    const val ARG_HOST_ID = "hostId"
    const val ARG_SHARE_ID = "shareId"

    const val SHARES = "hosts/{$ARG_HOST_ID}/shares"
    const val BROWSER = "hosts/{$ARG_HOST_ID}/shares/{$ARG_SHARE_ID}"

    /** Downloads: every fetch, in progress and finished (bottom navigation). */
    const val DOWNLOADS = "downloads"

    /** Settings (WP-28), with Storage and Host details below it (registered by `settingsGraph`). */
    const val SETTINGS = "settings"
    const val STORAGE = "settings/storage"
    const val HOST_DETAILS = "hosts/{$ARG_HOST_ID}/details"

    fun hostDetails(hostId: String) = "hosts/${Uri.encode(hostId)}/details"

    fun shares(hostId: String) = "hosts/${Uri.encode(hostId)}/shares"

    fun browser(hostId: String, shareId: String) = "hosts/${Uri.encode(hostId)}/shares/${Uri.encode(shareId)}"
}

/**
 * The app's navigation. A tapped file opens the file sheet (WP-25) over the browser; the sheet
 * hands the file to one of three destinations that are not built yet. Each hook is null until its
 * work package supplies it, and a null hook shows a "not available yet" dialog:
 * - [onPreview]: replaces the built-in preview viewers (WP-26: pages, image, text and video, see
 *   `ui.preview`), which open when this is null.
 * - [onFetch]: the fetch engine with its cellular dialog (WP-27).
 * - [onStream]: replaces the built-in stream viewer (WP-54: `ui.stream`), which opens when this is null.
 * - [onFetchSelected]: the multi-selection of files (WP-27 queues the downloads); while null the
 *   browser does not offer a Fetch action.
 * - [onZipFolder]: "Download folder as zip" (WP-68) for the open folder; while null the browser does
 *   not offer it.
 *
 * [navController] is a parameter so that what is drawn over the navigation (the fetch overlay's
 * "Preview instead") can navigate too. [onShowDownloads] opens Downloads from the Storage
 * screen (WP-28); [downloads] draws the Downloads destination.
 */
@Composable
fun GanderNavHost(
    graph: AppGraph,
    modifier: Modifier = Modifier,
    navController: NavHostController = rememberNavController(),
    onShowDownloads: (() -> Unit)? = null,
    onPreview: ((EntryTarget) -> Unit)? = null,
    onFetch: ((EntryTarget) -> Unit)? = null,
    onStream: ((EntryTarget) -> Unit)? = null,
    onFetchSelected: ((List<EntryTarget>) -> Unit)? = null,
    onZipFolder: ((EntryTarget) -> Unit)? = null,
    downloads: (@Composable () -> Unit)? = null,
) {
    var placeholder by remember { mutableStateOf<FileAction?>(null) }
    // Remembered, so the navigation graph below is not rebuilt when a caller passes new lambdas.
    val currentPreview by rememberUpdatedState(onPreview)
    val currentFetch by rememberUpdatedState(onFetch)
    val currentStream by rememberUpdatedState(onStream)
    val fileActions = remember {
        FileActionHandlers(
            onPreview = { target ->
                currentPreview?.invoke(target) ?: navController.navigate(PreviewRoutes.preview(target))
            },
            onFetch = { target -> currentFetch?.invoke(target) ?: run { placeholder = FileAction.FETCH } },
            onStream = { target ->
                currentStream?.invoke(target) ?: navController.navigate(StreamRoutes.stream(target))
            },
        )
    }
    NavHost(
        navController = navController,
        startDestination = Routes.HOSTS,
        modifier = modifier,
    ) {
        hostsGraph(graph, navController)
        browserGraph(graph, navController, fileActions, onFetchSelected, onZipFolder)
        pairingGraph(graph, navController)
        previewGraph(graph, navController, fileActions)
        streamGraph(graph, navController, fileActions)
        settingsGraph(graph, navController, onShowDownloads)
        composable(Routes.DOWNLOADS) { downloads?.invoke() }
    }
    placeholder?.let { action -> ActionPlaceholderDialog(action) { placeholder = null } }
}

/** Stands in for a file action's destination until its work package lands. */
@Composable
private fun ActionPlaceholderDialog(action: FileAction, onDismiss: () -> Unit) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.sheet_placeholder_title)) },
        text = { Text(stringResource(R.string.sheet_placeholder_body, action.label())) },
        confirmButton = { TextButton(onClick = onDismiss) { Text(stringResource(R.string.sheet_ok)) } },
    )
}
