package fyi.arlo.gander.spike.files

import android.app.Activity
import android.os.Bundle
import android.os.SystemClock
import android.provider.DocumentsContract
import java.io.FileInputStream
import java.nio.ByteBuffer
import java.util.Random

/**
 * Controlled read client. Runs a plan (comma separated steps) against one document through the
 * ContentResolver, so the provider sees exactly the client behaviour we choose. Each step opens its
 * own file descriptor. Same-app access: the shell is denied by MANAGE_DOCUMENTS, this activity is not.
 *
 *   seq:<buf>[:<startOffset>[:<maxBytes>]]  sequential read() with a <buf>-byte buffer
 *   rand:<buf>:<count>[:<seed>]             <count> preads of <buf> bytes at random offsets
 *   sleep:<ms>                              pause (no fd) so a parallel plan can get ahead
 *
 * Extras: doc (default sample-faststart.mp4), plan, plan2 (runs on a second thread in parallel).
 */
class ProbeActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val doc = intent.getStringExtra("doc") ?: "sample-faststart.mp4"
        val plans = listOfNotNull(intent.getStringExtra("plan") ?: "seq:65536", intent.getStringExtra("plan2"))
        val threads = plans.mapIndexed { i, plan ->
            Thread({ run("t$i", doc, plan) }, "probe-$i").also { it.start() }
        }
        Thread({
            threads.forEach { it.join() }
            spikeLog("PROBE_ALL_DONE")
            runOnUiThread { finish() }
        }, "probe-join").start()
    }

    private fun run(label: String, doc: String, plan: String) {
        val uri = DocumentsContract.buildDocumentUri(AUTHORITY, doc)
        for (step in plan.split(',')) {
            val p = step.trim().split(':')
            val t0 = SystemClock.elapsedRealtimeNanos()
            spikeLog("PROBE_BEGIN label=$label step=$step doc=$doc")
            if (p[0] == "sleep") {
                Thread.sleep(p[1].toLong())
                continue
            }
            try {
                contentResolver.openFileDescriptor(uri, "r")!!.use { pfd ->
                    FileInputStream(pfd.fileDescriptor).use { ins ->
                        val ch = ins.channel
                        var bytes = 0L
                        var calls = 0
                        when (p[0]) {
                            "seq" -> {
                                val buf = ByteArray(p[1].toInt())
                                val start = p.getOrNull(2)?.toLong() ?: 0L
                                val max = p.getOrNull(3)?.toLong() ?: Long.MAX_VALUE
                                if (start > 0) ch.position(start)
                                while (bytes < max) {
                                    val n = ins.read(buf)
                                    if (n < 0) break
                                    bytes += n
                                    calls++
                                }
                            }
                            "rand" -> {
                                val size = p[1].toInt()
                                val count = p[2].toInt()
                                val rnd = Random(p.getOrNull(3)?.toLong() ?: 42L)
                                val len = ch.size()
                                val bb = ByteBuffer.allocate(size)
                                repeat(count) {
                                    bb.clear()
                                    val pos = (rnd.nextDouble() * (len - size)).toLong()
                                    val n = ch.read(bb, pos)
                                    if (n > 0) bytes += n
                                    calls++
                                }
                            }
                            else -> spikeLog("PROBE_BAD_STEP step=$step")
                        }
                        val ms = (SystemClock.elapsedRealtimeNanos() - t0) / 1_000_000
                        spikeLog("PROBE_END label=$label step=$step client_calls=$calls bytes=$bytes ms=$ms")
                    }
                }
            } catch (e: Exception) {
                spikeLog("PROBE_ERROR label=$label step=$step err=${e.javaClass.simpleName}: ${e.message}")
            }
        }
    }
}
