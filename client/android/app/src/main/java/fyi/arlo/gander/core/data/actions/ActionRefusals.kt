package fyi.arlo.gander.core.data.actions

import fyi.arlo.gander.core.data.browse.EntryTarget
import fyi.arlo.gander.core.net.GanderError
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.update

/**
 * How a screen that ran an action tells the file sheet why it was refused. The preview viewers
 * (WP-26), the fetch engine (WP-27) and the stream viewer (WP-54) call [report] with the
 * [GanderError] they got (a 412 `host_locked` from `POST /stream`, a 415 from the preview...);
 * the sheet of that file observes [observe] and shows the protocol's reason on the action instead
 * of a generic failure. One instance lives in `AppGraph`, so the report outlives the screen that
 * made it and is there when Back returns to the sheet.
 */
interface ActionRefusals {
    fun report(target: EntryTarget, action: FileAction, error: GanderError)

    /** The reasons currently on [target]'s actions; emits again on every change. */
    fun observe(target: EntryTarget): Flow<Map<FileAction, ActionReason>>

    /** Forgets what was reported for [action] of [target], or for all its actions when [action] is null. */
    fun clear(target: EntryTarget, action: FileAction? = null)
}

class InMemoryActionRefusals : ActionRefusals {
    private data class Key(val hostId: String, val shareId: String, val path: String)

    private val state = MutableStateFlow<Map<Key, Map<FileAction, ActionReason>>>(emptyMap())

    private fun EntryTarget.key() = Key(hostId, shareId, path)

    override fun report(target: EntryTarget, action: FileAction, error: GanderError) {
        val reason = ActionReason.fromError(action, error, target.entry.extension())
        state.update { all ->
            val key = target.key()
            all + (key to ((all[key] ?: emptyMap()) + (action to reason)))
        }
    }

    override fun observe(target: EntryTarget): Flow<Map<FileAction, ActionReason>> {
        val key = target.key()
        return state.map { it[key] ?: emptyMap() }.distinctUntilChanged()
    }

    override fun clear(target: EntryTarget, action: FileAction?) {
        state.update { all ->
            val key = target.key()
            val current = all[key] ?: return@update all
            val left = if (action == null) emptyMap() else current - action
            if (left.isEmpty()) all - key else all + (key to left)
        }
    }
}
