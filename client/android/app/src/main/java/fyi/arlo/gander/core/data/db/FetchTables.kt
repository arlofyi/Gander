package fyi.arlo.gander.core.data.db

import androidx.room.ColumnInfo
import androidx.room.Dao
import androidx.room.Entity
import androidx.room.Index
import androidx.room.Insert
import androidx.room.OnConflictStrategy
import androidx.room.PrimaryKey
import androidx.room.Query
import androidx.room.Transaction
import androidx.room.Update
import fyi.arlo.gander.core.data.fetch.CachedFile
import fyi.arlo.gander.core.data.fetch.FetchFailure
import fyi.arlo.gander.core.data.fetch.FetchJob
import fyi.arlo.gander.core.data.fetch.FetchStatus
import fyi.arlo.gander.core.data.fetch.FetchStore
import fyi.arlo.gander.core.data.fetch.FileKey
import fyi.arlo.gander.core.net.GanderJson
import fyi.arlo.gander.core.net.protocol.Entry
import fyi.arlo.gander.core.net.protocol.HashAlgo
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.map

// Tables of the fetch engine (WP-27), added in database version 2 by an auto-migration. No
// foreign key to `hosts`: the engine also has bytes on disk to delete when a host goes, so it
// removes a forgotten host's rows itself (FetchEngine.forgetHost, and a sweep at start).

/** `fetch_jobs`: one download, its resume point and outcome. */
@Entity(
    tableName = "fetch_jobs",
    indices = [Index(value = ["host_id", "share_id", "path"]), Index(value = ["status"])],
)
data class FetchJobEntity(
    @PrimaryKey(autoGenerate = true) val id: Long = 0,
    @ColumnInfo(name = "host_id") val hostId: String,
    @ColumnInfo(name = "share_id") val shareId: String,
    val path: String,
    /** JSON of the protocol `Entry` as listed when the job was made. */
    @ColumnInfo(name = "entry_json") val entryJson: String,
    val status: String,
    @ColumnInfo(name = "bytes_done") val bytesDone: Long,
    @ColumnInfo(name = "total_bytes") val totalBytes: Long?,
    val etag: String?,
    val failure: String?,
    @ColumnInfo(name = "failure_code") val failureCode: String?,
    val restarts: Int,
    @ColumnInfo(name = "changed_on_host") val changedOnHost: Boolean,
    val attempts: Int,
    @ColumnInfo(name = "verified_with") val verifiedWith: String?,
    @ColumnInfo(name = "local_path") val localPath: String,
    @ColumnInfo(name = "created_at") val createdAt: Long,
    @ColumnInfo(name = "updated_at") val updatedAt: Long,
)

/** `cached_files` (client spec: local path, size, etag, pinned, last_used): finished downloads. */
@Entity(
    tableName = "cached_files",
    primaryKeys = ["host_id", "share_id", "path"],
    indices = [Index(value = ["last_used"])],
)
data class CachedFileEntity(
    @ColumnInfo(name = "host_id") val hostId: String,
    @ColumnInfo(name = "share_id") val shareId: String,
    val path: String,
    val name: String,
    val mime: String?,
    @ColumnInfo(name = "local_path") val localPath: String,
    val size: Long,
    val mtime: String,
    val etag: String?,
    val pinned: Boolean,
    @ColumnInfo(name = "last_used") val lastUsed: Long,
    @ColumnInfo(name = "fetched_at") val fetchedAt: Long,
    @ColumnInfo(name = "verified_with") val verifiedWith: String?,
)

@Dao
interface FetchDao {
    @Query("SELECT * FROM fetch_jobs ORDER BY id DESC")
    fun observeJobs(): Flow<List<FetchJobEntity>>

    @Query("SELECT * FROM fetch_jobs WHERE id = :id")
    suspend fun job(id: Long): FetchJobEntity?

    @Query("SELECT * FROM fetch_jobs WHERE status IN (:statuses) ORDER BY id ASC")
    suspend fun jobsWithStatus(statuses: List<String>): List<FetchJobEntity>

