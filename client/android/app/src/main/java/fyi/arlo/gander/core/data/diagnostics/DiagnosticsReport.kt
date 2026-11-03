package fyi.arlo.gander.core.data.diagnostics

import fyi.arlo.gander.core.data.actions.NetworkState
import fyi.arlo.gander.core.data.fetch.CachedFile
import fyi.arlo.gander.core.data.fetch.FetchJob
import fyi.arlo.gander.core.data.fetch.FetchStatus
import fyi.arlo.gander.core.data.hosts.HostRepository
import fyi.arlo.gander.core.data.hosts.PairedHost
import fyi.arlo.gander.core.data.settings.AppSettings
import kotlinx.coroutines.flow.first
import java.time.Instant

/** What the phone is, for the top of the report. Nothing here identifies the owner. */
data class DiagnosticsDevice(
    val appVersion: String,
    val androidRelease: String,
    val sdk: Int,
    val manufacturer: String,
    val model: String,
)

/**
 * Raw facts the report is built from (including values that are sensitive: names, addresses,
 * paths inside [cachedFiles] and [jobs]). It is only ever passed to [DiagnosticsReport.render],
 * which decides what of it may be written down.
 */
data class DiagnosticsSnapshot(
    val generatedAtMillis: Long,
    val device: DiagnosticsDevice,
    val settings: AppSettings,
    val network: NetworkState,
    val privateNetworkUp: Boolean?,
    val hosts: List<PairedHost>,
    val cachedFiles: List<CachedFile>,
    val jobs: List<FetchJob>,
    /** Thumbnail and preview disk caches. */
    val derivedCacheBytes: Long,
    val freeBytes: Long?,
    /** Bearer tokens of the paired hosts: never written, only searched for and blanked. */
    val secrets: List<String> = emptyList(),
)

/**
 * The diagnostics export (client spec, "Security on the device": a local, redacted export; no
 * analytics, no crash SDK). A plain-text summary of this phone's state built so that it holds
 * what support needs and nothing that identifies the owner or their files:
 *
 * - tokens and pairing secrets: never present;
 * - host addresses: masked ([DiagnosticsRedactor.maskAddress]);
 * - host, device and file names, and file paths: only a per-export hash ([DiagnosticsRedactor.tag]);
 *   for files the extension, size and state are kept;
 * - login names: never present;
 * - host ids (SPKI pins): kept, they are public.
 *
 * The finished text still goes through [DiagnosticsRedactor.redact] as a last net.
 */
object DiagnosticsReport {
    const val MAX_FILE_LINES = 200

    /** The redactor that knows every secret and name in [snapshot], for [render]. */
    fun redactorFor(snapshot: DiagnosticsSnapshot, salt: ByteArray = DiagnosticsRedactor.randomSalt()): DiagnosticsRedactor {
        val names = buildMap {
            snapshot.hosts.forEach { host ->
                put(host.name, "host")
                host.pairedAs?.let { put(it, "device") }
            }
            snapshot.cachedFiles.forEach { put(it.name, "file") }
            snapshot.jobs.forEach { put(it.name, "file") }
        }
        return DiagnosticsRedactor(secrets = snapshot.secrets, names = names, salt = salt)
    }

