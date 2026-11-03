package fyi.arlo.gander.core.data.browse

import fyi.arlo.gander.core.data.UiErrorKind
import fyi.arlo.gander.core.data.toUiError
import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.protocol.ServerEvent
import fyi.arlo.gander.testing.FakeHostConnector
import fyi.arlo.gander.testing.SampleData
import fyi.arlo.gander.testing.SampleData.dir
import fyi.arlo.gander.testing.SampleData.file
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.toList
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

/** Cursor paging, ETag revalidation, snapshots and the offline fallback rules, over a scripted host. */
@OptIn(ExperimentalCoroutinesApi::class)
class DefaultBrowseRepositoryTest {
    private val host = SampleData.host()
    private val connector = FakeHostConnector()
    private val api get() = connector.api
    private val snapshots = InMemorySnapshotStore()
    private var now = 1_000L
    private val repository = DefaultBrowseRepository(
        connector = connector,
        listings = snapshots,
        sharesStore = snapshots,
        clock = { now },
        pageSize = 200,
        reconnectBaseMs = 1_000,
        reconnectMaxMs = 4_000,
    )
    private val share = "shr_projects"
    private val query = ListQuery()

    private fun bigFolder(count: Int = 450) = (1..count).map { file("file%03d.txt".format(it), size = it.toLong()) }

    private fun offline() = GanderError.Unreachable(listOf("100.101.102.103"), null)

    private fun hostError(status: Int, code: String) =
        GanderError.fromResponse(status, """{"error":{"code":"$code","message":"m","retry_after_ms":0}}""")

    // Paging ------------------------------------------------------------------------------------

    @Test
    fun cursorPagingWalksAFolderInPagesOf200() = runTest {
        api.folders[""] = bigFolder(450)
        val first = repository.firstPage(host, share, "", query)
        assertEquals(200, first.entries.size)
        assertEquals("o200", first.nextCursor)
        assertNull(first.cachedAtMillis)

        val second = repository.nextPage(host, share, "", query, first.nextCursor!!)
        assertEquals(200, second.entries.size)
        assertEquals("o400", second.nextCursor)

        val third = repository.nextPage(host, share, "", query, second.nextCursor!!)
        assertEquals(50, third.entries.size)
        assertNull(third.nextCursor)

        val all = first.entries + second.entries + third.entries
        assertEquals(450, all.map { it.name }.toSet().size)
        assertEquals(listOf(null, "o200", "o400"), api.listCalls.map { it.cursor })
        assertEquals(listOf(200, 200, 200), api.listCalls.map { it.limit })
    }

    @Test
    fun requestCarriesSortOrderAndKinds() = runTest {
        api.folders["Clients"] = listOf(dir("Acme"), file("a.txt"))
        val q = ListQuery(SortSpec(SortField.MTIME, SortOrder.DESC), KindsFilter.FILES)
        repository.firstPage(host, share, "Clients", q)
        val call = api.listCalls.single()
        assertEquals("Clients", call.path)
        assertEquals("mtime", call.sort)
        assertEquals("desc", call.order)
        assertEquals("file", call.kinds)
    }

    // ETag and snapshots ------------------------------------------------------------------------

    @Test
    fun firstPageStoresASnapshotAndRevalidatesItWithTheEtag() = runTest {
        api.folders[""] = bigFolder(5)
        val first = repository.firstPage(host, share, "", query)
        val stored = snapshots.get(host.id, share, "")!!
        assertEquals(1_000L, stored.fetchedAtMillis)
        assertEquals(5, stored.entries.size)
        assertNull(api.listCalls[0].ifNoneMatch)

        now = 5_000
        val again = repository.firstPage(host, share, "", query)
        assertEquals(stored.etag, api.listCalls[1].ifNoneMatch)
        // 304: the stored entries come back as a live page, and the snapshot is re-stamped.
        assertEquals(first.entries, again.entries)
        assertNull(again.cachedAtMillis)
        assertEquals(5_000L, snapshots.get(host.id, share, "")!!.fetchedAtMillis)
    }

    @Test
    fun aChangedFolderReplacesTheSnapshot() = runTest {
        api.folders[""] = listOf(file("a.txt"))
        repository.firstPage(host, share, "", query)
        api.folders[""] = listOf(file("a.txt"), file("b.txt"))
        val page = repository.firstPage(host, share, "", query)
        assertEquals(2, page.entries.size)
        assertEquals(2, snapshots.get(host.id, share, "")!!.entries.size)
    }

