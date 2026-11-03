package fyi.arlo.gander.core.data.fetch.hash

/**
 * BLAKE3 in plain Kotlin, written from the BLAKE3 specification (Aumasson, Neves, O'Connor,
 * Wilcox-O'Hearn, 2020; the reference design is CC0 / Apache-2.0). Portable and single-threaded:
 * no SIMD, no tree parallelism, which is fine for checking a download once on the phone.
 *
 * Supports the three modes (hash, keyed hash, derive key) and extended output, so it is checked
 * against every column of the official test vectors (`src/test/resources/blake3/test_vectors.json`).
 *
 * Usage: `Blake3.newHasher().apply { update(bytes) }.digest()` or [Blake3.hash]. A hasher is not
 * thread-safe; [digest] does not consume it, more input may follow.
 */
class Blake3 private constructor(private val keyWords: IntArray, private val flags: Int) {

    private var chunk = ChunkState(keyWords, 0, flags)

    /** Chaining values of completed subtrees, at most one per level (54 levels cover 2^64 bytes). */
    private val cvStack = Array(MAX_DEPTH) { IntArray(8) }
    private var cvStackLen = 0

    private val scratchBlock = IntArray(16)
    private val scratchOut = IntArray(16)

    fun update(input: ByteArray, offset: Int = 0, length: Int = input.size - offset): Blake3 {
        require(offset >= 0 && length >= 0 && offset + length <= input.size) { "range outside the input" }
        var pos = offset
        var remaining = length
        while (remaining > 0) {
            // A full chunk is only closed once more input arrives: the last chunk must stay open
            // so it can be finalised with the ROOT flag when it is the only one.
            if (chunk.len() == CHUNK_LEN) {
                val chunkCv = chunk.output().chainingValue(scratchOut)
                val totalChunks = chunk.chunkCounter + 1
                addChunkChainingValue(chunkCv, totalChunks)
                chunk = ChunkState(keyWords, totalChunks, flags)
            }
            val take = minOf(CHUNK_LEN - chunk.len(), remaining)
            chunk.update(input, pos, take)
            pos += take
            remaining -= take
        }
        return this
    }

    /** The first [outputLength] bytes of the output (32 for the standard hash). */
    fun digest(outputLength: Int = OUT_LEN): ByteArray {
        require(outputLength >= 0) { "output length cannot be negative" }
        var output = chunk.output()
        var level = cvStackLen
        while (level > 0) {
            level -= 1
            output = parentOutput(cvStack[level], output.chainingValue(IntArray(16)), keyWords, flags)
        }
        return output.rootBytes(outputLength)
    }

    fun hexDigest(outputLength: Int = OUT_LEN): String = digest(outputLength).toHex()

    private fun addChunkChainingValue(newCv: IntArray, chunkCount: Long) {
        var cv = newCv.copyOf(8)
        var total = chunkCount
        // Merge completed subtrees: one parent per trailing zero bit of the chunk count.
        while (total and 1L == 0L) {
            cvStackLen -= 1
            cv = parentOutput(cvStack[cvStackLen], cv, keyWords, flags).chainingValue(scratchOut).copyOf(8)
            total = total ushr 1
        }
        cv.copyInto(cvStack[cvStackLen])
        cvStackLen += 1
    }

