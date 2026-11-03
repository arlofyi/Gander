package fyi.arlo.gander.core.data.actions

import fyi.arlo.gander.core.data.browse.EntryTarget

/**
 * Whether a copy of a file is already stored on the phone ("cached locally", client spec file
 * sheet). The cache itself arrives with WP-27/WP-28; until then [None] is wired in and the
 * sheet never shows a saved copy.
 */
fun interface CachedFileLookup {
    suspend fun isCached(target: EntryTarget): Boolean

    object None : CachedFileLookup {
        override suspend fun isCached(target: EntryTarget) = false
    }
}
