package fyi.arlo.gander.core.net

import fyi.arlo.gander.core.net.protocol.ClientEvent
import fyi.arlo.gander.core.net.protocol.EntryKind
import fyi.arlo.gander.core.net.protocol.EntryFlag
import fyi.arlo.gander.core.net.protocol.ErrorBody
import fyi.arlo.gander.core.net.protocol.ErrorCode
import fyi.arlo.gander.core.net.protocol.Entry
import fyi.arlo.gander.core.net.protocol.HashAlgo
import fyi.arlo.gander.core.net.protocol.HashResponse
import fyi.arlo.gander.core.net.protocol.HostHello
import fyi.arlo.gander.core.net.protocol.InputMessage
import fyi.arlo.gander.core.net.protocol.ListResponse
import fyi.arlo.gander.core.net.protocol.MeResponse
import fyi.arlo.gander.core.net.protocol.PairApproved
import fyi.arlo.gander.core.net.protocol.PairPollStatus
import fyi.arlo.gander.core.net.protocol.PairRequest
import fyi.arlo.gander.core.net.protocol.PairStarted
import fyi.arlo.gander.core.net.protocol.Permission
import fyi.arlo.gander.core.net.protocol.PreviewInfo
import fyi.arlo.gander.core.net.protocol.PreviewKind
import fyi.arlo.gander.core.net.protocol.RecentResponse
import fyi.arlo.gander.core.net.protocol.SearchCapability
import fyi.arlo.gander.core.net.protocol.SearchResponse
import fyi.arlo.gander.core.net.protocol.ServerEvent
import fyi.arlo.gander.core.net.protocol.SharesResponse
import fyi.arlo.gander.core.net.protocol.StreamCreated
import fyi.arlo.gander.core.net.protocol.StreamDecision
import fyi.arlo.gander.core.net.protocol.StreamReason
import fyi.arlo.gander.core.net.protocol.StreamRequest
import fyi.arlo.gander.core.net.protocol.StreamState
import fyi.arlo.gander.core.net.protocol.TilePage
import fyi.arlo.gander.core.net.protocol.TokenRotateResponse
import fyi.arlo.gander.core.net.protocol.VideoCodec
import fyi.arlo.gander.core.net.protocol.WireEnum
import fyi.arlo.gander.core.net.protocol.ZipCapability
import fyi.arlo.gander.core.net.protocol.ZipPlan
import kotlinx.serialization.KSerializer
import kotlinx.serialization.json.JsonElement
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Decodes every example under components.examples of protocol/openapi.yaml into the Kotlin DTO
 * its `x-gander-schema` names, and re-encodes it to the identical JSON. A new example whose
 * schema has no DTO, a renamed field or a changed enum fails here.
 */
class ProtocolDtoTest {
    private val json = GanderJson.json

    /** Schema name -> (decode, encode). ServerEvent goes through the tolerant event decoder. */
    private val codecs: Map<String, Pair<(String) -> Any, (Any) -> String>> = mapOf(
        "ErrorBody" to codec(ErrorBody.serializer()),
        "HostHello" to codec(HostHello.serializer()),
        "MeResponse" to codec(MeResponse.serializer()),
        "TokenRotateResponse" to codec(TokenRotateResponse.serializer()),
        "PairRequest" to codec(PairRequest.serializer()),
        "PairStarted" to codec(PairStarted.serializer()),
        "PairPollStatus" to codec(PairPollStatus.serializer()),
        "PairApproved" to codec(PairApproved.serializer()),
        "SharesResponse" to codec(SharesResponse.serializer()),
        "ListResponse" to codec(ListResponse.serializer()),
        "Entry" to codec(Entry.serializer()),
        "SearchResponse" to codec(SearchResponse.serializer()),
        "RecentResponse" to codec(RecentResponse.serializer()),
        "PreviewInfo" to codec(PreviewInfo.serializer()),
        "HashResponse" to codec(HashResponse.serializer()),
        "ZipPlan" to codec(ZipPlan.serializer()),
        "StreamRequest" to codec(StreamRequest.serializer()),
        "StreamCreated" to codec(StreamCreated.serializer()),
        "ServerEvent" to Pair(
            { text: String ->
                val event = GanderJson.decodeServerEvent(text)
                check(event !is ServerEvent.Unknown) { "example decoded as an unknown event type" }
                event
            },
            { value: Any -> json.encodeToString(ServerEvent.serializer(), value as ServerEvent) },
        ),
        "ClientEvent" to codec(ClientEvent.serializer()),
        "InputMessage" to codec(InputMessage.serializer()),
    )

