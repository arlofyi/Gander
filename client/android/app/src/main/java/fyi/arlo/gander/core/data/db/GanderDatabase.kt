package fyi.arlo.gander.core.data.db

import android.content.Context
import androidx.room.AutoMigration
import androidx.room.ColumnInfo
import androidx.room.Dao
import androidx.room.Database
import androidx.room.Entity
import androidx.room.ForeignKey
import androidx.room.Index
import androidx.room.Insert
import androidx.room.OnConflictStrategy
import androidx.room.PrimaryKey
import androidx.room.Query
import androidx.room.Room
import androidx.room.RoomDatabase
import androidx.room.Transaction
import kotlinx.coroutines.flow.Flow

/**
 * Room tables of the Android client (client spec, "Data and cache"). Version 1 (WP-23): `hosts`,
 * `shares` and `listing_snapshots`. Version 2 (WP-27): `fetch_jobs` and `cached_files`, added by
 * an auto-migration (schemas are exported to `app/schemas/`; never edit an exported one).
 *
 * Deviation from the spec text: the per-folder snapshot table is called `listing_snapshots`, not
 * `entries`, because one row is the stored listing of one folder, not one entry.
 *
 * Everything here is excluded from backup and device transfer (`data_extraction_rules.xml`).
 */
@Database(
    entities = [
        HostEntity::class,
        ShareEntity::class,
        ListingSnapshotEntity::class,
        FetchJobEntity::class,
        CachedFileEntity::class,
    ],
    version = 2,
    exportSchema = true,
    autoMigrations = [AutoMigration(from = 1, to = 2)],
)
abstract class GanderDatabase : RoomDatabase() {
    abstract fun hostDao(): HostDao

    abstract fun shareDao(): ShareDao

    abstract fun listingSnapshotDao(): ListingSnapshotDao

    abstract fun fetchDao(): FetchDao

    companion object {
        const val FILE_NAME = "gander.db"

        fun create(context: Context): GanderDatabase =
            Room.databaseBuilder(context.applicationContext, GanderDatabase::class.java, FILE_NAME).build()
    }
}

/** `hosts`: one row per paired computer. The token is NOT stored here (see `TokenStore`). */
@Entity(tableName = "hosts")
data class HostEntity(
    @PrimaryKey val id: String,
    val name: String,
    /** JSON array of address strings, preference order. */
    val addresses: String,
    val port: Int,
    /** Same value as [id] today; kept as its own column so a future id scheme can differ. */
    val pin: String,
    val os: String,
    @ColumnInfo(name = "paired_as") val pairedAs: String?,
    @ColumnInfo(name = "last_seen") val lastSeen: Long?,
    /** JSON of `/me.capabilities`, null until the first `/me`. */
    val capabilities: String?,
    /** Insertion order for the Hosts screen. */
    @ColumnInfo(name = "paired_at") val pairedAt: Long,
)

/** `shares`: the last `GET /shares` of a host, one row per share, replaced as a whole. */
@Entity(
    tableName = "shares",
    primaryKeys = ["host_id", "share_id"],
    foreignKeys = [
        ForeignKey(
            entity = HostEntity::class,
            parentColumns = ["id"],
            childColumns = ["host_id"],
            onDelete = ForeignKey.CASCADE,
        ),
    ],
)
data class ShareEntity(
    @ColumnInfo(name = "host_id") val hostId: String,
    @ColumnInfo(name = "share_id") val shareId: String,
    /** Position in the host's answer, to keep its order. */
    val position: Int,
    /** JSON of the `ShareInfo`. */
    val json: String,
    @ColumnInfo(name = "fetched_at") val fetchedAt: Long,
)

/** `listing_snapshots`: the stored listing of one folder (pages loaded so far). */
@Entity(
    tableName = "listing_snapshots",
    primaryKeys = ["host_id", "share_id", "path"],
    foreignKeys = [
        ForeignKey(
            entity = HostEntity::class,
            parentColumns = ["id"],
            childColumns = ["host_id"],
            onDelete = ForeignKey.CASCADE,
        ),
    ],
    indices = [Index(value = ["fetched_at"])],
)
data class ListingSnapshotEntity(
    @ColumnInfo(name = "host_id") val hostId: String,
    @ColumnInfo(name = "share_id") val shareId: String,
    val path: String,
    /** The request the entries answer, `<sort>|<order>|<kinds>|<ext,...>`. */
    val query: String,
    val etag: String,
    /** JSON array of protocol `Entry` objects, in host order. */
    @ColumnInfo(name = "entries_json") val entriesJson: String,
    @ColumnInfo(name = "next_cursor") val nextCursor: String?,
    val truncated: Boolean,
    val unlistable: Int,
    @ColumnInfo(name = "fetched_at") val fetchedAt: Long,
)

@Dao
interface HostDao {
    @Query("SELECT * FROM hosts ORDER BY paired_at ASC, name ASC")
    fun observeAll(): Flow<List<HostEntity>>

    @Query("SELECT * FROM hosts WHERE id = :id")
    suspend fun get(id: String): HostEntity?

    @Insert(onConflict = OnConflictStrategy.REPLACE)
    suspend fun upsert(host: HostEntity)

    @Query("UPDATE hosts SET name = :name WHERE id = :id")
    suspend fun rename(id: String, name: String)

    @Query("DELETE FROM hosts WHERE id = :id")
    suspend fun delete(id: String)

    @Query("UPDATE hosts SET last_seen = :atMillis WHERE id = :id")
    suspend fun updateLastSeen(id: String, atMillis: Long)

    @Query(
        "UPDATE hosts SET addresses = :addresses, os = :os, paired_as = :pairedAs, " +
            "capabilities = :capabilities, last_seen = :atMillis WHERE id = :id",
    )
    suspend fun updateFromMe(
        id: String,
        addresses: String,
        os: String,
        pairedAs: String?,
        capabilities: String?,
        atMillis: Long,
    )
}

@Dao
interface ShareDao {
    @Query("SELECT * FROM shares WHERE host_id = :hostId ORDER BY position ASC")
    suspend fun forHost(hostId: String): List<ShareEntity>

    @Query("DELETE FROM shares WHERE host_id = :hostId")
    suspend fun deleteForHost(hostId: String)

    @Insert(onConflict = OnConflictStrategy.REPLACE)
    suspend fun insertAll(shares: List<ShareEntity>)

    @Transaction
    suspend fun replaceForHost(hostId: String, shares: List<ShareEntity>) {
        deleteForHost(hostId)
        insertAll(shares)
    }
}

@Dao
interface ListingSnapshotDao {
    @Query("SELECT * FROM listing_snapshots WHERE host_id = :hostId AND share_id = :shareId AND path = :path")
    suspend fun get(hostId: String, shareId: String, path: String): ListingSnapshotEntity?

    @Insert(onConflict = OnConflictStrategy.REPLACE)
    suspend fun upsert(snapshot: ListingSnapshotEntity)

    /** Keeps the newest [keep] snapshots overall; a cache, not an archive. */
    @Query(
        "DELETE FROM listing_snapshots WHERE rowid IN (" +
            "SELECT rowid FROM listing_snapshots ORDER BY fetched_at DESC LIMIT -1 OFFSET :keep)",
    )
    suspend fun pruneToNewest(keep: Int)

    @Query("SELECT COUNT(*) FROM listing_snapshots")
    suspend fun count(): Int

    @Transaction
    suspend fun upsertAndPrune(snapshot: ListingSnapshotEntity, keep: Int) {
        upsert(snapshot)
        pruneToNewest(keep)
    }
}
