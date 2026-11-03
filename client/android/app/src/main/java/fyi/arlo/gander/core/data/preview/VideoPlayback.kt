package fyi.arlo.gander.core.data.preview

import android.content.Context
import androidx.annotation.OptIn
import androidx.media3.common.AudioAttributes
import androidx.media3.common.MediaItem
import androidx.media3.common.PlaybackException
import androidx.media3.common.util.UnstableApi
import androidx.media3.datasource.okhttp.OkHttpDataSource
import androidx.media3.exoplayer.DefaultLoadControl
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.exoplayer.source.DefaultMediaSourceFactory
import fyi.arlo.gander.core.net.GanderApi
import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.GanderHttpClients
import androidx.media3.datasource.HttpDataSource

/**
 * An ExoPlayer that reads the host's file bytes through the host's pinned `transfer` client
 * (client spec: "Media3 reads files through an OkHttp `DataSource` on the pinned client, so video
 * plays directly from the host with Range requests").
 *
 * The bearer token is added by the client's `GanderHeadersInterceptor`; it is never in the URL
 * (the URL is `.../v1/shares/<share>/file?path=<path>`) and nothing here logs it. ExoPlayer sends
 * `Range: bytes=<pos>-` for every open and for every seek, which the host honours.
 */
@OptIn(UnstableApi::class)
fun createGanderPlayer(context: Context, clients: GanderHttpClients): ExoPlayer =
    ExoPlayer.Builder(context)
        .setMediaSourceFactory(DefaultMediaSourceFactory(OkHttpDataSource.Factory(clients.transfer)))
        // Hand-picked buffering for a private network: start quickly, keep little ahead (the phone
        // is not a cache; the user may leave). Seeks reopen with a new Range anyway.
        .setLoadControl(
            DefaultLoadControl.Builder()
                .setBufferDurationsMs(
                    /* minBufferMs = */ 5_000,
                    /* maxBufferMs = */ 30_000,
                    /* bufferForPlaybackMs = */ 1_000,
                    /* bufferForPlaybackAfterRebufferMs = */ 2_500,
                )
                .build(),
        )
        .setAudioAttributes(AudioAttributes.DEFAULT, /* handleAudioFocus = */ true)
        .setHandleAudioBecomingNoisy(true)
        .build()

/** The media item of a host file; ExoPlayer sniffs the container, the URL has no file extension. */
fun videoMediaItem(clients: GanderHttpClients, shareId: String, path: String): MediaItem =
    MediaItem.fromUri(GanderApi(clients).fileUrl(shareId, path).toString())

/** Builds the player of one host's video; the app wires this to its pinned clients, tests may not. */
fun interface VideoPlayerFactory {
    /** A player with the media item set but not prepared. Throws [GanderError] if the host is unknown. */
    suspend fun create(context: Context, hostId: String, shareId: String, path: String): ExoPlayer
}

class PinnedVideoPlayerFactory(
    private val clientsFor: suspend (hostId: String) -> GanderHttpClients,
) : VideoPlayerFactory {
    override suspend fun create(context: Context, hostId: String, shareId: String, path: String): ExoPlayer {
        val clients = clientsFor(hostId)
        return createGanderPlayer(context, clients).also { it.setMediaItem(videoMediaItem(clients, shareId, path)) }
    }
}

/** What went wrong while playing, in terms the viewer explains. */
sealed interface PlaybackFailure {
    /** The host or the connection failed; [error] maps to the shared texts and to the file sheet's reason. */
    data class Host(val error: GanderError) : PlaybackFailure

    /** This phone cannot decode the video (HEVC/AV1 depend on the device) or does not know the container. */
    data object CodecUnsupported : PlaybackFailure

    /** Anything else; the viewer offers Retry. */
    data class Other(val errorCode: Int) : PlaybackFailure
}

/**
 * Classifies a Media3 failure. A non-2xx answer of the host reaches ExoPlayer as an
 * `InvalidResponseCodeException` carrying the status, headers and the (JSON) error body, which
 * [GanderError.fromResponse] turns into the same typed error as every other call: a `403
 * permission_denied` (a share that lets the phone preview but not fetch cannot play the video,
 * whose bytes come from `/file`), `401 token_revoked`, `404`, `503`...
 */
@OptIn(UnstableApi::class)
fun PlaybackException.toPlaybackFailure(): PlaybackFailure = playbackFailureOf(errorCode, cause)

@OptIn(UnstableApi::class)
internal fun playbackFailureOf(errorCode: Int, cause: Throwable?): PlaybackFailure {
    if (errorCode in CODEC_ERROR_CODES) return PlaybackFailure.CodecUnsupported
    val chain = generateSequence(cause) { it.cause?.takeIf { next -> next !== it } }.take(MAX_CAUSE_DEPTH).toList()
    chain.filterIsInstance<HttpDataSource.InvalidResponseCodeException>().firstOrNull()?.let { bad ->
        val retryAfter = bad.headerFields.entries
            .firstOrNull { it.key.equals("Retry-After", ignoreCase = true) }?.value?.firstOrNull()
        return PlaybackFailure.Host(
            GanderError.fromResponse(bad.responseCode, bad.responseBody.toString(Charsets.UTF_8), retryAfter),
        )
    }
    if (errorCode in NETWORK_ERROR_CODES) {
        val io = chain.firstOrNull() ?: return PlaybackFailure.Other(errorCode)
        return PlaybackFailure.Host(GanderError.fromThrowable(io))
    }
    return PlaybackFailure.Other(errorCode)
}

private const val MAX_CAUSE_DEPTH = 16

/** Media3's `ERROR_CODE_*` values (they are part of its public, stable numbering). */
private val CODEC_ERROR_CODES = setOf(
    PlaybackException.ERROR_CODE_DECODER_INIT_FAILED,
    PlaybackException.ERROR_CODE_DECODER_QUERY_FAILED,
    PlaybackException.ERROR_CODE_DECODING_FAILED,
    PlaybackException.ERROR_CODE_DECODING_FORMAT_EXCEEDS_CAPABILITIES,
    PlaybackException.ERROR_CODE_DECODING_FORMAT_UNSUPPORTED,
    PlaybackException.ERROR_CODE_PARSING_CONTAINER_UNSUPPORTED,
)

private val NETWORK_ERROR_CODES = setOf(
    PlaybackException.ERROR_CODE_IO_NETWORK_CONNECTION_FAILED,
    PlaybackException.ERROR_CODE_IO_NETWORK_CONNECTION_TIMEOUT,
    PlaybackException.ERROR_CODE_IO_UNSPECIFIED,
)
