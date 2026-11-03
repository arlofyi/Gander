package fyi.arlo.gander.core.data.actions

import fyi.arlo.gander.core.data.UiError
import fyi.arlo.gander.core.data.UiErrorKind
import fyi.arlo.gander.core.data.toUiError
import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.protocol.Capabilities
import fyi.arlo.gander.core.net.protocol.Entry
import fyi.arlo.gander.core.net.protocol.EntryFlag
import fyi.arlo.gander.core.net.protocol.EntryKind
import fyi.arlo.gander.core.net.protocol.OfficePreview
import fyi.arlo.gander.core.net.protocol.Permission
import fyi.arlo.gander.core.net.protocol.PreviewKind
import fyi.arlo.gander.core.net.protocol.ServerEvent
import fyi.arlo.gander.core.net.protocol.StreamDecision
import java.util.Locale

// The file sheet's rule table (client spec, "Default-action rules"). Everything in this file is
// plain Kotlin: no Android types, so the whole table is covered by JVM unit tests. The sheet's
// ViewModel gathers the inputs (entry, host capabilities, share permissions, network, remembered
// choice) and the UI turns the typed reasons, hints and warnings into sentences.

/** The three things a file can be asked to do (vision: the user picks whose resources are spent). */
enum class FileAction(val wire: String) {
    /** The computer renders a lightweight preview; the phone shows it. */
    PREVIEW("preview"),

    /** The file's bytes are streamed to the phone. */
    FETCH("fetch"),

    /** The file is opened on the computer and that window is streamed to the phone. */
    STREAM("stream"),
    ;

    /** The share permission this action needs. */
    val permission: Permission
        get() = when (this) {
            PREVIEW -> Permission.PREVIEW
            FETCH -> Permission.FETCH
            STREAM -> Permission.STREAM
        }

    companion object {
        fun fromWire(wire: String?): FileAction? = entries.firstOrNull { it.wire == wire }
    }
}

/** What the phone's connection is like right now. Provided by [NetworkStateMonitor]. */
data class NetworkState(
    /** The phone has a network at all. False means every action that needs the computer is off. */
    val connected: Boolean = true,
    /** The active network is metered (mobile data, or a Wi-Fi the user marked as metered). */
    val metered: Boolean = false,
) {
    companion object {
        val UNMETERED = NetworkState(connected = true, metered = false)
        val METERED = NetworkState(connected = true, metered = true)
        val OFFLINE = NetworkState(connected = false, metered = false)
    }
}

/** `host.state` as far as the rule table cares: is Stream possible on the computer right now. */
data class HostStateInfo(val locked: Boolean, val attended: Boolean, val streamAvailable: Boolean)

fun ServerEvent.HostState.toInfo() = HostStateInfo(locked, attended, streamAvailable)

private const val MIB = 1024L * 1024L

/** `capabilities.previews_unavailable` when gander-render.exe is missing on the computer. */
private const val RENDER_WORKER_MISSING = "render_worker_missing"

/** The extensions that are Stream-first (client spec, `stream_only_types`); user-editable in Settings. */
object StreamOnlyTypes {
    val DEFAULT: Set<String> = setOf(
        "blend", "psd", "ai", "indd", "dwg", "dxf", "skp", "step", "stp", "f3d", "kicad_pcb",
        "kicad_sch", "aep", "prproj", "drp", "als", "flp", "logicx", "cpr", "sln", "ipynb", "accdb",
        "mpp", "vsdx", "max", "ma", "mb", "zpr", "sbs", "sbsar", "uasset", "unity", "mat", "slx",
    )
}

/** Tunable numbers of the rule table. WP-28's Settings screen edits them; defaults are the spec's. */
data class RuleConfig(
    val streamOnlyTypes: Set<String> = StreamOnlyTypes.DEFAULT,
    /**
     * Rule 3: on a metered network a file the phone can open is only fetched by default when it is
     * smaller than this (spec: 25 MiB). Exactly this size is not smaller.
     */
    val meteredHandlerFetchBytes: Long = 25 * MIB,
    /**
     * From this size a fetch on a metered network carries the cellular warning (spec: 50 MiB). The
     * fetch engine (WP-27) shows the dialog; the sheet flags it ahead of time.
     */
    val cellularWarnBytes: Long = 50 * MIB,
) {
    init {
        require(meteredHandlerFetchBytes >= 0 && cellularWarnBytes >= 0) { "thresholds cannot be negative" }
    }
}

