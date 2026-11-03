package fyi.arlo.gander.spike.files

import android.database.Cursor
import android.database.MatrixCursor
import android.os.Binder
import android.os.CancellationSignal
import android.os.Handler
import android.os.HandlerThread
import android.os.ParcelFileDescriptor
import android.os.ProxyFileDescriptorCallback
import android.os.SystemClock
import android.os.storage.StorageManager
import android.provider.DocumentsContract.Document
import android.provider.DocumentsContract.Root
import android.provider.DocumentsProvider
import android.system.ErrnoException
import android.system.OsConstants
import java.io.File
import java.io.FileNotFoundException
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL
import java.util.TreeMap
import java.util.TreeSet
import java.util.concurrent.atomic.AtomicInteger

/**
 * THROWAWAY (WP-47). One root, one flat directory of *.mp4 files. Bytes never come from the
 * provider's own storage: every read goes to the loopback [RangeHttpServer] as an HTTP Range request,
 * and is handed to the caller through StorageManager.openProxyFileDescriptor.
 */
class SpikeDocumentsProvider : DocumentsProvider() {
    private val fdCounter = AtomicInteger(0)
    private val sizes = HashMap<String, Long>()

    @Volatile private var server: RangeHttpServer? = null
    private val handlerThread by lazy { HandlerThread("spike-proxy-fd").also { it.start() } }
    private val handler by lazy { Handler(handlerThread.looper) }

    private fun dataDir(): File? = context?.getExternalFilesDir(null)

    private fun server(): RangeHttpServer = server ?: synchronized(this) {
        server ?: RangeHttpServer(dataDir()!!).also { it.start(); server = it }
    }

    override fun onCreate(): Boolean {
        spikeLog("PROVIDER_CREATE pid=${android.os.Process.myPid()}")
        return true
    }

    private fun who(): String = "pkg=$callingPackage uid=${Binder.getCallingUid()}"

    override fun queryRoots(projection: Array<out String>?): Cursor {
        spikeLog("QUERY_ROOTS ${who()}")
        val c = MatrixCursor(projection ?: DEFAULT_ROOT_PROJECTION)
        c.newRow().apply {
            add(Root.COLUMN_ROOT_ID, ROOT_ID)
            add(Root.COLUMN_DOCUMENT_ID, ROOT_DOC)
            add(Root.COLUMN_TITLE, "Gander Spike")
            add(Root.COLUMN_SUMMARY, "WP-47 throwaway")
            add(Root.COLUMN_MIME_TYPES, "*/*")
            add(Root.COLUMN_FLAGS, Root.FLAG_SUPPORTS_IS_CHILD)
            add(Root.COLUMN_ICON, android.R.drawable.ic_menu_gallery)
        }
        return c
    }

    private fun files(): List<File> =
        dataDir()?.listFiles { f -> f.isFile && f.name.endsWith(".mp4") }?.sortedBy { it.name } ?: emptyList()

    private fun addDoc(c: MatrixCursor, id: String) {
        if (id == ROOT_DOC) {
            c.newRow().apply {
                add(Document.COLUMN_DOCUMENT_ID, ROOT_DOC)
                add(Document.COLUMN_DISPLAY_NAME, "Gander Spike")
                add(Document.COLUMN_MIME_TYPE, Document.MIME_TYPE_DIR)
                add(Document.COLUMN_FLAGS, 0)
                add(Document.COLUMN_SIZE, null)
            }
            return
        }
        val size = sizeOf(id)
        c.newRow().apply {
            add(Document.COLUMN_DOCUMENT_ID, id)
            add(Document.COLUMN_DISPLAY_NAME, id)
            add(Document.COLUMN_MIME_TYPE, "video/mp4")
            add(Document.COLUMN_FLAGS, 0) // read only: no write, delete or thumbnail flags
            add(Document.COLUMN_SIZE, size)
            add(Document.COLUMN_LAST_MODIFIED, File(dataDir(), id).lastModified())
        }
    }

    override fun queryDocument(documentId: String, projection: Array<out String>?): Cursor {
        spikeLog("QUERY_DOCUMENT id=$documentId ${who()}")
        val c = MatrixCursor(projection ?: DEFAULT_DOC_PROJECTION)
        addDoc(c, documentId)
        return c
    }

