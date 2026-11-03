package fyi.arlo.gander.ui.pairing

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.provider.Settings
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.BiasAlignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.core.content.ContextCompat
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LifecycleEventEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import fyi.arlo.gander.BuildConfig
import fyi.arlo.gander.R
import fyi.arlo.gander.core.data.pairing.DeviceInfo
import fyi.arlo.gander.core.data.pairing.PairingFailure
import fyi.arlo.gander.core.data.pairing.PairingMethod
import fyi.arlo.gander.ui.components.ActionPanel
import fyi.arlo.gander.ui.components.Banner
import fyi.arlo.gander.ui.components.BannerTone
import fyi.arlo.gander.ui.components.ButtonKind
import fyi.arlo.gander.ui.components.ChipStyle
import fyi.arlo.gander.ui.components.EmptyState
import fyi.arlo.gander.ui.components.GanderButton
import fyi.arlo.gander.ui.components.GanderChip
import fyi.arlo.gander.ui.components.GanderProgress
import fyi.arlo.gander.ui.components.GanderTopBar
import fyi.arlo.gander.ui.components.PairingIdentity
import fyi.arlo.gander.ui.components.PairingIdentityPair
import fyi.arlo.gander.ui.components.ResultBadge
import fyi.arlo.gander.ui.components.SasCode
import fyi.arlo.gander.ui.shell.isExpandedWidth
import fyi.arlo.gander.ui.theme.GanderIcons
import fyi.arlo.gander.ui.theme.GanderShape
import fyi.arlo.gander.ui.theme.GanderTheme
import fyi.arlo.gander.ui.theme.Immersive

/** Test tags for the Add host screens (instrumented tests, WP-29). */
object AddHostTags {
    const val SCAN = "pairing-scan"
    const val MANUAL = "pairing-manual"
    const val WARNING = "pairing-warning"
    const val SCAN_NOTICE = "pairing-scan-notice"
    const val CAMERA_ALLOW = "pairing-camera-allow"
    const val ADDRESS = "pairing-address"
    const val CONTINUE = "pairing-continue"
    const val CONNECTING = "pairing-connecting"
    const val SAS = "pairing-sas"
    const val CANCEL = "pairing-cancel"
    const val ERROR = "pairing-error"
    const val RETRY = "pairing-retry"
    const val START_OVER = "pairing-start-over"
    const val DONE = "pairing-done"
    const val DONE_OPEN = "pairing-done-open"
    const val DONE_CLOSE = "pairing-done-close"
}

/** What the Add host screens can ask for. The ViewModel implements all of it. */
class AddHostActions(
    val onScan: () -> Unit,
    val onManual: () -> Unit,
    val onQrScanned: (String) -> Unit,
    val onAddressChanged: (String) -> Unit,
    val onManualSubmit: () -> Unit,
    val onRetry: () -> Unit,
    val onCancel: () -> Unit,
    val onBack: () -> Unit,
    val onOpenHost: (hostId: String) -> Unit,
    val onClose: () -> Unit,
)

/** The Add host flow: scan the QR code or type an address, compare the code, wait for approval. */
@Composable
fun AddHostScreen(
    viewModel: AddHostViewModel,
    onClose: () -> Unit,
    onOpenHost: (hostId: String) -> Unit,
    modifier: Modifier = Modifier,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    // One step back through the flow; only from the first screen (or Done) does back leave.
    BackHandler { if (!viewModel.onBack()) onClose() }
    AddHostContent(
        state = state,
        actions = AddHostActions(
            onScan = viewModel::onChooseScan,
            onManual = viewModel::onChooseManual,
            onQrScanned = viewModel::onQrScanned,
            onAddressChanged = viewModel::onManualAddressChanged,
            onManualSubmit = viewModel::onManualSubmit,
            onRetry = viewModel::onRetry,
            onCancel = viewModel::onCancel,
            onBack = { if (!viewModel.onBack()) onClose() },
            onOpenHost = onOpenHost,
            onClose = onClose,
        ),
        modifier = modifier,
    )
}