    @Suppress("UNCHECKED_CAST")
    private fun <T : Any> codec(serializer: KSerializer<T>): Pair<(String) -> Any, (Any) -> String> = Pair(
        { text: String -> json.decodeFromString(serializer, text) },
        { value: Any -> json.encodeToString(serializer, value as T) },
    )

    @Test
    fun everySpecExampleDecodesAndRoundTrips() {
        val examples = ProtocolExamples.examples
        assertTrue("expected the full example set, got ${examples.size}", examples.size >= 40)
        val failures = mutableListOf<String>()
        for ((name, example) in examples) {
            val (schema, value) = example
            val (decode, encode) = codecs[schema] ?: run {
                failures += "$name: no Kotlin DTO for schema $schema"
                continue
            }
            try {
                val decoded = decode(value.toString())
                val reencoded: JsonElement = json.parseToJsonElement(encode(decoded))
                if (reencoded != value) failures += "$name: round trip differs\n  spec: $value\n  ours: $reencoded"
            } catch (e: Exception) {
                failures += "$name ($schema): ${e.javaClass.simpleName}: ${e.message}"
            }
        }
        assertTrue(failures.joinToString("\n"), failures.isEmpty())
    }

    @Test
    fun decodedExamplesCarryTheExpectedValues() {
        val me = json.decodeFromString(MeResponse.serializer(), ProtocolExamples.exampleJson("MeResponse"))
        assertEquals(SearchCapability.BOUNDED, me.capabilities.search)
        assertEquals(listOf(HashAlgo.BLAKE3, HashAlgo.SHA256), me.capabilities.fetch.hash)
        assertEquals(ZipCapability(maxBytes = 4_294_967_296L, maxEntries = 20_000L), me.capabilities.fetch.zip)
        val generic = json.decodeFromString(MeResponse.serializer(), ProtocolExamples.exampleJson("MeGenericAdapter"))
        assertEquals(SearchCapability.INDEXED, generic.capabilities.search)
        assertEquals(null, generic.network.identity)
        assertEquals(null, generic.capabilities.fetch.zip) // an older host: no folder download
        assertEquals(null, generic.device.lastSeenAt)

        val list = json.decodeFromString(ListResponse.serializer(), ProtocolExamples.exampleJson("ListResponse"))
        assertEquals(EntryKind.DIR, list.entries[0].kind)
        assertEquals(listOf(EntryFlag.CLOUD_ONLY), list.entries[1].flags)
        assertEquals(48213001L, list.entries[1].size)
        assertEquals(StreamDecision.ALLOWED, list.entries[1].stream)

        val plan = json.decodeFromString(ZipPlan.serializer(), ProtocolExamples.exampleJson("ZipPlan"))
        assertEquals(ZipPlan(entries = 1204, bytes = 83_886_080, skipped = 2, cloudOnly = 0), plan)
        val tooLarge = json.decodeFromString(ErrorBody.serializer(), ProtocolExamples.exampleJson("ErrorZipTooLarge"))
        assertEquals(ErrorCode.TOO_LARGE, tooLarge.error.code)

        val stat = json.decodeFromString(Entry.serializer(), ProtocolExamples.exampleJson("StatImage"))
        assertEquals(6, stat.meta?.image?.orientation)

        val pick = GanderJson.decodeServerEvent(ProtocolExamples.exampleJson("EventStreamPick"))
        assertEquals(StreamState.PICK, (pick as ServerEvent.StreamStateChanged).state)
        assertEquals(2, pick.candidates?.size)

        val approved = json.decodeFromString(PairApproved.serializer(), ProtocolExamples.exampleJson("PairApproved"))
        assertFalse("token must not reach toString", approved.toString().contains(approved.token))
    }

