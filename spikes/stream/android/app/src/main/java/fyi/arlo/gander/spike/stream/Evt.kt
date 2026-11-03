package fyi.arlo.gander.spike.stream

import android.util.Log
import org.json.JSONObject

/**
 * One structured log line per event: `GanderSpike {"ev":"...","t_ns":...,...}`.
 *
 * The host-side driver (`scripts/Invoke-StreamSpike.ps1`) captures logcat and parses these lines.
 * `t_ns` is `System.nanoTime()` on the phone, the clock every phone-side latency uses.
 */
object Evt {
    const val TAG = "GanderSpike"

    fun log(ev: String, vararg kv: Pair<String, Any?>) {
        val o = JSONObject()
        o.put("ev", ev)
        o.put("t_ns", System.nanoTime())
        for ((k, v) in kv) {
            val safe: Any? = when (v) {
                is Double -> if (v.isNaN() || v.isInfinite()) null else v
                is Float -> if (v.isNaN() || v.isInfinite()) null else v
                else -> v
            }
            if (safe != null) o.put(k, safe)
        }
        Log.i(TAG, o.toString())
    }
}
