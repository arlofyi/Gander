package fyi.arlo.gander

import android.os.Build
import android.os.Bundle
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.material3.windowsizeclass.ExperimentalMaterial3WindowSizeClassApi
import androidx.compose.material3.windowsizeclass.calculateWindowSizeClass
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.fragment.app.FragmentActivity
import androidx.lifecycle.lifecycleScope
import androidx.navigation.compose.currentBackStackEntryAsState
import androidx.navigation.compose.rememberNavController
import fyi.arlo.gander.ui.fetch.DownloadsScreen
import fyi.arlo.gander.ui.fetch.FetchOverlay
import fyi.arlo.gander.ui.fetch.rememberFetchController
import fyi.arlo.gander.ui.lock.AppLockGate
import fyi.arlo.gander.ui.lock.BiometricDeviceAuthenticator
import fyi.arlo.gander.ui.lock.DeviceAuthenticator
import fyi.arlo.gander.ui.lock.LocalDeviceAuthenticator
import fyi.arlo.gander.ui.navigation.GanderNavHost
import fyi.arlo.gander.ui.navigation.Routes
import fyi.arlo.gander.ui.preview.PreviewRoutes
import fyi.arlo.gander.ui.shell.GanderAppShell
import fyi.arlo.gander.ui.shell.LocalWindowWidthClass
import fyi.arlo.gander.ui.shell.navigateTop
import fyi.arlo.gander.ui.shell.shellChrome
import fyi.arlo.gander.ui.stream.StreamRoutes
import fyi.arlo.gander.ui.theme.GanderTheme
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch

/**
 * Single-activity app: Compose Navigation hosts every screen. A `FragmentActivity` (which is a
 * `ComponentActivity`) because `BiometricPrompt`, behind the optional app lock, needs one.
 */
@OptIn(ExperimentalMaterial3WindowSizeClassApi::class)
class MainActivity : FragmentActivity() {
    private lateinit var authenticator: DeviceAuthenticator

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        val graph = appGraph
        // Before the activity is started: BiometricPrompt re-attaches after a rotation only if it
        // existed by then.
        authenticator = BiometricDeviceAuthenticator(this)
        graph.startBackgroundWork()

        lifecycleScope.launch {
            graph.settingsRepository.settings.map { it.lock }.distinctUntilChanged().collect { config ->
                graph.appLock.onConfig(config)
                // With the lock on, the recents thumbnail must not show the app's content either.
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) setRecentsScreenshotEnabled(!config.enabled)
            }
        }

        setContent {
            val windowSize = calculateWindowSizeClass(this)
            GanderTheme {
                CompositionLocalProvider(
                    LocalWindowWidthClass provides windowSize.widthSizeClass,
                    LocalDeviceAuthenticator provides authenticator,
                ) {
                    AppLockGate(
                        lock = graph.appLock,
                        authenticator = authenticator,
                        onTurnOffLock = { lifecycleScope.launch { graph.settingsRepository.setAppLockEnabled(false) } },
                    ) {
                        val navController = rememberNavController()
                        val fetch = rememberFetchController(graph)
                        val backStack by navController.currentBackStackEntryAsState()
                        val jobs by graph.fetchEngine.jobs.collectAsState(initial = emptyList())
                        GanderAppShell(
                            chrome = shellChrome(backStack?.destination?.route),
                            downloadsBusy = jobs.any { it.status.active },
                            onNavigate = navController::navigateTop,
                        ) {
                            GanderNavHost(
                                graph = graph,
                                navController = navController,
                                onShowDownloads = fetch::showPanel,
                                onFetch = fetch::request,
                                onFetchSelected = fetch::requestAll,
                                onZipFolder = fetch::requestZip,
                                downloads = {
                                    DownloadsScreen(
                                        controller = fetch,
                                        graph = graph,
                                        onOpenStorage = { navController.navigate(Routes.STORAGE) },
                                    )
                                },
                            )
                        }
                        // A fetch that starts, or a fetch notification, opens Downloads; Back returns.
                        LaunchedEffect(fetch.panelVisible) {
                            if (fetch.panelVisible) {
                                navController.navigate(Routes.DOWNLOADS) { launchSingleTop = true }
                                fetch.hidePanel()
                            }
                        }
                        // The cellular dialog's "Preview instead" opens the same built-in viewers
                        // (WP-26) as the file sheet's Preview; "Stream instead" opens the stream viewer (WP-54).
                        FetchOverlay(
                            controller = fetch,
                            graph = graph,
                            onPreviewInstead = { target -> navController.navigate(PreviewRoutes.preview(target)) },
                            onStreamInstead = { target -> navController.navigate(StreamRoutes.stream(target)) },
                        )
                    }
                }
            }
        }
    }

    override fun onStart() {
        super.onStart()
        appGraph.appLock.onForeground()
    }

    override fun onStop() {
        super.onStop()
        // A rotation stops and restarts the activity but the user never left the app.
        if (!isChangingConfigurations) appGraph.appLock.onBackground()
    }
}
