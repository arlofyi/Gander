package fyi.arlo.gander.testing

import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.emptyPreferences
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow

/**
 * A `DataStore<Preferences>` that lives in memory. The real file-backed DataStore renames a
 * scratch file over its target, which fails on a Windows JVM when the target exists, so the JVM
 * unit tests cover the keying and encoding here and the instrumented test covers the file
 * (src/androidTest, `BrowserPrefsFileTest`).
 */
class InMemoryPreferencesDataStore : DataStore<Preferences> {
    private val state = MutableStateFlow(emptyPreferences())

    override val data: Flow<Preferences> = state

    override suspend fun updateData(transform: suspend (t: Preferences) -> Preferences): Preferences {
        val next = transform(state.value)
        state.value = next
        return next
    }
}
