package fyi.arlo.gander.core.data.actions

import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.update
import java.util.Locale

/**
 * The action the user chose to remember for a file extension (client spec: "default per extension
 * stored in DataStore"). Global to the phone, not per computer or share: a PDF is a PDF.
 * Extensions are lower case without the dot; files without an extension cannot be remembered.
 */
interface DefaultActionPrefsRepository {
    /** The remembered action for [ext]; emits null when none, and on every change. */
    fun observe(ext: String): Flow<FileAction?>

    suspend fun get(ext: String): FileAction? = observe(ext).first()

    /** Every remembered choice by extension, for Settings (WP-28: "default actions per type"). */
    fun observeAll(): Flow<Map<String, FileAction>>

    suspend fun set(ext: String, action: FileAction)

    /** Forgets the choice for [ext]: the rule table decides again. */
    suspend fun clear(ext: String)
}

/**
 * [DefaultActionPrefsRepository] on the app's settings DataStore: one string value per extension
 * under `defaultAction.<ext>`, holding [FileAction.wire]. A value this version does not know (a
 * newer app wrote it) reads as "nothing remembered".
 */
class DataStoreDefaultActionPrefs(private val store: DataStore<Preferences>) : DefaultActionPrefsRepository {

    override fun observe(ext: String): Flow<FileAction?> {
        val key = key(ext)
        return store.data.map { FileAction.fromWire(it[key]) }.distinctUntilChanged()
    }

    override fun observeAll(): Flow<Map<String, FileAction>> = store.data.map { prefs ->
        buildMap {
            for ((key, value) in prefs.asMap()) {
                if (!key.name.startsWith(PREFIX) || value !is String) continue
                val action = FileAction.fromWire(value) ?: continue
                put(key.name.removePrefix(PREFIX), action)
            }
        }
    }.distinctUntilChanged()

    override suspend fun set(ext: String, action: FileAction) {
        val key = key(ext)
        store.edit { it[key] = action.wire }
    }

    override suspend fun clear(ext: String) {
        val key = key(ext)
        store.edit { it.remove(key) }
    }

    internal companion object {
        const val PREFIX = "defaultAction."

        fun key(ext: String) = stringPreferencesKey(PREFIX + normalize(ext))

        fun normalize(ext: String): String = ext.trim().removePrefix(".").lowercase(Locale.ROOT)
    }
}

/** Test and preview implementation. */
class InMemoryDefaultActionPrefs(initial: Map<String, FileAction> = emptyMap()) : DefaultActionPrefsRepository {
    private val state = MutableStateFlow(initial.mapKeys { DataStoreDefaultActionPrefs.normalize(it.key) })

    override fun observe(ext: String): Flow<FileAction?> {
        val key = DataStoreDefaultActionPrefs.normalize(ext)
        return state.map { it[key] }.distinctUntilChanged()
    }

    override fun observeAll(): Flow<Map<String, FileAction>> = state

    override suspend fun set(ext: String, action: FileAction) {
        state.update { it + (DataStoreDefaultActionPrefs.normalize(ext) to action) }
    }

    override suspend fun clear(ext: String) {
        state.update { it - DataStoreDefaultActionPrefs.normalize(ext) }
    }
}
