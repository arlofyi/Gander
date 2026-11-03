package fyi.arlo.gander.core.net.protocol

import kotlinx.serialization.EncodeDefault
import kotlinx.serialization.ExperimentalSerializationApi
import kotlinx.serialization.KSerializer
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.SerializationException
import kotlinx.serialization.descriptors.SerialDescriptor
import kotlinx.serialization.encoding.Decoder
import kotlinx.serialization.encoding.Encoder
import kotlinx.serialization.json.JsonDecoder
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonEncoder
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull

// DTOs for host identity, /me and errors. Field names and nullability mirror
// protocol/openapi.yaml exactly: a required-but-nullable field has no default (it is always
// present on the wire, possibly as null); an optional field defaults to null and is omitted
// when null. Timestamps stay ISO-8601 strings; parse them where they are displayed.

/** `ErrorBody`: every refusal. Map it to a [fyi.arlo.gander.core.net.GanderError] for the UI. */
@Serializable
data class ErrorBody(val error: ErrorDetail)

@Serializable
data class ErrorDetail(
    val code: ErrorCode,
    /** English, never paths, tokens or file contents. Not localised; show the mapped error. */
    val message: String,
    // Always written: the host always sends it, and DTO round-trip tests compare byte for byte.
    @OptIn(ExperimentalSerializationApi::class)
    @EncodeDefault
    @SerialName("retry_after_ms")
    val retryAfterMs: Long = 0,
)

/** `GET /v1/host`, the only unauthenticated read. */
@Serializable
data class HostHello(
    val protocol: List<Int>,
    @SerialName("pairing_open") val pairingOpen: Boolean,
    /** base64url SHA-256 of the host certificate's SubjectPublicKeyInfo: the pin. */
    @SerialName("host_id") val hostId: String,
)

@Serializable
data class DeviceSummary(
    val id: String,
    val name: String,
    @SerialName("created_at") val createdAt: String,
    @SerialName("last_seen_at") val lastSeenAt: String?,
)

@Serializable
data class HostDetails(
    val id: String,
    val name: String,
    val os: HostOs,
    @SerialName("os_version") val osVersion: String,
    @SerialName("app_version") val appVersion: String,
    /** IPv4/IPv6 literals in the host's preferred order; feed them to `HostEndpoint`. */
    val addresses: List<String>,
    @SerialName("magic_dns") val magicDns: String?,
)

@Serializable
data class NetworkIdentity(val login: String, val node: String)

@Serializable
data class NetworkInfo(
    val adapter: NetworkAdapter,
    val identity: NetworkIdentity?,
    /** Null when unknown or not applicable (generic adapter). */
    val relayed: RelayPath?,
)

@Serializable
data class PreviewKinds(
    val image: Boolean,
    val pdf: Boolean,
    val text: Boolean,
    val office: OfficePreview,
    @SerialName("video_poster") val videoPoster: Boolean,
    val raw: Boolean,
)

/** `Capabilities.search`: `false`, `true` (bounded substring walk) or `"indexed"`. */
@Serializable(with = SearchCapability.Serializer::class)
enum class SearchCapability {
    NONE, BOUNDED, INDEXED;

    object Serializer : KSerializer<SearchCapability> {
        override val descriptor: SerialDescriptor = JsonElement.serializer().descriptor

        override fun deserialize(decoder: Decoder): SearchCapability {
            val element = (decoder as? JsonDecoder)?.decodeJsonElement()
                ?: throw SerializationException("SearchCapability is JSON only")
            val primitive = element as? JsonPrimitive
                ?: throw SerializationException("search must be a boolean or \"indexed\"")
            primitive.booleanOrNull?.let { return if (it) BOUNDED else NONE }
            // Any other string from a newer host is at least a working search.
            return if (primitive.isString && primitive.content == "indexed") INDEXED else BOUNDED
        }

        override fun serialize(encoder: Encoder, value: SearchCapability) {
            val json = encoder as? JsonEncoder ?: throw SerializationException("JSON only")
            json.encodeJsonElement(
                when (value) {
                    NONE -> JsonPrimitive(false)
                    BOUNDED -> JsonPrimitive(true)
                    INDEXED -> JsonPrimitive("indexed")
                },
            )
        }
    }
}

/** `capabilities.fetch.zip`: the host's folder download and its limits (absent on a host without it). */
@Serializable
data class ZipCapability(
    @SerialName("max_bytes") val maxBytes: Long,
    @SerialName("max_entries") val maxEntries: Long,
)

@Serializable
data class FetchCapabilities(
    val range: Boolean,
    val hash: List<HashAlgo>,
    val zip: ZipCapability? = null,
)

@Serializable
data class StreamCapabilities(
    val enabled: Boolean,
    val codecs: List<VideoCodec>,
    @SerialName("max_sessions") val maxSessions: Int,
    val audio: Boolean,
    val input: Boolean,
    @SerialName("fit_to_phone") val fitToPhone: Boolean,
    /** Which way clipboard text may cross during a stream (the owner's setting); absent means off. */
    val clipboard: ClipboardMode = ClipboardMode.OFF,
)

@Serializable
data class Capabilities(
    val list: Boolean,
    val search: SearchCapability,
    val recent: Boolean,
    val events: Boolean,
    @SerialName("thumb_sizes") val thumbSizes: List<Int>,
    @SerialName("preview_max_width") val previewMaxWidth: Int,
    @SerialName("preview_kinds") val previewKinds: PreviewKinds,
    val fetch: FetchCapabilities,
    val stream: StreamCapabilities,
    val write: Boolean,
    /**
     * Optional: why previews and thumbnails are off on this computer (`render_worker_missing`,
     * `cache_unavailable`, or a value a newer host adds). Absent when they work.
     */
    @SerialName("previews_unavailable") val previewsUnavailable: String? = null,
)

@Serializable
data class Limits(
    @SerialName("list_max") val listMax: Int,
    @SerialName("search_max") val searchMax: Int,
    val concurrency: Int,
)

/** `GET /v1/me`. Re-read on every connect; `host.addresses` refreshes the endpoint. */
@Serializable
data class MeResponse(
    val device: DeviceSummary,
    val host: HostDetails,
    val network: NetworkInfo,
    val capabilities: Capabilities,
    val limits: Limits,
)

/** `POST /v1/token/rotate`. The token is a secret: never log this object's [token]. */
@Serializable
data class TokenRotateResponse(val token: String) {
    override fun toString(): String = "TokenRotateResponse(token=<redacted>)"
}
