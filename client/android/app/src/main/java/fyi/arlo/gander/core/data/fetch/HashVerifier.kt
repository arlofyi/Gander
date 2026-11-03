package fyi.arlo.gander.core.data.fetch

import fyi.arlo.gander.core.data.fetch.hash.hashFile
import fyi.arlo.gander.core.data.fetch.hash.newDigest
import fyi.arlo.gander.core.data.hosts.PairedHost
import fyi.arlo.gander.core.net.GanderApi
import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.GanderHttpClients
import fyi.arlo.gander.core.net.protocol.Capabilities
import fyi.arlo.gander.core.net.protocol.HashAlgo
import fyi.arlo.gander.core.net.protocol.HashResponse
import fyi.arlo.gander.core.net.protocol.ZipPlan
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.withContext
import okhttp3.HttpUrl
import okhttp3.OkHttpClient
import java.io.File

/** What the fetch engine needs from a host: the byte-stream client, the file URL and `/hash`. */
interface FetchConnector {
    fun transfer(host: PairedHost): OkHttpClient

    fun fileUrl(host: PairedHost, share: String, path: String): HttpUrl

    suspend fun hash(host: PairedHost, share: String, path: String, algo: HashAlgo): HashResponse

    /** URL of a folder as one zip (`GET /v1/zip`, WP-68). */
    fun zipUrl(host: PairedHost, share: String, path: String): HttpUrl

    /** `GET /v1/zip?dry_run=1`: the totals a folder download would have. */
    suspend fun zipPlan(host: PairedHost, share: String, path: String): ZipPlan
}

/** [FetchConnector] over each host's pinned clients (`PinnedHostConnector.clients`). */
class ClientsFetchConnector(private val clients: (PairedHost) -> GanderHttpClients) : FetchConnector {
    override fun transfer(host: PairedHost): OkHttpClient = clients(host).transfer

    override fun fileUrl(host: PairedHost, share: String, path: String): HttpUrl =
        GanderApi(clients(host)).fileUrl(share, path)

    override suspend fun hash(host: PairedHost, share: String, path: String, algo: HashAlgo): HashResponse =
        GanderApi(clients(host)).hash(share, path, algo)

    override fun zipUrl(host: PairedHost, share: String, path: String): HttpUrl =
        GanderApi(clients(host)).zipUrl(share, path)

    override suspend fun zipPlan(host: PairedHost, share: String, path: String): ZipPlan =
        GanderApi(clients(host)).zipPlan(share, path)
}

/** Result of checking a finished download against the host. */
sealed interface VerifyOutcome {
    data class Match(val algo: HashAlgo) : VerifyOutcome

    /** Different bytes (or size): the copy is wrong and must go. */
    data class Mismatch(val algo: HashAlgo) : VerifyOutcome

    /** The host hashed a different version (ETag differs, or 412 `file_changed`): fetch again. */
    data object Changed : VerifyOutcome
}

/**
 * The algorithm to verify with: BLAKE3 when the host offers it (or its capabilities are not known
 * yet; every v1 host has both), else SHA-256, else null (cannot verify).
 */
fun verifyAlgorithm(capabilities: Capabilities?): HashAlgo? {
    val offered = capabilities?.fetch?.hash ?: return HashAlgo.BLAKE3
    return when {
        HashAlgo.BLAKE3 in offered -> HashAlgo.BLAKE3
        HashAlgo.SHA256 in offered -> HashAlgo.SHA256
        else -> null
    }
}

/**
 * Compares a downloaded file with `GET /hash` (protocol spec: blake3 or sha256, cached on the host
 * by ETag, 503 `generating` while it computes). Polls through 503 for up to [maxWaitMs], honouring
 * `retry_after_ms` within [minPollMs]..[maxPollMs]; other errors propagate as [GanderError].
 */
class HashVerifier(
    private val sleep: suspend (Long) -> Unit = { delay(it) },
    private val clock: () -> Long = System::currentTimeMillis,
    private val maxWaitMs: Long = 10 * 60 * 1000L,
    private val minPollMs: Long = 250,
    private val maxPollMs: Long = 10_000,
) {
    suspend fun verify(
        hash: suspend (HashAlgo) -> HashResponse,
        file: File,
        algo: HashAlgo,
        etag: String?,
    ): VerifyOutcome {
        val answer = poll(hash, algo) ?: return VerifyOutcome.Changed
        if (etag != null && answer.etag != etag) return VerifyOutcome.Changed
        // The host may answer with the algorithm it has; check with that one if this app has it.
        val used = answer.algo.takeIf { it.newDigest() != null } ?: algo
        if (answer.size != file.length()) return VerifyOutcome.Mismatch(used)
        val local = withContext(Dispatchers.Default) { hashFile(file, used) }
        return if (local.equals(answer.hex.trim(), ignoreCase = true)) {
            VerifyOutcome.Match(used)
        } else {
            VerifyOutcome.Mismatch(used)
        }
    }

    /** The host's answer, or null when it says the file changed. */
    private suspend fun poll(hash: suspend (HashAlgo) -> HashResponse, algo: HashAlgo): HashResponse? {
        val deadline = clock() + maxWaitMs
        while (true) {
            try {
                return hash(algo)
            } catch (e: GanderError.Host.Generating) {
                if (clock() >= deadline) throw e
                sleep(e.retryAfterMs.coerceIn(minPollMs, maxPollMs))
            } catch (_: GanderError.Host.FileChanged) {
                return null
            }
        }
    }
}
