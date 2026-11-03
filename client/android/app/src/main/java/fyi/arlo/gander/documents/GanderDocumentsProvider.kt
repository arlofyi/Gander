package fyi.arlo.gander.documents

import android.content.Context
import android.content.pm.ProviderInfo
import android.content.res.AssetFileDescriptor
import android.database.Cursor
import android.database.MatrixCursor
import android.graphics.Point
import android.net.Uri
import android.os.Bundle
import android.os.CancellationSignal
import android.os.Handler
import android.os.HandlerThread
import android.os.ParcelFileDescriptor
import android.os.ProxyFileDescriptorCallback
import android.os.storage.StorageManager
import android.provider.DocumentsContract
import android.provider.DocumentsContract.Document
import android.provider.DocumentsContract.Root
import android.provider.DocumentsProvider
import android.system.ErrnoException
import android.system.OsConstants
import fyi.arlo.gander.AppGraph
import fyi.arlo.gander.R
import fyi.arlo.gander.appGraph
import fyi.arlo.gander.core.DebugLog
import fyi.arlo.gander.core.data.UiError
import fyi.arlo.gander.core.data.UiErrorKind
import fyi.arlo.gander.core.data.browse.EntryTarget
import fyi.arlo.gander.core.data.browse.ListQuery
import fyi.arlo.gander.core.data.browse.childPath
import fyi.arlo.gander.core.data.browse.parentPath
import fyi.arlo.gander.core.data.documents.DocumentIds
import fyi.arlo.gander.core.data.documents.DocumentReadException
import fyi.arlo.gander.core.data.documents.DocumentRef
import fyi.arlo.gander.core.data.documents.HostRangeFetcher
import fyi.arlo.gander.core.data.documents.HostRefusedException
import fyi.arlo.gander.core.data.documents.ReadAheadCache
import fyi.arlo.gander.core.data.documents.ReadStats
import fyi.arlo.gander.core.data.hosts.PairedHost
import fyi.arlo.gander.core.data.thumbs.ThumbRef
import fyi.arlo.gander.core.data.thumbs.ThumbSizes
import fyi.arlo.gander.core.data.thumbs.thumbVersion
import fyi.arlo.gander.core.data.thumbs.wantsThumbnail
import fyi.arlo.gander.core.data.toUiError
import fyi.arlo.gander.core.net.GanderApi
import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.ThumbPriority
import fyi.arlo.gander.core.net.protocol.Entry
import fyi.arlo.gander.core.net.protocol.EntryKind
import fyi.arlo.gander.core.net.protocol.Permission
import fyi.arlo.gander.core.net.protocol.ShareInfo
import fyi.arlo.gander.ui.fetch.FileIntents
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.withTimeoutOrNull
import java.io.File
import java.io.FileNotFoundException
import java.io.IOException
import java.time.Instant
import java.time.OffsetDateTime
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import java.util.concurrent.ThreadFactory
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger

/**
 * The Files-app integration (WP-60, client spec "Files-app integration"): every paired computer
 * is a root of the system Files app (and of every app's document picker), its shares and folders
 * are documents, and opening a file gives the caller a read-only file descriptor.
 *
 * Shaped by what the Files-app spike measured (`docs/specs/2026-11-01-files-app-spike-findings.md`):
 *
 * - **Listings come from the snapshots first.** `queryChildDocuments` answers from the stored
 *   listing at once and refreshes it in the background (`EXTRA_LOADING`, then `notifyChange`).
 * - **Open, query and size do no network.** Sizes come from the listing; only a document that was
 *   never listed costs one `stat` (short timeout, then remembered). Bytes move on the first read.
 * - **A saved copy is a real fd.** When the current version is on the phone (Fetch mode), the
 *   caller gets that file, not a proxy.
 * - **Otherwise a proxy fd with a handler thread of its own** (a shared one stalled a second open
 *   for 4 s) in front of a [ReadAheadCache] over `Range` requests with `If-Range`. A read waits at
 *   most 20 s, then fails with `EIO`; a changed file fails with `EIO` instead of mixing versions.
 * - **Read only.** No document or root advertises create, write, rename, delete or move, and
 *   `openDocument` refuses every mode but `r`.
 * - **App lock and revocation.** With the lock engaged there is one root saying "Unlock Gander to
 *   browse" and nothing opens; a host that refused this phone's token disappears.
 * - **Nothing secret is logged**: no token, path, name or byte; debug builds log counts only.
 */