/** Stateless Add host content (mockups 04 to 10); the instrumented tests render it with fixed state. */
@Composable
fun AddHostContent(
    state: AddHostUiState,
    actions: AddHostActions,
    modifier: Modifier = Modifier,
) {
    // No screenshots or recents thumbnail while the QR code is in the viewfinder or the SAS is up.
    SecureWindowEffect(enabled = state.showsSecrets)
    val c = GanderTheme.colors
    if (state is AddHostUiState.Scan) {
        // The camera is immersive: the bar sits over the viewfinder.
        Box(modifier = modifier.fillMaxSize().background(Immersive.Background)) {
            ScanContent(state, actions)
            GanderTopBar(
                title = stringResource(R.string.pairing_scan_bar),
                navigationIcon = GanderIcons.Close,
                navigationDescription = stringResource(R.string.action_back),
                onNavigate = actions.onBack,
                contentColor = Color.White,
                modifier = Modifier.statusBarsPadding(),
            )
        }
        return
    }
    val title = when (state) {
        is AddHostUiState.Manual -> stringResource(R.string.pairing_manual_bar)
        is AddHostUiState.Waiting -> stringResource(
            if (state.method == PairingMethod.MANUAL) R.string.pairing_compare_bar else R.string.pairing_waiting_bar,
        )
        is AddHostUiState.Failed, is AddHostUiState.Done -> ""
        else -> stringResource(R.string.pairing_title)
    }
    val backIcon = if (state is AddHostUiState.Manual) GanderIcons.Back else GanderIcons.Close
    Scaffold(
        modifier = modifier,
        containerColor = c.bg,
        topBar = {
            GanderTopBar(
                title = title,
                navigationIcon = backIcon,
                navigationDescription = stringResource(R.string.action_back),
                onNavigate = actions.onBack,
                modifier = Modifier.statusBarsPadding(),
            )
        },
    ) { innerPadding ->
        Box(modifier = Modifier.fillMaxSize().padding(innerPadding)) {
            when (state) {
                AddHostUiState.Choose -> ChooseContent(actions)
                is AddHostUiState.Scan -> Unit
                is AddHostUiState.Manual -> ManualContent(state, actions)
                is AddHostUiState.Connecting -> ConnectingContent(state, actions)
                is AddHostUiState.Waiting -> WaitingContent(state, actions)
                is AddHostUiState.Failed -> FailedContent(state, actions)
                is AddHostUiState.Done -> DoneContent(state, actions)
            }
        }
    }
}

