package fyi.arlo.gander.spike.files

import android.util.Log
import java.io.File
import java.util.Properties

const val TAG = "GanderSpike"
const val AUTHORITY = "fyi.arlo.gander.spike.files.documents"

/** One log line, key=value formatted so the analysis script can parse it. */
fun spikeLog(line: String) {
    Log.i(TAG, line)
}

/**
 * Experiment knobs, re-read from `<externalFilesDir>/spike.conf` on every document open and every
 * HTTP request so a run can change them with `adb push` and no restart.
 *
 *  - delay_ms:    server sleeps this long before answering a GET whose range starts at or after delay_from
 *  - delay_from:  byte offset where the artificial delay starts (default 0)
 *  - readahead:   bytes fetched per HTTP request when the callback runs a one-block read-ahead
 *                 (0 = off: one HTTP request per onRead, the raw pattern)
 *  - handler:     "shared" (one HandlerThread for every open fd, default) or "perfd" (a new HandlerThread per fd)
 */
data class SpikeConf(
    val delayMs: Long = 0,
    val delayFrom: Long = 0,
    val readAhead: Int = 0,
    val perFdHandler: Boolean = false,
) {
    companion object {
        fun load(dir: File?): SpikeConf {
            val f = File(dir, "spike.conf")
            if (dir == null || !f.isFile) return SpikeConf()
            val p = Properties()
            f.inputStream().use { p.load(it) }
            return SpikeConf(
                delayMs = p.getProperty("delay_ms", "0").trim().toLong(),
                delayFrom = p.getProperty("delay_from", "0").trim().toLong(),
                readAhead = p.getProperty("readahead", "0").trim().toInt(),
                perFdHandler = p.getProperty("handler", "shared").trim() == "perfd",
            )
        }
    }
}