class GanderDocumentsProvider : DocumentsProvider() {

    private lateinit var authority: String

    /** Per folder (document id): a refresh in flight, when the last one ended, and how. */
    private class Refresh {
        @Volatile var running = false

        @Volatile var finishedAt = 0L

        @Volatile var error: UiError? = null

        @Volatile var offline = false
    }

    private val refreshes = ConcurrentHashMap<String, Refresh>()

    /** Hosts that answered `token_revoked`/`token_invalid` this process: hidden until paired again. */
    private val revoked = ConcurrentHashMap.newKeySet<String>()

    /** Entries met in listings and stats, by document id, so open and query need no network. */
    private val entries = object : LinkedHashMap<String, Entry>(64, 0.75f, true) {
        override fun removeEldestEntry(eldest: MutableMap.MutableEntry<String, Entry>?) = size > MAX_REMEMBERED
    }

    private val fdCounter = AtomicInteger()
    private val watching = AtomicBoolean(false)

    private val prefetchPool: ExecutorService by lazy {
        val count = AtomicInteger()
        Executors.newCachedThreadPool(
            ThreadFactory { r -> Thread(r, "gander-docs-prefetch-${count.incrementAndGet()}").apply { isDaemon = true } },
        )
    }

    override fun attachInfo(context: Context, info: ProviderInfo) {
        authority = info.authority
        super.attachInfo(context, info)
    }

    override fun onCreate(): Boolean = true

    private val graph: AppGraph get() = context!!.appGraph

    // Access ------------------------------------------------------------------------------------

    private enum class Access { OPEN, LOCKED, OFF }

    private fun access(): Access = runBlocking {
        val settings = graph.settingsRepository.current()
        when {
            !settings.filesAppEnabled -> Access.OFF
            graph.appLock.hidesContent(settings.appLockEnabled) -> Access.LOCKED
            else -> Access.OPEN
        }
    }

    private fun requireOpen() {
        when (access()) {
            Access.OPEN -> Unit
            Access.LOCKED -> throw FileNotFoundException("Gander is locked")
            Access.OFF -> throw FileNotFoundException("Gander is not shown in the Files app")
        }
    }

    /** Roots follow the switch, the lock and the paired hosts; tell the Files app when they change. */
    private fun watchRoots() {
        if (!watching.compareAndSet(false, true)) return
        val g = graph
        g.processScope.launch {
            combine(g.settingsRepository.settings, g.appLock.state, g.hostRepository.hosts) { settings, lock, hosts ->
                listOf(settings.filesAppEnabled, settings.appLockEnabled, lock, hosts.map { it.id to it.name })
            }.distinctUntilChanged().collect { notify(rootsUri()) }
        }
    }

    // Roots ---------------------------------------------------------------------------------------

