package fyi.arlo.gander.spike.files

import android.os.SystemClock
import java.io.BufferedInputStream
import java.io.File
import java.io.IOException
import java.io.OutputStream
import java.io.RandomAccessFile
import java.net.InetAddress
import java.net.ServerSocket
import java.net.Socket
import java.util.concurrent.Executors

/**
 * Tiny HTTP/1.1 file server for the spike. Binds 127.0.0.1 ONLY (never 0.0.0.0), serves
 * `GET|HEAD /f/<name>` for the files in [dir] with single-range `Range:` support and keep-alive.
 * Stands in for the Gander host's Range-capable download endpoint.
 */
class RangeHttpServer(private val dir: File) {
    private val loopback: InetAddress = InetAddress.getByAddress(byteArrayOf(127, 0, 0, 1))
    private val server = ServerSocket(0, 32, loopback)
    private val pool = Executors.newCachedThreadPool { r -> Thread(r, "spike-http") }

    val port: Int get() = server.localPort

    fun start() {
        spikeLog("HTTP_LISTEN addr=${server.inetAddress.hostAddress} port=$port")
        Thread({
            while (!server.isClosed) {
                try {
                    val s = server.accept()
                    pool.execute { serve(s) }
                } catch (_: IOException) {
                    break
                }
            }
        }, "spike-http-accept").start()
    }

    private fun serve(s: Socket) {
        s.use {
            s.soTimeout = 60_000
            val input = BufferedInputStream(s.getInputStream())
            // TCP_NODELAY plus one buffered stream: without it the header and a small body leave in two
            // segments and Nagle plus delayed ACK adds ~40 ms to every small Range answer.
            s.tcpNoDelay = true
            val out = java.io.BufferedOutputStream(s.getOutputStream(), 64 * 1024)
            try {
                while (true) {
                    val requestLine = readLine(input) ?: return
                    val headers = HashMap<String, String>()
                    while (true) {
                        val h = readLine(input) ?: return
                        if (h.isEmpty()) break
                        val i = h.indexOf(':')
                        if (i > 0) headers[h.substring(0, i).trim().lowercase()] = h.substring(i + 1).trim()
                    }
                    val parts = requestLine.split(' ')
                    if (parts.size < 2) return
                    respond(parts[0], parts[1], headers["range"], out)
                    if (headers["connection"].equals("close", ignoreCase = true)) return
                }
            } catch (_: IOException) {
                // client went away (keep-alive timeout, cancelled read): nothing to do
            }
        }
    }

    private fun readLine(input: BufferedInputStream): String? {
        val sb = StringBuilder()
        while (true) {
            val b = input.read()
            if (b < 0) return if (sb.isEmpty()) null else sb.toString()
            if (b == '\n'.code) return sb.toString().trimEnd('\r')
            sb.append(b.toChar())
            if (sb.length > 8192) return null
        }
    }

    private fun respond(method: String, target: String, range: String?, out: OutputStream) {
        val name = target.removePrefix("/f/")
        val ok = target.startsWith("/f/") && NAME_RE.matches(name)
        val file = if (ok) File(dir, name) else null
        if (file == null || !file.isFile) {
            status(out, "404 Not Found", 0)
            return
        }
        val length = file.length()
        var start = 0L
        var end = length - 1
        var partial = false
        if (range != null) {
            val m = RANGE_RE.matchEntire(range.trim())
            if (m == null) {
                status(out, "416 Range Not Satisfiable", 0, "Content-Range: bytes */$length")
                return
            }
            val a = m.groupValues[1]
            val b = m.groupValues[2]
            if (a.isEmpty()) { // suffix range
                val n = b.toLong()
                start = maxOf(0L, length - n)
            } else {
                start = a.toLong()
                if (b.isNotEmpty()) end = minOf(end, b.toLong())
            }
            if (start >= length || start > end) {
                status(out, "416 Range Not Satisfiable", 0, "Content-Range: bytes */$length")
                return
            }
            partial = true
        }
        val conf = SpikeConf.load(dir)
        val t0 = SystemClock.elapsedRealtimeNanos()
        if (conf.delayMs > 0 && start >= conf.delayFrom && method == "GET") {
            Thread.sleep(conf.delayMs)
        }
        val n = end - start + 1
        val head = StringBuilder()
            .append(if (partial) "HTTP/1.1 206 Partial Content\r\n" else "HTTP/1.1 200 OK\r\n")
            .append("Content-Type: application/octet-stream\r\n")
            .append("Accept-Ranges: bytes\r\n")
            .append("Content-Length: $n\r\n")
        if (partial) head.append("Content-Range: bytes $start-$end/$length\r\n")
        head.append("Connection: keep-alive\r\n\r\n")
        out.write(head.toString().toByteArray(Charsets.ISO_8859_1))
        if (method == "GET") {
            RandomAccessFile(file, "r").use { raf ->
                raf.seek(start)
                val buf = ByteArray(64 * 1024)
                var left = n
                while (left > 0) {
                    val r = raf.read(buf, 0, minOf(buf.size.toLong(), left).toInt())
                    if (r < 0) break
                    out.write(buf, 0, r)
                    left -= r
                }
            }
        }
        out.flush()
        val ms = (SystemClock.elapsedRealtimeNanos() - t0) / 1_000_000.0
        spikeLog("HTTP method=$method name=$name start=$start end=$end bytes=$n partial=$partial ms=${"%.1f".format(ms)}")
    }

    private fun status(out: OutputStream, status: String, length: Long, extra: String? = null) {
        val sb = StringBuilder("HTTP/1.1 $status\r\nContent-Length: $length\r\nConnection: keep-alive\r\n")
        if (extra != null) sb.append(extra).append("\r\n")
        sb.append("\r\n")
        out.write(sb.toString().toByteArray(Charsets.ISO_8859_1))
        out.flush()
        spikeLog("HTTP status=$status")
    }

    private companion object {
        val NAME_RE = Regex("[A-Za-z0-9][A-Za-z0-9._-]{0,63}")
        val RANGE_RE = Regex("bytes=(\\d*)-(\\d*)")
    }
}
