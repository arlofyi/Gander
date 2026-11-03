package fyi.arlo.gander.core.data.fetch

import fyi.arlo.gander.core.data.actions.ActionRefusals
import fyi.arlo.gander.core.data.actions.FileAction
import fyi.arlo.gander.core.data.actions.NetworkStateMonitor
import fyi.arlo.gander.core.data.browse.EntryTarget
import fyi.arlo.gander.core.data.hosts.HostRepository
import fyi.arlo.gander.core.data.hosts.PairedHost
import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.protocol.EntryKind
import fyi.arlo.gander.core.net.protocol.HashAlgo
import fyi.arlo.gander.core.net.protocol.ZipPlan
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Job
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import java.io.File
import java.io.IOException
import java.io.RandomAccessFile
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicBoolean

/** Backoff between connection attempts; [maxAttempts] failures in a row without progress fail the job. */
data class RetryPolicy(
    val maxAttempts: Int = 6,
    val baseDelayMs: Long = 1_000,
    val maxDelayMs: Long = 30_000,
) {
    fun delayMs(attempt: Int, error: GanderError): Long {
        val exponential = (baseDelayMs shl (attempt - 1).coerceIn(0, 16)).coerceAtMost(maxDelayMs)
        val hinted = (error as? GanderError.Host)?.retryAfterMs ?: 0
        return maxOf(exponential, hinted.coerceAtMost(2 * maxDelayMs))
    }
}

/**
 * The fetch queue (client spec, "Fetch engine"): per host at most [perHost] files at once, each a
 * single sequential Range stream; job state in a [FetchStore] (Room in the app) so a download
 * survives the process, the foreground-service time limit and network changes.
 *
 * A job: wait for a network -> download (resume with `Range` + `If-Range`; `412 file_changed`
 * restarts from zero with [FetchJob.changedOnHost] set for the UI) -> verify against `/hash` when
 * [FetchSettings.shouldVerify] (BLAKE3, else SHA-256; a mismatch deletes the copy and fails with
 * [FetchFailure.VERIFY_MISMATCH], Retry starts over) -> move into place and record the
 * [CachedFile]. Connection failures back off per [RetryPolicy]; a host refusal fails at once and
 * is reported to [refusals] so the file sheet shows the protocol's reason on Fetch.
 *
 * A folder download (WP-68, [FolderZip]) is the same job with three differences: it reads
 * `GET /v1/zip`, it never resumes (no `Range`: the host cannot resume an archive it builds as it
 * sends, so a broken connection starts over from zero) and nothing is verified.
 *
 * Downloads run in [scope] (the app's process scope). The Android foreground service only keeps
 * the process alive while [activeJobs] is not empty; [onWorkQueued] is how the engine asks for it.
 */
