package fyi.arlo.gander.core.data.fetch.hash

import fyi.arlo.gander.core.net.protocol.HashAlgo
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Test
import java.io.File
import java.security.MessageDigest

/**
 * The pure-Kotlin BLAKE3 against the official test vectors (`test_vectors/test_vectors.json` of
 * the BLAKE3 reference repository, CC0 / Apache-2.0, copied unmodified into test resources).
 * Every case is checked in all three modes, at the default 32-byte length and at the full
 * extended length, fed in one call and in awkward pieces.
 */
class Blake3Test {

    private class Case(val inputLen: Int, val hash: String, val keyedHash: String, val deriveKey: String)

    private val vectors: Triple<ByteArray, String, List<Case>> by lazy {
        val text = requireNotNull(javaClass.classLoader?.getResourceAsStream("blake3/test_vectors.json")) {
            "blake3/test_vectors.json missing from test resources"
        }.use { it.readBytes().toString(Charsets.UTF_8) }
        val root = Json.parseToJsonElement(text).jsonObject
        val key = root.getValue("key").jsonPrimitive.content.toByteArray(Charsets.US_ASCII)
        val context = root.getValue("context_string").jsonPrimitive.content
        val cases = root.getValue("cases").jsonArray.map {
            val o = it.jsonObject
            Case(
                o.getValue("input_len").jsonPrimitive.int,
                o.getValue("hash").jsonPrimitive.content,
                o.getValue("keyed_hash").jsonPrimitive.content,
                o.getValue("derive_key").jsonPrimitive.content,
            )
        }
        Triple(key, context, cases)
    }

    private fun input(len: Int) = ByteArray(len) { (it % 251).toByte() }

    @Test
    fun allOfficialVectorsInAllModes() {
        val (key, context, cases) = vectors
        assertEquals(35, cases.size)
        for (c in cases) {
            val data = input(c.inputLen)
            val extended = c.hash.length / 2
            check("hash", c, c.hash, extended) { Blake3.newHasher() }
            check("keyed_hash", c, c.keyedHash, extended) { Blake3.newKeyed(key) }
            check("derive_key", c, c.deriveKey, extended) { Blake3.newDeriveKey(context) }
            assertEquals(c.hash.substring(0, 64), Blake3.hash(data).toHex())
        }
    }

    private fun check(mode: String, c: Case, expected: String, extended: Int, make: () -> Blake3) {
        val data = input(c.inputLen)
        val whole = make().update(data)
        assertEquals("$mode len=${c.inputLen} (32 bytes)", expected.substring(0, 64), whole.hexDigest())
        assertEquals("$mode len=${c.inputLen} (extended)", expected, whole.hexDigest(extended))
        // Pieces that straddle block (64) and chunk (1024) boundaries.
        val pieces = make()
        var pos = 0
        var step = 1
        while (pos < data.size) {
            val n = minOf(step, data.size - pos)
            pieces.update(data, pos, n)
            pos += n
            step = step * 3 + 7
        }
        assertEquals("$mode len=${c.inputLen} (in pieces)", expected, pieces.hexDigest(extended))
    }

    @Test
    fun digestDoesNotConsumeTheHasher() {
        val hasher = Blake3.newHasher().update("ab".toByteArray())
        hasher.digest()
        hasher.update("c".toByteArray())
        assertEquals("6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85", hasher.hexDigest())
    }

    @Test
    fun hashFileMatchesInMemoryHashBothAlgorithms() = runTest {
        val data = input(3 * 1024 * 1024 + 17)
        val file = File.createTempFile("blake3", ".bin").apply { deleteOnExit() }
        try {
            file.writeBytes(data)
            assertEquals(Blake3.hash(data).toHex(), hashFile(file, HashAlgo.BLAKE3))
            val sha = MessageDigest.getInstance("SHA-256").digest(data).toHex()
            assertEquals(sha, hashFile(file, HashAlgo.SHA256))
        } finally {
            file.delete()
        }
    }
}
