package fyi.arlo.gander.ui.fetch

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import fyi.arlo.gander.R
import fyi.arlo.gander.ui.browser.formatFileSize
import fyi.arlo.gander.ui.common.text
import fyi.arlo.gander.ui.components.Banner
import fyi.arlo.gander.ui.components.BannerTone
import fyi.arlo.gander.ui.components.ButtonKind
import fyi.arlo.gander.ui.components.GanderButton
import fyi.arlo.gander.ui.components.GanderProgress
import fyi.arlo.gander.ui.theme.GanderIcons
import fyi.arlo.gander.ui.theme.GanderTheme

object ZipTags {
    const val DIALOG = "zip-dialog"
    const val CONFIRM = "zip-confirm"
    const val RETRY = "zip-retry"
}

/**
 * "Download folder as zip" (WP-68): the explicit step before a folder download. It shows the
 * folder, how many items and how many bytes the computer measured (`GET /v1/zip?dry_run=1`), what
 * will be left out and what will be slow, and only then offers Download. A folder over the host's
 * limit says so in the host's own words and offers no download.
 */
@Composable
fun ZipDialog(
    prompt: ZipPrompt,
    onConfirm: () -> Unit,
    onDismiss: () -> Unit,
    onRetry: () -> Unit,
) {
    val c = GanderTheme.colors
    val name = prompt.folder.entry.name.ifEmpty { stringResource(R.string.zip_this_folder) }
    AlertDialog(
        modifier = Modifier.testTag(ZipTags.DIALOG),
        onDismissRequest = onDismiss,
        containerColor = c.surface2,
        icon = { Icon(GanderIcons.Archive, contentDescription = null, tint = c.text2, modifier = Modifier.size(32.dp)) },
        title = { Text(stringResource(R.string.zip_title)) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                when (prompt) {
                    is ZipPrompt.Measuring -> {
                        Text(stringResource(R.string.zip_measuring, name), color = c.text2)
                        GanderProgress(fraction = null, modifier = Modifier.padding(top = 4.dp))
                    }
                    is ZipPrompt.Ready -> {
                        val plan = prompt.plan
                        Text(
                            pluralStringResource(
                                R.plurals.zip_summary,
                                plan.entries.toInt().coerceAtLeast(0),
                                name,
                                plan.entries,
                                formatFileSize(plan.bytes),
                            ),
                            color = c.text,
                        )
                        Text(stringResource(R.string.zip_how), color = c.text2)
                        if (plan.cloudOnly > 0) {
                            Banner(
                                tone = BannerTone.WARN,
                                icon = GanderIcons.Cloud,
                                text = pluralStringResource(R.plurals.zip_cloud_only, plan.cloudOnly.toInt(), plan.cloudOnly),
                            )
                        }
                        if (plan.skipped > 0) {
                            Banner(
                                tone = BannerTone.INFO,
                                text = pluralStringResource(R.plurals.zip_skipped, plan.skipped.toInt(), plan.skipped),
                            )
                        }
                    }
                    is ZipPrompt.TooLarge -> Banner(
                        tone = BannerTone.BAD,
                        title = stringResource(R.string.zip_too_large_title),
                        text = prompt.message.ifBlank { stringResource(R.string.zip_too_large_body) },
                    )
                    is ZipPrompt.Failed -> Banner(
                        tone = BannerTone.BAD,
                        text = prompt.error.text(),
                    )
                }
            }
        },
        confirmButton = {
            when (prompt) {
                is ZipPrompt.Ready -> GanderButton(
                    text = stringResource(R.string.zip_confirm, formatFileSize(prompt.plan.bytes)),
                    onClick = onConfirm,
                    icon = GanderIcons.Fetch,
                    modifier = Modifier.testTag(ZipTags.CONFIRM),
                )
                is ZipPrompt.Failed -> GanderButton(
                    text = stringResource(R.string.action_retry),
                    onClick = onRetry,
                    kind = ButtonKind.TONAL,
                    modifier = Modifier.testTag(ZipTags.RETRY),
                )
                is ZipPrompt.Measuring, is ZipPrompt.TooLarge -> Unit
            }
        },
        dismissButton = {
            GanderButton(
                text = stringResource(if (prompt is ZipPrompt.TooLarge) R.string.zip_close else R.string.fetch_cancel),
                onClick = onDismiss,
                kind = ButtonKind.TEXT,
            )
        },
    )
}
