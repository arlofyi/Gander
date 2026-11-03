package fyi.arlo.gander.core.data.preview

import fyi.arlo.gander.core.data.UiError
import fyi.arlo.gander.core.data.actions.ActionReason
import fyi.arlo.gander.core.data.actions.FileAction
import fyi.arlo.gander.core.data.actions.extension
import fyi.arlo.gander.core.data.browse.EntryTarget
import fyi.arlo.gander.core.data.toUiError
import fyi.arlo.gander.core.net.GanderError

/** Why a preview viewer shows an explanation instead of the file (client spec: say why, never a generic failure). */
sealed interface PreviewFailure {
    /** Trying again can change the answer. */
    val canRetry: Boolean

    /** Anything the shared error text explains: offline, revoked, wrong computer, no permission... */
    data class Host(val error: UiError) : PreviewFailure {
        override val canRetry: Boolean get() = true
    }

    /** `415 unsupported_type`: no renderer for this file, or a "text" file that is binary. */
    data object Unsupported : PreviewFailure {
        override val canRetry: Boolean get() = false
    }

    /** `413 too_large`: the preview is over the host's limit. */
    data object TooLarge : PreviewFailure {
        override val canRetry: Boolean get() = false
    }

    /** `503 generating` for longer than the retry budget: the computer is still busy rendering. */
    data object StillGenerating : PreviewFailure {
        override val canRetry: Boolean get() = true
    }

    /** The phone has no decoder for this video's format (client spec: "HEVC/AV1 depend on the device"). */
    data object CodecUnsupported : PreviewFailure {
        override val canRetry: Boolean get() = false
    }
}

fun GanderError.toPreviewFailure(): PreviewFailure = when (this) {
    is GanderError.Host.UnsupportedType -> PreviewFailure.Unsupported
    is GanderError.Host.TooLarge -> PreviewFailure.TooLarge
    is GanderError.Host.Generating -> PreviewFailure.StillGenerating
    else -> PreviewFailure.Host(toUiError())
}

/**
 * Whether [error] belongs in the file sheet as the reason Preview is off. Only lasting causes do:
 * the sheet keeps a reported refusal until its own Retry, so an outage or a busy renderer must not
 * be left on the button.
 */
fun shouldReportRefusal(target: EntryTarget, error: GanderError): Boolean = when (error) {
    // Busy and glitch answers say nothing about this file; the viewer's own Retry covers them.
    is GanderError.Host.Generating,
    is GanderError.Host.TooManyRequests,
    is GanderError.Host.Internal,
    -> false
    else -> !ActionReason.fromError(FileAction.PREVIEW, error, target.entry.extension()).transient
}

/**
 * A Coil failure as a [GanderError]: the fetcher throws [GanderError]s, which pass through; anything
 * else (a picture the decoder refuses) means the computer sent something unusable.
 */
fun Throwable.toPreviewError(): GanderError = this as? GanderError ?: GanderError.MalformedResponse(this)
