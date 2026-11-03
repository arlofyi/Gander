package fyi.arlo.gander.ui.browser

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.repeatOnLifecycle
import fyi.arlo.gander.R
import fyi.arlo.gander.core.net.protocol.Permission
import fyi.arlo.gander.core.net.protocol.ShareInfo
import fyi.arlo.gander.ui.common.CachedChip
import fyi.arlo.gander.ui.common.ErrorPanel
import fyi.arlo.gander.ui.common.text
import fyi.arlo.gander.ui.components.Banner
import fyi.arlo.gander.ui.components.BannerTone
import fyi.arlo.gander.ui.components.ChipStyle
import fyi.arlo.gander.ui.components.EmptyState
import fyi.arlo.gander.ui.components.GanderChip
import fyi.arlo.gander.ui.components.GanderProgress
import fyi.arlo.gander.ui.components.GanderTopBar
import fyi.arlo.gander.ui.components.ListRow
import fyi.arlo.gander.ui.components.SectionHeader
import fyi.arlo.gander.ui.components.TypeTile
import fyi.arlo.gander.ui.theme.GanderIcons
import fyi.arlo.gander.ui.theme.GanderTheme

object SharesTags {
    fun card(shareId: String) = "share-card-$shareId"
}

/** Shares of one host: the entry point to a browser. `shares.updated` refreshes it while visible. */
@Composable
fun SharesScreen(
    viewModel: SharesViewModel,
    onBack: () -> Unit,
    onOpenShare: (shareId: String) -> Unit,
    modifier: Modifier = Modifier,
) {
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    val lifecycleOwner = LocalLifecycleOwner.current
    LaunchedEffect(viewModel, lifecycleOwner) {
        lifecycleOwner.lifecycle.repeatOnLifecycle(Lifecycle.State.STARTED) { viewModel.watchEvents() }
    }
    SharesContent(state, onBack, onOpenShare, viewModel::refresh, modifier)
}

/** Stateless Shares screen (mockup 11): full-bleed folder rows with their permission chips. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SharesContent(
    state: SharesUiState,
    onBack: () -> Unit,
    onOpenShare: (shareId: String) -> Unit,
    onRefresh: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var pulled by remember { mutableStateOf(false) }
    LaunchedEffect(state.refreshing) { if (!state.refreshing) pulled = false }

    Scaffold(
        modifier = modifier,
        containerColor = GanderTheme.colors.bg,
        topBar = {
            GanderTopBar(
                title = state.hostName,
                subtitle = stringResource(R.string.shares_subtitle),
                navigationIcon = GanderIcons.Back,
                navigationDescription = stringResource(R.string.action_back),
                onNavigate = onBack,
                modifier = Modifier.statusBarsPadding(),
            )
        },
    ) { innerPadding ->
        PullToRefreshBox(
            isRefreshing = pulled && state.refreshing,
            onRefresh = {
                pulled = true
                onRefresh()
            },
            modifier = Modifier
                .fillMaxSize()
                .padding(innerPadding),
        ) {
            when {
                state.loading -> Column(
                    modifier = Modifier
                        .fillMaxSize()
                        .verticalScroll(rememberScrollState())
                        .padding(horizontal = 20.dp, vertical = 24.dp),
                ) { GanderProgress(fraction = null) }

                state.shares.isEmpty() && state.error != null -> Column(
                    modifier = Modifier
                        .fillMaxSize()
                        .verticalScroll(rememberScrollState()),
                    verticalArrangement = Arrangement.Center,
                ) { ErrorPanel(state.error, onRetry = onRefresh, modifier = Modifier.fillMaxWidth()) }

                state.shares.isEmpty() -> Column(
                    modifier = Modifier
                        .fillMaxSize()
                        .verticalScroll(rememberScrollState()),
                    verticalArrangement = Arrangement.Center,
                ) {
                    EmptyState(
                        title = stringResource(R.string.shares_empty_title),
                        body = stringResource(R.string.shares_empty_body),
                        visual = { TypeTile(GanderIcons.Folder, folder = true, size = 64.dp, iconSize = 32.dp) },
                    )
                }

                else -> LazyColumn(modifier = Modifier.fillMaxSize(), contentPadding = PaddingValues(bottom = 16.dp)) {
                    state.cachedAtMillis?.let { cachedAt ->
                        item {
                            Banner(
                                tone = BannerTone.WARN,
                                icon = GanderIcons.Cloud,
                                modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
                            ) {
                                CachedChip(cachedAt, Modifier.padding(bottom = 4.dp))
                                Text(stringResource(R.string.cached_hint), style = MaterialTheme.typography.bodyMedium)
                            }
                        }
                    }
                    state.error?.let { error ->
                        item {
                            Banner(
                                tone = BannerTone.BAD,
                                text = error.text(),
                                modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
                            )
                        }
                    }
                    item {
                        SectionHeader(
                            stringResource(R.string.shares_section),
                            padding = PaddingValues(start = 20.dp, end = 20.dp, top = 6.dp, bottom = 6.dp),
                        )
                    }
                    items(state.shares, key = { it.id }) { share ->
                        ShareCard(share, onClick = { onOpenShare(share.id) })
                    }
                    item {
                        Text(
                            stringResource(R.string.shares_footer, state.hostName),
                            style = MaterialTheme.typography.bodySmall.copy(fontSize = 12.5.sp),
                            color = GanderTheme.colors.text3,
                            modifier = Modifier.padding(horizontal = 20.dp, vertical = 14.dp),
                        )
                    }
                }
            }
        }
    }
}

/**
 * One shared folder as a full-bleed row: folder tile, name, the computer's path in mono, and the
 * three permission chips (filled when allowed, crossed out when not).
 */