    @Test
    fun theEtagIsNotReplayedForADifferentSortOrFilter() = runTest {
        api.folders[""] = listOf(file("a.txt"), file("b.txt"))
        repository.firstPage(host, share, "", query)
        repository.firstPage(host, share, "", ListQuery(SortSpec(SortField.SIZE, SortOrder.DESC)))
        assertNull(api.listCalls[1].ifNoneMatch)
    }

    @Test
    fun laterPagesAreAppendedToTheSnapshotSoOfflineIsNotCutAtPageOne() = runTest {
        api.folders[""] = bigFolder(450)
        var cursor: String? = repository.firstPage(host, share, "", query).nextCursor
        while (cursor != null) cursor = repository.nextPage(host, share, "", query, cursor).nextCursor
        val stored = snapshots.get(host.id, share, "")!!
        assertEquals(450, stored.entries.size)
        assertNull(stored.nextCursor)

        // A revalidated 304 hands the whole stored list back, with the cursor where it ended.
        val again = repository.firstPage(host, share, "", query)
        assertEquals(450, again.entries.size)
        assertNull(again.nextCursor)
    }

    @Test
    fun aPageFromADifferentListingIsNotAppended() = runTest {
        api.folders[""] = bigFolder(450)
        val first = repository.firstPage(host, share, "", query)
        // The folder changes between page 1 and page 2: a different ETag, so keep the snapshot as it was.
        api.folders[""] = bigFolder(451)
        repository.nextPage(host, share, "", query, first.nextCursor!!)
        val stored = snapshots.get(host.id, share, "")!!
        assertEquals(200, stored.entries.size)
        assertEquals("o200", stored.nextCursor)
    }

    // Offline -----------------------------------------------------------------------------------

    @Test
    fun anUnreachableHostFallsBackToTheSnapshotWithItsAge() = runTest {
        api.folders[""] = listOf(file("a.txt"), file("b.txt"))
        repository.firstPage(host, share, "", query)
        api.failure = offline()
        now = 9_000

        val page = repository.firstPage(host, share, "", query)
        assertEquals(listOf("a.txt", "b.txt"), page.entries.map { it.name })
        assertEquals(1_000L, page.cachedAtMillis) // age of the data, not of the failure
        assertNull(page.nextCursor)
        assertFalse(page.cacheIncomplete)
    }

    @Test
    fun aCachedPageSaysWhenTheFolderWasLongerThanWhatWasStored() = runTest {
        api.folders[""] = bigFolder(450)
        repository.firstPage(host, share, "", query)
        api.failure = offline()
        val page = repository.firstPage(host, share, "", query)
        assertEquals(200, page.entries.size)
        assertTrue(page.cacheIncomplete)
        assertNull(page.nextCursor) // cannot page without the host
    }

    @Test
    fun offlineWithoutASnapshotIsAnError() = runTest {
        api.failure = offline()
        try {
            repository.firstPage(host, share, "", query)
            fail("expected Unreachable")
        } catch (e: GanderError.Unreachable) {
            assertEquals(UiErrorKind.OFFLINE, e.toUiError().kind)
        }
    }

    @Test
    fun offlineSnapshotIsReSortedLocallyForANewSort() = runTest {
        api.folders[""] = listOf(file("small.txt", size = 1), file("big.txt", size = 99), dir("d"))
        repository.firstPage(host, share, "", query)
        api.failure = offline()
        val page = repository.firstPage(host, share, "", ListQuery(SortSpec(SortField.SIZE, SortOrder.DESC)))
        assertEquals(listOf("d", "big.txt", "small.txt"), page.entries.map { it.name })
        assertNotNull(page.cachedAtMillis)
    }

    @Test
    fun aRefusalIsNeverPaperedOverWithStaleData() = runTest {
        api.folders[""] = listOf(file("a.txt"))
        repository.firstPage(host, share, "", query)
        val refusals = listOf(
            hostError(401, "token_revoked"),
            hostError(403, "permission_denied"),
            hostError(404, "share_not_found"),
            GanderError.PinMismatch(SampleData.pin(), null, null),
        )
        for (refusal in refusals) {
            api.failure = refusal
            try {
                repository.firstPage(host, share, "", query)
                fail("expected $refusal to propagate")
            } catch (e: GanderError) {
                assertEquals(refusal, e)
            }
        }
    }