    override fun queryChildDocuments(
        parentDocumentId: String,
        projection: Array<out String>?,
        sortOrder: String?,
    ): Cursor {
        spikeLog("QUERY_CHILDREN parent=$parentDocumentId ${who()}")
        val c = MatrixCursor(projection ?: DEFAULT_DOC_PROJECTION)
        if (parentDocumentId == ROOT_DOC) files().forEach { addDoc(c, it.name) }
        return c
    }

    override fun isChildDocument(parentDocumentId: String, documentId: String): Boolean =
        parentDocumentId == ROOT_DOC && documentId != ROOT_DOC

    /** The size comes from an HTTP HEAD, like the real provider's listing metadata would. */
    private fun sizeOf(name: String): Long = synchronized(sizes) {
        sizes.getOrPut(name) {
            val u = URL("http://127.0.0.1:${server().port}/f/$name")
            val c = u.openConnection() as HttpURLConnection
            c.requestMethod = "HEAD"
            c.connectTimeout = 2000
            c.readTimeout = 5000
            try {
                // getContentLengthLong() swallows IOExceptions and answers -1; never cache that.
                if (c.responseCode != 200) throw IOException("HEAD $name: HTTP ${c.responseCode}")
                val len = c.contentLengthLong
                if (len < 0) throw IOException("HEAD $name: no Content-Length")
                spikeLog("HEAD name=$name size=$len")
                len
            } finally {
                c.disconnect()
            }
        }
    }

    override fun openDocument(documentId: String, mode: String, signal: CancellationSignal?): ParcelFileDescriptor {
        val id = fdCounter.incrementAndGet()
        spikeLog("OPEN fd=$id doc=$documentId mode=$mode ${who()} cancelable=${signal != null}")
        if (mode != "r") throw SecurityException("read-only provider, mode=$mode")
        if (files().none { it.name == documentId }) throw FileNotFoundException(documentId)
        val conf = SpikeConf.load(dataDir())
        // Only the (mode, callback, handler) overload is public API; the two-argument overload is @hide.
        val perFd = if (conf.perFdHandler) HandlerThread("spike-proxy-fd-$id").also { it.start() } else null
        val cb = HttpProxyCallback(
            id, documentId, sizeOf(documentId), server().port, conf, callingPackage ?: "?",
            onClosed = { perFd?.quitSafely() },
        )
        val sm = context!!.getSystemService(StorageManager::class.java)
        val pfd = sm.openProxyFileDescriptor(
            ParcelFileDescriptor.MODE_READ_ONLY,
            cb,
            if (perFd != null) Handler(perFd.looper) else handler,
        )
        spikeLog("OPEN_OK fd=$id readahead=${conf.readAhead} perFdHandler=${conf.perFdHandler}")
        return pfd
    }

    private companion object {
        const val ROOT_ID = "spike"
        const val ROOT_DOC = "root"
        val DEFAULT_ROOT_PROJECTION = arrayOf(
            Root.COLUMN_ROOT_ID, Root.COLUMN_DOCUMENT_ID, Root.COLUMN_TITLE, Root.COLUMN_SUMMARY,
            Root.COLUMN_MIME_TYPES, Root.COLUMN_FLAGS, Root.COLUMN_ICON,
        )
        val DEFAULT_DOC_PROJECTION = arrayOf(
            Document.COLUMN_DOCUMENT_ID, Document.COLUMN_DISPLAY_NAME, Document.COLUMN_MIME_TYPE,
            Document.COLUMN_FLAGS, Document.COLUMN_SIZE, Document.COLUMN_LAST_MODIFIED,
        )
    }
}