    /** One 1024-byte chunk being absorbed, 64 bytes at a time. */
    private inner class ChunkState(key: IntArray, val chunkCounter: Long, private val flags: Int) {
        private val cv = key.copyOf(8)
        private val block = ByteArray(BLOCK_LEN)
        private var blockLen = 0
        private var blocksCompressed = 0

        fun len(): Int = BLOCK_LEN * blocksCompressed + blockLen

        private fun startFlag(): Int = if (blocksCompressed == 0) CHUNK_START else 0

        fun update(input: ByteArray, offset: Int, length: Int) {
            var pos = offset
            var remaining = length
            while (remaining > 0) {
                if (blockLen == BLOCK_LEN) {
                    wordsOf(block, scratchBlock)
                    compress(cv, scratchBlock, chunkCounter, BLOCK_LEN, flags or startFlag(), scratchOut)
                    scratchOut.copyInto(cv, 0, 0, 8)
                    blocksCompressed += 1
                    block.fill(0)
                    blockLen = 0
                }
                val take = minOf(BLOCK_LEN - blockLen, remaining)
                System.arraycopy(input, pos, block, blockLen, take)
                blockLen += take
                pos += take
                remaining -= take
            }
        }

        fun output(): Output {
            val words = IntArray(16)
            wordsOf(block, words) // the block is zero beyond blockLen
            return Output(cv.copyOf(8), words, chunkCounter, blockLen, flags or startFlag() or CHUNK_END)
        }
    }

    /** The state just before the last compression of a node: chaining value or root output. */
    private class Output(
        private val inputCv: IntArray,
        private val blockWords: IntArray,
        private val counter: Long,
        private val blockLen: Int,
        private val flags: Int,
    ) {
        /** Writes the node's chaining value into the first 8 words of [out] and returns [out]. */
        fun chainingValue(out: IntArray): IntArray {
            compress(inputCv, blockWords, counter, blockLen, flags, out)
            return out
        }

        fun rootBytes(length: Int): ByteArray {
            val result = ByteArray(length)
            val words = IntArray(16)
            var blockCounter = 0L
            var pos = 0
            while (pos < length) {
                compress(inputCv, blockWords, blockCounter, blockLen, flags or ROOT, words)
                var w = 0
                while (w < 16 && pos < length) {
                    val word = words[w]
                    var b = 0
                    while (b < 4 && pos < length) {
                        result[pos] = (word ushr (8 * b)).toByte()
                        pos += 1
                        b += 1
                    }
                    w += 1
                }
                blockCounter += 1
            }
            return result
        }
    }