@Composable
private fun ScrollColumn(
    modifier: Modifier = Modifier,
    horizontalAlignment: Alignment.Horizontal = Alignment.Start,
    content: @Composable () -> Unit,
) {
    Column(
        modifier = modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(bottom = 16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
        horizontalAlignment = horizontalAlignment,
    ) {
        content()
    }
}

/** The stop-if-someone-told-you-to notice: a bad banner with the shield. */
@Composable
private fun WarningBanner(title: String?, body: String, tone: BannerTone, modifier: Modifier = Modifier) {
    Banner(
        tone = tone,
        icon = GanderIcons.Shield,
        title = title,
        text = body,
        modifier = modifier.padding(horizontal = 16.dp),
    )
}

/** Mockup 04: choose QR or address; the anti-scam warning comes first in weight. */
@Composable
private fun ChooseContent(actions: AddHostActions) {
    ScrollColumn {
        Text(
            stringResource(R.string.pairing_choose_intro),
            style = MaterialTheme.typography.bodyLarge.copy(fontSize = 15.sp),
            color = GanderTheme.colors.text2,
            modifier = Modifier.padding(start = 20.dp, end = 20.dp, top = 6.dp, bottom = 8.dp),
        )
        ActionPanel(
            title = stringResource(R.string.pairing_scan_action),
            icon = GanderIcons.Qr,
            hint = stringResource(R.string.pairing_scan_choice_hint),
            trailing = { Icon(GanderIcons.Chevron, contentDescription = null, tint = GanderTheme.colors.text3) },
            onClick = actions.onScan,
            modifier = Modifier.padding(horizontal = 16.dp).testTag(AddHostTags.SCAN),
        )
        ActionPanel(
            title = stringResource(R.string.pairing_manual_action),
            icon = GanderIcons.Keyboard,
            hint = stringResource(R.string.pairing_manual_choice_hint),
            trailing = { Icon(GanderIcons.Chevron, contentDescription = null, tint = GanderTheme.colors.text3) },
            onClick = actions.onManual,
            modifier = Modifier.padding(horizontal = 16.dp).testTag(AddHostTags.MANUAL),
        )
        WarningBanner(
            title = stringResource(R.string.pairing_warning_title),
            body = stringResource(R.string.pairing_warning_body),
            tone = BannerTone.WARN,
            modifier = Modifier.padding(top = 12.dp).testTag(AddHostTags.WARNING),
        )
    }
}

/** Mockup 05: the camera with the orange reticle, screenshots blocked, the way out to typing the address. */
@Composable
private fun ScanContent(state: AddHostUiState.Scan, actions: AddHostActions) {
    val context = LocalContext.current
    var granted by remember { mutableStateOf(hasCameraPermission(context)) }
    var cameraFailed by remember { mutableStateOf(false) }
    val launcher = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted = it }
    // Ask once on arrival; coming back from the settings screen re-checks without asking again.
    LaunchedEffect(Unit) { if (!granted) launcher.launch(Manifest.permission.CAMERA) }
    LifecycleEventEffect(Lifecycle.Event.ON_RESUME) { granted = hasCameraPermission(context) }

    GanderTheme(darkTheme = true) {
        Box(modifier = Modifier.fillMaxSize()) {
            when {
                !granted -> CameraPermissionPanel(
                    onAllow = { launcher.launch(Manifest.permission.CAMERA) },
                    onSettings = {
                        context.startActivity(
                            Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.fromParts("package", context.packageName, null)),
                        )
                    },
                    modifier = Modifier.align(Alignment.Center),
                )
                cameraFailed -> Text(
                    stringResource(R.string.pairing_camera_unavailable),
                    modifier = Modifier.align(Alignment.Center).padding(24.dp),
                    color = Immersive.Content,
                    textAlign = TextAlign.Center,
                )
                else -> {
                    QrScanner(
                        onCode = actions.onQrScanned,
                        onUnavailable = { cameraFailed = true },
                        modifier = Modifier.fillMaxSize(),
                    )
                    Reticle(Modifier.align(BiasAlignment(0f, -0.12f)))
                }
            }
            Column(
                modifier = Modifier
                    .align(Alignment.TopCenter)
                    .statusBarsPadding()
                    .padding(top = 72.dp),
            ) {
                GanderChip(
                    text = stringResource(R.string.pairing_screenshots_blocked),
                    icon = GanderIcons.EyeOff,
                    style = ChipStyle.OUTLINED,
                    small = true,
                )
            }
            Column(
                modifier = Modifier
                    .align(Alignment.BottomCenter)
                    .navigationBarsPadding()
                    .fillMaxWidth()
                    .padding(horizontal = 24.dp, vertical = 32.dp),
                verticalArrangement = Arrangement.spacedBy(14.dp),
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
                Text(
                    stringResource(R.string.pairing_scan_hint),
                    textAlign = TextAlign.Center,
                    style = MaterialTheme.typography.bodyLarge.copy(fontSize = 15.sp),
                    color = Immersive.Content,
                )
                state.notice?.let { notice ->
                    Banner(
                        tone = BannerTone.BAD,
                        text = stringResource(
                            when (notice) {
                                ScanNotice.NOT_GANDER -> R.string.pairing_scan_not_gander
                                ScanNotice.VERSION -> R.string.pairing_scan_version
                                ScanNotice.MALFORMED -> R.string.pairing_scan_malformed
                            },
                        ),
                        modifier = Modifier.testTag(AddHostTags.SCAN_NOTICE),
                    )
                }
                Button(
                    onClick = actions.onManual,
                    colors = ButtonDefaults.buttonColors(containerColor = Color.White.copy(alpha = 0.14f), contentColor = Color.White),
                    modifier = Modifier.heightIn(min = 44.dp).testTag(AddHostTags.MANUAL),
                ) {
                    Icon(GanderIcons.Keyboard, contentDescription = null, modifier = Modifier.size(18.dp))
                    Text(stringResource(R.string.pairing_manual_action), modifier = Modifier.padding(start = 8.dp))
                }
            }
        }
    }
}

