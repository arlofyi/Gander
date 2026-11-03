package fyi.arlo.gander.ui.preview

import androidx.lifecycle.SavedStateHandle
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import fyi.arlo.gander.core.data.UiError
import fyi.arlo.gander.core.data.UiErrorKind
import fyi.arlo.gander.core.data.actions.ActionRefusals
import fyi.arlo.gander.core.data.actions.FileAction
import fyi.arlo.gander.core.data.browse.EntryTarget
import fyi.arlo.gander.core.data.hosts.HostRepository
import fyi.arlo.gander.core.data.preview.PlaybackFailure
import fyi.arlo.gander.core.data.preview.PreviewFailure
import fyi.arlo.gander.core.data.preview.PreviewPageRef
import fyi.arlo.gander.core.data.preview.PreviewRetryPolicy
import fyi.arlo.gander.core.data.preview.PreviewSource
import fyi.arlo.gander.core.data.preview.PreviewTileRef
import fyi.arlo.gander.core.data.preview.PreviewWidths
import fyi.arlo.gander.core.data.preview.shouldReportRefusal
import fyi.arlo.gander.core.data.preview.toPreviewFailure
import fyi.arlo.gander.core.data.thumbs.thumbVersion
import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.protocol.PreviewKind
import fyi.arlo.gander.core.net.protocol.TilePage
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

data class SinglePageUiState(
    /** The `width` the picture is requested at; null until the viewport is measured. */
    val widthPx: Int? = null,
    val loaded: Boolean = false,
    val failure: PreviewFailure? = null,
    /** Bumped by [SinglePageViewModel.retry] so the image request is built again. */
    val attempt: Int = 0,
    val hostName: String = "",
    /**
     * Deep-zoom geometry of the picture (`preview/info` `tile_pages[0]`), once the host said so;
     * null while it is unknown, when the host has no tiles for it, and always for a poster.
     */
    val tilePage: TilePage? = null,
)

/**
 * Logic of the image viewer (client spec, "Preview: image"): one picture, `preview?page=1` at the
 * width of the screen, clamped to what the host renders. Coil draws it from [pageRef]; the UI
 * reports the outcome ([onLoaded], [onFailed]) and a lasting refusal reaches the file sheet. The
 * video viewer builds on it for its poster frame, which comes from the same endpoint.
 */
open class SinglePageViewModel(
    protected val target: EntryTarget,
    private val hosts: HostRepository,
    protected val refusalLog: ActionRefusals,
    /**
     * Where `preview/info` comes from for deep zoom (WP-67): given, the viewer asks once, after the
     * picture has loaded, whether the host has tiles for it. Null for a video poster, which never
     * asks and never has tiles.
     */
    private val tileInfo: PreviewSource? = null,
    private val tileInfoRetry: PreviewRetryPolicy = PreviewRetryPolicy(budgetMs = TILE_INFO_BUDGET_MS),
    private val sleep: (suspend (millis: Long) -> Unit)? = null,
    private val now: () -> Long = { System.nanoTime() / 1_000_000 },
) : ViewModel() {

    private val _state = MutableStateFlow(SinglePageUiState())
    val uiState: StateFlow<SinglePageUiState> = _state.asStateFlow()

    private var hostMaxWidth: Int? = null
    private var viewportPx: Int? = null
    private var refusalCleared = false
    private var tileInfoAsked = false

    init {
        viewModelScope.launch {
            val host = hosts.get(target.hostId)
            if (host == null) {
                _state.update { it.copy(failure = PreviewFailure.Host(UiError(UiErrorKind.NOT_PAIRED))) }
                return@launch
            }
            hostMaxWidth = host.capabilities?.previewMaxWidth
            _state.update { it.copy(hostName = host.name) }
            recomputeWidth()
        }
    }

    /** The viewport is [px] wide (client spec: request width = screen width px). */
    fun onViewport(px: Int) {
        if (px <= 0 || viewportPx == px) return
        viewportPx = px
        recomputeWidth()
    }

    private fun recomputeWidth() {
        val px = viewportPx ?: return
        val width = PreviewWidths.pick(px, hostMaxWidth)
        _state.update { if (it.widthPx == width) it else it.copy(widthPx = width, loaded = false) }
    }

    /** The picture to draw, or null while the viewport is not measured. */
    fun pageRef(): PreviewPageRef? {
        val width = _state.value.widthPx ?: return null
        return PreviewPageRef(
            hostId = target.hostId,
            shareId = target.shareId,
            path = target.path,
            version = target.entry.thumbVersion(),
            page = 1,
            widthPx = width,
        )
    }

    fun onLoaded() {
        _state.update { it.copy(loaded = true, failure = null) }
        if (!refusalCleared) {
            refusalCleared = true
            refusalLog.clear(target, FileAction.PREVIEW)
        }
        askTileInfo()
    }

    /**
     * Once the base picture is there, asks the host whether it has deep-zoom tiles for it. Cheap
     * (the host caches the answer) and never in the picture's way: a failure is ignored and the
     * picture simply has no deep zoom.
     */
    private fun askTileInfo() {
        val source = tileInfo ?: return
        if (tileInfoAsked) return
        tileInfoAsked = true
        viewModelScope.launch {
            try {
                val info = tileInfoRetry.run(sleep, now) { source.info(target.hostId, target.shareId, target.path) }
                val page = info.takeIf { it.kind == PreviewKind.IMAGE }?.usableTilePages()?.firstOrNull()
                if (page != null) _state.update { it.copy(tilePage = page) }
            } catch (e: CancellationException) {
                throw e
            } catch (_: GanderError) {
                // No deep zoom for this picture; the base page is all there is.
            }
        }
    }

    /** The request for tile [tile] of the picture (deep zoom, WP-67). */
    fun tileRef(tile: TileKey): PreviewTileRef = PreviewTileRef(
        hostId = target.hostId,
        shareId = target.shareId,
        path = target.path,
        version = target.entry.thumbVersion(),
        page = 1,
        level = tile.level,
        x = tile.x,
        y = tile.y,
    )

    open fun onFailed(error: GanderError) {
        _state.update { it.copy(loaded = false, failure = error.toPreviewFailure()) }
        reportRefusal(error)
    }

    fun retry() {
        _state.update { it.copy(failure = null, attempt = it.attempt + 1) }
    }

    protected fun reportRefusal(error: GanderError) {
        if (shouldReportRefusal(target, error)) refusalLog.report(target, FileAction.PREVIEW, error)
    }
}