    companion object {
        const val OUT_LEN = 32
        const val KEY_LEN = 32
        private const val BLOCK_LEN = 64
        private const val CHUNK_LEN = 1024
        private const val MAX_DEPTH = 54

        private const val CHUNK_START = 1 shl 0
        private const val CHUNK_END = 1 shl 1
        private const val PARENT = 1 shl 2
        private const val ROOT = 1 shl 3
        private const val KEYED_HASH = 1 shl 4
        private const val DERIVE_KEY_CONTEXT = 1 shl 5
        private const val DERIVE_KEY_MATERIAL = 1 shl 6

        private val IV = intArrayOf(
            0x6A09E667, 0xBB67AE85.toInt(), 0x3C6EF372, 0xA54FF53A.toInt(),
            0x510E527F, 0x9B05688C.toInt(), 0x1F83D9AB, 0x5BE0CD19,
        )

        private val MSG_PERMUTATION = intArrayOf(2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8)

        /** Message word order of each of the 7 rounds (the permutation applied 0..6 times). */
        private val SCHEDULE: Array<IntArray> = Array(7) { IntArray(16) }.also { rounds ->
            for (i in 0 until 16) rounds[0][i] = i
            for (r in 1 until 7) for (i in 0 until 16) rounds[r][i] = rounds[r - 1][MSG_PERMUTATION[i]]
        }

        /** Plain hashing mode. */
        fun newHasher(): Blake3 = Blake3(IV.copyOf(), 0)

        /** Keyed mode (a MAC); [key] is exactly 32 bytes. */
        fun newKeyed(key: ByteArray): Blake3 {
            require(key.size == KEY_LEN) { "a BLAKE3 key is $KEY_LEN bytes" }
            return Blake3(IntArray(8).also { wordsOf(key, it) }, KEYED_HASH)
        }

        /** Key derivation mode with a hard-coded, globally unique [context] string. */
        fun newDeriveKey(context: String): Blake3 {
            val contextKey = Blake3(IV.copyOf(), DERIVE_KEY_CONTEXT)
                .update(context.toByteArray(Charsets.UTF_8))
                .digest(KEY_LEN)
            return Blake3(IntArray(8).also { wordsOf(contextKey, it) }, DERIVE_KEY_MATERIAL)
        }

        /** BLAKE3 of [input], 32 bytes. */
        fun hash(input: ByteArray): ByteArray = newHasher().update(input).digest()

        private fun parentOutput(left: IntArray, right: IntArray, key: IntArray, flags: Int): Output {
            val block = IntArray(16)
            left.copyInto(block, 0, 0, 8)
            right.copyInto(block, 8, 0, 8)
            return Output(key, block, 0, BLOCK_LEN, PARENT or flags)
        }

        /** Little-endian words of [bytes] into [out] (as many as fit; [bytes] is a multiple of 4 long). */
        private fun wordsOf(bytes: ByteArray, out: IntArray) {
            for (i in out.indices) {
                val p = 4 * i
                out[i] = (bytes[p].toInt() and 0xFF) or
                    ((bytes[p + 1].toInt() and 0xFF) shl 8) or
                    ((bytes[p + 2].toInt() and 0xFF) shl 16) or
                    ((bytes[p + 3].toInt() and 0xFF) shl 24)
            }
        }

        /** The compression function; the full 16-word output goes to [out] (may not alias [cv] or [m]). */
        private fun compress(cv: IntArray, m: IntArray, counter: Long, blockLen: Int, flags: Int, out: IntArray) {
            val s = out
            cv.copyInto(s, 0, 0, 8)
            s[8] = IV[0]
            s[9] = IV[1]
            s[10] = IV[2]
            s[11] = IV[3]
            s[12] = counter.toInt()
            s[13] = (counter ushr 32).toInt()
            s[14] = blockLen
            s[15] = flags
            for (round in 0 until 7) {
                val o = SCHEDULE[round]
                // Columns.
                g(s, 0, 4, 8, 12, m[o[0]], m[o[1]])
                g(s, 1, 5, 9, 13, m[o[2]], m[o[3]])
                g(s, 2, 6, 10, 14, m[o[4]], m[o[5]])
                g(s, 3, 7, 11, 15, m[o[6]], m[o[7]])
                // Diagonals.
                g(s, 0, 5, 10, 15, m[o[8]], m[o[9]])
                g(s, 1, 6, 11, 12, m[o[10]], m[o[11]])
                g(s, 2, 7, 8, 13, m[o[12]], m[o[13]])
                g(s, 3, 4, 9, 14, m[o[14]], m[o[15]])
            }
            for (i in 0 until 8) {
                s[i] = s[i] xor s[i + 8]
                s[i + 8] = s[i + 8] xor cv[i]
            }
        }

        @Suppress("NOTHING_TO_INLINE")
        private inline fun g(s: IntArray, a: Int, b: Int, c: Int, d: Int, mx: Int, my: Int) {
            s[a] = s[a] + s[b] + mx
            s[d] = (s[d] xor s[a]).rotateRight(16)
            s[c] = s[c] + s[d]
            s[b] = (s[b] xor s[c]).rotateRight(12)
            s[a] = s[a] + s[b] + my
            s[d] = (s[d] xor s[a]).rotateRight(8)
            s[c] = s[c] + s[d]
            s[b] = (s[b] xor s[c]).rotateRight(7)
        }
    }
}

private val HEX_DIGITS = "0123456789abcdef".toCharArray()

/** Lower-case hex, as the host's `/hash` answers. */
fun ByteArray.toHex(): String {
    val out = CharArray(size * 2)
    for (i in indices) {
        val v = this[i].toInt() and 0xFF
        out[2 * i] = HEX_DIGITS[v ushr 4]
        out[2 * i + 1] = HEX_DIGITS[v and 0x0F]
    }
    return String(out)
}
