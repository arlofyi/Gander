package fyi.arlo.gander.ui.hosts

import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import androidx.navigation.NavController
import androidx.navigation.NavGraphBuilder
import androidx.navigation.compose.composable
import fyi.arlo.gander.AppGraph
import fyi.arlo.gander.ui.navigation.Routes

/** The Hosts home screen. */
fun NavGraphBuilder.hostsGraph(graph: AppGraph, navController: NavController) {
    composable(Routes.HOSTS) {
        val viewModel: HostsViewModel = viewModel(
            factory = viewModelFactory {
                initializer { HostsViewModel(
                        hosts = graph.hostRepository,
                        prober = graph.hostProber,
                        privateNetwork = graph.privateNetworkMonitor,
                        forgetHost = graph.hostForgetter::forget,
                    ) }
            },
        )
        HostsScreen(
            viewModel = viewModel,
            onOpenHost = { hostId -> navController.navigate(Routes.shares(hostId)) },
            onAddHost = { navController.navigate(Routes.ADD_HOST) },
            onOpenSettings = { navController.navigate(Routes.SETTINGS) },
            onOpenStorage = { navController.navigate(Routes.STORAGE) },
            onOpenDetails = { hostId -> navController.navigate(Routes.hostDetails(hostId)) },
        )
    }
}
