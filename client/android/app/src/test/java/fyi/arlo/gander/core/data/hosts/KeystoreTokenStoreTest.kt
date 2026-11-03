package fyi.arlo.gander.core.data.hosts

import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import fyi.arlo.gander.core.net.BearerToken
import fyi.arlo.gander.testing.InMemoryPreferencesDataStore
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.security.GeneralSecurityException
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey

/**
 * The token store on a software AES key (the Keystore itself is covered by the instrumented
 * `KeystoreTokenStoreInstrumentedTest`): what is persisted, what survives a restart, and what is
 * refused when a blob is tampered with, moved or encrypted under another key.
 */
class KeystoreTokenStoreTest {
    private fun newKey(): SecretKey = KeyGenerator.getInstance("AES").run {
        init(256)
        generateKey()
    }

    private val key = newKey()
    private val dataStore = InMemoryPreferencesDataStore()
    private val cipher = AesGcmTokenCipher { key }
    private val hostA = "hostA"
    private val hostB = "hostB"
    private val tokenA = BearerToken("tok_A_placeholder_not_a_secret")
    private val tokenB = BearerToken("tok_B_placeholder_not_a_secret")

    private fun store(c: TokenCipher = cipher, ds: InMemoryPreferencesDataStore = dataStore) = KeystoreTokenStore(ds, c)

    private fun stored(ds: InMemoryPreferencesDataStore = dataStore): Map<String, Any> =
        runBlocking { ds.data.first() }.asMap().mapKeys { it.key.name }

    @Test
    fun putGetRemove() = runBlocking {
        val store = store()
        assertNull(store.get(hostA))
        store.put(hostA, tokenA)
        store.put(hostB, tokenB)
        assertEquals(tokenA, store.get(hostA))
        assertEquals(tokenB, store.get(hostB))
        store.put(hostA, BearerToken("rotated"))
        assertEquals(BearerToken("rotated"), store.get(hostA))
        store.remove(hostA)
        assertNull(store.get(hostA))
        assertEquals(tokenB, store.get(hostB))
        assertEquals(setOf("token.v1.$hostB"), stored().keys)
    }

    @Test
    fun theFileNeverHoldsThePlaintext() = runBlocking {
        store().put(hostA, tokenA)
        val persisted = stored().values.joinToString()
        assertFalse(persisted.contains(tokenA.value))
        assertFalse(java.util.Base64.getDecoder().decode(persisted).decodeToString().contains(tokenA.value))
    }

    @Test
    fun aNewProcessReadsWhatTheLastOneWrote() = runBlocking {
        store().put(hostA, tokenA)
        val restarted = store()
        assertEquals(tokenA, restarted.get(hostA)) // blocking first read, as an OkHttp thread would do it
        val warmed = store().also { it.preload() }
        assertEquals(tokenA, warmed.get(hostA))
    }

    @Test
    fun theSameTokenEncryptsDifferentlyEachTime() = runBlocking {
        val first = cipher.encrypt(tokenA.value.toByteArray(), byteArrayOf(1))
        val second = cipher.encrypt(tokenA.value.toByteArray(), byteArrayOf(1))
        assertNotEquals(first.toList(), second.toList())
    }

    @Test
    fun aBlobMovedToAnotherHostDoesNotDecrypt() = runBlocking {
        store().put(hostA, tokenA)
        val blob = stored().getValue("token.v1.$hostA") as String
        dataStore.edit { it[stringPreferencesKey("token.v1.$hostB")] = blob }
        val restarted = store()
        assertEquals(tokenA, restarted.get(hostA))
        assertNull("the host id is bound into the ciphertext", restarted.get(hostB))
        assertEquals("the unreadable entry is dropped", setOf("token.v1.$hostA"), stored().keys)
    }

    @Test
    fun aTamperedBlobIsDroppedNotTrusted() = runBlocking {
        store().put(hostA, tokenA)
        val blob = java.util.Base64.getDecoder().decode(stored().getValue("token.v1.$hostA") as String)
        blob[blob.size - 1] = (blob[blob.size - 1].toInt() xor 1).toByte()
        dataStore.edit { it[stringPreferencesKey("token.v1.$hostA")] = java.util.Base64.getEncoder().encodeToString(blob) }
        assertNull(store().get(hostA))
        assertTrue(stored().isEmpty())
    }

    @Test
    fun anotherKeyMeansPairAgainNotACrash() = runBlocking {
        store().put(hostA, tokenA)
        val otherKey = newKey() // the Keystore key was replaced, or the data was restored elsewhere
        val restarted = store(AesGcmTokenCipher { otherKey })
        assertNull(restarted.get(hostA))
        assertTrue(stored().isEmpty())
        restarted.put(hostA, tokenB) // and pairing works again under the new key
        assertEquals(tokenB, restarted.get(hostA))
    }

    @Test
    fun garbageInTheFileIsDropped() = runBlocking {
        dataStore.edit {
            it[stringPreferencesKey("token.v1.$hostA")] = "not base64!!"
            it[stringPreferencesKey("token.v1.$hostB")] = java.util.Base64.getEncoder().encodeToString(ByteArray(5))
            it[stringPreferencesKey("unrelated")] = "kept"
        }
        val store = store()
        assertNull(store.get(hostA))
        assertNull(store.get(hostB))
        assertEquals(setOf("unrelated"), stored().keys)
    }

    @Test
    fun anUnavailableKeystoreKeepsTheBlobForNextTime() = runBlocking {
        store().put(hostA, tokenA)
        val unavailable = object : TokenCipher {
            override fun encrypt(plain: ByteArray, aad: ByteArray): ByteArray = throw GeneralSecurityException("keystore busy")
            override fun decrypt(blob: ByteArray, aad: ByteArray): ByteArray = throw GeneralSecurityException("keystore busy")
        }
        assertNull(store(unavailable).get(hostA))
        assertEquals("not destroyed by a Keystore hiccup", setOf("token.v1.$hostA"), stored().keys)
        assertEquals(tokenA, store().get(hostA))
    }

    @Test
    fun aFailedEncryptionStoresNothing() = runBlocking {
        val broken = object : TokenCipher {
            override fun encrypt(plain: ByteArray, aad: ByteArray): ByteArray = throw GeneralSecurityException("no key")
            override fun decrypt(blob: ByteArray, aad: ByteArray): ByteArray = throw GeneralSecurityException("no key")
        }
        val store = store(broken)
        try {
            store.put(hostA, tokenA)
            org.junit.Assert.fail("put must fail loudly")
        } catch (_: GeneralSecurityException) {
            // expected: the pairing then fails visibly instead of "succeeding" without a token
        }
        assertNull(store.get(hostA))
        assertTrue(stored().isEmpty())
    }

    @Test
    fun theStoreNeverPrintsTokens() = runBlocking {
        val store = store()
        store.put(hostA, tokenA)
        assertFalse(store.toString().contains(tokenA.value))
        assertFalse(tokenA.toString().contains(tokenA.value))
    }

}