    fun render(snapshot: DiagnosticsSnapshot, redactor: DiagnosticsRedactor = redactorFor(snapshot)): String {
        val out = StringBuilder()
        fun line(text: String = "") {
            out.append(text).append('\n')
        }
        val settings = snapshot.settings

        line("Gander diagnostics (redacted: no tokens, addresses, names or paths)")
        line("generated: ${Instant.ofEpochMilli(snapshot.generatedAtMillis)}")
        line()
        line("[app]")
        line("version: ${snapshot.device.appVersion}")
        line("android: ${snapshot.device.androidRelease} (api ${snapshot.device.sdk})")
        line("device: ${snapshot.device.manufacturer} ${snapshot.device.model}".trim())
        line()
        line("[settings]")
        line("verify_downloads: ${settings.verify.wire}")
        line("cellular_warn_bytes: ${if (settings.cellularWarnBytes == AppSettings.CELLULAR_NEVER) "never" else settings.cellularWarnBytes}")
        line("cache_cap_bytes: ${settings.cacheCapBytes}")
        line("stream_first_types: ${settings.streamOnlyTypes.size}")
        line("app_lock: ${if (settings.appLockEnabled) "on, after ${settings.appLockMinutes} min" else "off"}")
        line()
        line("[network]")
        line("connected: ${snapshot.network.connected}")
        line("metered: ${snapshot.network.metered}")
        line("private_network_up: ${snapshot.privateNetworkUp ?: "unknown"}")
        line()
        line("[computers] ${snapshot.hosts.size}")
        snapshot.hosts.forEach { host -> hostLine(host, redactor, ::line) }
        line()
        line("[storage]")
        val pinned = snapshot.cachedFiles.filter { it.pinned }
        line("fetched_files: ${snapshot.cachedFiles.size} (${snapshot.cachedFiles.sumOf { it.size }} bytes)")
        line("kept_offline: ${pinned.size} (${pinned.sumOf { it.size }} bytes)")
        line("thumbnail_and_preview_cache_bytes: ${snapshot.derivedCacheBytes}")
        line("free_bytes: ${snapshot.freeBytes ?: "unknown"}")
        snapshot.cachedFiles.take(MAX_FILE_LINES).forEach { cached ->
            line(
                "file ${redactor.tag("file", cached.name)} ext=${extensionOf(cached.name)} size=${cached.size} " +
                    "pinned=${cached.pinned} verified=${cached.verifiedWith?.wire ?: "no"} " +
                    "host=${redactor.tag("host", hostNameOf(snapshot, cached.key.hostId))} " +
                    "fetched=${Instant.ofEpochMilli(cached.fetchedAt)} last_used=${Instant.ofEpochMilli(cached.lastUsed)}",
            )
        }
        if (snapshot.cachedFiles.size > MAX_FILE_LINES) line("... ${snapshot.cachedFiles.size - MAX_FILE_LINES} more")
        line()
        val byStatus = snapshot.jobs.groupingBy { it.status }.eachCount()
        line("[downloads] ${snapshot.jobs.size}")
        line("by_status: " + FetchStatus.entries.filter { it in byStatus }.joinToString(" ") { "${it.wire}=${byStatus[it]}" })
        snapshot.jobs.take(MAX_FILE_LINES).forEach { job ->
            line(
                "job ${job.id} ${redactor.tag("file", job.name)} ext=${extensionOf(job.name)} status=${job.status.wire} " +
                    "bytes=${job.bytesDone}/${job.size} " +
                    "host=${redactor.tag("host", hostNameOf(snapshot, job.key.hostId))} " +
                    "failure=${job.failure?.wire ?: "-"} code=${job.failureCode ?: "-"} " +
                    "restarts=${job.restarts} attempts=${job.attempts} verified=${job.verifiedWith?.wire ?: "no"} " +
                    "updated=${Instant.ofEpochMilli(job.updatedAt)}",
            )
        }
        if (snapshot.jobs.size > MAX_FILE_LINES) line("... ${snapshot.jobs.size - MAX_FILE_LINES} more")
        return redactor.redact(out.toString())
    }

    private fun hostLine(host: PairedHost, redactor: DiagnosticsRedactor, line: (String) -> Unit) {
        val stream = host.capabilities?.stream
        val capabilities = host.capabilities?.let {
            "stream=${if (stream?.enabled == true) "on" else "off"} write=${it.write} range=${it.fetch.range} " +
                "hash=${it.fetch.hash.joinToString("+") { algo -> algo.wire }.ifEmpty { "none" }}"
        } ?: "capabilities=unknown"
        line(
            "computer ${redactor.tag("host", host.name)} id=${host.pin.hostId} os=${host.os.wire} " +
                "addresses=${host.addresses.joinToString(",") { redactor.maskAddress(it) }} port=${host.port} " +
                "paired_as=${host.pairedAs?.let { redactor.tag("device", it) } ?: "-"} " +
                "last_seen=${host.lastSeenAtMillis?.let { Instant.ofEpochMilli(it).toString() } ?: "never"} $capabilities",
        )
    }

    private fun hostNameOf(snapshot: DiagnosticsSnapshot, hostId: String): String =
        snapshot.hosts.firstOrNull { it.id == hostId }?.name ?: "forgotten-$hostId"

    /** The extension only: enough to see that `.psd` files fail, not which file. */
    private fun extensionOf(name: String): String = name.takeIf { it.lastIndexOf('.') > 0 }
        ?.substringAfterLast('.')?.lowercase()?.takeIf { it.length in 1..12 && it.all { c -> c.isLetterOrDigit() } } ?: "-"
}

/** Collects a [DiagnosticsSnapshot] from the app's stores. The app wires the lambdas from `AppGraph`. */
class DiagnosticsCollector(
    private val device: () -> DiagnosticsDevice,
    private val settings: suspend () -> AppSettings,
    private val network: () -> NetworkState,
    private val privateNetworkUp: suspend () -> Boolean?,
    private val hosts: HostRepository,
    private val cachedFiles: suspend () -> List<CachedFile>,
    private val jobs: suspend () -> List<FetchJob>,
    private val derivedCacheBytes: suspend () -> Long,
    private val freeBytes: () -> Long?,
    private val clock: () -> Long = System::currentTimeMillis,
) {
    suspend fun collect(): DiagnosticsSnapshot {
        val paired = hosts.hosts.first()
        return DiagnosticsSnapshot(
            generatedAtMillis = clock(),
            device = device(),
            settings = settings(),
            network = network(),
            privateNetworkUp = privateNetworkUp(),
            hosts = paired,
            cachedFiles = cachedFiles(),
            jobs = jobs(),
            derivedCacheBytes = derivedCacheBytes(),
            freeBytes = freeBytes(),
            secrets = paired.mapNotNull { hosts.token(it.id)?.value },
        )
    }

    /** The finished report text. */
    suspend fun report(): String = DiagnosticsReport.render(collect())
}