/** The four orange corners of the viewfinder (orange marks focus, design document 3.3). */
@Composable
private fun Reticle(modifier: Modifier = Modifier) {
    val accent = GanderTheme.colors.accent
    Canvas(modifier = modifier.size(240.dp)) {
        val w = 4.dp.toPx()
        val arm = 44.dp.toPx()
        val r = 14.dp.toPx()
        val stroke = Stroke(width = w, cap = StrokeCap.Round)
        val inset = w / 2f
        val max = size.width - inset
        // Each corner: an arc of radius r and two straight arms.
        listOf(
            Triple(Offset(inset, inset), 180f, Pair(1f, 1f)),
            Triple(Offset(max - 2 * r, inset), 270f, Pair(-1f, 1f)),
            Triple(Offset(max - 2 * r, max - 2 * r), 0f, Pair(-1f, -1f)),
            Triple(Offset(inset, max - 2 * r), 90f, Pair(1f, -1f)),
        ).forEach { (topLeft, start, dir) ->
            drawArc(accent, start, 90f, false, topLeft, Size(2 * r, 2 * r), style = stroke)
            val cx = if (dir.first > 0) inset else max
            val cy = if (dir.second > 0) inset else max
            drawLine(accent, Offset(cx + dir.first * r, cy), Offset(cx + dir.first * arm, cy), w, StrokeCap.Round)
            drawLine(accent, Offset(cx, cy + dir.second * r), Offset(cx, cy + dir.second * arm), w, StrokeCap.Round)
        }
    }
}

private fun hasCameraPermission(context: android.content.Context): Boolean =
    ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED

