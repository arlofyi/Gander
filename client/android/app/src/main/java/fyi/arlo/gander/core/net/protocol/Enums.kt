package fyi.arlo.gander.core.net.protocol

import kotlinx.serialization.Serializable

// Closed sets from protocol/openapi.yaml. Every enum has an UNKNOWN entry that a newer host's
// value decodes to; UNKNOWN is never sent.

/** `ErrorCode` in the protocol; the HTTP status that normally accompanies it is [status]. */
@Serializable(with = ErrorCode.Serializer::class)
enum class ErrorCode(override val wire: String, val status: Int) : WireEnum {
    BAD_REQUEST("bad_request", 400),
    BAD_HOST("bad_host", 400),
    BAD_PATH("bad_path", 400),
    TOKEN_MISSING("token_missing", 401),
    TOKEN_INVALID("token_invalid", 401),
    TOKEN_REVOKED("token_revoked", 401),
    FORBIDDEN("forbidden", 403),
    BROWSER_REQUEST("browser_request", 403),
    NETWORK_IDENTITY_MISMATCH("network_identity_mismatch", 403),
    PERMISSION_DENIED("permission_denied", 403),
    SHARE_NOT_FOUND("share_not_found", 404),
    NOT_FOUND("not_found", 404),
    PAIRING_CLOSED("pairing_closed", 404),
    STREAM_BUSY("stream_busy", 409),
    PAIRING_IN_PROGRESS("pairing_in_progress", 409),
    PAIRING_EXPIRED("pairing_expired", 410),
    HOST_LOCKED("host_locked", 412),
    HOST_UNATTENDED("host_unattended", 412),
    APP_UNAVAILABLE("app_unavailable", 412),
    FILE_CHANGED("file_changed", 412),
    TOO_LARGE("too_large", 413),
    UNSUPPORTED_TYPE("unsupported_type", 415),
    RANGE_NOT_SATISFIABLE("range_not_satisfiable", 416),
    STREAM_REFUSED("stream_refused", 422),
    TOO_MANY_REQUESTS("too_many_requests", 429),
    PAIRING_RATE_LIMITED("pairing_rate_limited", 429),
    INTERNAL("internal", 500),
    GENERATING("generating", 503),
    STREAM_UNAVAILABLE("stream_unavailable", 503),
    UNKNOWN("", 0),
    ;

    object Serializer : WireEnumSerializer<ErrorCode>("ErrorCode", ErrorCode.entries, ErrorCode.UNKNOWN)
}

@Serializable(with = HostOs.Serializer::class)
enum class HostOs(override val wire: String) : WireEnum {
    WINDOWS("windows"), MACOS("macos"), LINUX("linux"), UNKNOWN("");

    object Serializer : WireEnumSerializer<HostOs>("HostOs", HostOs.entries, HostOs.UNKNOWN)
}

@Serializable(with = NetworkAdapter.Serializer::class)
enum class NetworkAdapter(override val wire: String) : WireEnum {
    TAILSCALE("tailscale"), GENERIC("generic"), UNKNOWN("");

    object Serializer : WireEnumSerializer<NetworkAdapter>("NetworkAdapter", NetworkAdapter.entries, NetworkAdapter.UNKNOWN)
}

/** `NetworkInfo.relayed`: the path from the host to this device. */
@Serializable(with = RelayPath.Serializer::class)
enum class RelayPath(override val wire: String) : WireEnum {
    DIRECT("direct"), PEER_RELAY("peer_relay"), DERP("derp"), UNKNOWN("");

    object Serializer : WireEnumSerializer<RelayPath>("RelayPath", RelayPath.entries, RelayPath.UNKNOWN)
}

/**
 * `SearchResponse.via` (WP-66): what answered a search. [INDEX] is the computer's Windows Search
 * index (names, and file text when asked); [WALK] is the bounded walk over names. Absent from a
 * host that predates it, which is a walk.
 */
@Serializable(with = SearchVia.Serializer::class)
enum class SearchVia(override val wire: String) : WireEnum {
    INDEX("index"), WALK("walk"), UNKNOWN("");

    object Serializer : WireEnumSerializer<SearchVia>("SearchVia", SearchVia.entries, SearchVia.UNKNOWN)
}

@Serializable(with = HashAlgo.Serializer::class)
enum class HashAlgo(override val wire: String) : WireEnum {
    BLAKE3("blake3"), SHA256("sha256"), UNKNOWN("");

    object Serializer : WireEnumSerializer<HashAlgo>("HashAlgo", HashAlgo.entries, HashAlgo.UNKNOWN)
}

@Serializable(with = VideoCodec.Serializer::class)
enum class VideoCodec(override val wire: String) : WireEnum {
    H264("h264"), HEVC("hevc"), UNKNOWN("");

    object Serializer : WireEnumSerializer<VideoCodec>("VideoCodec", VideoCodec.entries, VideoCodec.UNKNOWN)
}

/**
 * `capabilities.stream.clipboard` (WP-56): which way clipboard text may cross during a stream, from
 * the owner's setting on the computer. Omitted by hosts that predate it and by hosts where it is
 * off, so an absent value is [OFF]; a value this client does not know is [UNKNOWN], which allows
 * neither direction.
 */
