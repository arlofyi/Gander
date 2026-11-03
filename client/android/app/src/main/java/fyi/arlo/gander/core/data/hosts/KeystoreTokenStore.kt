package fyi.arlo.gander.core.data.hosts

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import androidx.datastore.preferences.preferencesDataStoreFile
import fyi.arlo.gander.core.net.BearerToken
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import java.security.GeneralSecurityException
import java.security.KeyStore
import java.security.ProviderException
import java.security.UnrecoverableKeyException
import java.util.Base64
import java.util.concurrent.ConcurrentHashMap
import javax.crypto.AEADBadTagException
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/** Encrypts the small secrets the token store keeps. Implementations must be thread-safe. */
interface TokenCipher {
    /** Returns a self-contained blob (version, IV, ciphertext and tag). [aad] is authenticated, not stored. */
    fun encrypt(plain: ByteArray, aad: ByteArray): ByteArray

    /** Throws [GeneralSecurityException] if the blob was tampered with, [aad] differs or the key is gone. */
    fun decrypt(blob: ByteArray, aad: ByteArray): ByteArray
}

/** Supplies the AES key; the production one lives in the Android Keystore and never leaves it. */
fun interface TokenKeyProvider {
    fun key(): SecretKey
}

/**
 * AES-256-GCM with a fresh random 96-bit IV per message: `blob = 0x01 || iv(12) || ciphertext+tag`.
 * Works with a Keystore key (which picks the IV itself) and with a software key in JVM tests.
 */
class AesGcmTokenCipher(private val keys: TokenKeyProvider) : TokenCipher {

    override fun encrypt(plain: ByteArray, aad: ByteArray): ByteArray {
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.ENCRYPT_MODE, keys.key())
        cipher.updateAAD(aad)
        val iv = cipher.iv
        check(iv.size == IV_BYTES) { "unexpected IV size" }
        return byteArrayOf(VERSION) + iv + cipher.doFinal(plain)
    }

    override fun decrypt(blob: ByteArray, aad: ByteArray): ByteArray {
        if (blob.size < 1 + IV_BYTES + TAG_BYTES || blob[0] != VERSION) {
            throw CorruptTokenBlobException()
        }
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.DECRYPT_MODE, keys.key(), GCMParameterSpec(TAG_BYTES * 8, blob, 1, IV_BYTES))
        cipher.updateAAD(aad)
        return cipher.doFinal(blob, 1 + IV_BYTES, blob.size - 1 - IV_BYTES)
    }

    private companion object {
        const val TRANSFORMATION = "AES/GCM/NoPadding"
        const val VERSION: Byte = 1
        const val IV_BYTES = 12
        const val TAG_BYTES = 16
    }
}

/**
 * The AES key in the Android Keystore under [alias] (hardware-backed where the device has it, not
 * exportable, no user-authentication requirement: the optional biometric app lock gates the UI,
 * not the token, per the pairing spec). Created on first use. If the Keystore says the key is
 * unusable it is replaced; blobs written under the old key then fail to decrypt and are dropped,
 * which the user sees as "pair this computer again".
 */
class AndroidKeystoreKeyProvider(private val alias: String = DEFAULT_ALIAS) : TokenKeyProvider {

    override fun key(): SecretKey = synchronized(LOCK) {
        val store = KeyStore.getInstance(PROVIDER).apply { load(null) }
        val existing = try {
            store.getKey(alias, null) as? SecretKey
        } catch (_: UnrecoverableKeyException) {
            store.deleteEntry(alias)
            null
        }
        existing ?: generate()
    }

    private fun generate(): SecretKey {
        val spec = KeyGenParameterSpec.Builder(alias, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
            .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
            .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
            .setKeySize(KEY_BITS)
            .build()
        return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, PROVIDER).run {
            init(spec)
            generateKey()
        }
    }

    companion object {
        const val DEFAULT_ALIAS = "fyi.arlo.gander.tokens.v1"
        private const val PROVIDER = "AndroidKeyStore"
        private const val KEY_BITS = 256
        private val LOCK = Any()
    }
}

/**
 * [TokenStore] for the bearer tokens: each token is encrypted with AES-GCM under a Keystore key
 * ([TokenCipher]) and the blob is kept in a DataStore file. No deprecated
 * `EncryptedSharedPreferences`. The file is excluded from backup and device transfer
 * (`data_extraction_rules.xml`), and the key cannot leave the device anyway, so a restored copy
 * would be unreadable: hosts are paired again after a restore.
 *
 * - The host id is bound into every blob as GCM associated data, so a blob moved to another
 *   host's entry fails to decrypt.
 * - [get] answers from memory (the contract is "never block on I/O", it runs on OkHttp threads for
 *   every request). The map is filled once; [preload] does that off the main thread at startup,
 *   and a [get] that still beats it waits for the same single load.
 * - A blob that cannot be decrypted (key replaced, file damaged) is dropped at load and the host
 *   simply has no token: the Hosts screen then explains that it must be paired again.
 * - Tokens are never logged and never appear in exceptions or `toString`.
 */
