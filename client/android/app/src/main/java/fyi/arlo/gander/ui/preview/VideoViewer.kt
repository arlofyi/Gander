package fyi.arlo.gander.ui.preview

import android.content.Context
import androidx.annotation.OptIn
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material3.FilledIconButton
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LifecycleEventEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.media3.common.PlaybackException
import androidx.media3.common.Player
import androidx.media3.common.util.UnstableApi
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.ui.PlayerView
import coil3.ImageLoader
import fyi.arlo.gander.R
import fyi.arlo.gander.core.net.protocol.EntryFlag
import fyi.arlo.gander.ui.components.Banner
import fyi.arlo.gander.ui.components.BannerTone
import fyi.arlo.gander.ui.theme.Brand
import fyi.arlo.gander.ui.theme.GanderIcons
import fyi.arlo.gander.ui.theme.GanderTheme
import fyi.arlo.gander.ui.theme.Immersive
import androidx.compose.foundation.layout.size
import androidx.compose.material3.IconButtonDefaults
import fyi.arlo.gander.core.data.browse.EntryTarget
import fyi.arlo.gander.core.data.preview.PlaybackFailure
import fyi.arlo.gander.core.data.preview.VideoPlayerFactory
import fyi.arlo.gander.core.data.preview.toPlaybackFailure
import fyi.arlo.gander.core.net.GanderError
import kotlinx.coroutines.CancellationException

/**
 * The video viewer (client spec, "Preview: video"): the poster frame first with a play button,
 * and ExoPlayer on the host's pinned OkHttp client once the user taps it. Leaving the screen
 * releases the player; moving the app to the background pauses it.
 */
@Composable
fun VideoViewerScreen(
    viewModel: VideoViewModel,
    target: EntryTarget,
    title: String,
    imageLoader: ImageLoader?,
    playerFactory: VideoPlayerFactory,
    onBack: () -> Unit,
    onFetch: (() -> Unit)?,
    modifier: Modifier = Modifier,
) {
    val poster by viewModel.uiState.collectAsStateWithLifecycle()
    val video by viewModel.video.collectAsStateWithLifecycle()
    PreviewScaffold(
        title = title,
        onBack = onBack,
        onFetch = onFetch,
        modifier = modifier,
        footer = {
            if (EntryFlag.CLOUD_ONLY in target.entry.flags) {
                Banner(
                    tone = BannerTone.INFO,
                    icon = GanderIcons.Cloud,
                    text = stringResource(R.string.preview_video_cloud_only, poster.hostName),
                    containerColor = Immersive.Content.copy(alpha = 0.08f),
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
                )
            }
            PreviewRail(caption = stringResource(R.string.rail_video, poster.hostName))
        },
    ) { padding ->
        BoxWithConstraints(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .background(Color.Black),
            contentAlignment = Alignment.Center,
        ) {
            val widthPx = constraints.maxWidth
            LaunchedEffect(widthPx, viewModel) { viewModel.onViewport(widthPx) }
            if (!video.started) {
                val ref = remember(poster.widthPx) { viewModel.pageRef() }
                if (ref != null) {
                    PreviewPageImage(
                        ref = ref,
                        attempt = poster.attempt,
                        imageLoader = imageLoader,
                        contentDescription = stringResource(R.string.preview_video_poster_description, title),
                        loadedTag = PreviewTags.POSTER_LOADED,
                        onLoaded = viewModel::onLoaded,
                        onFailed = viewModel::onFailed,
                        modifier = Modifier.fillMaxSize(),
                    )
                }
                FilledIconButton(
                    onClick = viewModel::start,
                    modifier = Modifier
                        .size(60.dp)
                        .testTag(PreviewTags.VIDEO_PLAY),
                    colors = IconButtonDefaults.filledIconButtonColors(
                        containerColor = Immersive.Content,
                        contentColor = Brand.SlateInk,
                    ),
                ) {
                    Icon(GanderIcons.Play, contentDescription = stringResource(R.string.preview_video_play), modifier = Modifier.size(32.dp))
                }
            } else {
                VideoPlayer(viewModel, video, target, playerFactory)
            }
            video.failure?.let { failure ->
                Surface(
                    modifier = Modifier.align(Alignment.Center),
                    color = GanderTheme.colors.surface,
                    shape = MaterialTheme.shapes.large,
                ) {
                    PreviewFailurePanel(
                        failure = failure,
                        onRetry = viewModel::retryPlayback,
                        onFetch = onFetch,
                    )
                }
            }
        }
    }
}

@OptIn(UnstableApi::class)
@Composable
private fun VideoPlayer(
    viewModel: VideoViewModel,
    video: VideoUiState,
    target: EntryTarget,
    playerFactory: VideoPlayerFactory,
) {
    val context = LocalContext.current
    var player by remember(video.attempt) { mutableStateOf<ExoPlayer?>(null) }

    LaunchedEffect(video.attempt) {
        player = try {
            playerFactory.create(context, target.hostId, target.shareId, target.path)
        } catch (e: CancellationException) {
            throw e
        } catch (e: GanderError) {
            viewModel.onPlaybackFailure(PlaybackFailure.Host(e))
            null
        }
    }
    // Releases the player when the screen goes, or when Retry builds a new one.
    DisposableEffect(video.attempt) {
        onDispose {
            player?.let {
                viewModel.onPosition(it.currentPosition)
                it.release()
            }
            player = null
        }
    }

    val exo = player ?: return
    DisposableEffect(exo) {
        val listener = object : Player.Listener {
            override fun onPlayerError(error: PlaybackException) {
                viewModel.onPlaybackFailure(error.toPlaybackFailure())
            }

            override fun onIsPlayingChanged(isPlaying: Boolean) {
                if (isPlaying) viewModel.onPlaybackStarted()
            }
        }
        exo.addListener(listener)
        if (video.positionMs > 0) exo.seekTo(video.positionMs)
        exo.prepare()
        exo.playWhenReady = true
        onDispose { exo.removeListener(listener) }
    }
    LifecycleEventEffect(Lifecycle.Event.ON_STOP) {
        exo.pause()
        viewModel.onPosition(exo.currentPosition)
    }
    AndroidView(
        factory = { viewContext: Context ->
            PlayerView(viewContext).apply {
                this.player = exo
                useController = true
                contentDescription = viewContext.getString(R.string.preview_video_player_description)
            }
        },
        update = { it.player = exo },
        modifier = Modifier
            .fillMaxSize()
            .testTag(PreviewTags.VIDEO_PLAYER),
    )
}