@Composable
private fun CameraPermissionPanel(onAllow: () -> Unit, onSettings: () -> Unit, modifier: Modifier = Modifier) {
    Column(
        modifier = modifier.padding(24.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Icon(GanderIcons.Camera, contentDescription = null, tint = Immersive.Content, modifier = Modifier.size(40.dp))
        Text(
            stringResource(R.string.pairing_camera_permission_title),
            style = MaterialTheme.typography.titleMedium,
            color = Immersive.Content,
            textAlign = TextAlign.Center,
        )
        Text(stringResource(R.string.pairing_camera_permission_body), color = Immersive.ContentMuted, textAlign = TextAlign.Center)
        GanderButton(
            text = stringResource(R.string.pairing_camera_allow),
            onClick = onAllow,
            modifier = Modifier.testTag(AddHostTags.CAMERA_ALLOW),
        )
        GanderButton(text = stringResource(R.string.pairing_camera_settings), onClick = onSettings, kind = ButtonKind.TEXT)
    }
}

/** Mockup 06: typing the address (flow B). */
@Composable
private fun ManualContent(state: AddHostUiState.Manual, actions: AddHostActions) {
    val c = GanderTheme.colors
    ScrollColumn {
        Text(
            stringResource(R.string.pairing_manual_intro),
            style = MaterialTheme.typography.bodyLarge.copy(fontSize = 15.sp),
            color = c.text2,
            modifier = Modifier.padding(horizontal = 20.dp, vertical = 4.dp),
        )
        OutlinedTextField(
            value = state.address,
            onValueChange = actions.onAddressChanged,
            label = { Text(stringResource(R.string.pairing_manual_label)) },
            isError = state.invalid,
            supportingText = {
                Text(stringResource(if (state.invalid) R.string.pairing_manual_invalid else R.string.pairing_manual_help))
            },
            singleLine = true,
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri, imeAction = ImeAction.Go),
            keyboardActions = KeyboardActions(onGo = { actions.onManualSubmit() }),
            colors = OutlinedTextFieldDefaults.colors(
                focusedBorderColor = c.primary,
                unfocusedBorderColor = c.outline,
                focusedLabelColor = c.text2,
                cursorColor = c.text,
            ),
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 20.dp)
                .testTag(AddHostTags.ADDRESS),
        )
        Banner(
            tone = BannerTone.INFO,
            text = stringResource(R.string.pairing_manual_note),
            modifier = Modifier.padding(horizontal = 16.dp),
        )
        Row(Modifier.fillMaxWidth().padding(horizontal = 20.dp), horizontalArrangement = Arrangement.End) {
            GanderButton(
                text = stringResource(R.string.pairing_manual_continue),
                onClick = actions.onManualSubmit,
                enabled = state.address.isNotBlank(),
                modifier = Modifier.testTag(AddHostTags.CONTINUE),
            )
        }
    }
}

@Composable
private fun ConnectingContent(state: AddHostUiState.Connecting, actions: AddHostActions) {
    ScrollColumn(modifier = Modifier.testTag(AddHostTags.CONNECTING), horizontalAlignment = Alignment.CenterHorizontally) {
        GanderProgress(fraction = null, modifier = Modifier.padding(start = 40.dp, end = 40.dp, top = 64.dp))
        Text(
            stringResource(R.string.pairing_connecting, state.hostLabel),
            textAlign = TextAlign.Center,
            style = MaterialTheme.typography.titleMedium,
            modifier = Modifier.padding(horizontal = 24.dp),
        )
        GanderButton(
            text = stringResource(R.string.pairing_cancel),
            onClick = actions.onCancel,
            kind = ButtonKind.OUTLINED,
            modifier = Modifier.testTag(AddHostTags.CANCEL),
        )
    }
}

/** This phone (or tablet) as the right-hand identity of the pair. */
@Composable
private fun thisDevice(): PairingIdentity {
    val context = LocalContext.current
    val name = remember(context) { DeviceInfo.fromSystem(context, BuildConfig.VERSION_NAME).name }
    val tablet = isExpandedWidth() || LocalConfiguration.current.smallestScreenWidthDp >= 600
    return PairingIdentity(
        icon = if (tablet) GanderIcons.Tablet else GanderIcons.Phone,
        name = name,
        detail = stringResource(if (tablet) R.string.pairing_this_tablet else R.string.pairing_this_phone),
    )
}

/**
 * Mockups 07 and 08: both identities, the code, what to do on the computer, and Cancel. Flow B
 * (address) shows the code large to compare; flow A (QR) says the identity was checked and shows
 * it smaller.
 */