class KeystoreTokenStore(
    private val dataStore: DataStore<Preferences>,
    private val cipher: TokenCipher,
) : TokenStore {

    private val loadLock = Mutex()

    @Volatile
    private var tokens: ConcurrentHashMap<String, BearerToken>? = null

    /** Reads and decrypts everything now, so the first request does not wait for it. */
    suspend fun preload() {
        ensureLoaded()
    }

    override fun get(hostId: String): BearerToken? {
        val loaded = tokens ?: runBlocking { ensureLoaded() }
        return loaded[hostId]
    }

    override suspend fun put(hostId: String, token: BearerToken) {
        val loaded = ensureLoaded()
        val blob = cipher.encrypt(token.value.toByteArray(Charsets.UTF_8), aad(hostId))
        dataStore.edit { it[key(hostId)] = ENCODER.encodeToString(blob) }
        loaded[hostId] = token
    }

    override suspend fun remove(hostId: String) {
        val loaded = ensureLoaded()
        dataStore.edit { it.remove(key(hostId)) }
        loaded.remove(hostId)
    }

    override fun toString(): String = "KeystoreTokenStore(<tokens redacted>)"

    private suspend fun ensureLoaded(): ConcurrentHashMap<String, BearerToken> {
        tokens?.let { return it }
        return loadLock.withLock {
            tokens ?: load().also { tokens = it }
        }
    }

    private suspend fun load(): ConcurrentHashMap<String, BearerToken> {
        val map = ConcurrentHashMap<String, BearerToken>()
        val unreadable = mutableListOf<Preferences.Key<String>>()
        val stored = dataStore.data.first().asMap()
        for ((key, value) in stored) {
            val hostId = key.name.removePrefix(KEY_PREFIX).takeIf { key.name.startsWith(KEY_PREFIX) } ?: continue
            when (val outcome = if (value is String) decode(hostId, value) else Decoded.Corrupt) {
                is Decoded.Token -> map[hostId] = outcome.token
                Decoded.Corrupt -> unreadable += stringPreferencesKey(key.name)
                Decoded.KeyUnavailable -> Unit // keep the blob: the Keystore may answer next time
            }
        }
        if (unreadable.isNotEmpty()) dataStore.edit { prefs -> unreadable.forEach { prefs.remove(it) } }
        return map
    }

    private sealed interface Decoded {
        class Token(val token: BearerToken) : Decoded

        /** The blob can never be read again (wrong key, tampered, damaged): drop it. */
        data object Corrupt : Decoded

        /** The Keystore did not answer; says nothing about the blob. */
        data object KeyUnavailable : Decoded
    }

    private fun decode(hostId: String, stored: String): Decoded = try {
        Decoded.Token(BearerToken(String(cipher.decrypt(DECODER.decode(stored), aad(hostId)), Charsets.UTF_8)))
    } catch (_: AEADBadTagException) { // another key (restore, key replaced), another host id, or tampering
        Decoded.Corrupt
    } catch (_: CorruptTokenBlobException) {
        Decoded.Corrupt
    } catch (_: IllegalArgumentException) { // bad Base64, or a decrypted value that is not a token
        Decoded.Corrupt
    } catch (_: GeneralSecurityException) {
        Decoded.KeyUnavailable
    } catch (_: ProviderException) {
        Decoded.KeyUnavailable
    }

    private fun key(hostId: String) = stringPreferencesKey(KEY_PREFIX + hostId)

    private fun aad(hostId: String): ByteArray = (AAD_PREFIX + hostId).toByteArray(Charsets.UTF_8)

    companion object {
        private const val KEY_PREFIX = "token.v1."
        private const val AAD_PREFIX = "gander-token-v1:"
        private val ENCODER = Base64.getEncoder()
        private val DECODER = Base64.getDecoder()

        /**
         * The production store: `datastore/tokens.preferences_pb` in the app's private storage,
         * keyed by the Android Keystore. [scope] owns the DataStore and the startup [preload].
         */
        fun create(context: Context, scope: CoroutineScope): KeystoreTokenStore {
            val appContext = context.applicationContext
            val dataStore = PreferenceDataStoreFactory.create(scope = scope) {
                appContext.preferencesDataStoreFile("tokens")
            }
            val store = KeystoreTokenStore(dataStore, AesGcmTokenCipher(AndroidKeystoreKeyProvider()))
            scope.launch { runCatching { store.preload() } } // a failure here resurfaces on the first get()
            return store
        }
    }
}

/** A stored blob that is not in the format [AesGcmTokenCipher] writes. */
class CorruptTokenBlobException : GeneralSecurityException("unrecognised token blob")
