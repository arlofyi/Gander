package fyi.arlo.gander.core.data.fetch

import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.update
import java.util.concurrent.atomic.AtomicLong

/**
 * Finished downloads on the phone (`cached_files`). The fetch engine writes them; WP-28's cache
 * (LRU with a cap, pinned files exempt) and Storage screen read and prune them through
 * [OfflineFiles], which also deletes the bytes.
 */
interface CachedFileStore {
    /** Every cached file, most recently used first. Emits again on any change. */
    val cachedFiles: Flow<List<CachedFile>>

    suspend fun cached(key: FileKey): CachedFile?

    /** "Keep offline". Returns false when there is no such cached file. */
    suspend fun setPinned(key: FileKey, pinned: Boolean): Boolean

    /** Records that the copy was opened or shared at [atMillis] (LRU order). */
    suspend fun touch(key: FileKey, atMillis: Long)

    /** Drops the row only; [OfflineFiles.remove] also deletes the file. */
    suspend fun deleteCached(key: FileKey)
}

/** Persisted fetch jobs (`fetch_jobs`) plus the cached files they produce. */
interface FetchStore : CachedFileStore {
    /** Every job, newest first. Emits again on any change (including progress). */
    val jobs: Flow<List<FetchJob>>

    suspend fun job(id: Long): FetchJob?

    /** Jobs in [statuses], oldest first (queue order). */
    suspend fun jobsWithStatus(statuses: Set<FetchStatus>): List<FetchJob>

    /** The newest job for [key] in [statuses], if any. */
    suspend fun latestJob(key: FileKey, statuses: Set<FetchStatus>): FetchJob?

    /** Inserts [job] (its id is ignored) and returns the new id. */
    suspend fun insert(job: FetchJob): Long

    suspend fun update(job: FetchJob)

    /** Cheap progress write, leaves status and everything else alone. */
    suspend fun updateProgress(id: Long, bytesDone: Long, totalBytes: Long?, etag: String?, atMillis: Long)

    suspend fun deleteJob(id: Long)

    /** Removes finished jobs (done, failed, cancelled); their cached files stay. Returns how many. */
    suspend fun deleteFinishedJobs(): Int

    /** Ids of every job row (to keep their folders when sweeping orphans). */
    suspend fun jobIds(): Set<Long>

    /** Atomically marks [job] done and records [cached] (replacing an older copy of the same file). */
    suspend fun complete(job: FetchJob, cached: CachedFile)

    /** Drops every job and cached row of a forgotten host. */
    suspend fun deleteHost(hostId: String)
}

/** [FetchStore] in memory: JVM tests, instrumented tests that need no database, previews. */
class InMemoryFetchStore : FetchStore {
    private val jobState = MutableStateFlow<Map<Long, FetchJob>>(emptyMap())
    private val cachedState = MutableStateFlow<Map<FileKey, CachedFile>>(emptyMap())
    private val nextId = AtomicLong(1)

    override val jobs: Flow<List<FetchJob>> = jobState.map { all -> all.values.sortedByDescending { it.id } }

    override val cachedFiles: Flow<List<CachedFile>> =
        cachedState.map { all -> all.values.sortedByDescending { it.lastUsed } }

    override suspend fun job(id: Long): FetchJob? = jobState.value[id]

    override suspend fun jobsWithStatus(statuses: Set<FetchStatus>): List<FetchJob> =
        jobState.value.values.filter { it.status in statuses }.sortedBy { it.id }

    override suspend fun latestJob(key: FileKey, statuses: Set<FetchStatus>): FetchJob? =
        jobState.value.values.filter { it.key == key && it.status in statuses }.maxByOrNull { it.id }

    override suspend fun insert(job: FetchJob): Long {
        val id = nextId.getAndIncrement()
        jobState.update { it + (id to job.copy(id = id)) }
        return id
    }

    override suspend fun update(job: FetchJob) {
        jobState.update { if (job.id in it) it + (job.id to job) else it }
    }

    override suspend fun updateProgress(id: Long, bytesDone: Long, totalBytes: Long?, etag: String?, atMillis: Long) {
        jobState.update { all ->
            val current = all[id] ?: return@update all
            all + (id to current.copy(bytesDone = bytesDone, totalBytes = totalBytes, etag = etag, updatedAt = atMillis))
        }
    }

    override suspend fun deleteJob(id: Long) {
        jobState.update { it - id }
    }

    override suspend fun deleteFinishedJobs(): Int {
        var removed = 0
        jobState.update { all ->
            val left = all.filterValues { it.status !in FetchStatus.FINISHED }
            removed = all.size - left.size
            left
        }
        return removed
    }

    override suspend fun jobIds(): Set<Long> = jobState.value.keys

    override suspend fun complete(job: FetchJob, cached: CachedFile) {
        update(job)
        cachedState.update { it + (cached.key to cached) }
    }

    override suspend fun deleteHost(hostId: String) {
        jobState.update { all -> all.filterValues { it.key.hostId != hostId } }
        cachedState.update { all -> all.filterKeys { it.hostId != hostId } }
    }

    override suspend fun cached(key: FileKey): CachedFile? = cachedState.value[key]

    override suspend fun setPinned(key: FileKey, pinned: Boolean): Boolean {
        var found = false
        cachedState.update { all ->
            val current = all[key] ?: return@update all
            found = true
            all + (key to current.copy(pinned = pinned))
        }
        return found
    }

    override suspend fun touch(key: FileKey, atMillis: Long) {
        cachedState.update { all ->
            val current = all[key] ?: return@update all
            all + (key to current.copy(lastUsed = atMillis))
        }
    }

    override suspend fun deleteCached(key: FileKey) {
        cachedState.update { it - key }
    }
}
