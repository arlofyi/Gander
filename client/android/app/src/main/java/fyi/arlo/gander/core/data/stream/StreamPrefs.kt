package fyi.arlo.gander.core.data.stream

import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.map

/**
 * How touches on the stream drive the pointer (client spec, "Stream viewer"): the Windows App
 * convention, labelled on screen and remembered per host.
 */
enum class ControlMode(val wire: String) {
    /** A touch is a mouse action at the touched point: tap = click, long press = right click. */
    DIRECT("direct"),

    /** The screen is a trackpad: one finger moves the pointer, a tap clicks where the pointer is. */
    TRACKPAD("trackpad"),
    ;

    companion object {
        val DEFAULT = DIRECT

        fun fromWire(wire: String?): ControlMode = entries.firstOrNull { it.wire == wire } ?: DEFAULT
    }
}

/** The stream viewer's remembered choices. */
interface StreamPrefsRepository {
    /** The control mode of [hostId]; [ControlMode.DEFAULT] until the user picks one. */
    fun observeMode(hostId: String): Flow<ControlMode>

    suspend fun mode(hostId: String): ControlMode = observeMode(hostId).first()

    suspend fun setMode(hostId: String, mode: ControlMode)

    /** Forgets what is stored for [hostId] (the computer was forgotten). */
    suspend fun forgetHost(hostId: String)
}

/**
 * [StreamPrefsRepository] on the app's settings DataStore: one string per host under
 * `stream.mode.<hostId>`. A value this version cannot read (a newer app wrote it) reads as the
 * default rather than failing.
 */
class DataStoreStreamPrefs(private val store: DataStore<Preferences>) : StreamPrefsRepository {

    override fun observeMode(hostId: String): Flow<ControlMode> {
        val key = key(hostId)
        return store.data.map { ControlMode.fromWire(it[key]) }.distinctUntilChanged()
    }

    override suspend fun setMode(hostId: String, mode: ControlMode) {
        store.edit { it[key(hostId)] = mode.wire }
    }

    override suspend fun forgetHost(hostId: String) {
        store.edit { it.remove(key(hostId)) }
    }

    internal companion object {
        fun key(hostId: String) = stringPreferencesKey("stream.mode.$hostId")
    }
}
