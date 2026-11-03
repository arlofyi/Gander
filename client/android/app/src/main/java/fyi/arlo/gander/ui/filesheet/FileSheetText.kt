package fyi.arlo.gander.ui.filesheet

import androidx.compose.runtime.Composable
import androidx.compose.ui.res.stringResource
import fyi.arlo.gander.R
import fyi.arlo.gander.core.data.actions.ActionHint
import fyi.arlo.gander.core.data.actions.ActionReason
import fyi.arlo.gander.core.data.actions.ActionWarning
import fyi.arlo.gander.core.data.actions.FileAction
import fyi.arlo.gander.core.net.protocol.Permission
import fyi.arlo.gander.core.net.protocol.PreviewKind
import fyi.arlo.gander.ui.browser.formatFileSize
import fyi.arlo.gander.ui.common.text

// The sentences of the file sheet. The rule table produces typed reasons, hints and warnings; each
// `when` here is exhaustive, so adding a case there fails the build until it has a sentence.
// [host] is the computer's display name ("studio-pc").

@Composable
fun FileAction.label(): String = stringResource(
    when (this) {
        FileAction.PREVIEW -> R.string.sheet_action_preview
        FileAction.FETCH -> R.string.sheet_action_fetch
        FileAction.STREAM -> R.string.sheet_action_stream
    },
)

@Composable
fun ActionReason.text(host: String): String = when (this) {
    ActionReason.NotAFile -> stringResource(R.string.reason_not_a_file)
    is ActionReason.NoPermission -> {
        val base = stringResource(
            when (permission) {
                Permission.PREVIEW -> R.string.reason_no_permission_preview
                Permission.STREAM -> R.string.reason_no_permission_stream
                else -> R.string.reason_no_permission_fetch
            },
        )
        if (hostDetail.isNullOrBlank()) base else stringResource(R.string.error_with_detail, base, hostDetail)
    }
    is ActionReason.NoRenderer ->
        if (ext == null) {
            stringResource(R.string.reason_no_renderer, host)
        } else {
            stringResource(R.string.reason_no_renderer_ext, ".$ext", host)
        }
    is ActionReason.HostCannotRender -> stringResource(
        R.string.reason_cannot_render,
        host,
        stringResource(
            when (kind) {
                PreviewKind.IMAGE -> R.string.reason_kind_image
                PreviewKind.PDF -> R.string.reason_kind_pdf
                PreviewKind.TEXT -> R.string.reason_kind_text
                PreviewKind.OFFICE -> R.string.reason_kind_office
                PreviewKind.VIDEO_POSTER -> R.string.reason_kind_video
                PreviewKind.NONE, PreviewKind.UNKNOWN -> R.string.reason_kind_other
            },
        ),
    )
    is ActionReason.PreviewsOffOnHost -> stringResource(
        if (renderWorkerMissing) R.string.reason_previews_off_worker else R.string.reason_previews_off,
        host,
    )
    ActionReason.PreviewTooLarge -> stringResource(R.string.reason_preview_too_large, host)
    ActionReason.StreamNotEnabled -> stringResource(R.string.reason_stream_not_enabled, host)
    ActionReason.StreamRefusedByPolicy -> stringResource(R.string.reason_stream_policy, host)
    is ActionReason.StreamRefused ->
        if (hostMessage.isBlank()) {
            stringResource(R.string.reason_stream_policy, host)
        } else {
            stringResource(R.string.reason_stream_refused, host, hostMessage)
        }
    ActionReason.StreamUnavailableNow -> stringResource(R.string.reason_stream_unavailable_now, host)
    ActionReason.HostLocked -> stringResource(R.string.reason_host_locked, host)
    ActionReason.HostUnattended -> stringResource(R.string.reason_host_unattended, host)
    ActionReason.AppUnavailable -> stringResource(R.string.reason_app_unavailable, host)
    ActionReason.StreamBusy -> stringResource(R.string.reason_stream_busy, host)
    ActionReason.PhoneOffline -> stringResource(R.string.reason_phone_offline)
    ActionReason.HostUnreachable -> stringResource(R.string.reason_host_unreachable, host)
    ActionReason.FileGone -> stringResource(R.string.reason_file_gone, host)
    ActionReason.IncompatibleHost -> stringResource(R.string.error_incompatible)
    is ActionReason.HostError -> error.text()
}

@Composable
fun ActionHint.text(host: String): String = when (this) {
    is ActionHint.Preview -> when (kind) {
        PreviewKind.PDF, PreviewKind.OFFICE -> stringResource(R.string.hint_preview_pages, host)
        PreviewKind.IMAGE -> stringResource(R.string.hint_preview_image, host)
        PreviewKind.TEXT -> stringResource(R.string.hint_preview_text, host)
        PreviewKind.VIDEO_POSTER -> stringResource(R.string.hint_preview_video, host)
        PreviewKind.NONE, PreviewKind.UNKNOWN -> stringResource(R.string.hint_preview_pages, host)
    }
    is ActionHint.Fetch ->
        if (metered) {
            stringResource(R.string.hint_fetch_metered, formatFileSize(sizeBytes))
        } else {
            stringResource(R.string.hint_fetch_unmetered, formatFileSize(sizeBytes))
        }
    ActionHint.SavedCopy -> stringResource(R.string.hint_saved_copy)
    ActionHint.Stream -> stringResource(R.string.hint_stream, host)
}

@Composable
fun ActionWarning.text(host: String): String = when (this) {
    is ActionWarning.CellularLarge -> stringResource(R.string.warn_cellular_large)
    ActionWarning.CloudOnly -> stringResource(R.string.entry_flag_cloud)
    ActionWarning.NoResume -> stringResource(R.string.warn_no_resume, host)
    ActionWarning.LowFidelityPreview -> stringResource(R.string.warn_low_fidelity, host)
    ActionWarning.RelayedConnection -> stringResource(R.string.warn_relayed)
    ActionWarning.OwnerApproval -> stringResource(R.string.warn_owner_approval, host)
}
