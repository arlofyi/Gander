package fyi.arlo.gander.ui.hosts

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExtendedFloatingActionButton
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.repeatOnLifecycle
import fyi.arlo.gander.R
import fyi.arlo.gander.core.data.UiErrorKind
import fyi.arlo.gander.core.data.hosts.HostStatus
import fyi.arlo.gander.core.data.hosts.PairedHost
import fyi.arlo.gander.core.data.hosts.isRemovedOnHost
import fyi.arlo.gander.core.net.protocol.HostOs
import fyi.arlo.gander.core.net.protocol.RelayPath
import fyi.arlo.gander.ui.common.relativeTimeText
import fyi.arlo.gander.ui.common.text
import fyi.arlo.gander.ui.components.ButtonKind
import fyi.arlo.gander.ui.components.ChipStyle
import fyi.arlo.gander.ui.components.EmptyState
import fyi.arlo.gander.ui.components.GanderButton
import fyi.arlo.gander.ui.components.GanderChip
import fyi.arlo.gander.ui.components.GanderIconButton
import fyi.arlo.gander.ui.components.GanderTopBar
import fyi.arlo.gander.ui.components.LargeTitle
import fyi.arlo.gander.ui.components.RowDivider
import fyi.arlo.gander.ui.components.StatusKind
import fyi.arlo.gander.ui.components.StatusLine
import fyi.arlo.gander.ui.components.TypeTile
import fyi.arlo.gander.ui.theme.GanderIcons
import fyi.arlo.gander.ui.theme.GanderTheme
import fyi.arlo.gander.ui.theme.LensMark

/** Test tags for the Hosts screen (instrumented tests, WP-29). */
object HostsTags {
    fun card(hostId: String) = "host-card-$hostId"
    const val ADD = "hosts-add"
    const val SETTINGS = "hosts-settings"
    const val STORAGE = "hosts-storage"

    fun details(hostId: String) = "host-details-$hostId"

    fun removed(hostId: String) = "host-removed-$hostId"

    fun pairAgain(hostId: String) = "host-pair-again-$hostId"

    fun forgetNow(hostId: String) = "host-forget-$hostId"

    /** The Forget button of the confirmation dialog (the card's own Forget says the same word). */
    const val FORGET_CONFIRM = "host-forget-confirm"
}

/**
 * Home screen: your computers. Pings every host every 20 s while the screen is visible
 * ([HostsViewModel.pingWhileVisible]), pull-to-refresh pings now.
 */
@Composable
fun HostsScreen(
    viewModel: HostsViewModel,
    onOpenHost: (hostId: String) -> Unit,
    onAddHost: () -> Unit,
    modifier: Modifier = Modifier,
    onOpenSettings: () -> Unit = {},
    onOpenStorage: () -> Unit = {},
    onOpenDetails: (hostId: String) -> Unit = {},
) {
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    val lifecycleOwner = LocalLifecycleOwner.current
    LaunchedEffect(viewModel, lifecycleOwner) {
        lifecycleOwner.lifecycle.repeatOnLifecycle(Lifecycle.State.STARTED) { viewModel.pingWhileVisible() }
    }
    HostsContent(
        state = state,
        onOpenHost = onOpenHost,
        onAddHost = onAddHost,
        onRefresh = viewModel::refresh,
        onRename = viewModel::rename,
        onForget = viewModel::forget,
        modifier = modifier,
        onOpenSettings = onOpenSettings,
        onOpenStorage = onOpenStorage,
        onOpenDetails = onOpenDetails,
    )
}

