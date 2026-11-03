package fyi.arlo.gander.core.data.db

import fyi.arlo.gander.core.data.browse.KindsFilter
import fyi.arlo.gander.core.data.browse.ListQuery
import fyi.arlo.gander.core.data.browse.ListingSnapshot
import fyi.arlo.gander.core.data.browse.SortField
import fyi.arlo.gander.core.data.browse.SortOrder
import fyi.arlo.gander.core.data.browse.SortSpec
import fyi.arlo.gander.core.net.protocol.EntryFlag
import fyi.arlo.gander.testing.SampleData
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Test

/** The pure side of the Room stores: rows round-trip to the domain types without a database. */
class RowCodecTest {

    private fun snapshot(query: ListQuery = ListQuery()) = ListingSnapshot(
        hostId = "host",
        shareId = "shr_1",
        path = "Clients/Acme",
        query = query,
        etag = "W/\"abc\"",
        entries = listOf(
            SampleData.dir("Sub"),
            SampleData.file("Deck.pptx", size = 48_213_001, flags = listOf(EntryFlag.CLOUD_ONLY)),
        ),
        nextCursor = "b2Zmc2V0OjE",
        truncated = true,
        unlistable = 3,
        fetchedAtMillis = 123_456L,
    )

    @Test
    fun aListingSnapshotRoundTripsThroughItsRow() {
        val original = snapshot(ListQuery(SortSpec(SortField.MTIME, SortOrder.DESC), KindsFilter.FILES, listOf("pdf", "pptx")))
        val back = RowCodec.toSnapshotOrNull(RowCodec.toEntity(original))
        assertEquals(original, back)
    }

    @Test
    fun theQueryColumnIsReadableAndStable() {
        assertEquals("name|asc||", RowCodec.encodeQuery(ListQuery()))
        assertEquals(
            "size|desc|dir|pdf,txt",
            RowCodec.encodeQuery(ListQuery(SortSpec(SortField.SIZE, SortOrder.DESC), KindsFilter.FOLDERS, listOf("pdf", "txt"))),
        )
    }

    @Test
    fun aRowFromANewerAppReadsAsNoSnapshot() {
        val row = RowCodec.toEntity(snapshot())
        assertNull(RowCodec.toSnapshotOrNull(row.copy(query = "volume|up|||")))
        assertNull(RowCodec.toSnapshotOrNull(row.copy(entriesJson = "{not json")))
        assertNotNull(RowCodec.toSnapshotOrNull(row))
    }

    @Test
    fun aHostRoundTripsThroughItsRow() {
        val host = SampleData.host(lastSeenAtMillis = 99L).copy(capabilities = SampleData.me().capabilities)
        val back = RowCodec.toDomainOrNull(RowCodec.toEntity(host, pairedAt = 5L))
        assertEquals(host, back)
    }

    @Test
    fun aCorruptHostRowIsSkippedNotFatal() {
        val row = RowCodec.toEntity(SampleData.host(), pairedAt = 1L)
        assertNull(RowCodec.toDomainOrNull(row.copy(pin = "not-a-pin")))
        assertNull(RowCodec.toDomainOrNull(row.copy(addresses = "[]")))
        assertNull(RowCodec.toDomainOrNull(row.copy(addresses = "garbage")))
        // unreadable capabilities only cost the capabilities
        assertNull(RowCodec.toDomainOrNull(row.copy(capabilities = "{}"))?.capabilities)
    }

    @Test
    fun aShareRoundTripsThroughItsJson() {
        val share = SampleData.share(pathDisplay = "D:\\Projects")
        assertEquals(share, RowCodec.decodeShareOrNull(RowCodec.encodeShare(share)))
        assertNull(RowCodec.decodeShareOrNull("nope"))
    }
}
