package fyi.arlo.gander.ui.fetch

import android.Manifest
import android.annotation.SuppressLint
import android.app.Notification
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.app.NotificationChannelCompat
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat
import fyi.arlo.gander.MainActivity
import fyi.arlo.gander.R
import fyi.arlo.gander.core.data.fetch.FetchJob
import fyi.arlo.gander.ui.browser.formatFileSize

/**
 * The fetch notifications (client spec: "progress in the sheet and a notification"). One ongoing
 * progress notification belongs to the foreground service; finished and failed files get their
 * own notification each. Tapping any of them opens the app with the fetch panel shown.
 * Names of files appear (the user's own content); nothing else about the host is shown.
 */
object FetchNotifications {
    const val CHANNEL_PROGRESS = "fetch_progress"
    const val CHANNEL_RESULT = "fetch_result"
    const val PROGRESS_ID = 2701
    private const val RESULT_ID_BASE = 2800

    /** Intent extra that makes the app show the fetch panel. */
    const val EXTRA_SHOW_FETCHES = "fyi.arlo.gander.extra.SHOW_FETCHES"

    fun ensureChannels(context: Context) {
        val manager = NotificationManagerCompat.from(context)
        manager.createNotificationChannel(
            NotificationChannelCompat.Builder(CHANNEL_PROGRESS, NotificationManagerCompat.IMPORTANCE_LOW)
                .setName(context.getString(R.string.fetch_channel_progress))
                .setShowBadge(false)
                .build(),
        )
        manager.createNotificationChannel(
            NotificationChannelCompat.Builder(CHANNEL_RESULT, NotificationManagerCompat.IMPORTANCE_DEFAULT)
                .setName(context.getString(R.string.fetch_channel_result))
                .build(),
        )
    }

    private fun openPanel(context: Context): PendingIntent {
        val intent = Intent(context, MainActivity::class.java)
            .putExtra(EXTRA_SHOW_FETCHES, true)
            .addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP)
        return PendingIntent.getActivity(
            context,
            0,
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
    }

    /** The service's ongoing notification for [active] jobs (may be empty while starting). */
    fun progress(context: Context, active: List<FetchJob>): Notification {
        val builder = NotificationCompat.Builder(context, CHANNEL_PROGRESS)
            .setSmallIcon(R.drawable.ic_stat_gander)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setSilent(true)
            .setCategory(NotificationCompat.CATEGORY_PROGRESS)
            .setForegroundServiceBehavior(NotificationCompat.FOREGROUND_SERVICE_IMMEDIATE)
            .setContentIntent(openPanel(context))
        if (active.isEmpty()) {
            return builder.setContentTitle(context.getString(R.string.fetch_notification_starting))
                .setProgress(0, 0, true)
                .build()
        }
        val title = if (active.size == 1) {
            context.getString(R.string.fetch_notification_one, active[0].name)
        } else {
            context.resources.getQuantityString(R.plurals.fetch_notification_many, active.size, active.size)
        }
        val done = active.sumOf { it.bytesDone }
        val total = active.sumOf { it.size.coerceAtLeast(0) }
        val text = context.getString(R.string.fetch_progress_of, formatFileSize(done), formatFileSize(total))
        val permille = if (total > 0) ((done * 1000) / total).toInt().coerceIn(0, 1000) else 0
        return builder.setContentTitle(title)
            .setContentText(text)
            .setProgress(1000, permille, total <= 0)
            .build()
    }

    /** A file finished (or failed); [ok] picks the sentence. */
    fun result(context: Context, job: FetchJob, ok: Boolean): Notification =
        NotificationCompat.Builder(context, CHANNEL_RESULT)
            .setSmallIcon(R.drawable.ic_stat_gander)
            .setContentTitle(
                context.getString(if (ok) R.string.fetch_notification_done else R.string.fetch_notification_failed, job.name),
            )
            .setContentText(context.getString(if (ok) R.string.fetch_notification_done_text else R.string.fetch_notification_failed_text))
            .setAutoCancel(true)
            .setContentIntent(openPanel(context))
            .build()

    fun paused(context: Context): Notification =
        NotificationCompat.Builder(context, CHANNEL_RESULT)
            .setSmallIcon(R.drawable.ic_stat_gander)
            .setContentTitle(context.getString(R.string.fetch_notification_paused))
            .setContentText(context.getString(R.string.fetch_notification_paused_text))
            .setAutoCancel(true)
            .setContentIntent(openPanel(context))
            .build()

    fun canPost(context: Context): Boolean {
        val granted = Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU ||
            ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) ==
            PackageManager.PERMISSION_GRANTED
        return granted && NotificationManagerCompat.from(context).areNotificationsEnabled()
    }

    @SuppressLint("MissingPermission") // checked by canPost
    fun post(context: Context, id: Int, notification: Notification) {
        if (canPost(context)) NotificationManagerCompat.from(context).notify(id, notification)
    }

    fun resultId(job: FetchJob): Int = RESULT_ID_BASE + (job.id % 10_000).toInt()
}