/** What the video viewer shows besides the poster. */
data class VideoUiState(
    /** The user tapped play: the player exists from now on. */
    val started: Boolean = false,
    /** Where playback was when the viewer last left the foreground, to resume after a rotation. */
    val positionMs: Long = 0,
    val failure: PreviewFailure? = null,
    /** Bumped by [VideoViewModel.retryPlayback] so the player is built again. */
    val attempt: Int = 0,
)

/**
 * Video viewer (client spec, "Preview: video"): the poster first ([SinglePageViewModel]), and the
 * player only after the user starts it, so opening a video on mobile data costs one poster frame,
 * not the first megabytes of the file. Playback failures are classified by
 * [fyi.arlo.gander.core.data.preview.toPlaybackFailure] and passed to [onPlaybackFailure].
 */
class VideoViewModel(
    target: EntryTarget,
    hosts: HostRepository,
    refusalLog: ActionRefusals,
    private val saved: SavedStateHandle,
) : SinglePageViewModel(target, hosts, refusalLog) {

    private val _video = MutableStateFlow(
        VideoUiState(
            started = saved.get<Boolean>(KEY_STARTED) ?: false,
            positionMs = saved.get<Long>(KEY_POSITION) ?: 0,
        ),
    )
    val video: StateFlow<VideoUiState> = _video.asStateFlow()

    /** A missing poster is not an error: the video may still play (the host has no poster for it). */
    override fun onFailed(error: GanderError) = Unit

    fun start() {
        saved[KEY_STARTED] = true
        _video.update { it.copy(started = true, failure = null) }
    }

    fun onPosition(positionMs: Long) {
        saved[KEY_POSITION] = positionMs
        _video.update { it.copy(positionMs = positionMs) }
    }

    fun onPlaybackFailure(failure: PlaybackFailure) {
        val shown = when (failure) {
            is PlaybackFailure.Host -> failure.error.toPreviewFailure().also { reportRefusal(failure.error) }
            PlaybackFailure.CodecUnsupported -> PreviewFailure.CodecUnsupported
            is PlaybackFailure.Other -> PreviewFailure.Host(UiError(UiErrorKind.OTHER))
        }
        _video.update { it.copy(failure = shown) }
    }

    /** Playback began without an error: the file sheet's earlier refusal, if any, no longer holds. */
    fun onPlaybackStarted() {
        refusalLog.clear(target, FileAction.PREVIEW)
    }

    fun retryPlayback() {
        _video.update { it.copy(failure = null, attempt = it.attempt + 1, started = true) }
    }

    private companion object {
        const val KEY_STARTED = "started"
        const val KEY_POSITION = "position"
    }
}

/** How long the image viewer waits for `preview/info` (deep zoom) before giving up on tiles. */
internal const val TILE_INFO_BUDGET_MS = 20_000L
