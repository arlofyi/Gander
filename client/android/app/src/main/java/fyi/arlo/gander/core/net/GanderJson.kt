package fyi.arlo.gander.core.net

import fyi.arlo.gander.core.net.protocol.ServerEvent
import kotlinx.serialization.SerializationException
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive

/** The one JSON configuration for the Gander protocol. */
object GanderJson {
    /**
     * - `ignoreUnknownKeys`: protocol rule "clients ignore unknown JSON fields".
     * - `explicitNulls` stays true so required-but-nullable fields (`ctime`, `next_cursor`, ...)
     *   are written as `null`; optional fields default to null and `encodeDefaults = false`
     *   leaves them out.
     */
    val json: Json = Json {
        ignoreUnknownKeys = true
        encodeDefaults = false
        explicitNulls = true
    }

    /** The protocol event types this client understands (serial names of [ServerEvent]). */
    private val knownEventTypes: Set<String> by lazy {
        val descriptor = ServerEvent.serializer().descriptor
        // A sealed descriptor has element 0 = the discriminator, element 1 = the subclasses.
        val subclasses = descriptor.getElementDescriptor(1)
        (0 until subclasses.elementsCount).map { subclasses.getElementName(it) }.toSet() -
            UNKNOWN_SERIAL_NAME
    }

    /**
     * Decodes one `/v1/events` text frame. Unknown `type` values decode to [ServerEvent.Unknown]
     * (protocol compatibility rule). Throws [SerializationException] for a frame that is not a
     * JSON object with a string `type`, or a known type with a malformed body.
     */
    fun decodeServerEvent(text: String): ServerEvent {
        val element = json.parseToJsonElement(text)
        val type = ((element as? JsonObject)?.get("type") as? JsonPrimitive)
            ?.takeIf { it.isString }?.content
            ?: throw SerializationException("event frame without a string type")
        if (type !in knownEventTypes) return ServerEvent.Unknown(type)
        return json.decodeFromJsonElement(ServerEvent.serializer(), element)
    }

    private const val UNKNOWN_SERIAL_NAME = "gander.client.unknown"
}
