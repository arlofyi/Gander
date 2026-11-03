package fyi.arlo.gander.ui.pairing

import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import androidx.navigation.NavController
import androidx.navigation.NavGraphBuilder
import androidx.navigation.compose.composable
import fyi.arlo.gander.AppGraph
import fyi.arlo.gander.ui.navigation.Routes

/** The Add host flow (WP-22): scan, manual entry, code comparison, approval wait, done. */
fun NavGraphBuilder.pairingGraph(graph: AppGraph, navController: NavController) {
    composable(Routes.ADD_HOST) {
        val viewModel: AddHostViewModel = viewModel(
            factory = viewModelFactory { initializer { AddHostViewModel(graph.pairingFlow) } },
        )
        AddHostScreen(
            viewModel = viewModel,
            onClose = { navController.popBackStack() },
            onOpenHost = { hostId ->
                navController.navigate(Routes.shares(hostId)) {
                    // Leave the pairing screen behind: back from the shares goes to the host list.
                    popUpTo(Routes.HOSTS)
                }
            },
        )
    }
}
