package fyi.arlo.gander.ui.shell

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.WindowInsetsSides
import androidx.compose.foundation.layout.consumeWindowInsets
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.only
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.material3.Badge
import androidx.compose.material3.BadgedBox
import androidx.compose.material3.Icon
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.NavigationBarItemDefaults
import androidx.compose.material3.NavigationRail
import androidx.compose.material3.NavigationRailItem
import androidx.compose.material3.NavigationRailItemDefaults
import androidx.compose.material3.Text
import androidx.compose.material3.windowsizeclass.WindowWidthSizeClass
import androidx.compose.runtime.Composable
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.navigation.NavController
import androidx.navigation.NavGraph.Companion.findStartDestination
import fyi.arlo.gander.R
import fyi.arlo.gander.ui.navigation.Routes
import fyi.arlo.gander.ui.theme.GanderIcons
import fyi.arlo.gander.ui.theme.GanderSpacing
import fyi.arlo.gander.ui.theme.GanderTheme
import fyi.arlo.gander.ui.theme.LensMark

/**
 * The window's width class, from `calculateWindowSizeClass` in the activity. Compact by default,
 * so screens rendered on their own (tests, previews) use the phone layout.
 */
val LocalWindowWidthClass = staticCompositionLocalOf { WindowWidthSizeClass.Compact }

/** True on a tablet in landscape (and other expanded widths): two panes and a navigation rail. */
@Composable
fun isExpandedWidth(): Boolean = LocalWindowWidthClass.current == WindowWidthSizeClass.Expanded

/** Test tags of the app shell. */
object ShellTags {
    const val BOTTOM_BAR = "shell-bottom-bar"
    const val RAIL = "shell-rail"

    fun item(destination: TopDestination) = "shell-nav-${destination.name.lowercase()}"
}

/** The three top-level destinations (owner decision: bottom navigation Computers / Downloads / Settings). */
enum class TopDestination(val label: Int) {
    COMPUTERS(R.string.nav_computers),
    DOWNLOADS(R.string.nav_downloads),
    SETTINGS(R.string.nav_settings),
    ;

    val icon: ImageVector
        get() = when (this) {
            COMPUTERS -> GanderIcons.Desktop
            DOWNLOADS -> GanderIcons.Fetch
            SETTINGS -> GanderIcons.Settings
        }
}

/** Where the navigation chrome is drawn for the current screen. */
data class ShellChrome(
    /** The selected destination, or null when none applies (the bar is then hidden on phones). */
    val selected: TopDestination?,
    /** Phone: show the bottom navigation bar (top-level screens only). */
    val bottomBar: Boolean,
    /** Tablet: show the navigation rail (everything but the immersive viewers and the stream). */
    val rail: Boolean,
    /** Tablet: centre the screen at a 600 dp reading width (phone layouts reused on tablets). */
    val readableWidth: Boolean,
)

/**
 * Phone: the content above an M3 bottom navigation bar. Tablet ([isExpandedWidth]): a navigation
 * rail with the Lens mark beside the content. The selection indicator is `secondaryContainer`
 * (accent-container); [downloadsBusy] puts the orange dot on Downloads while fetches run.
 */
@Composable
fun GanderAppShell(
    chrome: ShellChrome,
    downloadsBusy: Boolean,
    onNavigate: (TopDestination) -> Unit,
    content: @Composable () -> Unit,
) {
    if (isExpandedWidth()) {
        Row(Modifier.fillMaxSize()) {
            if (chrome.rail) {
                ShellRail(chrome.selected, downloadsBusy, onNavigate)
            }
            Box(
                modifier = Modifier
                    .weight(1f)
                    .fillMaxHeight(),
                contentAlignment = Alignment.TopCenter,
            ) {
                val width = if (chrome.readableWidth) Modifier.widthIn(max = GanderSpacing.ReadableMaxWidth) else Modifier
                Box(width.fillMaxHeight()) { content() }
            }
        }
    } else {
        Column(Modifier.fillMaxSize()) {
            Box(
                modifier = Modifier
                    .weight(1f)
                    .then(
                        if (chrome.bottomBar) {
                            Modifier.consumeWindowInsets(WindowInsets.navigationBars.only(WindowInsetsSides.Bottom))
                        } else {
                            Modifier
                        },
                    ),
            ) { content() }
            if (chrome.bottomBar) ShellBottomBar(chrome.selected, downloadsBusy, onNavigate)
        }
    }
}

