package fyi.arlo.gander.ui.fetch

import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat
import fyi.arlo.gander.appGraph
import fyi.arlo.gander.core.data.fetch.FetchJob
import fyi.arlo.gander.core.data.fetch.FetchStatus
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.conflate
import kotlinx.coroutines.launch

/**
 * Foreground service of type `dataSync` that keeps the process alive while the fetch engine has
 * active jobs (client spec: "foreground service type dataSync; survives onTimeout by persisting
 * state and resuming on next launch"). The downloads themselves run in the engine's scope; this
 * service shows the progress notification, posts a result per finished file, and stops itself
 * when nothing is active.
 *
 * Android 15+ (API 35) limits `dataSync` services to 6 hours a day and calls [onTimeout]; Android
 * 14 (API 34) has the single-argument variant. Both pause every job (progress is already
 * persisted) and stop the service at once; [FetchLaunch] resumes paused jobs on the next app
 * launch. API 26-33 have no limit and never call it.
 */
class FetchService : Service() {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private var observer: Job? = null
    private var timedOut = false

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        FetchNotifications.ensureChannels(this)
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (timedOut || !enterForeground()) {
            stopSelf()
            return START_NOT_STICKY
        }
        if (observer == null) observer = scope.launch { observe() }
        return START_NOT_STICKY
    }

    private fun enterForeground(): Boolean = try {
        val type = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC else 0
        ServiceCompat.startForeground(this, FetchNotifications.PROGRESS_ID, FetchNotifications.progress(this, emptyList()), type)
        true
    } catch (_: IllegalStateException) {
        // ForegroundServiceStartNotAllowedException (background start, or the daily dataSync time
        // used up). The engine keeps going while the app is visible; jobs stay persisted.
        false
    } catch (_: SecurityException) {
        false
    }

    private suspend fun observe() {
        val engine = applicationContext.appGraph.fetchEngine
        var previous: Map<Long, FetchStatus> = emptyMap()
        engine.jobs.conflate().collect { jobs ->
            announceResults(previous, jobs)
            previous = jobs.associate { it.id to it.status }
            val active = jobs.filter { it.status.active }
            if (active.isEmpty()) {
                ServiceCompat.stopForeground(this, ServiceCompat.STOP_FOREGROUND_REMOVE)
                stopSelf()
                return@collect
            }
            FetchNotifications.post(this, FetchNotifications.PROGRESS_ID, FetchNotifications.progress(this, active))
            delay(NOTIFICATION_INTERVAL_MS) // the platform rate-limits updates; once a second is plenty
        }
    }

    private fun announceResults(previous: Map<Long, FetchStatus>, jobs: List<FetchJob>) {
        for (job in jobs) {
            val before = previous[job.id] ?: continue
            if (before == job.status || !before.active) continue
            when (job.status) {
                FetchStatus.DONE -> FetchNotifications.post(this, FetchNotifications.resultId(job), FetchNotifications.result(this, job, ok = true))
                FetchStatus.FAILED -> FetchNotifications.post(this, FetchNotifications.resultId(job), FetchNotifications.result(this, job, ok = false))
                else -> Unit
            }
        }
    }

    @Deprecated("API 35 adds onTimeout(Int, Int); this is the API 34 callback")
    override fun onTimeout(startId: Int) = handleTimeout()

    override fun onTimeout(startId: Int, fgsType: Int) = handleTimeout()

    /** Must stop within seconds: pause in the engine's own scope, stop the service right away. */
    private fun handleTimeout() {
        timedOut = true
        observer?.cancel()
        applicationContext.appGraph.fetchEngine.pauseAllInBackground()
        ServiceCompat.stopForeground(this, ServiceCompat.STOP_FOREGROUND_REMOVE)
        FetchNotifications.post(this, FetchNotifications.PROGRESS_ID + 1, FetchNotifications.paused(this))
        stopSelf()
    }

    override fun onDestroy() {
        scope.cancel()
        super.onDestroy()
    }

    companion object {
        private const val NOTIFICATION_INTERVAL_MS = 1_000L

        /**
         * Asks for the service. Allowed while the app is visible (every fetch starts from a tap);
         * refused in the background on Android 12+, which is fine: the engine still runs and
         * the jobs are persisted for the next launch.
         */
        fun start(context: Context) {
            try {
                ContextCompat.startForegroundService(context, Intent(context, FetchService::class.java))
            } catch (_: IllegalStateException) {
                // Background start not allowed; see above.
            } catch (_: SecurityException) {
                // Missing FOREGROUND_SERVICE_DATA_SYNC would be a build error; never expected.
            }
        }
    }
}
