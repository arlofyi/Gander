package fyi.arlo.gander.core.data.stream

import androidx.datastore.preferences.core.edit
import fyi.arlo.gander.core.data.browse.InMemoryBrowserPrefs
import fyi.arlo.gander.core.data.cache.DerivedCaches
import fyi.arlo.gander.core.data.hosts.HostForgetter
import fyi.arlo.gander.core.data.hosts.InMemoryHostRepository
import fyi.arlo.gander.testing.InMemoryPreferencesDataStore
import fyi.arlo.gander.testing.SampleData
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Test

/** The touch mode is remembered per host (client spec: "labelled on screen, remembered per host"). */
class StreamPrefsTest {
    private val store = InMemoryPreferencesDataStore()
    private val prefs = DataStoreStreamPrefs(store)

    @Test
    fun theDefaultIsDirectTouch() = runBlocking {
        assertEquals(ControlMode.DIRECT, prefs.mode("host-a"))
    }

    @Test
    fun eachHostRemembersItsOwnMode() = runBlocking {
        prefs.setMode("host-a", ControlMode.TRACKPAD)
        assertEquals(ControlMode.TRACKPAD, prefs.mode("host-a"))
        assertEquals(ControlMode.DIRECT, prefs.mode("host-b"))
        prefs.setMode("host-a", ControlMode.DIRECT)
        assertEquals(ControlMode.DIRECT, prefs.mode("host-a"))
    }

    @Test
    fun theModeIsObservedAsItChanges() = runBlocking {
        prefs.setMode("host-a", ControlMode.TRACKPAD)
        assertEquals(ControlMode.TRACKPAD, prefs.observeMode("host-a").first())
    }

    @Test
    fun aValueFromANewerAppReadsAsTheDefault() = runBlocking {
        store.edit { it[DataStoreStreamPrefs.key("host-a")] = "air-mouse" }
        assertEquals(ControlMode.DIRECT, prefs.mode("host-a"))
    }

    @Test
    fun forgettingAHostForgetsItsMode() = runBlocking {
        prefs.setMode("host-a", ControlMode.TRACKPAD)
        prefs.setMode("host-b", ControlMode.TRACKPAD)
        prefs.forgetHost("host-a")
        assertEquals(ControlMode.DIRECT, prefs.mode("host-a"))
        assertEquals(ControlMode.TRACKPAD, prefs.mode("host-b"))
    }

    @Test
    fun forgettingAComputerRemovesItsStreamModeToo() = runBlocking {
        val studio = SampleData.host(name = "studio-pc")
        prefs.setMode(studio.id, ControlMode.TRACKPAD)
        val forgetter = HostForgetter(
            hosts = InMemoryHostRepository(listOf(studio)),
            derivedCaches = object : DerivedCaches {
                override suspend fun sizeBytes() = 0L

                override suspend fun clear() {}
            },
            browserPrefs = InMemoryBrowserPrefs(),
            purgeFetched = {},
            streamPrefs = prefs,
        )
        forgetter.forget(studio.id)
        assertEquals(ControlMode.DIRECT, prefs.mode(studio.id))
    }

    @Test
    fun theWireNamesAreStable() {
        assertEquals("direct", ControlMode.DIRECT.wire)
        assertEquals("trackpad", ControlMode.TRACKPAD.wire)
        assertEquals(ControlMode.DEFAULT, ControlMode.fromWire(null))
    }
}