    @Query(
        "SELECT * FROM fetch_jobs WHERE host_id = :hostId AND share_id = :shareId AND path = :path " +
            "AND status IN (:statuses) ORDER BY id DESC LIMIT 1",
    )
    suspend fun latestJob(hostId: String, shareId: String, path: String, statuses: List<String>): FetchJobEntity?

    @Insert
    suspend fun insert(job: FetchJobEntity): Long

    @Update
    suspend fun update(job: FetchJobEntity)

    @Query(
        "UPDATE fetch_jobs SET bytes_done = :bytesDone, total_bytes = :totalBytes, etag = :etag, " +
            "updated_at = :atMillis WHERE id = :id",
    )
    suspend fun updateProgress(id: Long, bytesDone: Long, totalBytes: Long?, etag: String?, atMillis: Long)

    @Query("DELETE FROM fetch_jobs WHERE id = :id")
    suspend fun deleteJob(id: Long)

    @Query("DELETE FROM fetch_jobs WHERE status IN (:statuses)")
    suspend fun deleteJobsWithStatus(statuses: List<String>): Int

    @Query("SELECT id FROM fetch_jobs")
    suspend fun jobIds(): List<Long>

    @Query("DELETE FROM fetch_jobs WHERE host_id = :hostId")
    suspend fun deleteJobsOfHost(hostId: String)

    @Query("SELECT * FROM cached_files ORDER BY last_used DESC")
    fun observeCached(): Flow<List<CachedFileEntity>>

    @Query("SELECT * FROM cached_files WHERE host_id = :hostId AND share_id = :shareId AND path = :path")
    suspend fun cached(hostId: String, shareId: String, path: String): CachedFileEntity?

    @Insert(onConflict = OnConflictStrategy.REPLACE)
    suspend fun upsertCached(file: CachedFileEntity)

    @Query("UPDATE cached_files SET pinned = :pinned WHERE host_id = :hostId AND share_id = :shareId AND path = :path")
    suspend fun setPinned(hostId: String, shareId: String, path: String, pinned: Boolean): Int

    @Query("UPDATE cached_files SET last_used = :atMillis WHERE host_id = :hostId AND share_id = :shareId AND path = :path")
    suspend fun touch(hostId: String, shareId: String, path: String, atMillis: Long)

    @Query("DELETE FROM cached_files WHERE host_id = :hostId AND share_id = :shareId AND path = :path")
    suspend fun deleteCached(hostId: String, shareId: String, path: String)

    @Query("DELETE FROM cached_files WHERE host_id = :hostId")
    suspend fun deleteCachedOfHost(hostId: String)

    @Transaction
    suspend fun complete(job: FetchJobEntity, cached: CachedFileEntity) {
        update(job)
        upsertCached(cached)
    }

    @Transaction
    suspend fun deleteHost(hostId: String) {
        deleteJobsOfHost(hostId)
        deleteCachedOfHost(hostId)
    }
}

/** Room-backed [FetchStore]. Rows this version cannot read (a newer app's) are skipped. */
class RoomFetchStore(private val dao: FetchDao) : FetchStore {

    override val jobs: Flow<List<FetchJob>> = dao.observeJobs().map { rows -> rows.mapNotNull(FetchRows::toJobOrNull) }

    override val cachedFiles: Flow<List<CachedFile>> = dao.observeCached().map { rows -> rows.map(FetchRows::toCached) }

    override suspend fun job(id: Long): FetchJob? = dao.job(id)?.let(FetchRows::toJobOrNull)

    override suspend fun jobsWithStatus(statuses: Set<FetchStatus>): List<FetchJob> =
        dao.jobsWithStatus(statuses.map { it.wire }).mapNotNull(FetchRows::toJobOrNull)

    override suspend fun latestJob(key: FileKey, statuses: Set<FetchStatus>): FetchJob? =
        dao.latestJob(key.hostId, key.shareId, key.path, statuses.map { it.wire })?.let(FetchRows::toJobOrNull)

    override suspend fun insert(job: FetchJob): Long = dao.insert(FetchRows.toEntity(job.copy(id = 0)))

    override suspend fun update(job: FetchJob) = dao.update(FetchRows.toEntity(job))