/** The extension of [this] in lower case without the dot: the host's `ext`, else from the name; null if none. */
fun Entry.extension(): String? {
    val fromHost = ext?.trim()?.removePrefix(".")?.takeIf { it.isNotEmpty() }
    // A leading dot alone (".gitignore") is a name, not an extension.
    val fromName = name.takeIf { it.lastIndexOf('.') > 0 }?.substringAfterLast('.')?.takeIf { it.isNotEmpty() }
    return (fromHost ?: fromName)?.lowercase(Locale.ROOT)
}

/** Why an action is off. Typed so the UI picks the sentence and tests assert on the cause. */
sealed interface ActionReason {
    /** Waiting or trying again can change the answer (locked PC, busy, offline). */
    val transient: Boolean

    /** A folder, a link or a kind this app does not know: nothing to do with it from here. */
    data object NotAFile : ActionReason {
        override val transient = false
    }

    /** The share does not allow it. [hostDetail] is the host's own words (`permission_denied`), if any. */
    data class NoPermission(val permission: Permission, val hostDetail: String? = null) : ActionReason {
        override val transient = false
    }

    /** `preview: none` for this file ("no renderer for .blend on studio-pc"). */
    data class NoRenderer(val ext: String?) : ActionReason {
        override val transient = false
    }

    /** The host's capabilities say it cannot render this kind of preview. */
    data class HostCannotRender(val kind: PreviewKind) : ActionReason {
        override val transient = false
    }

    /**
     * `capabilities.previews_unavailable`: previews are off on that computer altogether (its
     * render worker is missing, or its preview cache did not open). [renderWorkerMissing] picks
     * the sentence that says what the owner can fix.
     */
    data class PreviewsOffOnHost(val renderWorkerMissing: Boolean) : ActionReason {
        override val transient = false
    }

    /** `too_large`: the requested preview is over the host's limit. */
    data object PreviewTooLarge : ActionReason {
        override val transient = false
    }

    /** `capabilities.stream.enabled` is false: the owner has not turned streaming on. */
    data object StreamNotEnabled : ActionReason {
        override val transient = false
    }

    /** The entry's `stream` outcome is `refused`: policy does not allow this file or type. */
    data object StreamRefusedByPolicy : ActionReason {
        override val transient = false
    }

    /** `stream_refused` from `POST /stream`; [hostMessage] says why in user terms. */
    data class StreamRefused(val hostMessage: String) : ActionReason {
        override val transient = false
    }

    /** `host.state.stream_available` is false right now. */
    data object StreamUnavailableNow : ActionReason {
        override val transient = true
    }

    /** `host_locked` / `host.state.locked`. */
    data object HostLocked : ActionReason {
        override val transient = true
    }

    /** `host_unattended` / `host.state.attended` false: nobody at the computer to see the session. */
    data object HostUnattended : ActionReason {
        override val transient = true
    }

    /** `app_unavailable`: no allowed desktop app for this type on the computer. */
    data object AppUnavailable : ActionReason {
        override val transient = false
    }

    /** `stream_busy`: the computer is already streaming to a phone. */
    data object StreamBusy : ActionReason {
        override val transient = true
    }

    /** The phone has no network. */
    data object PhoneOffline : ActionReason {
        override val transient = true
    }

    /** The phone is online but the computer did not answer. */
    data object HostUnreachable : ActionReason {
        override val transient = true
    }

    /** `not_found`: the file is gone, renamed or hidden on the computer. */
    data object FileGone : ActionReason {
        override val transient = false
    }

    /** The computer sent something this app version does not understand. */
    data object IncompatibleHost : ActionReason {
        override val transient = false
    }

    /** Any other host-side failure the user is told about with the shared error text (revoked, wrong computer...). */
    data class HostError(val error: UiError) : ActionReason {
        override val transient = false
    }

