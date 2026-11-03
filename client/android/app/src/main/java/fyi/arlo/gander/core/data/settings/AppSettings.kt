package fyi.arlo.gander.core.data.settings

import fyi.arlo.gander.core.data.actions.RuleConfig
import fyi.arlo.gander.core.data.actions.StreamOnlyTypes
import fyi.arlo.gander.core.data.fetch.FetchSettings
import fyi.arlo.gander.core.data.fetch.VerifyPolicy
import java.util.Locale

private const val MIB = 1024L * 1024L
private const val GIB = 1024L * MIB

/**
 * Everything the Settings screen edits (client spec, "Settings"): the cellular rule, download
 * verification, the cache cap, the Stream-first file types, the app lock and the Files-app switch. Defaults are the
 * spec's. The remembered default action per file type lives in `DefaultActionPrefsRepository`
 * (WP-25) and is edited from the same screen.
 */
data class AppSettings(
    val verify: VerifyPolicy = VerifyPolicy.LARGE_FILES,
    /** On a metered network a fetch of this many bytes or more asks first. [CELLULAR_NEVER] never asks. */
    val cellularWarnBytes: Long = DEFAULT_CELLULAR_WARN_BYTES,
    /** Fetched files may use this much room; the least recently used unpinned ones go first. */
    val cacheCapBytes: Long = DEFAULT_CACHE_CAP_BYTES,
    /** Extensions whose files list Stream first (client spec, `stream_only_types`). */
    val streamOnlyTypes: Set<String> = StreamOnlyTypes.DEFAULT,
    val appLockEnabled: Boolean = false,
    /** Minutes in the background after which the lock engages again; 0 locks every time the app leaves the screen. */
    val appLockMinutes: Int = DEFAULT_LOCK_MINUTES,
    /** "Show computers in the Files app": the SAF provider lists paired computers (WP-60). */
    val filesAppEnabled: Boolean = true,
) {
    /** The app lock's view of these settings. */
    val lock: LockConfig get() = LockConfig(appLockEnabled, appLockMinutes)

    companion object {
        const val DEFAULT_CELLULAR_WARN_BYTES: Long = 50 * MIB

        /** "Never ask": no file is ever large enough. */
        const val CELLULAR_NEVER: Long = Long.MAX_VALUE

        /** Client spec: "Cache cap default 2 GiB". */
        const val DEFAULT_CACHE_CAP_BYTES: Long = 2 * GIB

        /** Below this a cap would evict a single large file the moment it lands. */
        const val MIN_CACHE_CAP_BYTES: Long = 100 * MIB

        /** Client spec: "gating the UI after 1 minute in background". */
        const val DEFAULT_LOCK_MINUTES = 1
        const val MAX_LOCK_MINUTES = 24 * 60

        /** The choices the Settings screen offers; the repository accepts any value in range. */
        val CELLULAR_CHOICES: List<Long> =
            listOf(0L, 10 * MIB, 25 * MIB, DEFAULT_CELLULAR_WARN_BYTES, 100 * MIB, 500 * MIB, CELLULAR_NEVER)
        val CACHE_CAP_CHOICES: List<Long> = listOf(512 * MIB, GIB, 2 * GIB, 5 * GIB, 10 * GIB)
        val LOCK_MINUTES_CHOICES: List<Int> = listOf(0, 1, 5, 15)
    }
}

/** What the app lock needs to know: whether it is on and how long a background stay it forgives. */
data class LockConfig(val enabled: Boolean = false, val minutes: Int = AppSettings.DEFAULT_LOCK_MINUTES) {
    val timeoutMillis: Long get() = minutes.coerceAtLeast(0) * 60_000L
}

/** The fetch engine's numbers (WP-27) from the user's settings; the verify threshold stays at its default. */
fun AppSettings.toFetchSettings(): FetchSettings = FetchSettings(verify = verify, cellularWarnBytes = cellularWarnBytes)

/** The file sheet's rule table inputs (WP-25) from the user's settings. */
fun AppSettings.toRuleConfig(): RuleConfig = RuleConfig(streamOnlyTypes = streamOnlyTypes, cellularWarnBytes = cellularWarnBytes)

/** Reading and writing a list of file extensions the user typed ("psd, .AI  blend"). */
object ExtensionList {
    private val SEPARATORS = Regex("[\\s,;]+")
    private val VALID = Regex("[a-z0-9][a-z0-9_+-]{0,15}")

    /** Lower-case extensions without dot or `*.`; anything that is not a plausible extension is dropped. */
    fun parse(text: String): Set<String> = text.split(SEPARATORS)
        .map { it.trim().removePrefix("*").removePrefix(".").lowercase(Locale.ROOT) }
        .filter { VALID.matches(it) }
        .toCollection(LinkedHashSet())

    /** Sorted and space separated, as the editor shows it. */
    fun format(extensions: Set<String>): String = extensions.sorted().joinToString(" ")
}
