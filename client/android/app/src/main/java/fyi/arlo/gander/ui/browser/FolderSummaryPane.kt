package fyi.arlo.gander.ui.browser

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import fyi.arlo.gander.R
import fyi.arlo.gander.ui.components.EmptyState
import fyi.arlo.gander.ui.components.TypeTile
import fyi.arlo.gander.ui.theme.GanderIcons

/**
 * The tablet detail pane while no file is chosen (owner decision: the pane always shows): the
 * open folder, how many items it has, and what choosing a file does.
 */
@Composable
fun FolderSummaryPane(state: BrowserUiState, modifier: Modifier = Modifier) {
    val count = state.visibleEntries.size
    Column(modifier = modifier.fillMaxSize(), verticalArrangement = Arrangement.Center) {
        EmptyState(
            title = state.crumbs.lastOrNull()?.label ?: state.shareName,
            body = pluralStringResource(R.plurals.browser_pane_items, count, count),
            note = stringResource(R.string.browser_pane_hint, state.hostName),
            visual = { TypeTile(GanderIcons.Folder, folder = true, size = 72.dp, iconSize = 36.dp) },
        )
    }
}