    override fun queryRoots(projection: Array<out String>?): Cursor {
        watchRoots()
        val cursor = MatrixCursor(projection ?: ROOT_PROJECTION)
        when (access()) {
            Access.OFF -> Unit
            Access.LOCKED -> cursor.newRow().apply {
                add(Root.COLUMN_ROOT_ID, DocumentIds.LOCKED)
                add(Root.COLUMN_DOCUMENT_ID, DocumentIds.LOCKED)
                add(Root.COLUMN_TITLE, string(R.string.files_app_locked_title))
                add(Root.COLUMN_SUMMARY, string(R.string.files_app_locked_summary))
                add(Root.COLUMN_FLAGS, 0)
                add(Root.COLUMN_ICON, R.mipmap.ic_launcher)
                add(Root.COLUMN_MIME_TYPES, "*/*")
            }
            Access.OPEN -> for (host in visibleHosts()) {
                val id = DocumentIds.host(host.id)
                cursor.newRow().apply {
                    add(Root.COLUMN_ROOT_ID, id)
                    add(Root.COLUMN_DOCUMENT_ID, id)
                    add(Root.COLUMN_TITLE, host.name)
                    add(Root.COLUMN_SUMMARY, string(R.string.files_app_root_summary))
                    // Read only: no FLAG_SUPPORTS_CREATE, no recents or search (they would need the network).
                    add(Root.COLUMN_FLAGS, Root.FLAG_SUPPORTS_IS_CHILD)
                    add(Root.COLUMN_ICON, R.mipmap.ic_launcher)
                    add(Root.COLUMN_MIME_TYPES, "*/*")
                }
            }
        }
        cursor.setNotificationUri(context!!.contentResolver, rootsUri())
        return cursor
    }

    /** Paired hosts with a token that were reached at least once or have stored shares, minus revoked ones. */
    private fun visibleHosts(): List<PairedHost> = runBlocking {
        graph.hostRepository.hosts.first().filter { host ->
            host.id !in revoked &&
                graph.hostRepository.token(host.id) != null &&
                (host.lastSeenAtMillis != null || graph.browseRepository.cachedShares(host) != null)
        }
    }

    private fun hostOf(ref: DocumentRef): PairedHost? {
        if (ref.hostId in revoked) return null
        val host = runBlocking { graph.hostRepository.get(ref.hostId) } ?: return null
        return host.takeIf { graph.hostRepository.token(it.id) != null }
    }

    // Documents ---------------------------------------------------------------------------------

    override fun queryDocument(documentId: String, projection: Array<out String>?): Cursor {
        val cursor = MatrixCursor(projection ?: DOCUMENT_PROJECTION)
        if (documentId == DocumentIds.LOCKED) {
            addDirectory(cursor, DocumentIds.LOCKED, string(R.string.files_app_locked_title))
            return cursor
        }
        requireOpen()
        val ref = DocumentIds.decode(documentId) ?: throw FileNotFoundException("unknown document")
        val host = hostOf(ref) ?: throw FileNotFoundException("unknown computer")
        when (ref) {
            is DocumentRef.Host -> addDirectory(cursor, documentId, host.name)
            is DocumentRef.Item -> if (ref.isShareRoot) {
                val share = cachedShares(host)?.firstOrNull { it.id == ref.shareId }
                addDirectory(cursor, documentId, share?.name ?: ref.shareId)
            } else {
                val entry = entryFor(host, ref, statIfMissing = true) ?: throw FileNotFoundException("not found")
                addEntry(cursor, documentId, entry)
            }
        }
        return cursor
    }

