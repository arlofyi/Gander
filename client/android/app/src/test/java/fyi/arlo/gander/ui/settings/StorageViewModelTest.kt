package fyi.arlo.gander.ui.settings

import fyi.arlo.gander.core.data.cache.FakeDerivedCaches
import fyi.arlo.gander.core.data.fetch.CachedFile
import fyi.arlo.gander.core.data.fetch.FetchFiles
import fyi.arlo.gander.core.data.fetch.FetchJob
import fyi.arlo.gander.core.data.fetch.FileKey
import fyi.arlo.gander.core.data.fetch.InMemoryFetchStore
import fyi.arlo.gander.core.data.fetch.OfflineFiles
import fyi.arlo.gander.core.data.hosts.InMemoryHostRepository
import fyi.arlo.gander.core.data.settings.AppSettings
import fyi.arlo.gander.core.data.settings.InMemorySettings
import fyi.arlo.gander.testing.MainDispatcherRule
import fyi.arlo.gander.testing.SampleData
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import java.io.File

@OptIn(ExperimentalCoroutinesApi::class)
class StorageViewModelTest {
    @get:Rule
    val main = MainDispatcherRule()

    @get:Rule
    val temp = TemporaryFolder()

    private val mib = 1024L * 1024
    private val store = InMemoryFetchStore()
    private val files by lazy { FetchFiles(temp.root) }
    private val offline by lazy { OfflineFiles(store, files) { 1_000_000L } }
    private val studio = SampleData.host(name = "studio-pc")
    private val laptop = SampleData.host(name = "Laptop", id = "laptop-id")
    private val hosts = InMemoryHostRepository(listOf(studio, laptop))
    private val settings = InMemorySettings()
    private val derived = FakeDerivedCaches(size = 7 * mib)

    private suspend fun add(
        jobId: Long,
        host: String,
        name: String,
        size: Long,
        lastUsed: Long,
        pinned: Boolean = false,
    ): CachedFile {
        File(files.jobDir(jobId).also { it.mkdirs() }, name).writeBytes(ByteArray(8))
        val row = CachedFile(
            key = FileKey(host, "s", name),
            name = name,
            mime = null,
            localPath = files.finalRelativePath(jobId, name),
            size = size,
            mtime = "m",
            etag = null,
            pinned = pinned,
            lastUsed = lastUsed,
            fetchedAt = lastUsed,
        )
        store.complete(FetchJob(id = jobId, key = row.key, entry = SampleData.file(name, size)), row)
        return row
    }

    private fun viewModel() = StorageViewModel(offline, hosts, settings, derived)

    private fun TestScope.collect(vm: StorageViewModel) {
        backgroundScope.launch { vm.uiState.collect {} }
        runCurrent()
    }

    @Test
    fun usageIsGroupedPerComputerNewestUseFirstWithKnownComputersByNameFirst() = runTest {
        add(1, studio.id, "old.bin", 100 * mib, lastUsed = 1)
        add(2, studio.id, "new.bin", 200 * mib, lastUsed = 9, pinned = true)
        add(3, "laptop-id", "l.bin", 50 * mib, lastUsed = 5)
        add(4, "gone-id", "orphan.bin", 10 * mib, lastUsed = 5)
        val vm = viewModel()
        collect(vm)

        val state = vm.uiState.value
        assertTrue(state.loaded)
        assertEquals(360 * mib, state.usedBytes)
        assertEquals(200 * mib, state.pinnedBytes)
        assertEquals(160 * mib, state.unpinnedBytes)
        assertEquals(AppSettings.DEFAULT_CACHE_CAP_BYTES, state.capBytes)
        assertEquals(7 * mib, state.derivedBytes)
        assertEquals(listOf("Laptop", "studio-pc", null), state.hosts.map { it.hostName })
        val studioGroup = state.hosts[1]
        assertEquals(listOf("new.bin", "old.bin"), studioGroup.files.map { it.name })
        assertEquals(300 * mib, studioGroup.bytes)
        assertEquals(200 * mib, studioGroup.pinnedBytes)
        assertEquals("gone-id", state.hosts[2].hostId)
    }

    @Test
    fun theCapComesFromSettingsAndFlagsKeptFilesThatAloneExceedIt() = runTest {
        add(1, studio.id, "kept.bin", 600 * mib, lastUsed = 1, pinned = true)
        val vm = viewModel()
        collect(vm)
        assertFalse(vm.uiState.value.pinnedOverCap)

        settings.setCacheCapBytes(512 * mib)
        runCurrent()
        assertEquals(512 * mib, vm.uiState.value.capBytes)
        assertTrue(vm.uiState.value.pinnedOverCap)
    }

    @Test
    fun keepOfflineAndDeleteActOnTheFileAndItsBytes() = runTest {
        val a = add(1, studio.id, "a.bin", 10 * mib, lastUsed = 1)
        val b = add(2, studio.id, "b.bin", 10 * mib, lastUsed = 2)
        val vm = viewModel()
        collect(vm)

        vm.setKeepOffline(a, true)
        runCurrent()
        assertTrue(vm.uiState.value.hosts.single().files.first { it.name == "a.bin" }.pinned)

        vm.remove(b)
        runCurrent()
        assertEquals(listOf("a.bin"), vm.uiState.value.hosts.single().files.map { it.name })
        assertNull(offline.get(b.key))
        assertFalse(files.jobDir(2).exists())
    }

    @Test
    fun clearUnpinnedDeletesOnlyWhatIsNotKeptOffline() = runTest {
        add(1, studio.id, "kept.bin", 10 * mib, lastUsed = 1, pinned = true)
        add(2, studio.id, "temp.bin", 10 * mib, lastUsed = 2)
        add(3, "laptop-id", "l.bin", 10 * mib, lastUsed = 3)
        val vm = viewModel()
        collect(vm)

        vm.clearUnpinned(studio.id)
        runCurrent()
        assertEquals(listOf("kept.bin"), vm.uiState.value.hosts.first { it.hostId == studio.id }.files.map { it.name })
        assertEquals("the other computer is untouched", 1, vm.uiState.value.hosts.first { it.hostId == "laptop-id" }.files.size)

        vm.clearUnpinned(null)
        runCurrent()
        assertEquals(listOf(studio.id), vm.uiState.value.hosts.map { it.hostId })
        assertEquals(10 * mib, vm.uiState.value.usedBytes)
    }

    @Test
    fun clearingThumbnailsAndPreviewsEmptiesThemAndRefreshesTheSize() = runTest {
        val vm = viewModel()
        collect(vm)
        assertEquals(7 * mib, vm.uiState.value.derivedBytes)

        vm.clearDerivedCaches()
        runCurrent()

        assertEquals(1, derived.clears)
        assertEquals(0L, vm.uiState.value.derivedBytes)
    }

    @Test
    fun anEmptyCacheHasNoGroups() = runTest {
        val vm = viewModel()
        collect(vm)
        assertTrue(vm.uiState.value.hosts.isEmpty())
        assertEquals(0L, vm.uiState.value.usedBytes)
    }
}