/** Stateless Hosts screen (mockups 02 and 03); the instrumented tests render it with fixed state. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun HostsContent(
    state: HostsUiState,
    onOpenHost: (hostId: String) -> Unit,
    onAddHost: () -> Unit,
    onRefresh: () -> Unit,
    onRename: (hostId: String, name: String) -> Unit,
    onForget: (hostId: String) -> Unit,
    modifier: Modifier = Modifier,
    onOpenSettings: () -> Unit = {},
    onOpenStorage: () -> Unit = {},
    onOpenDetails: (hostId: String) -> Unit = {},
) {
    val c = GanderTheme.colors
    Scaffold(
        modifier = modifier,
        containerColor = c.bg,
        topBar = {
            GanderTopBar(title = "", modifier = Modifier.statusBarsPadding()) {
                GanderIconButton(
                    GanderIcons.Storage,
                    stringResource(R.string.hosts_storage),
                    onOpenStorage,
                    modifier = Modifier.testTag(HostsTags.STORAGE),
                )
                GanderIconButton(
                    GanderIcons.Settings,
                    stringResource(R.string.hosts_settings),
                    onOpenSettings,
                    modifier = Modifier.testTag(HostsTags.SETTINGS),
                )
            }
        },
        floatingActionButton = {
            if (state.cards.isNotEmpty()) {
                ExtendedFloatingActionButton(
                    onClick = onAddHost,
                    icon = { Icon(GanderIcons.Add, contentDescription = null) },
                    text = { Text(stringResource(R.string.hosts_add)) },
                    containerColor = c.primary,
                    contentColor = c.onPrimary,
                    shape = MaterialTheme.shapes.large,
                    modifier = Modifier.testTag(HostsTags.ADD),
                )
            }
        },
    ) { innerPadding ->
        PullToRefreshBox(
            isRefreshing = state.refreshing,
            onRefresh = onRefresh,
            modifier = Modifier
                .fillMaxSize()
                .padding(innerPadding),
        ) {
            if (state.cards.isEmpty()) {
                if (state.loaded) EmptyHosts(state, onAddHost)
            } else {
                LazyColumn(
                    modifier = Modifier.fillMaxSize(),
                    contentPadding = PaddingValues(bottom = 96.dp),
                ) {
                    item {
                        LargeTitle(stringResource(R.string.hosts_title))
                        PrivateNetworkHint(state.privateNetworkUp, Modifier.padding(start = 20.dp, end = 20.dp, bottom = 8.dp))
                    }
                    itemsIndexed(state.cards, key = { _, card -> card.host.id }) { index, card ->
                        if (index > 0) RowDivider()
                        HostCardView(
                            card = card,
                            onOpen = { onOpenHost(card.host.id) },
                            onRename = { onRename(card.host.id, it) },
                            onForget = { onForget(card.host.id) },
                            onDetails = { onOpenDetails(card.host.id) },
                            onPairAgain = onAddHost,
                        )
                    }
                }
            }
        }
    }
}

/** First run (mockup 02): the Lens mark, what Gander needs, and Add a computer. */
@Composable
private fun EmptyHosts(state: HostsUiState, onAddHost: () -> Unit) {
    Column(
        modifier = Modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState()),
    ) {
        LargeTitle(stringResource(R.string.hosts_title))
        PrivateNetworkHint(state.privateNetworkUp, Modifier.padding(horizontal = 20.dp))
        EmptyState(
            title = stringResource(R.string.hosts_empty_title),
            body = stringResource(R.string.hosts_empty_body),
            note = stringResource(R.string.hosts_empty_note),
            visual = { LensMark(size = 120.dp) },
            modifier = Modifier.padding(top = 24.dp),
        ) {
            GanderButton(
                text = stringResource(R.string.hosts_add),
                onClick = onAddHost,
                icon = GanderIcons.Add,
                modifier = Modifier.testTag(HostsTags.ADD),
            )
        }
    }
}

@Composable
private fun PrivateNetworkHint(up: Boolean, modifier: Modifier = Modifier) {
    StatusLine(
        kind = if (up) StatusKind.ONLINE else StatusKind.WARN,
        text = stringResource(if (up) R.string.hosts_private_network_up else R.string.hosts_private_network_down),
        style = MaterialTheme.typography.bodyMedium.copy(fontSize = 13.sp),
        modifier = modifier,
    )
}

/**
 * One paired computer as a full-bleed block (not a card): device tile, name with the relay chip,
 * status dot and word, the OS, "Paired as", the relay sentence or the problem, and the menu.
 */
