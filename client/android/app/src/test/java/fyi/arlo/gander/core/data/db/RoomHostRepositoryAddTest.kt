package fyi.arlo.gander.core.data.db

import fyi.arlo.gander.core.data.hosts.InMemoryTokenStore
import fyi.arlo.gander.core.net.BearerToken
import fyi.arlo.gander.testing.SampleData
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.fail
import org.junit.Test

/** What `RoomHostRepository.add` guarantees when pairing succeeds but the row write does not. */
class RoomHostRepositoryAddTest {

    /** A [HostDao] in memory whose [upsert] can be made to fail. */
    private class FakeHostDao : HostDao {
        val rows = MutableStateFlow<List<HostEntity>>(emptyList())
        var failUpsert = false

        override fun observeAll(): Flow<List<HostEntity>> = rows

        override suspend fun get(id: String): HostEntity? = rows.value.firstOrNull { it.id == id }

        override suspend fun upsert(host: HostEntity) {
            if (failUpsert) throw IllegalStateException("database is locked")
            rows.value = rows.value.filterNot { it.id == host.id } + host
        }

        override suspend fun rename(id: String, name: String) = Unit

        override suspend fun delete(id: String) {
            rows.value = rows.value.filterNot { it.id == id }
        }

        override suspend fun updateLastSeen(id: String, atMillis: Long) = Unit

        override suspend fun updateFromMe(
            id: String,
            addresses: String,
            os: String,
            pairedAs: String?,
            capabilities: String?,
            atMillis: Long,
        ) = Unit
    }

    private val dao = FakeHostDao()
    private val tokens = InMemoryTokenStore()
    private val repo = RoomHostRepository(dao, tokens, clock = { 1_000L })
    private val host = SampleData.host()
    private val token = BearerToken("tok_new_placeholder")

    @Test
    fun addStoresTheRowAndTheToken() = runBlocking {
        repo.add(host, token)
        assertEquals(listOf(host.id), repo.hosts.first().map { it.id })
        assertEquals(token, repo.token(host.id))
        assertEquals(1_000L, dao.get(host.id)!!.pairedAt)
    }

    @Test
    fun aFailedRowWriteLeavesNoOrphanedToken() = runBlocking {
        dao.failUpsert = true
        try {
            repo.add(host, token)
            fail("the database failure must reach the caller")
        } catch (e: IllegalStateException) {
            assertEquals("database is locked", e.message)
        }
        assertNull(repo.token(host.id))
        assertEquals(emptyList<String>(), repo.hosts.first().map { it.id })
    }

    @Test
    fun aFailedRePairingKeepsTheOldWorkingToken() = runBlocking {
        val old = BearerToken("tok_old_placeholder")
        repo.add(host, old)
        dao.failUpsert = true
        try {
            repo.add(host, token)
            fail("the database failure must reach the caller")
        } catch (_: IllegalStateException) {
            // expected
        }
        assertEquals(old, repo.token(host.id))
    }
}