    companion object {
        /**
         * What a refusal from [action] means for the sheet: the screens that run an action (the
         * preview viewers, the fetch engine, the stream viewer) report their failures through
         * this, so the sheet shows the protocol's own reason instead of a generic failure.
         * [ext] names the type in "no renderer for .blend".
         */
        fun fromError(action: FileAction, error: GanderError, ext: String? = null): ActionReason = when (error) {
            is GanderError.Host.PermissionDenied ->
                NoPermission(action.permission, error.hostMessage.takeIf { it.isNotBlank() })
            is GanderError.Host.UnsupportedType ->
                if (action == FileAction.PREVIEW) NoRenderer(ext) else fromUiError(error.toUiError())
            is GanderError.Host.TooLarge ->
                if (action == FileAction.PREVIEW) PreviewTooLarge else fromUiError(error.toUiError())
            is GanderError.Host.HostLocked -> HostLocked
            is GanderError.Host.HostUnattended -> HostUnattended
            is GanderError.Host.AppUnavailable -> AppUnavailable
            is GanderError.Host.StreamBusy -> StreamBusy
            is GanderError.Host.StreamRefused -> StreamRefused(error.hostMessage)
            // 503 stream_unavailable: the host has not turned Stream mode on (capabilities.stream.enabled).
            is GanderError.Host.StreamUnavailable -> StreamNotEnabled
            else -> fromUiError(error.toUiError())
        }

        /** The block that a failed `stat` puts on every action of the file. */
        fun fromUiError(error: UiError): ActionReason = when (error.kind) {
            UiErrorKind.OFFLINE -> HostUnreachable
            UiErrorKind.NOT_FOUND -> FileGone
            UiErrorKind.INCOMPATIBLE -> IncompatibleHost
            else -> HostError(error)
        }
    }
}

/** The one-line "what this costs" under an enabled action. */
sealed interface ActionHint {
    /** The computer renders [kind]: pages, a resized image, text, a poster frame. */
    data class Preview(val kind: PreviewKind) : ActionHint

    /** The bytes come over; [metered] says it is mobile data (client spec: "1.8 GB over cellular"). */
    data class Fetch(val sizeBytes: Long, val metered: Boolean) : ActionHint

    /** A copy is already on the phone: opening it costs no data. */
    data object SavedCopy : ActionHint

    /** The computer opens the file in a desktop app and streams that window. */
    data object Stream : ActionHint
}

/** Something to know before running an enabled action; shown beside the hint. */
sealed interface ActionWarning {
    /** Fetch on a metered network at or above [RuleConfig.cellularWarnBytes]. */
    data class CellularLarge(val sizeBytes: Long) : ActionWarning

    /** `cloud_only`: the computer has to download the file before it can send it. */
    data object CloudOnly : ActionWarning

    /** `capabilities.fetch.range` is false: an interrupted download starts over. */
    data object NoResume : ActionWarning

    /** The computer only has an embedded thumbnail for Office files: page 1, low fidelity. */
    data object LowFidelityPreview : ActionWarning

    /** `/me.network.relayed`: the connection goes through a relay, expect low quality. */
    data object RelayedConnection : ActionWarning

    /** The stream outcome is `ask`: the owner of the computer will be prompted. */
    data object OwnerApproval : ActionWarning
}

sealed interface Availability {
    data object Enabled : Availability

    data class Disabled(val reason: ActionReason) : Availability
}

/** One of the three actions as the sheet shows it. */
data class ActionOption(
    val action: FileAction,
    val availability: Availability,
    /** Set while enabled. */
    val hint: ActionHint?,
    /** Set while enabled. */
    val warnings: List<ActionWarning>,
    /** For a lasting "no": the enabled action to try instead ("no renderer for .blend, try Stream"). */
    val suggestion: FileAction?,
    val isDefault: Boolean,
) {
    val enabled: Boolean get() = availability is Availability.Enabled

    val reason: ActionReason? get() = (availability as? Availability.Disabled)?.reason
}

enum class DefaultSource {
    /** The user picked this action for this extension earlier. */
    REMEMBERED,

    /** The rule table chose it. */
    RULES,
}

/**
 * The answer: [options] in display order (the default first, then the rest by what suits the
 * network, enabled before disabled), and the [default]. It is null only when no action is possible.
 */
