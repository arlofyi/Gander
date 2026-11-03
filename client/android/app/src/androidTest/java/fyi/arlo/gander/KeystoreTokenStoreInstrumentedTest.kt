package fyi.arlo.gander

import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import fyi.arlo.gander.core.data.hosts.AesGcmTokenCipher
import fyi.arlo.gander.core.data.hosts.AndroidKeystoreKeyProvider
import fyi.arlo.gander.core.data.hosts.KeystoreTokenStore
import fyi.arlo.gander.core.net.BearerToken
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.runBlocking
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File
import java.security.KeyStore
import java.util.UUID

/** The token store on the real Android Keystore and a real DataStore file (emulator or device, run by WP-29). */
@RunWith(AndroidJUnit4::class)
class KeystoreTokenStoreInstrumentedTest {
    private val context = InstrumentationRegistry.getInstrumentation().targetContext
    private val alias = "fyi.arlo.gander.tokens.test.${UUID.randomUUID()}"
    private val file = File(context.cacheDir, "tokens-test-${UUID.randomUUID()}.preferences_pb")
    private val scopes = mutableListOf<CoroutineScope>()
    private val token = BearerToken("tok_placeholder_for_the_keystore_test")

    /** A store instance, like one process start. DataStore allows one active instance per file, see [closeStores]. */
    private fun store(): KeystoreTokenStore {
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO).also(scopes::add)
        val dataStore = PreferenceDataStoreFactory.create(scope = scope) { file }
        return KeystoreTokenStore(dataStore, AesGcmTokenCipher(AndroidKeystoreKeyProvider(alias)))
    }

    /** Ends every store instance (the "process dies"), waiting until DataStore has released the file. */
    private suspend fun closeStores() {
        scopes.forEach { it.coroutineContext[Job]!!.cancelAndJoin() }
        scopes.clear()
    }

    private fun keystore() = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }

    @Before
    fun setUp() {
        assertTrue(!keystore().containsAlias(alias))
    }

    @After
    fun tearDown() {
        scopes.forEach { it.cancel() }
        file.delete()
        runCatching { keystore().deleteEntry(alias) }
    }

    @Test
    fun aTokenSurvivesARestartAndTheKeyNeverLeavesTheKeystore() = runBlocking {
        store().put("host-a", token)
        assertTrue("the key was created in the Keystore", keystore().containsAlias(alias))
        assertNull("the AES key cannot be exported", keystore().getKey(alias, null).encoded)
        closeStores()

        val restarted = store()
        assertEquals(token, restarted.get("host-a"))
        restarted.remove("host-a")
        closeStores()
        assertNull(store().get("host-a"))
    }

    @Test
    fun aReplacedKeyMeansTheOldTokenIsGoneNotACrash() = runBlocking {
        store().put("host-a", token)
        closeStores()
        keystore().deleteEntry(alias) // what a lost or reset Keystore looks like

        val restarted = store()
        assertNull(restarted.get("host-a"))
        restarted.put("host-a", BearerToken("tok_second_placeholder"))
        assertEquals(BearerToken("tok_second_placeholder"), restarted.get("host-a"))
    }
}
