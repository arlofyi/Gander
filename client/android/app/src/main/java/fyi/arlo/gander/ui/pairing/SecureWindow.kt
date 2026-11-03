package fyi.arlo.gander.ui.pairing

import android.app.Activity
import android.content.Context
import android.content.ContextWrapper
import android.view.Window
import android.view.WindowManager
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.ui.platform.LocalContext

/**
 * Sets `FLAG_SECURE` on the activity window while [enabled] is true and the composable is in the
 * composition: no screenshots, no screen recording, and a blank thumbnail in the recents list.
 * Used while the pairing QR code or the SAS is visible (client spec, "Security on the device").
 *
 * The flag is removed again when [enabled] turns false or the composable leaves, but only if this
 * effect was the one that set it, so a flag set by anything else stays.
 */
@Composable
fun SecureWindowEffect(enabled: Boolean) {
    val context = LocalContext.current
    DisposableEffect(context, enabled) {
        val window: Window? = context.findActivity()?.window
        val added = enabled && window != null &&
            (window.attributes.flags and WindowManager.LayoutParams.FLAG_SECURE) == 0
        if (added) window.addFlags(WindowManager.LayoutParams.FLAG_SECURE)
        onDispose {
            if (added) window.clearFlags(WindowManager.LayoutParams.FLAG_SECURE)
        }
    }
}

internal tailrec fun Context.findActivity(): Activity? = when (this) {
    is Activity -> this
    is ContextWrapper -> baseContext.findActivity()
    else -> null
}
