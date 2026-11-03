package fyi.arlo.gander.ui.common

import android.text.format.DateUtils
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import fyi.arlo.gander.R
import fyi.arlo.gander.core.data.UiError
import fyi.arlo.gander.core.data.UiErrorKind

/** The sentence that explains [this] (client spec honesty rules), with the host's detail when it has one. */
@Composable
fun UiError.text(): String {
    val base = stringResource(
        when (kind) {
            UiErrorKind.OFFLINE -> R.string.error_offline
            UiErrorKind.WRONG_COMPUTER -> R.string.error_wrong_computer
            UiErrorKind.SECURE_CONNECTION -> R.string.error_secure_connection
            UiErrorKind.REVOKED -> R.string.error_revoked
            UiErrorKind.NO_PERMISSION -> R.string.error_no_permission
            UiErrorKind.IDENTITY_MISMATCH -> R.string.error_identity_mismatch
            UiErrorKind.NOT_PAIRED -> R.string.error_not_paired
            UiErrorKind.SHARE_GONE -> R.string.error_share_gone
            UiErrorKind.NOT_FOUND -> R.string.error_not_found
            UiErrorKind.BUSY -> R.string.error_busy
            UiErrorKind.INCOMPATIBLE -> R.string.error_incompatible
            UiErrorKind.OTHER -> R.string.error_other
        },
    )
    // permission_denied names the missing permission in the host's message; other details are
    // shown too, they are English and path-free by protocol.
    val detail = detail
    return if (detail.isNullOrBlank()) base else stringResource(R.string.error_with_detail, base, detail)
}

/** Full-area explanation of a failure, with Retry when retrying can help. */
@Composable
fun ErrorPanel(error: UiError, onRetry: (() -> Unit)?, modifier: Modifier = Modifier) {
    Column(
        modifier = modifier.padding(32.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp, Alignment.CenterVertically),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text(
            text = error.text(),
            style = MaterialTheme.typography.bodyLarge,
            textAlign = TextAlign.Center,
        )
        if (onRetry != null) {
            TextButton(onClick = onRetry) { Text(stringResource(R.string.action_retry)) }
        }
    }
}

/** "3 hours ago" for a wall-clock [millis]. */
@Composable
fun relativeTimeText(millis: Long): String = remember(millis) {
    DateUtils.getRelativeTimeSpanString(
        millis,
        System.currentTimeMillis(),
        DateUtils.MINUTE_IN_MILLIS,
        DateUtils.FORMAT_ABBREV_RELATIVE,
    ).toString()
}
