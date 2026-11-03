package fyi.arlo.gander.core.data.fetch

import fyi.arlo.gander.core.data.browse.EntryTarget
import fyi.arlo.gander.core.net.protocol.Entry
import fyi.arlo.gander.core.net.protocol.HashAlgo

// The fetch engine's data (client spec, "Fetch engine"). Plain Kotlin: the engine, the stores and
// the JVM tests use these; Room rows are converted in `core.data.db.FetchRows`.

private const val MIB = 1024L * 1024L

/** One file on one host: what a job downloads and what a cached copy is a copy of. */
data class FileKey(val hostId: String, val shareId: String, val path: String)

val EntryTarget.fileKey: FileKey get() = FileKey(hostId, shareId, path)

/** Where a job is. Persisted by [wire]; unknown values read back as [FAILED]. */
enum class FetchStatus(val wire: String) {
    /** Waiting for a download slot (2 per host). */
    QUEUED("queued"),

    /** Bytes are coming over. */
    RUNNING("running"),

    /** Complete; the host's hash is being compared with the local file. */
    VERIFYING("verifying"),

    /** Offline, or backing off after a connection failure; resumes by itself. */
    WAITING("waiting"),

    /** Stopped by the system (foreground-service time limit) or the user; resumes on the next launch. */
    PAUSED("paused"),

    DONE("done"),

    /** Gave up; [FetchJob.failure] says why. Retry starts it again. */
    FAILED("failed"),

    CANCELLED("cancelled"),
    ;

    /** Counts toward "fetching": the foreground service runs while any job is active. */
    val active: Boolean get() = this == QUEUED || this == RUNNING || this == VERIFYING || this == WAITING

    companion object {
        val ACTIVE: Set<FetchStatus> = entries.filter { it.active }.toSet()
        val FINISHED: Set<FetchStatus> = setOf(DONE, FAILED, CANCELLED)

        fun fromWire(wire: String): FetchStatus = entries.firstOrNull { it.wire == wire } ?: FAILED
    }
}

/** Why a job failed. Typed so the UI picks the sentence; persisted by [wire]. */
enum class FetchFailure(val wire: String) {
    /** The host could not be reached or the connection kept failing (after the retries). */
    NETWORK("network"),

    /** The host refused (`permission_denied`, `bad_path`...); [FetchJob.failureCode] is the protocol code. */
    HOST_REFUSED("host_refused"),

    /** `not_found`: the file is gone, renamed or hidden on the computer. */
    NOT_FOUND("not_found"),

    /** The token is no good (revoked or invalid). */
    NOT_AUTHORISED("not_authorised"),

    /** The certificate is not the paired host's. Never retried automatically. */
    WRONG_COMPUTER("wrong_computer"),

    /** The host answered something this app does not understand. */
    INCOMPATIBLE("incompatible"),

    /** The file kept changing on the host while it was being fetched. */
    FILE_CHANGED("file_changed"),

    /** The host's hash did not match the downloaded bytes; the copy was deleted. */
    VERIFY_MISMATCH("verify_mismatch"),

    /** Not enough free space on the phone. */
    NO_SPACE("no_space"),

    /** Writing the file on the phone failed for another reason. */
    STORAGE("storage"),

    /** The host was forgotten on this phone. */
    HOST_FORGOTTEN("host_forgotten"),
    ;

    companion object {
        fun fromWire(wire: String?): FetchFailure? = entries.firstOrNull { it.wire == wire }
    }
}

/**
 * One download (Room `fetch_jobs`). [bytesDone] and [etag] are what `Range` + `If-Range` resume
 * from; [localPath] is relative to the app's `filesDir` and is where the finished file lands.
 */
data class FetchJob(
    val id: Long = 0,
    val key: FileKey,
    /** The entry as listed when the job was created (name, size, mime, flags; rebuilds the [EntryTarget]). */
    val entry: Entry,
    val status: FetchStatus = FetchStatus.QUEUED,
    val bytesDone: Long = 0,
    /** From the response (`Content-Range` total or `Content-Length`); null until the first response. */
    val totalBytes: Long? = null,
    /** Strong ETag of the version being downloaded; null before the first response. */
    val etag: String? = null,
    val failure: FetchFailure? = null,
    /** The protocol error code (or transport kind) behind [failure], for the detail line. */
    val failureCode: String? = null,
    /** Times the download started over because the file changed on the host (412 `file_changed`). */
    val restarts: Int = 0,
    /** The file changed on the host and the download started over: tell the user. */
    val changedOnHost: Boolean = false,
    /** Connection attempts that failed in a row (reset when bytes arrive). */
    val attempts: Int = 0,
    /** The algorithm the finished file was checked with, null when it was not checked. */
    val verifiedWith: HashAlgo? = null,
    /** `fetched/j<id>/<safe name>` once [id] is known; relative to `filesDir`. */
    val localPath: String = "",
    val createdAt: Long = 0,
    val updatedAt: Long = 0,
) {
    val target: EntryTarget get() = EntryTarget(key.hostId, key.shareId, key.path, entry)

    val name: String get() = entry.name

    /** Expected size: the response's when known, else the listing's. */
    val size: Long get() = totalBytes ?: entry.size

    /** 0..1, or null when the size is unknown. */
    val fraction: Float?
        get() {
            val total = size
            if (status == FetchStatus.DONE) return 1f
            return if (total > 0) (bytesDone.toFloat() / total).coerceIn(0f, 1f) else null
        }
}

/**
 * A finished download on the phone (Room `cached_files`): one per host file, the newest fetch
 * wins. [pinned] is "Keep offline": WP-28's LRU eviction never removes it. [mtime] and [size]
 * are the listing's, so a newer version on the host makes this copy stale.
 */
data class CachedFile(
    val key: FileKey,
    val name: String,
    val mime: String?,
    /** Relative to `filesDir`, under `fetched/`. */
    val localPath: String,
    val size: Long,
    val mtime: String,
    val etag: String?,
    val pinned: Boolean = false,
    val lastUsed: Long,
    val fetchedAt: Long,
    val verifiedWith: HashAlgo? = null,
) {
    /** The copy is of the version [entry] describes (same size and modification time). */
    fun matches(entry: Entry): Boolean = entry.size == size && entry.mtime == mtime
}

/** When a finished download is checked against the host's hash. */
enum class VerifyPolicy(val wire: String) {
    NEVER("never"),

    /** Files at or over [FetchSettings.verifyFromBytes] (protocol spec: default on for files over 50 MiB). */
    LARGE_FILES("large"),
    ALWAYS("always"),
}

/** The engine's user-tunable numbers; WP-28's Settings screen backs [FetchSettingsSource] with DataStore. */
data class FetchSettings(
    val verify: VerifyPolicy = VerifyPolicy.LARGE_FILES,
    val verifyFromBytes: Long = 50 * MIB,
    /** Cellular dialog threshold (client spec: 50 MiB), same as `RuleConfig.cellularWarnBytes`. */
    val cellularWarnBytes: Long = 50 * MIB,
) {
    fun shouldVerify(size: Long): Boolean = when (verify) {
        VerifyPolicy.NEVER -> false
        VerifyPolicy.ALWAYS -> true
        VerifyPolicy.LARGE_FILES -> size >= verifyFromBytes
    }
}

fun interface FetchSettingsSource {
    suspend fun current(): FetchSettings

    object Defaults : FetchSettingsSource {
        override suspend fun current() = FetchSettings()
    }
}