    override suspend fun updateProgress(id: Long, bytesDone: Long, totalBytes: Long?, etag: String?, atMillis: Long) =
        dao.updateProgress(id, bytesDone, totalBytes, etag, atMillis)

    override suspend fun deleteJob(id: Long) = dao.deleteJob(id)

    override suspend fun deleteFinishedJobs(): Int = dao.deleteJobsWithStatus(FetchStatus.FINISHED.map { it.wire })

    override suspend fun jobIds(): Set<Long> = dao.jobIds().toSet()

    override suspend fun complete(job: FetchJob, cached: CachedFile) =
        dao.complete(FetchRows.toEntity(job), FetchRows.toEntity(cached))

    override suspend fun deleteHost(hostId: String) = dao.deleteHost(hostId)

    override suspend fun cached(key: FileKey): CachedFile? =
        dao.cached(key.hostId, key.shareId, key.path)?.let(FetchRows::toCached)

    override suspend fun setPinned(key: FileKey, pinned: Boolean): Boolean =
        dao.setPinned(key.hostId, key.shareId, key.path, pinned) > 0

    override suspend fun touch(key: FileKey, atMillis: Long) = dao.touch(key.hostId, key.shareId, key.path, atMillis)

    override suspend fun deleteCached(key: FileKey) = dao.deleteCached(key.hostId, key.shareId, key.path)
}

/** Row <-> domain conversion for the fetch tables; pure Kotlin, covered by JVM tests. */
internal object FetchRows {
    fun toEntity(job: FetchJob) = FetchJobEntity(
        id = job.id,
        hostId = job.key.hostId,
        shareId = job.key.shareId,
        path = job.key.path,
        entryJson = GanderJson.json.encodeToString(Entry.serializer(), job.entry),
        status = job.status.wire,
        bytesDone = job.bytesDone,
        totalBytes = job.totalBytes,
        etag = job.etag,
        failure = job.failure?.wire,
        failureCode = job.failureCode,
        restarts = job.restarts,
        changedOnHost = job.changedOnHost,
        attempts = job.attempts,
        verifiedWith = job.verifiedWith?.wire,
        localPath = job.localPath,
        createdAt = job.createdAt,
        updatedAt = job.updatedAt,
    )

    fun toJobOrNull(row: FetchJobEntity): FetchJob? {
        val entry = runCatching { GanderJson.json.decodeFromString(Entry.serializer(), row.entryJson) }.getOrNull()
            ?: return null
        return FetchJob(
            id = row.id,
            key = FileKey(row.hostId, row.shareId, row.path),
            entry = entry,
            status = FetchStatus.fromWire(row.status),
            bytesDone = row.bytesDone,
            totalBytes = row.totalBytes,
            etag = row.etag,
            failure = FetchFailure.fromWire(row.failure),
            failureCode = row.failureCode,
            restarts = row.restarts,
            changedOnHost = row.changedOnHost,
            attempts = row.attempts,
            verifiedWith = algoOf(row.verifiedWith),
            localPath = row.localPath,
            createdAt = row.createdAt,
            updatedAt = row.updatedAt,
        )
    }

    fun toEntity(file: CachedFile) = CachedFileEntity(
        hostId = file.key.hostId,
        shareId = file.key.shareId,
        path = file.key.path,
        name = file.name,
        mime = file.mime,
        localPath = file.localPath,
        size = file.size,
        mtime = file.mtime,
        etag = file.etag,
        pinned = file.pinned,
        lastUsed = file.lastUsed,
        fetchedAt = file.fetchedAt,
        verifiedWith = file.verifiedWith?.wire,
    )

    fun toCached(row: CachedFileEntity) = CachedFile(
        key = FileKey(row.hostId, row.shareId, row.path),
        name = row.name,
        mime = row.mime,
        localPath = row.localPath,
        size = row.size,
        mtime = row.mtime,
        etag = row.etag,
        pinned = row.pinned,
        lastUsed = row.lastUsed,
        fetchedAt = row.fetchedAt,
        verifiedWith = algoOf(row.verifiedWith),
    )

    private fun algoOf(wire: String?): HashAlgo? =
        wire?.let { w -> HashAlgo.entries.firstOrNull { it.wire == w && it != HashAlgo.UNKNOWN } }
}