data class ActionPlan(
    val options: List<ActionOption>,
    val default: FileAction?,
    val defaultSource: DefaultSource?,
    /** The remembered action for this extension was set aside because it is off right now. */
    val rememberedUnavailable: FileAction?,
) {
    operator fun get(action: FileAction): ActionOption = options.first { it.action == action }
}

/** Everything the table looks at. */
data class ActionInput(
    val entry: Entry,
    /** `/me.capabilities` as last seen; null before the first contact (does not block anything). */
    val capabilities: Capabilities?,
    /** The share's permissions; null while they are not known (does not block anything). */
    val permissions: Set<Permission>?,
    val network: NetworkState,
    /** The user's remembered action for this extension, if any. */
    val remembered: FileAction? = null,
    /** The phone has an app that can open this type (`ACTION_VIEW`). */
    val phoneHasHandler: Boolean = false,
    /** A copy of the file is already stored on the phone (cache, WP-27/28). */
    val cachedLocally: Boolean = false,
    val hostState: HostStateInfo? = null,
    /** The connection to the computer goes through a relay. */
    val relayed: Boolean = false,
    /** Refusals learned from running an action, by action (see [ActionReason.fromError]). */
    val refusals: Map<FileAction, ActionReason> = emptyMap(),
    /** A failed `stat` (unreachable, file gone, access revoked...) blocks everything that needs the computer. */
    val fileBlock: ActionReason? = null,
)

/**
 * The rule table. [plan] is a pure function of [ActionInput]:
 *
 * 1. Each action is enabled or disabled with a reason, from (in this order) what the entry is,
 *    the share's permissions, the entry's own preview/stream outcome, the host's capabilities,
 *    refusals learned from running it, then transient blocks (no network, computer unreachable,
 *    locked). Permanent causes come first so they are not hidden behind a passing outage.
 * 2. The default is the remembered action for the extension if it is enabled; otherwise the
 *    first branch of the spec's table whose action is enabled:
 *    ```
 *    stream enabled on host and (ext in stream_only_types or (preview none and no phone handler)) -> Stream
 *    ext is pdf, image, text, video or audio (or the host previews it as such)                     -> Preview
 *    phone has a handler and (not metered or size < 25 MiB)                                         -> Fetch
 *    preview available                                                                              -> Preview
 *    else                                                                                           -> Fetch (cellular warning on metered)
 *    ```
 *    A branch whose action is off is skipped, so the default is never a disabled button; when the
 *    table leaves nothing, the first enabled action in display order is the default.
 * 3. Display order: default first; the rest as Preview, Fetch, Stream, except that on a metered
 *    network a file of [RuleConfig.meteredHandlerFetchBytes] or more lists Stream before Fetch
 *    ("metered -> Preview/Stream first"); enabled actions come before disabled ones.
 */
class ActionRules(private val config: RuleConfig = RuleConfig()) {

    fun plan(input: ActionInput): ActionPlan {
        val evaluated = FileAction.entries.associateWith { evaluate(it, input) }
        val enabled = evaluated.filterValues { it.reason == null }.keys

        val remembered = input.remembered
        val rememberedEnabled = remembered?.takeIf { it in enabled }
        val ruleChoice = ruleDefault(input, enabled)
        val default = rememberedEnabled ?: ruleChoice
        val source = when {
            rememberedEnabled != null -> DefaultSource.REMEMBERED
            default != null -> DefaultSource.RULES
            else -> null
        }

        val options = displayOrder(input, default, enabled).map { action ->
            val e = evaluated.getValue(action)
            val reason = e.reason
            ActionOption(
                action = action,
                availability = if (reason == null) Availability.Enabled else Availability.Disabled(reason),
                hint = e.hint,
                warnings = e.warnings,
                suggestion = if (reason == null) null else suggestion(action, reason, enabled),
                isDefault = action == default,
            )
        }
        return ActionPlan(
            options = options,
            default = default,
            defaultSource = source,
            rememberedUnavailable = remembered?.takeIf { it !in enabled },
        )
    }

    // Availability ------------------------------------------------------------------------------

    private class Evaluated(val reason: ActionReason?, val hint: ActionHint?, val warnings: List<ActionWarning>)