    override fun queryChildDocuments(
        parentDocumentId: String,
        projection: Array<out String>?,
        sortOrder: String?,
    ): Cursor {
        val cursor = MatrixCursor(projection ?: DOCUMENT_PROJECTION)
        if (parentDocumentId == DocumentIds.LOCKED) {
            cursor.extras = Bundle().apply {
                putString(DocumentsContract.EXTRA_INFO, string(R.string.files_app_locked_summary))
            }
            return cursor
        }
        requireOpen()
        val ref = DocumentIds.decode(parentDocumentId) ?: throw FileNotFoundException("unknown document")
        val host = hostOf(ref) ?: throw FileNotFoundException("unknown computer")
        val uri = DocumentsContract.buildChildDocumentsUri(authority, parentDocumentId)
        val shown: Boolean
        val refresh: Refresh
        when (ref) {
            is DocumentRef.Host -> {
                val shares = cachedShares(host)
                shares?.forEach { share ->
                    addDirectory(cursor, DocumentIds.item(host.id, share.id, ""), share.name)
                }
                shown = shares != null
                refresh = refreshIfStale(parentDocumentId, uri, host) {
                    graph.browseRepository.shares(host).isCached
                }
            }
            is DocumentRef.Item -> {
                val page = runBlocking { graph.browseRepository.cachedFirstPage(host, ref.shareId, ref.path, ListQuery()) }
                page?.entries?.forEach { entry ->
                    val id = DocumentIds.item(host.id, ref.shareId, childPath(ref.path, entry.name))
                    remember(id, entry)
                    addEntry(cursor, id, entry)
                }
                shown = page != null
                refresh = refreshIfStale(parentDocumentId, uri, host) { refreshListing(host, ref) }
            }
        }
        cursor.extras = Bundle().apply {
            if (refresh.running) putBoolean(DocumentsContract.EXTRA_LOADING, true)
            val error = refresh.error
            when {
                refresh.running -> Unit
                error != null -> putString(DocumentsContract.EXTRA_ERROR, string(R.string.files_app_error, describe(error)))
                refresh.offline && shown -> putString(DocumentsContract.EXTRA_INFO, string(R.string.files_app_offline_info))
            }
        }
        cursor.setNotificationUri(context!!.contentResolver, uri)
        return cursor
    }

    override fun isChildDocument(parentDocumentId: String, documentId: String): Boolean {
        val parent = DocumentIds.decode(parentDocumentId) ?: return false
        val child = DocumentIds.decode(documentId) ?: return false
        return DocumentIds.isDescendant(parent, child)
    }

    // Opening -------------------------------------------------------------------------------------

    override fun openDocument(documentId: String, mode: String, signal: CancellationSignal?): ParcelFileDescriptor {
        if (mode != "r") throw FileNotFoundException("Gander documents are read only")
        requireOpen()
        val ref = DocumentIds.decode(documentId) as? DocumentRef.Item ?: throw FileNotFoundException("not a file")
        if (ref.isShareRoot) throw FileNotFoundException("not a file")
        val host = hostOf(ref) ?: throw FileNotFoundException("unknown computer")
        val entry = entryFor(host, ref, statIfMissing = true) ?: throw FileNotFoundException("not found")
        if (entry.kind != EntryKind.FILE) throw FileNotFoundException("not a file")
        val share = cachedShares(host)?.firstOrNull { it.id == ref.shareId }
        if (share != null && Permission.FETCH !in share.permissions) {
            throw FileNotFoundException("this share does not allow copying files to the phone")
        }

        // The current version is already on the phone: hand out the file itself.
        val target = EntryTarget(host.id, ref.shareId, ref.path, entry)
        runBlocking { runCatching { graph.savedCopies.currentCopy(target) }.getOrNull() }?.let { saved ->
            return ParcelFileDescriptor.open(saved, ParcelFileDescriptor.MODE_READ_ONLY)
        }
        return openProxy(host, ref, entry, signal)
    }

    private fun openProxy(host: PairedHost, ref: DocumentRef.Item, entry: Entry, signal: CancellationSignal?): ParcelFileDescriptor {
        // The clients are built from the stored host; nothing here touches the network.
        val clients = runBlocking { graph.hostClients(host.id) }
        val url = GanderApi(clients).fileUrl(ref.shareId, ref.path)
        val size = entry.size.coerceAtLeast(0)
        val cache = ReadAheadCache(size, HostRangeFetcher(clients.transfer, url, size), prefetchPool)
        val fd = fdCounter.incrementAndGet()
        // One thread per fd: openProxyFileDescriptor waits for onGetSize on it, so a shared thread
        // lets one slow host hang every other open (4 s measured in the spike).
        val thread = HandlerThread("gander-docs-fd-$fd").apply { start() }
        val callback = ProxyCallback(
            size = size,
            cache = cache,
            onFailure = { e -> noteFailure(host.id, e) },
            onReleased = { stats ->
                thread.quitSafely()
                FilesAppReads.record(stats)
                DebugLog.note("files-app fd $fd released: $stats")
            },
        )
        signal?.setOnCancelListener { cache.cancel() }
        return try {
            context!!.getSystemService(StorageManager::class.java)
                .openProxyFileDescriptor(ParcelFileDescriptor.MODE_READ_ONLY, callback, Handler(thread.looper))
        } catch (e: IOException) {
            cache.cancel()
            thread.quitSafely()
            throw FileNotFoundException("could not open the file").apply { initCause(e) }
        }
    }