    @Test
    fun theDeepZoomFieldsOfPreviewInfoAreDecodedAndOptional() {
        val pdf = json.decodeFromString(PreviewInfo.serializer(), ProtocolExamples.exampleJson("PreviewInfoTiles"))
        assertEquals(512, pdf.tileSize)
        assertEquals(6, pdf.tileLevels)
        assertEquals(
            listOf(TilePage(7016, 9933, 6), TilePage(7016, 9933, 6), TilePage(2480, 3508, 4)),
            pdf.tilePages,
        )
        val photo = json.decodeFromString(PreviewInfo.serializer(), ProtocolExamples.exampleJson("PreviewInfoImageTiles"))
        assertEquals(PreviewKind.IMAGE, photo.kind)
        assertEquals(listOf(TilePage(6000, 4500, 5)), photo.tilePages)
        // a host without deep zoom omits the fields; so does an older one
        val plain = json.decodeFromString(PreviewInfo.serializer(), ProtocolExamples.exampleJson("PreviewInfo"))
        assertNull(plain.tileSize)
        assertNull(plain.tileLevels)
        assertNull(plain.tilePages)
        assertFalse(json.encodeToString(PreviewInfo.serializer(), plain).contains("tile_"))
    }

    @Test
    fun kotlinEnumsMatchTheSpecEnums() {
        fun <E> wires(values: List<E>): List<String> where E : Enum<E>, E : WireEnum =
            values.map { it.wire }.filter { it.isNotEmpty() }
        assertEquals(ProtocolExamples.schemaEnum("ErrorCode"), wires(ErrorCode.entries))
        assertEquals(ProtocolExamples.schemaEnum("HashAlgo"), wires(HashAlgo.entries))
        assertEquals(ProtocolExamples.schemaEnum("VideoCodec"), wires(VideoCodec.entries))
        assertEquals(ProtocolExamples.schemaEnum("Permission"), wires(Permission.entries))
        assertEquals(ProtocolExamples.schemaEnum("StreamDecision"), wires(StreamDecision.entries))
        assertEquals(ProtocolExamples.schemaEnum("PreviewKind"), wires(PreviewKind.entries))
        assertEquals(ProtocolExamples.schemaEnum("StreamState"), wires(StreamState.entries))
        assertEquals(ProtocolExamples.schemaEnum("StreamReason"), wires(StreamReason.entries))
    }

    @Test
    fun theScrollSignConventionInTheSpecIsTheOneTheClientSends() {
        // The client sends the Windows wheel convention (StreamGesturesTest pins its signs). If the
        // protocol document changes the convention, this fails until the client follows.
        val dy = ProtocolExamples.scrollAxisDescription("dy")
        assertTrue(dy, dy.contains("Windows convention") && dy.contains("positive is the wheel turned away from the user"))
        val dx = ProtocolExamples.scrollAxisDescription("dx")
        assertTrue(dx, dx.contains("positive is a tilt to the right"))
    }

    @Test
    fun forwardCompatibleDecoding() {
        // Unknown fields, unknown enum values and unknown event types do not break a client.
        val entry = json.decodeFromString(
            Entry.serializer(),
            """{"name":"x","kind":"socket","size":1,"mtime":"2026-01-01T00:00:00Z","ctime":null,
               "flags":["sparse","hidden"],"thumb":false,"preview":"cad","stream":"maybe","new_field":{"a":1}}""",
        )
        assertEquals(EntryKind.UNKNOWN, entry.kind)
        assertEquals(listOf(EntryFlag.UNKNOWN, EntryFlag.HIDDEN), entry.flags)
        assertEquals(PreviewKind.UNKNOWN, entry.preview)
        assertEquals(StreamDecision.UNKNOWN, entry.stream)
        assertEquals(ServerEvent.Unknown("host.future"), GanderJson.decodeServerEvent("""{"type":"host.future","x":1}"""))
        assertEquals(ErrorCode.UNKNOWN, json.decodeFromString(ErrorBody.serializer(), """{"error":{"code":"nope","message":""}}""").error.code)
    }
}