    private fun off(reason: ActionReason) = Evaluated(reason, null, emptyList())

    private fun evaluate(action: FileAction, i: ActionInput): Evaluated = when (action) {
        FileAction.PREVIEW -> preview(i)
        FileAction.FETCH -> fetch(i)
        FileAction.STREAM -> stream(i)
    }

    /** What stops every action that needs the computer right now, if anything. */
    private fun transientBlock(i: ActionInput, action: FileAction): ActionReason? =
        i.refusals[action]
            ?: if (!i.network.connected) ActionReason.PhoneOffline else i.fileBlock

    private fun preview(i: ActionInput): Evaluated {
        val e = i.entry
        val kind = e.preview
        val reason = when {
            !e.isActionable() -> ActionReason.NotAFile
            i.lacks(Permission.PREVIEW) -> ActionReason.NoPermission(Permission.PREVIEW)
            i.capabilities?.previewsUnavailable != null ->
                ActionReason.PreviewsOffOnHost(i.capabilities.previewsUnavailable == RENDER_WORKER_MISSING)
            kind == PreviewKind.NONE -> ActionReason.NoRenderer(e.extension())
            kind == PreviewKind.UNKNOWN -> ActionReason.IncompatibleHost
            !i.hostRenders(kind) -> ActionReason.HostCannotRender(kind)
            else -> transientBlock(i, FileAction.PREVIEW)
        }
        if (reason != null) return off(reason)
        val warnings = buildList {
            if (kind == PreviewKind.OFFICE && i.capabilities?.previewKinds?.office == OfficePreview.THUMBNAIL) {
                add(ActionWarning.LowFidelityPreview)
            }
        }
        return Evaluated(null, ActionHint.Preview(kind), warnings)
    }

    private fun fetch(i: ActionInput): Evaluated {
        val e = i.entry
        if (!e.isActionable()) return off(ActionReason.NotAFile)
        // A copy already on the phone opens without the computer, whatever the share allows now.
        if (i.cachedLocally) return Evaluated(null, ActionHint.SavedCopy, emptyList())
        val reason = if (i.lacks(Permission.FETCH)) {
            ActionReason.NoPermission(Permission.FETCH)
        } else {
            transientBlock(i, FileAction.FETCH)
        }
        if (reason != null) return off(reason)
        val warnings = buildList {
            if (i.network.metered && e.size >= config.cellularWarnBytes) add(ActionWarning.CellularLarge(e.size))
            if (EntryFlag.CLOUD_ONLY in e.flags) add(ActionWarning.CloudOnly)
            if (i.capabilities?.fetch?.range == false) add(ActionWarning.NoResume)
        }
        return Evaluated(null, ActionHint.Fetch(e.size, i.network.metered), warnings)
    }

    private fun stream(i: ActionInput): Evaluated {
        val e = i.entry
        val state = i.hostState
        val reason = when {
            !e.isActionable() -> ActionReason.NotAFile
            i.lacks(Permission.STREAM) -> ActionReason.NoPermission(Permission.STREAM)
            i.capabilities?.stream?.enabled == false -> ActionReason.StreamNotEnabled
            e.stream == StreamDecision.REFUSED -> ActionReason.StreamRefusedByPolicy
            e.stream == StreamDecision.UNKNOWN -> ActionReason.IncompatibleHost
            state?.locked == true -> ActionReason.HostLocked
            state?.attended == false -> ActionReason.HostUnattended
            state?.streamAvailable == false -> ActionReason.StreamUnavailableNow
            else -> transientBlock(i, FileAction.STREAM)
        }
        if (reason != null) return off(reason)
        val warnings = buildList {
            if (e.stream == StreamDecision.ASK) add(ActionWarning.OwnerApproval)
            if (i.relayed) add(ActionWarning.RelayedConnection)
        }
        return Evaluated(null, ActionHint.Stream, warnings)
    }

    private fun Entry.isActionable() = kind == EntryKind.FILE && EntryFlag.LINK !in flags

    private fun ActionInput.lacks(permission: Permission) = permissions?.let { permission !in it } ?: false