@Composable
fun ShareCard(share: ShareInfo, onClick: () -> Unit, modifier: Modifier = Modifier) {
    val canList = Permission.LIST in share.permissions
    ListRow(
        headline = share.name,
        modifier = modifier.testTag(SharesTags.card(share.id)),
        onClick = if (canList) onClick else null,
        alignTop = true,
        leading = { TypeTile(GanderIcons.Folder, folder = true) },
        trailing = { if (canList) Icon(GanderIcons.Chevron, contentDescription = null, tint = GanderTheme.colors.text3) },
        supportingContent = {
            share.pathDisplay?.let {
                Text(it, style = GanderTheme.type.mono.copy(fontSize = 12.sp), color = GanderTheme.colors.text3, maxLines = 1)
            }
            if (canList) {
                Row(horizontalArrangement = Arrangement.spacedBy(6.dp), modifier = Modifier.padding(top = 6.dp)) {
                    PermissionChip(stringResource(R.string.permission_preview), GanderIcons.Preview, Permission.PREVIEW in share.permissions)
                    PermissionChip(stringResource(R.string.permission_fetch), GanderIcons.Fetch, Permission.FETCH in share.permissions)
                    PermissionChip(stringResource(R.string.permission_stream), GanderIcons.Stream, Permission.STREAM in share.permissions)
                }
            } else {
                Text(
                    stringResource(R.string.share_no_list),
                    style = MaterialTheme.typography.bodySmall,
                    color = GanderTheme.colors.bad,
                    modifier = Modifier.padding(top = 4.dp),
                )
            }
        },
    )
}

/** A permission chip: filled when allowed, crossed out (and announced "not allowed") when not. */
@Composable
fun PermissionChip(label: String, icon: ImageVector, allowed: Boolean) {
    val notAllowed = stringResource(R.string.permission_not_allowed, label)
    GanderChip(
        text = label,
        icon = icon,
        small = true,
        style = if (allowed) ChipStyle.FILLED else ChipStyle.NOT_ALLOWED,
        modifier = if (allowed) Modifier else Modifier.semantics { contentDescription = notAllowed },
    )
}
