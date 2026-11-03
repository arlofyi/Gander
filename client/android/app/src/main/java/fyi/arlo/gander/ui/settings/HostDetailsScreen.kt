package fyi.arlo.gander.ui.settings

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import fyi.arlo.gander.R
import fyi.arlo.gander.core.data.hosts.PairedHost
import fyi.arlo.gander.core.net.protocol.HostOs
import fyi.arlo.gander.ui.common.relativeTimeText
import fyi.arlo.gander.ui.common.text

/** Test tags for the host details screen (instrumented tests). */
object HostDetailsTags {
    const val SCREEN = "host-details-screen"
    const val ROTATE = "host-details-rotate"
    const val ROTATE_CONFIRM = "host-details-rotate-confirm"
    const val ROTATE_STATUS = "host-details-rotate-status"
    const val FORGET = "host-details-forget"
    const val FORGET_CONFIRM = "host-details-forget-confirm"
    const val FINGERPRINT = "host-details-fingerprint"
    const val ADDRESSES = "host-details-addresses"
}

/**
 * Host details: the identity this phone trusts for the computer (name, certificate fingerprint,
 * addresses), "paired as", token rotation and forgetting. Closes itself ([onClosed]) once the
 * computer is gone from this phone.
 */
@Composable
fun HostDetailsScreen(
    viewModel: HostDetailsViewModel,
    onBack: () -> Unit,
    onClosed: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    LaunchedEffect(state.forgotten, state.loaded) { if (state.loaded && state.forgotten) onClosed() }
    HostDetailsContent(
        state = state,
        onBack = onBack,
        onRotate = viewModel::rotateToken,
        onDismissRotateMessage = viewModel::dismissRotateMessage,
        onForget = viewModel::forget,
        modifier = modifier,
    )
}