class FetchEngine(
    private val store: FetchStore,
    private val files: FetchFiles,
    private val hosts: HostRepository,
    private val connector: FetchConnector,
    private val network: NetworkStateMonitor,
    private val scope: CoroutineScope,
    private val settings: FetchSettingsSource = FetchSettingsSource.Defaults,
    private val refusals: ActionRefusals? = null,
    private val clock: () -> Long = System::currentTimeMillis,
    private val sleep: suspend (Long) -> Unit = { delay(it) },
    private val downloader: FileDownloader = FileDownloader(clock),
    private val verifier: HashVerifier = HashVerifier(sleep, clock),
    private val retry: RetryPolicy = RetryPolicy(),
    private val perHost: Int = PARALLEL_PER_HOST,
    private val maxRestarts: Int = MAX_CHANGED_RESTARTS,
) {
    /** Called whenever work is queued; the app starts the foreground service from here. */
    @Volatile
    var onWorkQueued: () -> Unit = {}

    /** Every job, newest first. */
    val jobs: Flow<List<FetchJob>> get() = store.jobs

    /** Jobs that are queued, running, verifying or waiting. Empty means the service can stop. */
    val activeJobs: Flow<List<FetchJob>> =
        store.jobs.map { all -> all.filter { it.status.active } }.distinctUntilChanged()

    private class Worker(val hostId: String, val job: Job)

    private val workers = ConcurrentHashMap<Long, Worker>()
    private val dispatchLock = Mutex()
    private val wake = Channel<Unit>(Channel.CONFLATED)
    private val started = AtomicBoolean(false)

    /**
     * Starts the queue (idempotent): jobs a dead process left running are queued again, rows of
     * forgotten hosts and orphan folders are cleaned up, then queued jobs start.
     */
    fun start() {
        if (!started.compareAndSet(false, true)) return
        scope.launch {
            recover()
            for (signal in wake) dispatch()
        }
        wake.trySend(Unit)
    }

    // Commands ---------------------------------------------------------------------------------

    /** Queues [target] (a file). Returns the existing job when the same file is already being fetched. */
    suspend fun enqueue(target: EntryTarget): Long {
        require(target.entry.kind == EntryKind.FILE) { "only files can be fetched" }
        start()
        store.latestJob(target.fileKey, FetchStatus.ACTIVE)?.let { return it.id }
        val now = clock()
        val id = store.insert(FetchJob(key = target.fileKey, entry = target.entry, createdAt = now, updatedAt = now))
        refusals?.clear(target, FileAction.FETCH)
        kick()
        return id
    }

    /**
     * What a folder download of [folder] (an [EntryTarget] of a directory) would hold and weigh
     * (`GET /v1/zip?dry_run=1`), for the size and count confirmation. Throws [GanderError]; a folder
     * over the host's limits is [GanderError.Host.TooLarge] with the host's own words.
     */
    suspend fun planZip(folder: EntryTarget): ZipPlan {
        val host = hosts.get(folder.hostId) ?: throw GanderError.Host.NotFound(404, "This computer was forgotten on this phone.")
        return connector.zipPlan(host, folder.shareId, folder.path)
    }

    /** Queues the files among [targets] (folders are skipped: no folder fetch in v1). */
    suspend fun enqueueAll(targets: List<EntryTarget>): List<Long> =
        targets.filter { it.entry.kind == EntryKind.FILE }.map { enqueue(it) }

    /** Stops a job that is not done and deletes its partial bytes. */
    suspend fun cancel(id: Long) = dispatchLock.withLock {
        stopWorker(id)
        val job = store.job(id) ?: return@withLock
        if (job.status == FetchStatus.DONE || job.status == FetchStatus.CANCELLED) return@withLock
        save(job.copy(status = FetchStatus.CANCELLED, bytesDone = 0, totalBytes = null, etag = null))
        files.deleteJobDir(id)
    }

    /** Queues a failed, cancelled or paused job again (a failed verification starts from zero). */
    suspend fun retry(id: Long) {
        dispatchLock.withLock {
            stopWorker(id)
            val job = store.job(id) ?: return
            if (job.status !in RETRYABLE) return
            save(job.copy(status = FetchStatus.QUEUED, failure = null, failureCode = null, attempts = 0))
        }
        start()
        kick()
    }

    /**
     * Stops every active job and keeps its progress (`PAUSED`). The foreground service calls this
     * from `onTimeout`; [resumePaused] picks the jobs up again on the next launch.
     */
    suspend fun pauseAll() = dispatchLock.withLock {
        for (job in store.jobsWithStatus(FetchStatus.ACTIVE)) {
            stopWorker(job.id)
            val current = store.job(job.id) ?: continue
            if (current.status.active) save(current.copy(status = FetchStatus.PAUSED))
        }
    }

    /** [pauseAll] in the engine's scope, for callers that must return at once (`Service.onTimeout`). */
    fun pauseAllInBackground(): Job = scope.launch { pauseAll() }

    /** Queues every paused job again. Returns how many. */
    suspend fun resumePaused(): Int {
        val paused = dispatchLock.withLock {
            store.jobsWithStatus(setOf(FetchStatus.PAUSED)).onEach { save(it.copy(status = FetchStatus.QUEUED, attempts = 0)) }
        }
        start()
        if (paused.isNotEmpty()) kick()
        return paused.size
    }

    /** True when jobs are waiting to run (queued or paused): the app resumes them at launch. */
    suspend fun hasPendingWork(): Boolean =
        store.jobsWithStatus(FetchStatus.ACTIVE + FetchStatus.PAUSED).isNotEmpty()

    /** Removes a job from the list. A finished file stays in the cache; partial bytes are deleted. */
    suspend fun remove(id: Long) = dispatchLock.withLock {
        stopWorker(id)
        val job = store.job(id) ?: return@withLock
        if (job.status != FetchStatus.DONE) files.deleteJobDir(id)
        store.deleteJob(id)
    }

    /** Clears done, failed and cancelled jobs from the list (cached files stay). */
    suspend fun clearFinished() = dispatchLock.withLock {
        store.jobsWithStatus(setOf(FetchStatus.FAILED, FetchStatus.CANCELLED)).forEach { files.deleteJobDir(it.id) }
        store.deleteFinishedJobs()
    }

    /** A host was forgotten: stop its jobs and delete its jobs, cached files and bytes. */
    suspend fun forgetHost(hostId: String) = dispatchLock.withLock {
        workers.filterValues { it.hostId == hostId }.keys.forEach { stopWorker(it) }
        store.deleteHost(hostId)
        files.sweep(keepIds())
    }

    // Queue ------------------------------------------------------------------------------------

    private fun kick() {
        wake.trySend(Unit)
        onWorkQueued()
    }

    private suspend fun stopWorker(id: Long) {
        workers[id]?.job?.cancelAndJoin()
        workers.remove(id)
    }

    private suspend fun recover() = dispatchLock.withLock {
        val stale = setOf(FetchStatus.RUNNING, FetchStatus.VERIFYING, FetchStatus.WAITING)
        store.jobsWithStatus(stale).forEach { save(it.copy(status = FetchStatus.QUEUED)) }
        val known = hosts.hosts.first().map { it.id }.toSet()
        val orphanHosts = store.jobsWithStatus(FetchStatus.entries.toSet()).map { it.key.hostId } +
            store.cachedFiles.first().map { it.key.hostId }
        orphanHosts.filter { it !in known }.toSet().forEach { store.deleteHost(it) }
        files.sweep(keepIds())
    }

    private suspend fun keepIds(): Set<Long> =
        store.jobIds() + store.cachedFiles.first().mapNotNull { FetchFiles.jobIdOf(it.localPath) }

    private suspend fun dispatch() = dispatchLock.withLock {
        val running = workers.values.groupingBy { it.hostId }.eachCount().toMutableMap()
        for (job in store.jobsWithStatus(setOf(FetchStatus.QUEUED))) {
            if (workers.containsKey(job.id)) continue
            val hostId = job.key.hostId
            val count = running[hostId] ?: 0
            if (count >= perHost) continue
            running[hostId] = count + 1
            val id = job.id
            val worker = Worker(hostId, scope.launch(start = CoroutineStart.LAZY) { run(id) })
            workers[id] = worker
            worker.job.invokeOnCompletion {
                workers.remove(id, worker)
                wake.trySend(Unit)
            }
            worker.job.start()
        }
    }

    // One job ----------------------------------------------------------------------------------

    private suspend fun save(job: FetchJob): FetchJob {
        val stamped = job.copy(updatedAt = clock())
        store.update(stamped)
        return stamped
    }

    private suspend fun run(id: Long) {
        var job = store.job(id) ?: return
        if (job.status != FetchStatus.QUEUED) return
        val host = hosts.get(job.key.hostId) ?: return fail(job, FetchFailure.HOST_FORGOTTEN, null)
        val config = settings.current()
        val zip = FolderZip.isZip(job.key)
        val partial = try {
            files.preparePartial(id)
        } catch (_: IOException) {
            return fail(job, FetchFailure.STORAGE, null)
        }
        while (true) {
            if (!network.current().connected) {
                job = save(job.copy(status = FetchStatus.WAITING))
                network.updates.first { it.connected }
            }
            val bytesBefore = job.bytesDone
            try {
                // An archive cannot be resumed: every attempt starts from zero.
                if (zip && (job.bytesDone != 0L || job.totalBytes != null || job.etag != null)) {
                    job = job.copy(bytesDone = 0, totalBytes = null, etag = null)
                }
                if (!isComplete(job, partial)) {
                    val remaining = job.size - job.bytesDone
                    if (remaining > 0 && files.usableSpace() < remaining + SPACE_MARGIN) {
                        return fail(job, FetchFailure.NO_SPACE, null)
                    }
                    job = save(job.copy(status = FetchStatus.RUNNING))
                    val point = downloader.download(
                        client = connector.transfer(host),
                        url = if (zip) {
                            connector.zipUrl(host, job.key.shareId, FolderZip.folderPath(job.key))
                        } else {
                            connector.fileUrl(host, job.key.shareId, job.key.path)
                        },
                        file = partial,
                        from = if (zip) ResumePoint.START else ResumePoint(job.bytesDone, job.totalBytes, job.etag),
                        listener = progressListener(id),
                    )
                    job = (store.job(id) ?: return).copy(
                        bytesDone = point.bytesDone,
                        totalBytes = point.totalBytes,
                        etag = point.etag,
                        attempts = 0,
                    )
                }
                val algo = if (!zip && config.shouldVerify(job.size)) verifyAlgorithm(host.capabilities) else null
                var verified: HashAlgo? = null
                if (algo != null) {
                    job = save(job.copy(status = FetchStatus.VERIFYING))
                    val outcome = verifier.verify(
                        hash = { a -> connector.hash(host, job.key.shareId, job.key.path, a) },
                        file = partial,
                        algo = algo,
                        etag = job.etag,
                    )
                    when (outcome) {
                        is VerifyOutcome.Match -> verified = outcome.algo
                        is VerifyOutcome.Mismatch -> {
                            files.deleteJobDir(id)
                            return fail(job.copy(bytesDone = 0, totalBytes = null, etag = null), FetchFailure.VERIFY_MISMATCH, outcome.algo.wire)
                        }
                        VerifyOutcome.Changed -> {
                            job = restartBecauseChanged(id, partial)
                            continue
                        }
                    }
                }
                finish(job, partial, verified, host)
                return
            } catch (e: CancellationException) {
                throw e
            } catch (_: FileChangedTooOftenException) {
                val latest = store.job(id) ?: return
                files.deleteJobDir(id)
                return fail(latest.copy(bytesDone = 0, totalBytes = null, etag = null), FetchFailure.FILE_CHANGED, null)
            } catch (e: LocalStorageException) {
                fyi.arlo.gander.core.DebugLog.failure("fetch (local storage)", e)
                val latest = store.job(id) ?: return
                val failure = if (files.usableSpace() < SPACE_MARGIN) FetchFailure.NO_SPACE else FetchFailure.STORAGE
                return fail(latest, failure, null)
            } catch (e: GanderError) {
                fyi.arlo.gander.core.DebugLog.failure("fetch", e)
                val latest = store.job(id) ?: return
                if (!e.isTransient()) {
                    refusals?.report(latest.target, FileAction.FETCH, e)
                    return fail(latest, failureOf(e), codeOf(e))
                }
                // Bytes of a zip that broke off are thrown away, so they are not progress.
                val attempts = if (!zip && latest.bytesDone > bytesBefore) 1 else latest.attempts + 1
                if (attempts >= retry.maxAttempts) return fail(latest, FetchFailure.NETWORK, codeOf(e))
                job = save(latest.copy(status = FetchStatus.WAITING, attempts = attempts))
                sleep(retry.delayMs(attempts, e))
            }
        }
    }

    private fun isComplete(job: FetchJob, partial: File): Boolean {
        val total = job.totalBytes ?: return false
        return job.etag != null && job.bytesDone == total && partial.isFile && partial.length() == total
    }

    private fun progressListener(id: Long) = object : FileDownloader.Listener {
        override suspend fun onProgress(point: ResumePoint) =
            store.updateProgress(id, point.bytesDone, point.totalBytes, point.etag, clock())

        override suspend fun onFileChanged() {
            val job = store.job(id) ?: return
            if (job.restarts + 1 > maxRestarts) throw FileChangedTooOftenException()
            save(job.copy(restarts = job.restarts + 1, changedOnHost = true, bytesDone = 0, totalBytes = null, etag = null))
        }
    }

    private suspend fun restartBecauseChanged(id: Long, partial: File): FetchJob {
        val job = store.job(id) ?: throw CancellationException("job removed")
        if (job.restarts + 1 > maxRestarts) throw FileChangedTooOftenException()
        partial.delete()
        return save(
            job.copy(
                status = FetchStatus.RUNNING,
                restarts = job.restarts + 1,
                changedOnHost = true,
                bytesDone = 0,
                totalBytes = null,
                etag = null,
            ),
        )
    }

    private suspend fun finish(job: FetchJob, partial: File, verified: HashAlgo?, host: PairedHost) {
        val relative = files.finalRelativePath(job.id, job.name)
        val dest = files.resolve(relative) ?: throw LocalStorageException(IOException("bad destination"))
        try {
            RandomAccessFile(partial, "rw").use { it.fd.sync() }
        } catch (e: IOException) {
            throw LocalStorageException(e)
        }
        if (dest.exists()) dest.delete()
        if (!partial.renameTo(dest)) throw LocalStorageException(IOException("cannot move the finished file"))
        val previous = store.cached(job.key)
        val now = clock()
        val cached = CachedFile(
            key = job.key,
            name = job.name,
            mime = job.entry.mime,
            localPath = relative,
            size = dest.length(),
            mtime = job.entry.mtime,
            etag = job.etag,
            pinned = previous?.pinned ?: false,
            lastUsed = now,
            fetchedAt = now,
            verifiedWith = verified,
        )
        store.complete(
            job.copy(
                status = FetchStatus.DONE,
                localPath = relative,
                verifiedWith = verified,
                failure = null,
                failureCode = null,
                attempts = 0,
                updatedAt = now,
            ),
            cached,
        )
        if (previous != null && previous.localPath != relative) files.deleteContaining(previous.localPath)
        refusals?.clear(job.target, FileAction.FETCH)
        hosts.recordContact(host.id, now)
    }

    private suspend fun fail(job: FetchJob, failure: FetchFailure, code: String?) {
        save(job.copy(status = FetchStatus.FAILED, failure = failure, failureCode = code))
    }

    companion object {
        /** Client spec: "Single downloader per host with 2 parallel files". */
        const val PARALLEL_PER_HOST = 2

        /** Times a job starts over because the file changed before it gives up. */
        const val MAX_CHANGED_RESTARTS = 3

        /** Free space kept beyond the remaining bytes. */
        const val SPACE_MARGIN = 32L * 1024 * 1024

        private val RETRYABLE = setOf(FetchStatus.FAILED, FetchStatus.CANCELLED, FetchStatus.PAUSED)

        internal fun GanderError.isTransient(): Boolean = when (this) {
            is GanderError.Unreachable, is GanderError.Timeout, is GanderError.Network, is GanderError.Tls -> true
            is GanderError.Host.TooManyRequests, is GanderError.Host.Generating, is GanderError.Host.Internal -> true
            is GanderError.Host.Unrecognised -> status >= 500 || status == 429
            else -> false
        }

        internal fun failureOf(e: GanderError): FetchFailure = when (e) {
            is GanderError.Host.NotFound, is GanderError.Host.ShareNotFound -> FetchFailure.NOT_FOUND
            is GanderError.Host.TokenMissing, is GanderError.Host.TokenInvalid, is GanderError.Host.TokenRevoked ->
                FetchFailure.NOT_AUTHORISED
            is GanderError.Host.FileChanged -> FetchFailure.FILE_CHANGED
            is GanderError.PinMismatch -> FetchFailure.WRONG_COMPUTER
            is GanderError.MalformedResponse -> FetchFailure.INCOMPATIBLE
            is GanderError.Host -> FetchFailure.HOST_REFUSED
            else -> FetchFailure.NETWORK
        }

        /** The protocol code, or the transport kind; never a message (which could carry detail). */
        internal fun codeOf(e: GanderError): String = when (e) {
            is GanderError.Host.Unrecognised -> e.rawCode ?: "http_${e.status}"
            is GanderError.Host -> e.code.wire
            is GanderError.Unreachable -> "unreachable"
            is GanderError.Timeout -> "timeout"
            is GanderError.Tls -> "tls"
            is GanderError.PinMismatch -> "pin_mismatch"
            is GanderError.MalformedResponse -> "malformed"
            is GanderError.Network -> "network"
        }
    }
}