@Serializable(with = ClipboardMode.Serializer::class)
enum class ClipboardMode(override val wire: String) : WireEnum {
    OFF("off"), TO_PHONE("to_phone"), FROM_PHONE("from_phone"), BOTH("both"), UNKNOWN("");

    /** The phone may read the computer's clipboard text ("Copy from computer"). */
    val canCopyFromComputer: Boolean get() = this == TO_PHONE || this == BOTH

    /** The phone may put text on the computer's clipboard ("Paste to computer"). */
    val canPasteToComputer: Boolean get() = this == FROM_PHONE || this == BOTH

    object Serializer : WireEnumSerializer<ClipboardMode>("ClipboardMode", ClipboardMode.entries, ClipboardMode.UNKNOWN)
}

/** `PreviewKinds.office`: how this host previews Office documents. */
@Serializable(with = OfficePreview.Serializer::class)
enum class OfficePreview(override val wire: String) : WireEnum {
    THUMBNAIL("thumbnail"), RENDER("render"), NONE("none"), UNKNOWN("");

    object Serializer : WireEnumSerializer<OfficePreview>("OfficePreview", OfficePreview.entries, OfficePreview.UNKNOWN)
}

@Serializable(with = Permission.Serializer::class)
enum class Permission(override val wire: String) : WireEnum {
    LIST("list"), PREVIEW("preview"), FETCH("fetch"), STREAM("stream"), UNKNOWN("");

    object Serializer : WireEnumSerializer<Permission>("Permission", Permission.entries, Permission.UNKNOWN)
}

/** Stream policy outcome for this device and file (`StreamDecision`). */
@Serializable(with = StreamDecision.Serializer::class)
enum class StreamDecision(override val wire: String) : WireEnum {
    ALLOWED("allowed"), REFUSED("refused"), ASK("ask"), UNKNOWN("");

    object Serializer : WireEnumSerializer<StreamDecision>("StreamDecision", StreamDecision.entries, StreamDecision.UNKNOWN)
}

@Serializable(with = EntryKind.Serializer::class)
enum class EntryKind(override val wire: String) : WireEnum {
    FILE("file"), DIR("dir"), UNKNOWN("");

    object Serializer : WireEnumSerializer<EntryKind>("EntryKind", EntryKind.entries, EntryKind.UNKNOWN)
}

@Serializable(with = EntryFlag.Serializer::class)
enum class EntryFlag(override val wire: String) : WireEnum {
    HIDDEN("hidden"),
    READONLY("readonly"),
    SYSTEM("system"),
    CLOUD_ONLY("cloud_only"),
    LINK("link"),
    ENCRYPTED("encrypted"),
    UNKNOWN(""),
    ;

    object Serializer : WireEnumSerializer<EntryFlag>("EntryFlag", EntryFlag.entries, EntryFlag.UNKNOWN)
}

@Serializable(with = PreviewKind.Serializer::class)
enum class PreviewKind(override val wire: String) : WireEnum {
    IMAGE("image"),
    PDF("pdf"),
    TEXT("text"),
    OFFICE("office"),
    VIDEO_POSTER("video_poster"),
    NONE("none"),
    UNKNOWN(""),
    ;

    object Serializer : WireEnumSerializer<PreviewKind>("PreviewKind", PreviewKind.entries, PreviewKind.UNKNOWN)
}

@Serializable(with = StreamState.Serializer::class)
enum class StreamState(override val wire: String) : WireEnum {
    REQUESTED("requested"),
    APPROVING("approving"),
    LAUNCHING("launching"),
    PICK("pick"),
    WINDOW_FOUND("window_found"),
    STREAMING("streaming"),
    PAUSED("paused"),
    ENDED("ended"),
    UNKNOWN(""),
    ;

    object Serializer : WireEnumSerializer<StreamState>("StreamState", StreamState.entries, StreamState.UNKNOWN)
}

@Serializable(with = StreamReason.Serializer::class)
enum class StreamReason(override val wire: String) : WireEnum {
    FOCUS_LOST("focus_lost"),
    LOCAL_USER_ACTIVE("local_user_active"),
    PHONE_BACKGROUND("phone_background"),
    NETWORK("network"),
    CLIENT_CLOSED("client_closed"),
    HOST_KILL_SWITCH("host_kill_switch"),
    WINDOW_CLOSED("window_closed"),
    REVOKED("revoked"),
    TIMEOUT("timeout"),
    ERROR("error"),
    UNKNOWN(""),
    ;

    object Serializer : WireEnumSerializer<StreamReason>("StreamReason", StreamReason.entries, StreamReason.UNKNOWN)
}

@Serializable(with = MouseButton.Serializer::class)
enum class MouseButton(override val wire: String) : WireEnum {
    LEFT("left"), RIGHT("right"), MIDDLE("middle"), UNKNOWN("");

    object Serializer : WireEnumSerializer<MouseButton>("MouseButton", MouseButton.entries, MouseButton.UNKNOWN)
}

@Serializable(with = KeyModifier.Serializer::class)
enum class KeyModifier(override val wire: String) : WireEnum {
    CTRL("ctrl"), SHIFT("shift"), ALT("alt"), META("meta"), UNKNOWN("");

    object Serializer : WireEnumSerializer<KeyModifier>("KeyModifier", KeyModifier.entries, KeyModifier.UNKNOWN)
}