    @Test
    fun cachedFirstPageServesTheSnapshotWithoutTheNetwork() = runTest {
        api.folders[""] = listOf(file("a.txt"))
        assertNull(repository.cachedFirstPage(host, share, "", query))
        repository.firstPage(host, share, "", query)
        val callsBefore = api.listCalls.size
        val cached = repository.cachedFirstPage(host, share, "", query)!!
        assertEquals(listOf("a.txt"), cached.entries.map { it.name })
        assertEquals(callsBefore, api.listCalls.size)
    }

    // Shares ------------------------------------------------------------------------------------

    @Test
    fun sharesAreStoredAndServedFromTheSnapshotWhenOffline() = runTest {
        api.shares = listOf(SampleData.share(name = "Projects"), SampleData.share(id = "shr_b", name = "Photos"))
        assertNull(repository.cachedShares(host))
        val live = repository.shares(host)
        assertNull(live.cachedAtMillis)
        assertEquals(listOf("Projects", "Photos"), live.value.map { it.name })

        api.failure = offline()
        now = 7_000
        val cached = repository.shares(host)
        assertEquals(1_000L, cached.cachedAtMillis)
        assertEquals(listOf("Projects", "Photos"), cached.value.map { it.name })
    }

    @Test
    fun sharesOfflineWithoutASnapshotOrWithARefusalThrow() = runTest {
        api.failure = offline()
        try {
            repository.shares(host)
            fail("expected Unreachable")
        } catch (_: GanderError.Unreachable) {
        }
        api.failure = null
        repository.shares(host)
        api.failure = hostError(401, "token_revoked")
        try {
            repository.shares(host)
            fail("expected TokenRevoked")
        } catch (_: GanderError.Host.TokenRevoked) {
        }
    }

    // Search and recent -----------------------------------------------------------------------------

    @Test
    fun searchPassesTheScopeTextAndCursor() = runTest {
        api.searchHandler = { call ->
            fyi.arlo.gander.core.net.protocol.SearchResponse(
                listOf(file("deck.pptx", path = "Clients/${call.q}.pptx")),
                partial = true,
                nextCursor = "next",
            )
        }
        val page = repository.search(host, share, "Clients", "deck", cursor = "c1")
        assertTrue(page.partial)
        assertEquals("next", page.nextCursor)
        val call = api.searchCalls.single()
        assertEquals("deck", call.q)
        assertEquals("Clients", call.path)
        assertEquals("c1", call.cursor)
        assertNull(call.scope) // names unless asked, so an older host sees the request it knows
    }

    @Test
    fun insideFilesAsksForTheContentScopeAndReportsWhatAnswered() = runTest {
        api.searchHandler = {
            fyi.arlo.gander.core.net.protocol.SearchResponse(
                listOf(file("notes.md", path = "notes.md")),
                partial = false,
                nextCursor = null,
                via = fyi.arlo.gander.core.net.protocol.SearchVia.INDEX,
            )
        }
        val page = repository.search(host, share, "", "budget", inside = true)
        assertEquals("content", api.searchCalls.single().scope)
        assertEquals(fyi.arlo.gander.core.net.protocol.SearchVia.INDEX, page.via)
    }

    // Events ------------------------------------------------------------------------------------

    @Test
    fun eventsReconnectWithBackoffAfterTheSocketCloses() = runTest {
        connector.eventsFailure = offline()
        val received = mutableListOf<ServerEvent>()
        backgroundScope.launch { repository.events(host).collect { received += it } }
        runCurrent()
        assertEquals(1, connector.eventsOpened)
        advanceTimeBy(1_001)
        runCurrent()
        assertEquals(2, connector.eventsOpened)
        advanceTimeBy(2_001) // backoff doubled
        runCurrent()
        assertEquals(3, connector.eventsOpened)

        connector.eventsFailure = null
        advanceTimeBy(4_001)
        runCurrent()
        connector.events.emit(ServerEvent.SharesUpdated)
        runCurrent()
        assertEquals(listOf<ServerEvent>(ServerEvent.SharesUpdated), received)
    }

    @Test
    fun eventsGiveUpWhenAccessWasRevokedOrTheCertificateIsWrong() = runTest {
        for (failure in listOf(hostError(401, "token_revoked"), GanderError.PinMismatch(SampleData.pin(), null, null))) {
            connector.eventsFailure = failure
            try {
                repository.events(host).toList()
                fail("expected $failure")
            } catch (e: GanderError) {
                assertEquals(failure, e)
            }
        }
    }
}