    private fun noteFailure(hostId: String, e: DocumentReadException) {
        val refused = e as? HostRefusedException ?: return
        if (refused.error.toUiError().kind == UiErrorKind.REVOKED && revoked.add(hostId)) notify(rootsUri())
    }

    private class ProxyCallback(
        private val size: Long,
        private val cache: ReadAheadCache,
        private val onFailure: (DocumentReadException) -> Unit,
        private val onReleased: (ReadStats) -> Unit,
    ) : ProxyFileDescriptorCallback() {
        override fun onGetSize(): Long = size

        override fun onRead(offset: Long, size: Int, data: ByteArray): Int = try {
            cache.read(offset, size, data)
        } catch (e: DocumentReadException) {
            onFailure(e)
            throw ErrnoException("onRead", OsConstants.EIO)
        } catch (e: RuntimeException) {
            DebugLog.failure("files-app read", e)
            throw ErrnoException("onRead", OsConstants.EIO)
        }

        override fun onRelease() {
            cache.close()
            onReleased(cache.stats())
        }
    }

    // Thumbnails ----------------------------------------------------------------------------------

    override fun openDocumentThumbnail(
        documentId: String,
        sizeHint: Point,
        signal: CancellationSignal?,
    ): AssetFileDescriptor {
        requireOpen()
        val ref = DocumentIds.decode(documentId) as? DocumentRef.Item ?: throw FileNotFoundException("no thumbnail")
        val host = hostOf(ref) ?: throw FileNotFoundException("unknown computer")
        val entry = entryFor(host, ref, statIfMissing = false)?.takeIf { it.wantsThumbnail() }
            ?: throw FileNotFoundException("no thumbnail")
        val px = ThumbSizes.pick(
            displayPx = maxOf(sizeHint.x, sizeHint.y),
            supported = host.capabilities?.thumbSizes ?: ThumbSizes.DEFAULT,
            tablet = false,
        )
        val thumb = ThumbRef(host.id, ref.shareId, ref.path, entry.thumbVersion(), px)
        val bytes = try {
            runBlocking(Dispatchers.IO) {
                val job = coroutineContext[Job]
                signal?.setOnCancelListener { job?.cancel() }
                withTimeout(THUMB_TIMEOUT_MS) {
                    // Low priority: the host renders what is on Gander's own screens first.
                    val loaded = graph.thumbLoader.load(thumb, ThumbPriority.PREFETCH, graph.imageLoader.diskCache)
                    loaded.source.use { it.source().readByteArray() }
                }
            }
        } catch (e: CancellationException) {
            throw FileNotFoundException("thumbnail cancelled or late").apply { initCause(e) }
        } catch (e: Exception) {
            DebugLog.failure("files-app thumbnail", e)
            throw FileNotFoundException("no thumbnail").apply { initCause(e) }
        }
        // A file that is unlinked at once: the descriptor stays readable, nothing is left behind.
        val dir = File(context!!.cacheDir, "files-app-thumbs").apply { mkdirs() }
        val file = File.createTempFile("thumb", ".img", dir)
        try {
            file.writeBytes(bytes)
            val pfd = ParcelFileDescriptor.open(file, ParcelFileDescriptor.MODE_READ_ONLY)
            return AssetFileDescriptor(pfd, 0, AssetFileDescriptor.UNKNOWN_LENGTH)
        } finally {
            file.delete()
        }
    }