/** Stateless host details; the instrumented tests render it with fixed state. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun HostDetailsContent(
    state: HostDetailsUiState,
    modifier: Modifier = Modifier,
    onBack: () -> Unit = {},
    onRotate: () -> Unit = {},
    onDismissRotateMessage: () -> Unit = {},
    onForget: () -> Unit = {},
) {
    var confirmRotate by remember { mutableStateOf(false) }
    var confirmForget by remember { mutableStateOf(false) }

    Scaffold(
        modifier = modifier.testTag(HostDetailsTags.SCREEN),
        containerColor = fyi.arlo.gander.ui.theme.GanderTheme.colors.bg,
        topBar = {
            fyi.arlo.gander.ui.components.GanderTopBar(
                title = stringResource(R.string.host_details_title),
                navigationIcon = fyi.arlo.gander.ui.theme.GanderIcons.Back,
                navigationDescription = stringResource(R.string.action_back),
                onNavigate = onBack,
                modifier = Modifier.statusBarsPadding(),
            )
        },
    ) { innerPadding ->
        val host = state.host ?: return@Scaffold
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(innerPadding)
                .verticalScroll(rememberScrollState())
                .padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            Identity(host)
            HorizontalDivider()
            Access(
                state = state,
                onRotate = { confirmRotate = true },
                onDismissRotateMessage = onDismissRotateMessage,
            )
            HorizontalDivider()
            Text(
                stringResource(R.string.host_details_forget_summary),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Button(
                onClick = { confirmForget = true },
                colors = ButtonDefaults.buttonColors(
                    containerColor = MaterialTheme.colorScheme.error,
                    contentColor = MaterialTheme.colorScheme.onError,
                ),
                modifier = Modifier
                    .fillMaxWidth()
                    .testTag(HostDetailsTags.FORGET),
            ) { Text(stringResource(R.string.host_details_forget)) }
        }

        if (confirmRotate) {
            AlertDialog(
                onDismissRequest = { confirmRotate = false },
                title = { Text(stringResource(R.string.host_rotate_confirm_title)) },
                text = { Text(stringResource(R.string.host_rotate_confirm_body, host.name)) },
                confirmButton = {
                    TextButton(
                        onClick = {
                            confirmRotate = false
                            onRotate()
                        },
                        modifier = Modifier.testTag(HostDetailsTags.ROTATE_CONFIRM),
                    ) { Text(stringResource(R.string.host_rotate_confirm)) }
                },
                dismissButton = { TextButton(onClick = { confirmRotate = false }) { Text(stringResource(R.string.action_cancel)) } },
            )
        }
        if (confirmForget) {
            AlertDialog(
                onDismissRequest = { confirmForget = false },
                title = { Text(stringResource(R.string.host_forget_title, host.name)) },
                text = { Text(stringResource(R.string.host_forget_body)) },
                confirmButton = {
                    TextButton(
                        onClick = {
                            confirmForget = false
                            onForget()
                        },
                        modifier = Modifier.testTag(HostDetailsTags.FORGET_CONFIRM),
                    ) { Text(stringResource(R.string.host_forget_confirm)) }
                },
                dismissButton = { TextButton(onClick = { confirmForget = false }) { Text(stringResource(R.string.action_cancel)) } },
            )
        }
    }
}

@Composable
private fun Identity(host: PairedHost) {
    Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
        Text(host.name, style = MaterialTheme.typography.headlineSmall)
        Detail(stringResource(R.string.host_details_system), osLabel(host.os))
        host.pairedAs?.let { Detail(stringResource(R.string.host_details_paired_as), it) }
        Detail(
            stringResource(R.string.host_details_last_seen),
            host.lastSeenAtMillis?.let { relativeTimeText(it) } ?: stringResource(R.string.host_never_seen),
        )
        Detail(
            label = stringResource(R.string.host_details_addresses),
            value = host.addresses.joinToString("\n") + "\n" + stringResource(R.string.host_details_port, host.port),
            tag = HostDetailsTags.ADDRESSES,
        )
        Detail(
            label = stringResource(R.string.host_details_fingerprint),
            value = host.pin.hostId,
            monospace = true,
            tag = HostDetailsTags.FINGERPRINT,
        )
        Text(
            stringResource(R.string.host_details_fingerprint_hint),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
private fun Detail(label: String, value: String, monospace: Boolean = false, tag: String? = null) {
    Column(modifier = if (tag != null) Modifier.testTag(tag) else Modifier) {
        Text(label, style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
        SelectionContainer {
            Text(
                value,
                style = MaterialTheme.typography.bodyLarge,
                fontFamily = if (monospace) FontFamily.Monospace else null,
            )
        }
    }
}

@Composable
private fun Access(state: HostDetailsUiState, onRotate: () -> Unit, onDismissRotateMessage: () -> Unit) {
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(stringResource(R.string.host_details_access), style = MaterialTheme.typography.titleMedium)
        Text(
            stringResource(R.string.host_details_rotate_summary),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        OutlinedButton(
            onClick = onRotate,
            enabled = state.rotate != RotateOutcome.Working,
            modifier = Modifier
                .fillMaxWidth()
                .testTag(HostDetailsTags.ROTATE),
        ) { Text(stringResource(R.string.host_details_rotate)) }
        when (val outcome = state.rotate) {
            RotateOutcome.None -> Unit
            RotateOutcome.Working -> CircularProgressIndicator(Modifier.testTag(HostDetailsTags.ROTATE_STATUS))
            RotateOutcome.Done -> RotateMessage(stringResource(R.string.host_rotate_done), isError = false, onDismissRotateMessage)
            is RotateOutcome.Failed -> RotateMessage(
                stringResource(R.string.host_rotate_failed, outcome.error.text()),
                isError = true,
                onDismiss = onDismissRotateMessage,
            )
        }
    }
}

@Composable
private fun RotateMessage(message: String, isError: Boolean, onDismiss: () -> Unit) {
    Column(modifier = Modifier.testTag(HostDetailsTags.ROTATE_STATUS)) {
        Text(
            message,
            style = MaterialTheme.typography.bodyMedium,
            color = if (isError) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.primary,
        )
        TextButton(onClick = onDismiss) { Text(stringResource(R.string.sheet_ok)) }
    }
}

@Composable
private fun osLabel(os: HostOs): String = stringResource(
    when (os) {
        HostOs.WINDOWS -> R.string.host_os_windows
        HostOs.MACOS -> R.string.host_os_macos
        HostOs.LINUX -> R.string.host_os_linux
        HostOs.UNKNOWN -> R.string.host_os_unknown
    },
)
