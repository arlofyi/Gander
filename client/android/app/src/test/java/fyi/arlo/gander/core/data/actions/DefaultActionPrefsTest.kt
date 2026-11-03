package fyi.arlo.gander.core.data.actions

import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import fyi.arlo.gander.testing.InMemoryPreferencesDataStore
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class DefaultActionPrefsTest {
    private val store = InMemoryPreferencesDataStore()
    private val prefs = DataStoreDefaultActionPrefs(store)

    @Test
    fun nothingIsRememberedAtFirst() = runTest {
        assertNull(prefs.get("pdf"))
        assertEquals(emptyMap<String, FileAction>(), prefs.observeAll().first())
    }

    @Test
    fun aChoiceIsRememberedPerExtension() = runTest {
        prefs.set("pdf", FileAction.FETCH)
        prefs.set("blend", FileAction.STREAM)
        assertEquals(FileAction.FETCH, prefs.get("pdf"))
        assertEquals(FileAction.STREAM, prefs.get("blend"))
        assertNull(prefs.get("docx"))
        assertEquals(mapOf("pdf" to FileAction.FETCH, "blend" to FileAction.STREAM), prefs.observeAll().first())
    }

    @Test
    fun aLaterChoiceReplacesAnEarlierOne() = runTest {
        prefs.set("pdf", FileAction.FETCH)
        prefs.set("pdf", FileAction.PREVIEW)
        assertEquals(FileAction.PREVIEW, prefs.get("pdf"))
    }

    @Test
    fun clearingForgetsOnlyThatExtension() = runTest {
        prefs.set("pdf", FileAction.FETCH)
        prefs.set("png", FileAction.PREVIEW)
        prefs.clear("pdf")
        assertNull(prefs.get("pdf"))
        assertEquals(FileAction.PREVIEW, prefs.get("png"))
    }

    @Test
    fun extensionsAreNormalised() = runTest {
        prefs.set(".PDF", FileAction.FETCH)
        assertEquals(FileAction.FETCH, prefs.get("pdf"))
        assertEquals(FileAction.FETCH, prefs.get("Pdf"))
        assertEquals(setOf("pdf"), prefs.observeAll().first().keys)
    }

    @Test
    fun theStoredValueIsTheWireNameUnderAnExtensionKey() = runTest {
        prefs.set("pdf", FileAction.STREAM)
        val raw = store.data.first()[stringPreferencesKey("defaultAction.pdf")]
        assertEquals("stream", raw)
    }

    @Test
    fun aValueFromANewerAppReadsAsNothingRemembered() = runTest {
        store.edit { it[stringPreferencesKey("defaultAction.pdf")] = "teleport" }
        assertNull(prefs.get("pdf"))
        assertEquals(emptyMap<String, FileAction>(), prefs.observeAll().first())
    }

    @Test
    fun otherSettingsInTheSameStoreAreNotMistakenForChoices() = runTest {
        store.edit { it[stringPreferencesKey("browser.host.share")] = "name|asc|list" }
        prefs.set("pdf", FileAction.FETCH)
        assertEquals(mapOf("pdf" to FileAction.FETCH), prefs.observeAll().first())
    }

    @Test
    fun theInMemoryVersionBehavesTheSame() = runTest {
        val memory = InMemoryDefaultActionPrefs(mapOf("PDF" to FileAction.FETCH))
        assertEquals(FileAction.FETCH, memory.get("pdf"))
        memory.set("png", FileAction.PREVIEW)
        memory.clear("pdf")
        assertNull(memory.get("pdf"))
        assertEquals(mapOf("png" to FileAction.PREVIEW), memory.observeAll().first())
    }
}