    // Listing refresh -----------------------------------------------------------------------------

    /**
     * Starts a background refresh of one folder unless one runs or the last ended less than
     * [FRESH_MS] ago (the re-query that our own `notifyChange` triggers must not start another).
     * [work] returns true when the host was unreachable and the snapshot was used.
     */
    private fun refreshIfStale(key: String, uri: Uri, host: PairedHost, work: suspend () -> Boolean): Refresh {
        val refresh = refreshes.getOrPut(key) { Refresh() }
        synchronized(refresh) {
            if (refresh.running || System.currentTimeMillis() - refresh.finishedAt < FRESH_MS) return refresh
            refresh.running = true
        }
        graph.processScope.launch {
            try {
                refresh.offline = work()
                refresh.error = null
            } catch (e: CancellationException) {
                throw e
            } catch (e: GanderError) {
                val error = e.toUiError()
                refresh.error = error
                if (error.kind == UiErrorKind.REVOKED && revoked.add(host.id)) notify(rootsUri())
            } catch (e: Exception) {
                DebugLog.failure("files-app refresh", e)
                refresh.error = UiError(UiErrorKind.OTHER)
            } finally {
                refresh.finishedAt = System.currentTimeMillis()
                refresh.running = false
                notify(uri)
            }
        }
        return refresh
    }

    /** The first page and, while the snapshot has room, the next ones (the repository stores them). */
    private suspend fun refreshListing(host: PairedHost, ref: DocumentRef.Item): Boolean {
        val browse = graph.browseRepository
        val query = ListQuery()
        val first = browse.firstPage(host, ref.shareId, ref.path, query)
        if (first.cachedAtMillis != null) return true
        var cursor = first.nextCursor
        var count = first.entries.size
        while (cursor != null && count < MAX_LISTED) {
            val next = browse.nextPage(host, ref.shareId, ref.path, query, cursor)
            count += next.entries.size
            cursor = next.nextCursor
        }
        return false
    }

    // Entries -------------------------------------------------------------------------------------

    private fun cachedShares(host: PairedHost): List<ShareInfo>? =
        runBlocking { graph.browseRepository.cachedShares(host) }?.value

    private fun remember(id: String, entry: Entry) {
        synchronized(entries) { entries[id] = entry }
    }

    /**
     * The entry of a file or folder: remembered from a listing, else from the parent's stored
     * listing, else (only when [statIfMissing]) one `stat` with a short timeout, remembered.
     */
    private fun entryFor(host: PairedHost, ref: DocumentRef.Item, statIfMissing: Boolean): Entry? {
        val id = DocumentIds.encode(ref)
        synchronized(entries) { entries[id] }?.let { return it }
        val name = ref.path.substringAfterLast('/')
        val listed = runBlocking {
            graph.browseRepository.cachedFirstPage(host, ref.shareId, parentPath(ref.path), ListQuery())
        }?.entries?.firstOrNull { it.name == name }
        if (listed != null) return listed.also { remember(id, it) }
        if (!statIfMissing) return null
        return try {
            runBlocking(Dispatchers.IO) {
                withTimeoutOrNull(STAT_TIMEOUT_MS) { graph.hostConnector.connect(host).api.stat(ref.shareId, ref.path) }
            }?.also { remember(id, it) }
        } catch (e: GanderError) {
            if (e.toUiError().kind == UiErrorKind.REVOKED && revoked.add(host.id)) notify(rootsUri())
            null
        }
    }

    // Rows ------------------------------------------------------------------------------------------