@Composable
private fun WaitingContent(state: AddHostUiState.Waiting, actions: AddHostActions) {
    val c = GanderTheme.colors
    ScrollColumn(horizontalAlignment = Alignment.CenterHorizontally) {
        PairingIdentityPair(
            computer = PairingIdentity(GanderIcons.Desktop, state.hostLabel, stringResource(R.string.pairing_computer)),
            device = thisDevice(),
        )
        if (state.method == PairingMethod.QR) {
            Surface(
                shape = GanderShape.Card,
                color = c.surface,
                border = BorderStroke(1.dp, c.line),
                modifier = Modifier.fillMaxWidth().padding(horizontal = 20.dp),
            ) {
                Column(Modifier.padding(18.dp), horizontalAlignment = Alignment.CenterHorizontally) {
                    Row(
                        horizontalArrangement = Arrangement.spacedBy(10.dp),
                        verticalAlignment = Alignment.CenterVertically,
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        Icon(GanderIcons.Shield, contentDescription = null, tint = c.ok, modifier = Modifier.size(18.dp))
                        Text(
                            stringResource(R.string.pairing_identity_checked),
                            style = MaterialTheme.typography.bodyMedium.copy(fontWeight = FontWeight.SemiBold, fontSize = 13.5.sp),
                            color = c.ok,
                        )
                    }
                    Text(
                        stringResource(R.string.pairing_code_label),
                        style = MaterialTheme.typography.bodySmall.copy(fontSize = 13.sp),
                        color = c.text3,
                        modifier = Modifier.padding(top = 16.dp, bottom = 8.dp),
                    )
                    SasCode(state.sas, small = true, modifier = Modifier.testTag(AddHostTags.SAS))
                }
            }
        } else {
            Text(
                stringResource(R.string.pairing_code_label),
                style = MaterialTheme.typography.bodySmall.copy(fontSize = 13.sp),
                color = c.text3,
                modifier = Modifier.padding(top = 8.dp),
            )
            SasCode(state.sas, modifier = Modifier.testTag(AddHostTags.SAS))
        }
        Text(
            text = when (state.method) {
                PairingMethod.QR -> stringResource(R.string.pairing_waiting_qr, state.hostLabel)
                PairingMethod.MANUAL -> stringResource(R.string.pairing_waiting_manual, state.hostLabel)
            },
            textAlign = TextAlign.Center,
            style = MaterialTheme.typography.bodyLarge.copy(fontSize = 15.sp),
            modifier = Modifier.padding(horizontal = 24.dp, vertical = 4.dp),
        )
        Column(Modifier.fillMaxWidth().padding(horizontal = 20.dp), horizontalAlignment = Alignment.CenterHorizontally) {
            GanderProgress(fraction = null)
            Text(
                stringResource(R.string.pairing_waiting_title, state.hostLabel),
                style = MaterialTheme.typography.bodySmall.copy(fontSize = 13.sp),
                color = c.text3,
                textAlign = TextAlign.Center,
                modifier = Modifier.padding(top = 8.dp),
            )
        }
        WarningBanner(
            title = null,
            body = stringResource(R.string.pairing_waiting_warning),
            tone = BannerTone.BAD,
            modifier = Modifier.padding(top = 12.dp).testTag(AddHostTags.WARNING),
        )
        Row(Modifier.fillMaxWidth().padding(horizontal = 20.dp)) {
            GanderButton(
                text = stringResource(R.string.pairing_cancel),
                onClick = actions.onCancel,
                kind = ButtonKind.OUTLINED,
                modifier = Modifier.testTag(AddHostTags.CANCEL),
            )
        }
    }
}