@Composable
fun HostCardView(
    card: HostCard,
    onOpen: () -> Unit,
    onRename: (String) -> Unit,
    onForget: () -> Unit,
    modifier: Modifier = Modifier,
    onDetails: () -> Unit = {},
    /** "Pair again" on a host that removed this phone: opens Add host. */
    onPairAgain: () -> Unit = {},
) {
    val host = card.host
    val c = GanderTheme.colors
    var menuOpen by remember { mutableStateOf(false) }
    var renaming by remember { mutableStateOf(false) }
    var forgetting by remember { mutableStateOf(false) }
    val reachable = card.status is HostStatus.Online
    val relayed = (card.status as? HostStatus.Online)?.isRelayed == true

    Row(
        modifier = modifier
            .fillMaxWidth()
            .clickable(onClick = onOpen)
            .testTag(HostsTags.card(host.id))
            .padding(start = 20.dp, end = 8.dp, top = 14.dp, bottom = 14.dp),
        verticalAlignment = Alignment.Top,
        horizontalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        TypeTile(
            icon = GanderIcons.Desktop,
            size = 48.dp,
            container = if (reachable) c.surface3 else c.surface2,
            tint = if (reachable) c.text else c.text2,
        )
        Column(modifier = Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(3.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(
                    host.name,
                    style = MaterialTheme.typography.titleMedium.copy(fontSize = 17.sp),
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f, fill = false),
                )
                if (relayed) {
                    GanderChip(
                        text = stringResource(R.string.host_relay_chip),
                        icon = GanderIcons.Relay,
                        style = ChipStyle.WARN,
                        small = true,
                    )
                }
            }
            StatusLine(host, card.status)
            host.pairedAs?.let {
                Text(
                    text = stringResource(R.string.host_paired_as, it),
                    style = MaterialTheme.typography.bodySmall.copy(fontSize = 12.5.sp),
                    color = c.text3,
                )
            }
            RelayLine(card.status)
            if (card.status.isRemovedOnHost) {
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.padding(top = 6.dp)) {
                    GanderButton(
                        text = stringResource(R.string.host_removed_pair_again),
                        onClick = onPairAgain,
                        small = true,
                        modifier = Modifier.testTag(HostsTags.pairAgain(host.id)),
                    )
                    GanderButton(
                        text = stringResource(R.string.host_menu_forget),
                        onClick = { forgetting = true },
                        kind = ButtonKind.TEXT,
                        small = true,
                        modifier = Modifier.testTag(HostsTags.forgetNow(host.id)),
                    )
                }
            }
        }
        Box {
            GanderIconButton(GanderIcons.More, stringResource(R.string.action_more), { menuOpen = true })
            DropdownMenu(expanded = menuOpen, onDismissRequest = { menuOpen = false }, containerColor = c.surface2) {
                DropdownMenuItem(
                    text = { Text(stringResource(R.string.host_menu_details)) },
                    modifier = Modifier.testTag(HostsTags.details(host.id)),
                    onClick = {
                        menuOpen = false
                        onDetails()
                    },
                )
                DropdownMenuItem(
                    text = { Text(stringResource(R.string.host_menu_rename)) },
                    onClick = {
                        menuOpen = false
                        renaming = true
                    },
                )
                DropdownMenuItem(
                    text = { Text(stringResource(R.string.host_menu_forget), color = c.bad) },
                    onClick = {
                        menuOpen = false
                        forgetting = true
                    },
                )
            }
        }
    }

    if (renaming) {
        RenameDialog(
            currentName = host.name,
            onDismiss = { renaming = false },
            onConfirm = {
                renaming = false
                onRename(it)
            },
        )
    }
    if (forgetting) {
        AlertDialog(
            onDismissRequest = { forgetting = false },
            containerColor = c.surface2,
            title = { Text(stringResource(R.string.host_forget_title, host.name)) },
            text = { Text(stringResource(R.string.host_forget_body)) },
            confirmButton = {
                TextButton(
                    onClick = {
                        forgetting = false
                        onForget()
                    },
                    modifier = Modifier.testTag(HostsTags.FORGET_CONFIRM),
                ) { Text(stringResource(R.string.host_forget_confirm), color = c.bad, fontWeight = FontWeight.SemiBold) }
            },
            dismissButton = {
                TextButton(onClick = { forgetting = false }) { Text(stringResource(R.string.action_cancel), color = c.text) }
            },
        )
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

/**
 * The status dot and word, then the OS; offline adds "last seen", and a problem that is not plain
 * "offline" gets its own sentence. Never colour alone: each state has its own dot shape and word.
 */
@Composable
private fun StatusLine(host: PairedHost, status: HostStatus) {
    val c = GanderTheme.colors
    val lastSeen = host.lastSeenAtMillis
    val meta = MaterialTheme.typography.bodyMedium.copy(fontSize = 13.sp)
    @Composable
    fun line(kind: StatusKind, word: String, extra: String?, tag: String? = null) {
        Row(
            // Its own node, so a tag on the line is found even though the block is one tap target.
            modifier = Modifier.semantics(mergeDescendants = true) {}.then(if (tag != null) Modifier.testTag(tag) else Modifier),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            StatusLine(kind = kind, text = word, style = meta)
            if (extra != null) Text(extra, style = meta, color = c.text3, maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
    }
    when (status) {
        HostStatus.Checking -> line(StatusKind.CHECKING, stringResource(R.string.host_status_checking), osLabel(host.os))
        is HostStatus.Online -> {
            line(StatusKind.ONLINE, stringResource(R.string.host_status_online), osLabel(host.os))
            status.notice?.let { ProblemText(it.text()) }
        }
        is HostStatus.Unavailable if status.isRemovedOnHost -> {
            // Not an error to puzzle over: the owner removed this phone on the computer (or its
            // key no longer works there). Say so, and offer the two ways forward right here.
            line(StatusKind.BAD, stringResource(R.string.host_status_removed), null, HostsTags.removed(host.id))
            Text(
                text = stringResource(R.string.host_removed_body),
                style = MaterialTheme.typography.bodySmall.copy(fontSize = 12.5.sp),
                color = c.text2,
            )
        }
        is HostStatus.Unavailable -> {
            val offline = status.error.kind == UiErrorKind.OFFLINE
            val label = stringResource(if (offline) R.string.host_status_offline else R.string.host_status_problem)
            val seen = if (lastSeen != null) {
                stringResource(R.string.host_last_seen, relativeTimeText(lastSeen))
            } else {
                stringResource(R.string.host_never_seen)
            }
            line(if (offline) StatusKind.OFFLINE else StatusKind.WARN, "$label · $seen", null)
            if (!offline) ProblemText(status.error.text())
        }
    }
}

@Composable
private fun ProblemText(text: String) {
    Text(
        text = text,
        style = MaterialTheme.typography.bodySmall.copy(fontSize = 12.5.sp),
        color = GanderTheme.colors.bad,
    )
}

/** Always followed by one plain sentence (design document section 8, relay chip). */
@Composable
private fun RelayLine(status: HostStatus) {
    if (status !is HostStatus.Online || !status.isRelayed) return
    Text(
        text = stringResource(if (status.relay == RelayPath.PEER_RELAY) R.string.host_relay_peer else R.string.host_relay_derp),
        style = MaterialTheme.typography.bodySmall.copy(fontSize = 12.5.sp),
        color = GanderTheme.colors.text3,
    )
}

@Composable
private fun RenameDialog(currentName: String, onDismiss: () -> Unit, onConfirm: (String) -> Unit) {
    var name by remember { mutableStateOf(currentName) }
    AlertDialog(
        onDismissRequest = onDismiss,
        containerColor = GanderTheme.colors.surface2,
        title = { Text(stringResource(R.string.host_rename_title)) },
        text = {
            OutlinedTextField(
                value = name,
                onValueChange = { name = it },
                label = { Text(stringResource(R.string.host_rename_label)) },
                singleLine = true,
                modifier = Modifier.fillMaxWidth(),
            )
        },
        confirmButton = {
            TextButton(onClick = { onConfirm(name) }, enabled = name.isNotBlank()) {
                Text(stringResource(R.string.action_save))
            }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text(stringResource(R.string.action_cancel)) } },
    )
}
