package fyi.arlo.gander.ui.preview

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import fyi.arlo.gander.R
import fyi.arlo.gander.core.data.actions.FileAction
import fyi.arlo.gander.core.data.preview.PreviewFailure
import fyi.arlo.gander.ui.common.text
import fyi.arlo.gander.ui.components.ButtonKind
import fyi.arlo.gander.ui.components.GanderButton
import fyi.arlo.gander.ui.components.GanderIconButton
import fyi.arlo.gander.ui.components.GanderProgress
import fyi.arlo.gander.ui.components.GanderTopBar
import fyi.arlo.gander.ui.components.WorkRail
import fyi.arlo.gander.ui.theme.GanderIcons
import fyi.arlo.gander.ui.theme.GanderTheme
import fyi.arlo.gander.ui.theme.Immersive

/** Test tags of the preview viewers. */
object PreviewTags {
    const val PAGER = "preview-pager"
    const val PAGE_COUNTER = "preview-page-counter"
    const val TEXT_LIST = "preview-text-list"
    const val TEXT_ENCODING = "preview-text-encoding"
    const val TEXT_LOAD_MORE = "preview-text-load-more"
    const val VIDEO_PLAY = "preview-video-play"
    const val VIDEO_PLAYER = "preview-video-player"
    const val FAILURE = "preview-failure"
    const val FETCH = "preview-fetch"

    /** Present on the image of 1-based page [page] only once it has loaded. */
    fun pageLoaded(page: Int) = "preview-page-$page"

    /** Present on the single image of the image viewer only once it has loaded. */
    const val IMAGE_LOADED = "preview-image"

    /** Present on the poster of the video viewer only once it has loaded. */
    const val POSTER_LOADED = "preview-poster"
}

/**
 * The viewers' frame (mockups 19 to 22): a top bar with Back, the file name, an optional
 * [subtitle] and the "Fetch this file" shortcut; a [footer] (usually [PreviewRail]) under the
 * content. [immersive] viewers (pages, image, video) are always dark so the content stands out;
 * the text viewer follows the app theme.
 */
@Composable
fun PreviewScaffold(
    title: String,
    onBack: () -> Unit,
    onFetch: (() -> Unit)?,
    modifier: Modifier = Modifier,
    subtitle: String? = null,
    immersive: Boolean = true,
    footer: (@Composable () -> Unit)? = null,
    content: @Composable (PaddingValues) -> Unit,
) {
    val body: @Composable () -> Unit = {
        val c = GanderTheme.colors
        val contentColor = if (immersive) Immersive.Content else c.text
        Scaffold(
            modifier = modifier,
            containerColor = if (immersive) Immersive.Background else c.bg,
            contentColor = contentColor,
            topBar = {
                GanderTopBar(
                    title = title,
                    subtitle = subtitle,
                    navigationIcon = GanderIcons.Back,
                    navigationDescription = stringResource(R.string.action_back),
                    onNavigate = onBack,
                    contentColor = contentColor,
                    subtitleColor = if (immersive) Immersive.ContentMuted else c.text3,
                    modifier = Modifier.statusBarsPadding(),
                ) {
                    if (onFetch != null) {
                        GanderIconButton(
                            icon = GanderIcons.Fetch,
                            contentDescription = stringResource(R.string.preview_fetch),
                            onClick = onFetch,
                            tint = contentColor,
                            modifier = Modifier.testTag(PreviewTags.FETCH),
                        )
                    }
                }
            },
            bottomBar = {
                if (footer != null) {
                    Column(Modifier.navigationBarsPadding()) { footer() }
                }
            },
            content = content,
        )
    }
    // The immersive viewers use the dark tokens whatever the system theme (progress, panels, text).
    if (immersive && !GanderTheme.colors.isDark) GanderTheme(darkTheme = true) { body() } else body()
}

/** The footer line of a viewer: the work rail with where the work happened ("Pages rendered on studio-pc"). */
@Composable
fun PreviewRail(caption: String, modifier: Modifier = Modifier, trailing: String? = null) {
    androidx.compose.foundation.layout.Row(
        modifier = modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        WorkRail(
            mode = FileAction.PREVIEW,
            caption = caption,
            captionColor = GanderTheme.colors.text,
            modifier = Modifier.weight(1f),
        )
        if (trailing != null) {
            Text(trailing, style = MaterialTheme.typography.bodySmall, color = GanderTheme.colors.text3)
        }
    }
}

/** The sentence that explains [this]. */
@Composable
fun PreviewFailure.message(): String = when (this) {
    is PreviewFailure.Host -> error.text()
    PreviewFailure.Unsupported -> stringResource(R.string.preview_failure_unsupported)
    PreviewFailure.TooLarge -> stringResource(R.string.preview_failure_too_large)
    PreviewFailure.StillGenerating -> stringResource(R.string.preview_failure_still_generating)
    PreviewFailure.CodecUnsupported -> stringResource(R.string.preview_failure_codec)
}

/** Why there is nothing to show, with Retry when trying again can help and Fetch as the way out. */
@Composable
fun PreviewFailurePanel(
    failure: PreviewFailure,
    onRetry: (() -> Unit)?,
    onFetch: (() -> Unit)?,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier
            .widthIn(max = 420.dp)
            .padding(24.dp)
            .testTag(PreviewTags.FAILURE),
        verticalArrangement = Arrangement.spacedBy(14.dp, Alignment.CenterVertically),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        androidx.compose.material3.Icon(GanderIcons.Info, contentDescription = null, tint = GanderTheme.colors.text2)
        Text(
            text = failure.message(),
            style = MaterialTheme.typography.bodyLarge,
            color = GanderTheme.colors.text,
            textAlign = TextAlign.Center,
        )
        if (failure.canRetry && onRetry != null) {
            GanderButton(stringResource(R.string.action_retry), onClick = onRetry, kind = ButtonKind.TONAL, icon = GanderIcons.Refresh)
        }
        if (!failure.canRetry && onFetch != null) {
            GanderButton(stringResource(R.string.preview_fetch), onClick = onFetch, kind = ButtonKind.TONAL, icon = GanderIcons.Fetch)
        }
    }
}

/** A progress bar with one line saying what is going on (the computer rendering, asking). */
@Composable
fun PreviewProgress(text: String?, modifier: Modifier = Modifier) {
    Column(
        modifier = modifier
            .widthIn(max = 320.dp)
            .padding(24.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp, Alignment.CenterVertically),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        GanderProgress(fraction = null)
        if (text != null) {
            Text(
                text = text,
                style = MaterialTheme.typography.bodyMedium,
                color = GanderTheme.colors.text2,
                textAlign = TextAlign.Center,
            )
        }
    }
}
