package fyi.arlo.gander.core.data.settings

import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.booleanPreferencesKey
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.intPreferencesKey
import androidx.datastore.preferences.core.longPreferencesKey
import androidx.datastore.preferences.core.stringPreferencesKey
import fyi.arlo.gander.core.data.actions.StreamOnlyTypes
import fyi.arlo.gander.core.data.fetch.FetchSettings
import fyi.arlo.gander.core.data.fetch.FetchSettingsSource
import fyi.arlo.gander.core.data.fetch.VerifyPolicy
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.update

/**
 * The user's [AppSettings], persisted. Setters sanitise their input (a cap below the minimum, a
 * negative threshold) so whatever the screen sends, the stored value is usable.
 */
interface SettingsRepository {
    /** The stored settings, defaults for anything missing or unreadable; emits again on every change. */
    val settings: Flow<AppSettings>

    suspend fun current(): AppSettings = settings.first()

    suspend fun setVerify(policy: VerifyPolicy)

    suspend fun setCellularWarnBytes(bytes: Long)

    suspend fun setCacheCapBytes(bytes: Long)

    /** Replaces the Stream-first types; null goes back to the shipped list. */
    suspend fun setStreamOnlyTypes(types: Set<String>?)

    suspend fun setAppLockEnabled(enabled: Boolean)

    suspend fun setAppLockMinutes(minutes: Int)

    suspend fun setFilesAppEnabled(enabled: Boolean)
}

/**
 * [SettingsRepository] on the app's settings DataStore, next to the browser's and the default
 * actions' keys (`browser.*`, `defaultAction.*`); these are all under `settings.`. A value this
 * version cannot read (a newer app wrote it) reads as the default instead of failing.
 */
class DataStoreSettings(private val store: DataStore<Preferences>) : SettingsRepository {

    override val settings: Flow<AppSettings> = store.data.map(::decode).distinctUntilChanged()

    override suspend fun setVerify(policy: VerifyPolicy) {
        store.edit { it[VERIFY] = policy.wire }
    }

    override suspend fun setCellularWarnBytes(bytes: Long) {
        store.edit { it[CELLULAR_WARN] = bytes.coerceAtLeast(0) }
    }

    override suspend fun setCacheCapBytes(bytes: Long) {
        store.edit { it[CACHE_CAP] = bytes.coerceAtLeast(AppSettings.MIN_CACHE_CAP_BYTES) }
    }

    override suspend fun setStreamOnlyTypes(types: Set<String>?) {
        store.edit { prefs ->
            if (types == null) prefs.remove(STREAM_ONLY) else prefs[STREAM_ONLY] = ExtensionList.format(types)
        }
    }

    override suspend fun setAppLockEnabled(enabled: Boolean) {
        store.edit { it[LOCK_ENABLED] = enabled }
    }

    override suspend fun setAppLockMinutes(minutes: Int) {
        store.edit { it[LOCK_MINUTES] = minutes.coerceIn(0, AppSettings.MAX_LOCK_MINUTES) }
    }

    override suspend fun setFilesAppEnabled(enabled: Boolean) {
        store.edit { it[FILES_APP] = enabled }
    }

    internal companion object {
        val VERIFY = stringPreferencesKey("settings.verify")
        val CELLULAR_WARN = longPreferencesKey("settings.cellularWarnBytes")
        val CACHE_CAP = longPreferencesKey("settings.cacheCapBytes")
        val STREAM_ONLY = stringPreferencesKey("settings.streamOnlyTypes")
        val LOCK_ENABLED = booleanPreferencesKey("settings.appLock")
        val LOCK_MINUTES = intPreferencesKey("settings.appLockMinutes")
        val FILES_APP = booleanPreferencesKey("settings.filesApp")

        fun decode(prefs: Preferences): AppSettings {
            val defaults = AppSettings()
            return AppSettings(
                verify = VerifyPolicy.entries.firstOrNull { it.wire == prefs[VERIFY] } ?: defaults.verify,
                cellularWarnBytes = prefs[CELLULAR_WARN]?.takeIf { it >= 0 } ?: defaults.cellularWarnBytes,
                cacheCapBytes = prefs[CACHE_CAP]?.takeIf { it >= AppSettings.MIN_CACHE_CAP_BYTES } ?: defaults.cacheCapBytes,
                streamOnlyTypes = prefs[STREAM_ONLY]?.let(ExtensionList::parse) ?: StreamOnlyTypes.DEFAULT,
                appLockEnabled = prefs[LOCK_ENABLED] ?: defaults.appLockEnabled,
                appLockMinutes = prefs[LOCK_MINUTES]?.takeIf { it in 0..AppSettings.MAX_LOCK_MINUTES }
                    ?: defaults.appLockMinutes,
                filesAppEnabled = prefs[FILES_APP] ?: defaults.filesAppEnabled,
            )
        }
    }
}

/** The fetch engine's settings (WP-27) read from the user's settings at the moment a job needs them. */
class SettingsFetchSource(private val settings: SettingsRepository) : FetchSettingsSource {
    override suspend fun current(): FetchSettings = settings.current().toFetchSettings()
}

/** Test and preview implementation. */
class InMemorySettings(initial: AppSettings = AppSettings()) : SettingsRepository {
    private val state = MutableStateFlow(initial)

    override val settings: Flow<AppSettings> = state

    override suspend fun setVerify(policy: VerifyPolicy) = state.update { it.copy(verify = policy) }

    override suspend fun setCellularWarnBytes(bytes: Long) =
        state.update { it.copy(cellularWarnBytes = bytes.coerceAtLeast(0)) }

    override suspend fun setCacheCapBytes(bytes: Long) =
        state.update { it.copy(cacheCapBytes = bytes.coerceAtLeast(AppSettings.MIN_CACHE_CAP_BYTES)) }

    override suspend fun setStreamOnlyTypes(types: Set<String>?) =
        state.update { it.copy(streamOnlyTypes = types ?: StreamOnlyTypes.DEFAULT) }

    override suspend fun setAppLockEnabled(enabled: Boolean) = state.update { it.copy(appLockEnabled = enabled) }

    override suspend fun setAppLockMinutes(minutes: Int) =
        state.update { it.copy(appLockMinutes = minutes.coerceIn(0, AppSettings.MAX_LOCK_MINUTES)) }

    override suspend fun setFilesAppEnabled(enabled: Boolean) = state.update { it.copy(filesAppEnabled = enabled) }
}
