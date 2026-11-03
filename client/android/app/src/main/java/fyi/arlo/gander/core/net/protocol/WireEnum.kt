package fyi.arlo.gander.core.net.protocol

import kotlinx.serialization.KSerializer
import kotlinx.serialization.descriptors.PrimitiveKind
import kotlinx.serialization.descriptors.PrimitiveSerialDescriptor
import kotlinx.serialization.descriptors.SerialDescriptor
import kotlinx.serialization.encoding.Decoder
import kotlinx.serialization.encoding.Encoder

/** An enum whose JSON form is [wire]. */
interface WireEnum {
    val wire: String
}

/**
 * Forward-compatible enum serializer: a value this client does not know (a newer host added it)
 * decodes to [unknown] instead of failing the whole response. Protocol rule: clients tolerate
 * additive changes; the UI then shows the value as "not supported by this app version".
 */
abstract class WireEnumSerializer<E>(
    serialName: String,
    private val values: List<E>,
    private val unknown: E,
) : KSerializer<E> where E : Enum<E>, E : WireEnum {
    override val descriptor: SerialDescriptor =
        PrimitiveSerialDescriptor("fyi.arlo.gander.$serialName", PrimitiveKind.STRING)

    override fun deserialize(decoder: Decoder): E {
        val raw = decoder.decodeString()
        return values.firstOrNull { it.wire == raw } ?: unknown
    }

    override fun serialize(encoder: Encoder, value: E) {
        encoder.encodeString(value.wire)
    }
}