/** Mockup 10: why it stopped and what to do on the computer. */
@Composable
private fun FailedContent(state: AddHostUiState.Failed, actions: AddHostActions) {
    // Retrying a certificate or code mismatch only invites the same trap; start over instead.
    val canRetry = state.kind != PairingFailure.Kind.WRONG_COMPUTER && state.kind != PairingFailure.Kind.SAS_MISMATCH
    ScrollColumn(horizontalAlignment = Alignment.CenterHorizontally) {
        EmptyState(
            title = stringResource(R.string.pairing_failed_title),
            visual = { ResultBadge(GanderIcons.Shield, BannerTone.BAD) },
            modifier = Modifier.padding(top = 0.dp),
        )
        Text(
            text = failureText(state),
            textAlign = TextAlign.Center,
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier.padding(horizontal = 32.dp).testTag(AddHostTags.ERROR),
        )
        Banner(
            tone = BannerTone.BAD,
            title = stringResource(R.string.pairing_nothing_shared_title),
            text = stringResource(R.string.pairing_nothing_shared_body),
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
        )
        Column(
            modifier = Modifier.fillMaxWidth().padding(horizontal = 20.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            if (canRetry) {
                GanderButton(
                    text = stringResource(R.string.pairing_try_again),
                    onClick = actions.onRetry,
                    modifier = Modifier.fillMaxWidth().testTag(AddHostTags.RETRY),
                )
            }
            GanderButton(
                text = stringResource(R.string.pairing_start_over),
                onClick = actions.onCancel,
                kind = if (canRetry) ButtonKind.TEXT else ButtonKind.PRIMARY,
                modifier = Modifier.fillMaxWidth().testTag(AddHostTags.START_OVER),
            )
        }
    }
}

@Composable
private fun failureText(state: AddHostUiState.Failed): String {
    val base = stringResource(
        when (state.kind) {
            PairingFailure.Kind.CLOSED -> R.string.pairing_error_closed
            PairingFailure.Kind.RATE_LIMITED -> R.string.pairing_error_rate_limited
            PairingFailure.Kind.IN_PROGRESS -> R.string.pairing_error_in_progress
            PairingFailure.Kind.CODE_REJECTED -> R.string.pairing_error_code_rejected
            PairingFailure.Kind.WRONG_COMPUTER ->
                if (state.method == PairingMethod.QR) R.string.pairing_error_wrong_computer_qr else R.string.pairing_error_wrong_computer_manual
            PairingFailure.Kind.SAS_MISMATCH -> R.string.pairing_error_sas_mismatch
            PairingFailure.Kind.DENIED -> R.string.pairing_error_denied
            PairingFailure.Kind.EXPIRED -> R.string.pairing_error_expired
            PairingFailure.Kind.UNREACHABLE -> R.string.error_offline
            PairingFailure.Kind.SECURE_CONNECTION -> R.string.error_secure_connection
            PairingFailure.Kind.INCOMPATIBLE -> R.string.error_incompatible
            PairingFailure.Kind.STORAGE -> R.string.pairing_error_storage
            PairingFailure.Kind.APP_ERROR -> R.string.pairing_error_app
            PairingFailure.Kind.OTHER -> R.string.pairing_error_other
        },
    )
    // The computer's own English text (never paths or secrets) helps for the generic cases.
    val detail = state.detail?.takeIf {
        state.kind == PairingFailure.Kind.OTHER || state.kind == PairingFailure.Kind.CODE_REJECTED ||
            state.kind == PairingFailure.Kind.APP_ERROR
    }
    return if (detail.isNullOrBlank()) base else stringResource(R.string.error_with_detail, base, detail)
}

/** Mockup 09: paired. */
@Composable
private fun DoneContent(state: AddHostUiState.Done, actions: AddHostActions) {
    ScrollColumn(modifier = Modifier.testTag(AddHostTags.DONE), horizontalAlignment = Alignment.CenterHorizontally) {
        EmptyState(
            title = stringResource(R.string.pairing_done_title, state.host.name),
            body = stringResource(R.string.pairing_done_body),
            visual = { ResultBadge(GanderIcons.Check, BannerTone.OK) },
        )
        Column(
            modifier = Modifier.fillMaxWidth().padding(horizontal = 20.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            GanderButton(
                text = stringResource(R.string.pairing_done_open),
                onClick = { actions.onOpenHost(state.host.id) },
                modifier = Modifier.fillMaxWidth().testTag(AddHostTags.DONE_OPEN),
            )
            GanderButton(
                text = stringResource(R.string.pairing_done_close),
                onClick = actions.onClose,
                kind = ButtonKind.TEXT,
                modifier = Modifier.fillMaxWidth().testTag(AddHostTags.DONE_CLOSE),
            )
        }
    }
}
