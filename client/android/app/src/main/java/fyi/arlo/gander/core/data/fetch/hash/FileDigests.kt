package fyi.arlo.gander.core.data.fetch.hash

import fyi.arlo.gander.core.net.protocol.HashAlgo
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import java.io.File
import java.security.MessageDigest

/** An incremental hash with a lower-case hex result, as `/hash` reports it. */
interface StreamingDigest {
    fun update(bytes: ByteArray, offset: Int, length: Int)

    fun hex(): String
}

private class Blake3Digest : StreamingDigest {
    private val hasher = Blake3.newHasher()

    override fun update(bytes: ByteArray, offset: Int, length: Int) {
        hasher.update(bytes, offset, length)
    }

    override fun hex(): String = hasher.hexDigest()
}

private class Sha256Digest : StreamingDigest {
    private val md = MessageDigest.getInstance("SHA-256")

    override fun update(bytes: ByteArray, offset: Int, length: Int) = md.update(bytes, offset, length)

    override fun hex(): String = md.digest().toHex()
}

/** A fresh digest for [algo]; null for an algorithm this client does not implement. */
fun HashAlgo.newDigest(): StreamingDigest? = when (this) {
    HashAlgo.BLAKE3 -> Blake3Digest()
    HashAlgo.SHA256 -> Sha256Digest()
    HashAlgo.UNKNOWN -> null
}

/** Hex digest of [file] under [algo]. Cancellable between 256 KiB reads. Never logs content. */
suspend fun hashFile(file: File, algo: HashAlgo): String {
    val digest = requireNotNull(algo.newDigest()) { "unsupported hash algorithm" }
    val buffer = ByteArray(256 * 1024)
    file.inputStream().use { input ->
        while (true) {
            currentCoroutineContext().ensureActive()
            val n = input.read(buffer)
            if (n < 0) break
            digest.update(buffer, 0, n)
        }
    }
    return digest.hex()
}