/** Serves one open file descriptor. Logs every callback; optionally runs a one-block read-ahead. */
private class HttpProxyCallback(
    private val id: Int,
    private val name: String,
    private val fileSize: Long,
    private val port: Int,
    private val conf: SpikeConf,
    private val opener: String,
    private val onClosed: () -> Unit,
) : ProxyFileDescriptorCallback() {
    private var reads = 0
    private var totalBytes = 0L
    private var httpRequests = 0
    private var httpBytes = 0L
    private var nextExpected = -1L
    private var sequential = 0
    private var forwardJumps = 0
    private var backwardJumps = 0
    private val sizeHistogram = TreeMap<Int, Int>()
    private val threads = TreeSet<String>()
    private val inFlight = AtomicInteger(0)
    private var maxInFlight = 0
    private val opened = SystemClock.elapsedRealtimeNanos()

    // Read-ahead window (only used when conf.readAhead > 0).
    private var window: ByteArray? = null
    private var windowStart = -1L
    private var windowLen = 0

    private fun thr() = Thread.currentThread().name
    private fun ms(t0: Long) = "%.2f".format((SystemClock.elapsedRealtimeNanos() - t0) / 1_000_000.0)

    override fun onGetSize(): Long {
        threads += thr()
        spikeLog("GETSIZE fd=$id size=$fileSize thr=${thr()}")
        return fileSize
    }

    override fun onRead(offset: Long, size: Int, data: ByteArray): Int {
        val t0 = SystemClock.elapsedRealtimeNanos()
        val cur = inFlight.incrementAndGet()
        if (cur > maxInFlight) maxInFlight = cur
        threads += thr()
        try {
            if (offset < 0 || size > data.size) throw ErrnoException("onRead", OsConstants.EINVAL)
            val want = minOf(size.toLong(), maxOf(0L, fileSize - offset)).toInt()
            if (want > 0) {
                if (conf.readAhead > 0) readAhead(offset, want, data) else fetch(offset, want, data, 0)
            }
            // Classify against the previous read's end.
            if (nextExpected < 0 || offset == nextExpected) sequential++
            else if (offset > nextExpected) forwardJumps++ else backwardJumps++
            nextExpected = offset + want
            reads++
            totalBytes += want
            sizeHistogram.merge(size, 1, Int::plus)
            spikeLog(
                "READ fd=$id name=$name off=$offset size=$size ret=$want datalen=${data.size} " +
                    "cb_ms=${ms(t0)} thr=${thr()} t=${(t0 - opened) / 1000}us",
            )
            return want
        } catch (e: IOException) {
            spikeLog("READ_ERR fd=$id off=$offset size=$size err=${e.message}")
            throw ErrnoException("onRead", OsConstants.EIO)
        } finally {
            inFlight.decrementAndGet()
        }
    }

    private fun readAhead(offset: Long, want: Int, data: ByteArray) {
        val w = window
        if (w == null || offset < windowStart || offset + want > windowStart + windowLen) {
            val buf = w ?: ByteArray(conf.readAhead).also { window = it }
            val len = minOf(buf.size.toLong(), fileSize - offset).toInt()
            val n = fetch(offset, len, buf, 0)
            windowStart = offset
            windowLen = n
        }
        System.arraycopy(window!!, (offset - windowStart).toInt(), data, 0, want)
    }

    /** One HTTP Range request for [len] bytes at [offset]; returns the bytes actually received. */
    private fun fetch(offset: Long, len: Int, out: ByteArray, outOff: Int): Int {
        val t0 = SystemClock.elapsedRealtimeNanos()
        val c = URL("http://127.0.0.1:$port/f/$name").openConnection() as HttpURLConnection
        c.connectTimeout = 2000
        c.readTimeout = 300_000 // long on purpose: only platform-level timeouts should show in the delay test
        c.setRequestProperty("Range", "bytes=$offset-${offset + len - 1}")
        try {
            val code = c.responseCode
            if (code != 206) throw IOException("HTTP $code")
            var got = 0
            c.inputStream.use { ins ->
                while (got < len) {
                    val r = ins.read(out, outOff + got, len - got)
                    if (r < 0) break
                    got += r
                }
            }
            httpRequests++
            httpBytes += got
            spikeLog("FETCH fd=$id off=$offset len=$len got=$got http_ms=${ms(t0)}")
            return got
        } finally {
            c.disconnect()
        }
    }

    override fun onRelease() {
        val lifeMs = (SystemClock.elapsedRealtimeNanos() - opened) / 1_000_000
        spikeLog(
            "RELEASE fd=$id name=$name opener=$opener reads=$reads bytes=$totalBytes " +
                "http_requests=$httpRequests http_bytes=$httpBytes sequential=$sequential " +
                "fwd_jumps=$forwardJumps back_jumps=$backwardJumps sizes=$sizeHistogram " +
                "threads=$threads max_in_flight=$maxInFlight life_ms=$lifeMs",
        )
        onClosed()
    }
}