    /** Capabilities are a second line behind the entry's own `preview`: a stale cached listing can promise too much. */
    private fun ActionInput.hostRenders(kind: PreviewKind): Boolean {
        val k = capabilities?.previewKinds ?: return true
        return when (kind) {
            PreviewKind.IMAGE -> k.image
            PreviewKind.PDF -> k.pdf
            PreviewKind.TEXT -> k.text
            PreviewKind.OFFICE -> k.office != OfficePreview.NONE
            PreviewKind.VIDEO_POSTER -> k.videoPoster
            PreviewKind.NONE, PreviewKind.UNKNOWN -> false
        }
    }

    /** Names a way out of a lasting refusal, never out of a missing file or a folder. */
    private fun suggestion(action: FileAction, reason: ActionReason, enabled: Set<FileAction>): FileAction? {
        if (reason.transient || reason == ActionReason.NotAFile || reason == ActionReason.FileGone) return null
        val preferred = when (action) {
            FileAction.PREVIEW -> listOf(FileAction.STREAM, FileAction.FETCH)
            FileAction.FETCH -> listOf(FileAction.PREVIEW, FileAction.STREAM)
            FileAction.STREAM -> listOf(FileAction.PREVIEW, FileAction.FETCH)
        }
        return preferred.firstOrNull { it in enabled }
    }

    // Default and order ---------------------------------------------------------------------------

    private fun ruleDefault(i: ActionInput, enabled: Set<FileAction>): FileAction? {
        val e = i.entry
        val ext = e.extension()
        val metered = i.network.metered

        val streamOnly = (ext != null && ext in config.streamOnlyTypes) ||
            (e.preview == PreviewKind.NONE && !i.phoneHasHandler)
        if (FileAction.STREAM in enabled && i.capabilities?.stream?.enabled == true && streamOnly) {
            return FileAction.STREAM
        }
        if (FileAction.PREVIEW in enabled && isPreviewType(e, ext)) return FileAction.PREVIEW
        if (FileAction.FETCH in enabled && i.phoneHasHandler &&
            (!metered || e.size < config.meteredHandlerFetchBytes)
        ) {
            return FileAction.FETCH
        }
        if (FileAction.PREVIEW in enabled) return FileAction.PREVIEW
        if (FileAction.FETCH in enabled) return FileAction.FETCH
        return baseOrder(i).firstOrNull { it in enabled }
    }

    private fun isPreviewType(e: Entry, ext: String?): Boolean =
        (ext != null && ext in PREVIEW_EXTENSIONS) || e.preview in HOST_PREVIEWED_KINDS

    private fun baseOrder(i: ActionInput): List<FileAction> =
        if (i.network.metered && i.entry.size >= config.meteredHandlerFetchBytes) {
            listOf(FileAction.PREVIEW, FileAction.STREAM, FileAction.FETCH)
        } else {
            listOf(FileAction.PREVIEW, FileAction.FETCH, FileAction.STREAM)
        }

    private fun displayOrder(i: ActionInput, default: FileAction?, enabled: Set<FileAction>): List<FileAction> {
        val rest = baseOrder(i).filter { it != default }
        val (on, off) = rest.partition { it in enabled }
        return listOfNotNull(default) + on + off
    }

    companion object {
        private val HOST_PREVIEWED_KINDS =
            setOf(PreviewKind.IMAGE, PreviewKind.PDF, PreviewKind.TEXT, PreviewKind.VIDEO_POSTER)

        /** The table's "pdf, images, text, video, audio" by extension. */
        val PREVIEW_EXTENSIONS: Set<String> = setOf(
            "pdf",
            // images
            "jpg", "jpeg", "png", "gif", "webp", "bmp", "heic", "heif", "avif", "tif", "tiff", "svg",
            "dng", "cr2", "nef", "arw", "raw",
            // text
            "txt", "md", "csv", "json", "xml", "yaml", "yml", "log", "ini", "toml", "kt", "rs", "py", "js",
            "ts", "html", "css",
            // video
            "mp4", "mkv", "mov", "avi", "webm", "m4v", "wmv", "mpg", "mpeg",
            // audio
            "mp3", "wav", "flac", "m4a", "aac", "ogg", "opus", "wma",
        )
    }
}