@Composable
private fun ShellBottomBar(selected: TopDestination?, downloadsBusy: Boolean, onNavigate: (TopDestination) -> Unit) {
    val c = GanderTheme.colors
    NavigationBar(containerColor = c.surface2, modifier = Modifier.testTag(ShellTags.BOTTOM_BAR)) {
        TopDestination.entries.forEach { destination ->
            val on = destination == selected
            NavigationBarItem(
                selected = on,
                onClick = { onNavigate(destination) },
                icon = { NavIcon(destination, downloadsBusy) },
                label = {
                    Text(
                        stringResource(destination.label),
                        fontWeight = if (on) FontWeight.SemiBold else FontWeight.Medium,
                    )
                },
                colors = NavigationBarItemDefaults.colors(
                    selectedIconColor = c.onAccentContainer,
                    selectedTextColor = c.text,
                    indicatorColor = c.accentContainer,
                    unselectedIconColor = c.text2,
                    unselectedTextColor = c.text2,
                ),
                modifier = Modifier.testTag(ShellTags.item(destination)),
            )
        }
    }
}

@Composable
private fun ShellRail(selected: TopDestination?, downloadsBusy: Boolean, onNavigate: (TopDestination) -> Unit) {
    val c = GanderTheme.colors
    NavigationRail(
        containerColor = c.surface2,
        modifier = Modifier.testTag(ShellTags.RAIL),
        header = {
            LensMark(size = 34.dp, modifier = Modifier.padding(top = 18.dp, bottom = 10.dp))
        },
    ) {
        Column(verticalArrangement = Arrangement.spacedBy(14.dp)) {
            TopDestination.entries.forEach { destination ->
                val on = destination == selected
                NavigationRailItem(
                    selected = on,
                    onClick = { onNavigate(destination) },
                    icon = { NavIcon(destination, downloadsBusy) },
                    label = {
                        Text(
                            stringResource(destination.label),
                            fontWeight = if (on) FontWeight.SemiBold else FontWeight.Medium,
                        )
                    },
                    colors = NavigationRailItemDefaults.colors(
                        selectedIconColor = c.onAccentContainer,
                        selectedTextColor = c.text,
                        indicatorColor = c.accentContainer,
                        unselectedIconColor = c.text2,
                        unselectedTextColor = c.text2,
                    ),
                    modifier = Modifier.testTag(ShellTags.item(destination)),
                )
            }
        }
    }
}

@Composable
private fun NavIcon(destination: TopDestination, downloadsBusy: Boolean) {
    BadgedBox(
        badge = {
            if (destination == TopDestination.DOWNLOADS && downloadsBusy) Badge(containerColor = GanderTheme.colors.accent)
        },
    ) {
        Icon(destination.icon, contentDescription = null)
    }
}

/** The chrome for a navigation [route] pattern (the back stack entry's `destination.route`). */
fun shellChrome(route: String?): ShellChrome {
    val r = route.orEmpty()
    val immersive = r.startsWith("preview/") || r.startsWith("stream/")
    val selected = when {
        r == Routes.DOWNLOADS -> TopDestination.DOWNLOADS
        r.startsWith(Routes.SETTINGS) -> TopDestination.SETTINGS
        r.startsWith("hosts") || r.isEmpty() -> TopDestination.COMPUTERS
        else -> null
    }
    val topLevel = r in setOf(Routes.HOSTS, Routes.SHARES, Routes.DOWNLOADS, Routes.SETTINGS)
    return ShellChrome(
        selected = if (immersive) null else selected,
        bottomBar = topLevel,
        rail = !immersive && r != Routes.ADD_HOST,
        readableWidth = !immersive && r != Routes.BROWSER && r != Routes.HOSTS && r != Routes.DOWNLOADS,
    )
}

/** Opens a top-level destination the M3 way: one copy on the back stack, state saved and restored. */
fun NavController.navigateTop(destination: TopDestination) {
    val route = when (destination) {
        TopDestination.COMPUTERS -> Routes.HOSTS
        TopDestination.DOWNLOADS -> Routes.DOWNLOADS
        TopDestination.SETTINGS -> Routes.SETTINGS
    }
    navigate(route) {
        popUpTo(graph.findStartDestination().id) { saveState = true }
        launchSingleTop = true
        restoreState = true
    }
}