    private fun addDirectory(cursor: MatrixCursor, id: String, name: String) {
        cursor.newRow().apply {
            add(Document.COLUMN_DOCUMENT_ID, id)
            add(Document.COLUMN_DISPLAY_NAME, name)
            add(Document.COLUMN_MIME_TYPE, Document.MIME_TYPE_DIR)
            add(Document.COLUMN_FLAGS, 0)
            add(Document.COLUMN_SIZE, null)
            add(Document.COLUMN_LAST_MODIFIED, null)
        }
    }

    private fun addEntry(cursor: MatrixCursor, id: String, entry: Entry) {
        if (entry.kind == EntryKind.DIR) {
            addDirectory(cursor, id, entry.name)
            return
        }
        cursor.newRow().apply {
            add(Document.COLUMN_DOCUMENT_ID, id)
            add(Document.COLUMN_DISPLAY_NAME, entry.name)
            add(Document.COLUMN_MIME_TYPE, FileIntents.mimeOf(entry.name, entry.mime))
            // Read only: never FLAG_SUPPORTS_WRITE, _DELETE, _RENAME, _MOVE, _COPY or _REMOVE.
            add(Document.COLUMN_FLAGS, if (entry.wantsThumbnail()) Document.FLAG_SUPPORTS_THUMBNAIL else 0)
            add(Document.COLUMN_SIZE, entry.size.coerceAtLeast(0))
            add(Document.COLUMN_LAST_MODIFIED, millisOf(entry.mtime))
        }
    }

    // Plumbing --------------------------------------------------------------------------------------

    private fun rootsUri(): Uri = DocumentsContract.buildRootsUri(authority)

    private fun notify(uri: Uri) {
        context?.contentResolver?.notifyChange(uri, null)
    }

    private fun string(id: Int, vararg args: Any): String = context!!.getString(id, *args)

    private fun describe(error: UiError): String = error.detail ?: error.kind.name.lowercase().replace('_', ' ')

    companion object {
        /** A refreshed folder is not refreshed again for this long (the Files app re-queries on every notify). */
        const val FRESH_MS = 30_000L
        const val STAT_TIMEOUT_MS = 5_000L
        const val THUMB_TIMEOUT_MS = 15_000L
        const val MAX_REMEMBERED = 4_000

        /** Pages are followed up to this many entries, the snapshot's own limit. */
        const val MAX_LISTED = 2_000

        val ROOT_PROJECTION = arrayOf(
            Root.COLUMN_ROOT_ID, Root.COLUMN_DOCUMENT_ID, Root.COLUMN_TITLE, Root.COLUMN_SUMMARY,
            Root.COLUMN_FLAGS, Root.COLUMN_ICON, Root.COLUMN_MIME_TYPES,
        )
        val DOCUMENT_PROJECTION = arrayOf(
            Document.COLUMN_DOCUMENT_ID, Document.COLUMN_DISPLAY_NAME, Document.COLUMN_MIME_TYPE,
            Document.COLUMN_FLAGS, Document.COLUMN_SIZE, Document.COLUMN_LAST_MODIFIED,
        )

        /** `host` `mtime` (ISO-8601) as epoch millis, null when it cannot be read. */
        fun millisOf(mtime: String): Long? =
            runCatching { Instant.parse(mtime).toEpochMilli() }.getOrNull()
                ?: runCatching { OffsetDateTime.parse(mtime).toInstant().toEpochMilli() }.getOrNull()
    }
}

/**
 * The read statistics of the last released proxy fds (counts only: no names, paths or bytes), for
 * the instrumented test and debug logs.
 */
object FilesAppReads {
    private const val KEEP = 16
    private val recent = ArrayDeque<ReadStats>()

    fun record(stats: ReadStats) {
        synchronized(recent) {
            recent.addLast(stats)
            while (recent.size > KEEP) recent.removeFirst()
        }
    }

    fun recent(): List<ReadStats> = synchronized(recent) { recent.toList() }

    fun clear() = synchronized(recent) { recent.clear() }
}
