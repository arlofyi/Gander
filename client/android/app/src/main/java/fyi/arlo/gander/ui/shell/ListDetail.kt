package fyi.arlo.gander.ui.shell

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.width
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import fyi.arlo.gander.ui.theme.GanderSpacing
import fyi.arlo.gander.ui.theme.GanderTheme

/**
 * The tablet's list-detail layout (design document 10.2, screen 35): a fixed-width list pane, a
 * 1 dp line, and the detail pane on `surface`. A hand-rolled `Row` rather than
 * `ListDetailPaneScaffold`: the detail pane always shows (owner decision), so no pane navigation
 * is needed, and it avoids the material3-adaptive dependency.
 */
@Composable
fun ListDetailLayout(
    list: @Composable () -> Unit,
    detail: @Composable () -> Unit,
    modifier: Modifier = Modifier,
    listWidth: Dp = GanderSpacing.ListPaneWidth,
) {
    Row(modifier = modifier.fillMaxSize()) {
        Box(Modifier.width(listWidth).fillMaxHeight()) { list() }
        Box(Modifier.width(1.dp).fillMaxHeight().background(GanderTheme.colors.line))
        Box(
            Modifier
                .weight(1f)
                .fillMaxHeight()
                .background(GanderTheme.colors.surface),
        ) { detail() }
    }
}
