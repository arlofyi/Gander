package fyi.arlo.gander.ui.preview

import fyi.arlo.gander.core.data.UiErrorKind
import fyi.arlo.gander.core.data.actions.ActionReason
import fyi.arlo.gander.core.data.actions.FileAction
import fyi.arlo.gander.core.data.actions.InMemoryActionRefusals
import fyi.arlo.gander.core.data.browse.EntryTarget
import fyi.arlo.gander.core.data.preview.PreviewFailure
import fyi.arlo.gander.core.data.preview.TextChunk
import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.protocol.PreviewKind
import fyi.arlo.gander.testing.FakePreviewSource
import fyi.arlo.gander.testing.MainDispatcherRule
import fyi.arlo.gander.testing.SampleData
import fyi.arlo.gander.testing.TextCall
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.runTest
import okhttp3.Headers
import okio.ByteString.Companion.encodeUtf8
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

@OptIn(ExperimentalCoroutinesApi::class)
class TextViewModelTest {
    @get:Rule
    val main = MainDispatcherRule()

    private val host = SampleData.host()
    private val source = FakePreviewSource()
    private val refusals = InMemoryActionRefusals()
    private val entry = SampleData.file("server.log", size = 300, preview = PreviewKind.TEXT)
    private val target = EntryTarget(host.id, "shr_projects", "Logs/server.log", entry)

    private fun viewModel(sliceBytes: Int = 100, maxBytes: Long = TextViewModel.DEFAULT_MAX_BYTES) =
        TextViewModel(target, source, refusals, sliceBytes, maxBytes)

    /**
     * Serves [full] (ASCII, so characters are bytes) in slices, the way the host does: headers for
     * encoding and total, and `X-Gander-Text-Next` when [withNext] is true.
     */
    private fun serve(full: String, encoding: String = "utf-8", withNext: Boolean = true) {
        source.textHandler = { call ->
            val bytes = full.encodeUtf8()
            val from = call.offset.toInt().coerceAtMost(bytes.size)
            val to = (from + call.limit).coerceAtMost(bytes.size)
            val body = bytes.substring(from, to)
            val headers = Headers.Builder()
                .add("X-Gander-Text-Encoding", encoding)
                .add("X-Gander-Text-Total", bytes.size.toString())
                .apply { if (withNext) add("X-Gander-Text-Next", to.toString()) }
                .build()
            TextChunk.fromResponse(call.offset, body, headers)
        }
    }

    private fun TextUiState.text(): String = blocks.joinToString("\n") { it.text }

    private suspend fun refusalsOf() = refusals.observe(target).first()

    // Reading ----------------------------------------------------------------------------------------

    @Test
    fun theFirstSliceIsShownWithItsEncodingAndSize() {
        val full = "line one\nline two\nline three\n"
        serve(full)
        val vm = viewModel(sliceBytes = 1000)
        val state = vm.uiState.value
        assertEquals(TextPhase.Ready, state.phase)
        assertEquals("line one\nline two\nline three\n", state.text())
        assertEquals(TextEncoding("utf-8", "UTF-8", converted = false), state.encoding)
        assertEquals(full.length.toLong(), state.loadedBytes)
        assertEquals(full.length.toLong(), state.totalBytes)
        assertFalse(state.hasMore)
        assertEquals(listOf(TextCall(0, 1000)), source.textCalls)
    }

    @Test
    fun loadMoreContinuesWhereTheHostSaidAndStopsAtTheEnd() {
        val full = "a".repeat(100) + "\n" + "b".repeat(100) + "\n" + "c".repeat(97) + "\n" // 300 bytes
        serve(full)
        val vm = viewModel(sliceBytes = 100)
        assertTrue(vm.uiState.value.hasMore)
        assertEquals(100, vm.uiState.value.loadedBytes)

        vm.loadMore()
        assertEquals(200, vm.uiState.value.loadedBytes)
        vm.loadMore()
        val state = vm.uiState.value
        assertEquals(full.length.toLong(), state.loadedBytes)
        assertFalse("the end", state.hasMore)
        assertEquals(listOf(0L, 100L, 200L), source.textCalls.map { it.offset })

        vm.loadMore()
        assertEquals("nothing more is asked after the end", 3, source.textCalls.size)
        assertEquals(full.trimEnd('\n').length + 1, state.text().length)
    }

    @Test
    fun aLineCutByASliceBoundaryIsOneLine() {
        serve("first line is long enough to be cut\nsecond\n")
        val vm = viewModel(sliceBytes = 20)
        vm.loadMore()
        vm.loadMore()
        assertEquals(listOf("first line is long enough to be cut", "second", ""), vm.uiState.value.text().split("\n"))
    }

    @Test
    fun aHostWithoutTheNextHeaderIsContinuedByTheBytesReceived() {
        val full = "x".repeat(250)
        serve(full, withNext = false)
        val vm = viewModel(sliceBytes = 100)
        vm.loadMore()
        vm.loadMore()
        assertEquals(listOf(0L, 100L, 200L), source.textCalls.map { it.offset })
        assertFalse(vm.uiState.value.hasMore)
        assertEquals(250, vm.uiState.value.text().length)
    }

    @Test
    fun theHostsNextOffsetBeatsTheBodyLength() {
        // a slice whose invalid bytes were replaced: it was 3 bytes on the file, 3 bytes of U+FFFD in the body
        var calls = 0
        source.textHandler = { call ->
            calls++
            if (call.offset == 0L) {
                TextChunk("ab�", 0, nextOffset = 5, encoding = "windows-1252", totalBytes = 8, hasMore = true)
            } else {
                TextChunk("xyz", call.offset, nextOffset = 8, encoding = "windows-1252", totalBytes = 8, hasMore = false)
            }
        }
        val vm = viewModel()
        vm.loadMore()
        assertEquals(listOf(0L, 5L), source.textCalls.map { it.offset })
        assertEquals("ab�xyz", vm.uiState.value.text())
        assertEquals(2, calls)
    }

    // Encoding ---------------------------------------------------------------------------------------

    @Test
    fun theEncodingTheHostDetectedIsNamedAndMarkedAsConverted() {
        assertEquals(TextEncoding("utf-8", "UTF-8", false), TextEncoding.parse("utf-8"))
        assertEquals(TextEncoding("UTF-8", "UTF-8", false), TextEncoding.parse("UTF-8"))
        assertEquals(TextEncoding("utf-16le", "UTF-16 LE", true), TextEncoding.parse("utf-16le"))
        assertEquals(TextEncoding("utf-16be", "UTF-16 BE", true), TextEncoding.parse("utf-16be"))
        assertEquals(TextEncoding("windows-1252", "Windows-1252", true), TextEncoding.parse("windows-1252"))
        assertEquals("a newer host's label is shown as sent", "SHIFT_JIS", TextEncoding.parse("shift_jis")!!.name)
        assertNull(TextEncoding.parse(null))
        assertNull(TextEncoding.parse("  "))
    }

    @Test
    fun aUtf16FileShowsItsEncodingFromTheFirstSlice() {
        serve("hello\n", encoding = "utf-16le")
        val state = viewModel(sliceBytes = 1000).uiState.value
        assertEquals("UTF-16 LE", state.encoding!!.name)
        assertTrue(state.encoding.converted)
    }

    @Test
    fun aHostThatSendsNoEncodingShowsNone() {
        source.textHandler = { call -> TextChunk("abc", call.offset, call.offset + 3, null, null, hasMore = false) }
        val state = viewModel().uiState.value
        assertNull(state.encoding)
        assertNull(state.totalBytes)
        assertEquals("abc", state.text())
    }

    // Failures ---------------------------------------------------------------------------------------

    @Test
    fun aBinaryFileIsExplainedAndTheSheetLearnsWhy() = runTest {
        source.textHandler = { throw GanderError.Host.UnsupportedType(415, "Binary file.") }
        val vm = viewModel()
        assertEquals(TextPhase.Failed(PreviewFailure.Unsupported), vm.uiState.value.phase)
        assertEquals(ActionReason.NoRenderer("log"), refusalsOf()[FileAction.PREVIEW])
    }

    @Test
    fun anOfflineStartCanBeRetried() = runTest {
        var online = false
        serve("hello\n")
        val served = source.textHandler
        source.textHandler = { call ->
            if (!online) throw GanderError.Unreachable(listOf("100.64.0.1"), null)
            served(call)
        }
        val vm = viewModel(sliceBytes = 1000)
        val failed = (vm.uiState.value.phase as TextPhase.Failed).failure as PreviewFailure.Host
        assertEquals(UiErrorKind.OFFLINE, failed.error.kind)
        assertTrue("an outage is not a reason to switch Preview off", refusalsOf().isEmpty())

        online = true
        vm.retry()
        assertEquals(TextPhase.Ready, vm.uiState.value.phase)
        assertEquals("hello\n", vm.uiState.value.text())
    }

    @Test
    fun aFailedLoadMoreKeepsWhatWasReadAndRetriesFromTheSameOffset() {
        val full = "a".repeat(100) + "b".repeat(100)
        serve(full)
        val served = source.textHandler
        var failNext = false
        source.textHandler = { call ->
            if (failNext && call.offset > 0) throw GanderError.Timeout(java.io.IOException("slow"))
            served(call)
        }
        val vm = viewModel(sliceBytes = 100)
        failNext = true
        vm.loadMore()
        var state = vm.uiState.value
        assertEquals("what was read stays", "a".repeat(100), state.text())
        assertNotNullFailure(state)
        assertFalse(state.loadingMore)

        failNext = false
        vm.retry()
        state = vm.uiState.value
        assertEquals(full, state.text())
        assertNull(state.loadMoreFailure)
        assertEquals(listOf(0L, 100L, 100L), source.textCalls.map { it.offset })
    }

    private fun assertNotNullFailure(state: TextUiState) {
        val failure = state.loadMoreFailure as PreviewFailure.Host
        assertEquals(UiErrorKind.OFFLINE, failure.error.kind)
    }

    @Test
    fun aSecondLoadMoreWhileOneIsRunningIsIgnored() {
        val full = "z".repeat(300)
        serve(full)
        val served = source.textHandler
        val gate = CompletableDeferred<Unit>()
        var hold = false
        source.textHandler = { call ->
            if (hold) gate.await()
            served(call)
        }
        val vm = viewModel(sliceBytes = 100)
        hold = true
        vm.loadMore()
        assertTrue(vm.uiState.value.loadingMore)
        vm.loadMore()
        vm.loadMore()
        assertEquals("one request in flight", 2, source.textCalls.size)
        gate.complete(Unit)
        assertEquals(200, vm.uiState.value.loadedBytes)
    }

    // Memory cap ---------------------------------------------------------------------------------------

    @Test
    fun theTextStopsGrowingAtTheCapAndSaysSo() {
        serve("w".repeat(1000))
        val vm = viewModel(sliceBytes = 100, maxBytes = 250)
        vm.loadMore()
        vm.loadMore()
        val state = vm.uiState.value
        assertEquals(300, state.loadedBytes)
        assertTrue(state.hasMore)
        assertTrue(state.capped)
        vm.loadMore()
        assertEquals("no slice past the cap", 3, source.textCalls.size)
    }

    @Test
    fun anEmptyFileIsReadyWithNothingToShow() {
        serve("")
        val state = viewModel().uiState.value
        assertEquals(TextPhase.Ready, state.phase)
        assertTrue(state.blocks.isEmpty())
        assertFalse(state.hasMore)
    }

    @Test
    fun aTextThatLoadsClearsAnEarlierRefusal() = runTest {
        refusals.report(target, FileAction.PREVIEW, GanderError.Host.UnsupportedType(415, ""))
        serve("ok\n")
        viewModel(sliceBytes = 100)
        assertTrue(refusalsOf().isEmpty())
    }
}

class TextAssemblerTest {
    private fun TextAssembler.text() = snapshot().joinToString("\n") { it.text }

    @Test
    fun slicesJoinIntoLinesAndCrLfLosesItsCarriageReturn() {
        val a = TextAssembler()
        a.append("one\r\ntwo\r\nthr")
        a.append("ee\r\nfour")
        assertEquals("one\ntwo\nthree\nfour", a.text())
    }

    @Test
    fun linesAreGroupedIntoBlocksOfTheConfiguredSize() {
        val a = TextAssembler(linesPerBlock = 3)
        a.append((1..7).joinToString("\n"))
        val blocks = a.snapshot()
        assertEquals(listOf("1\n2\n3", "4\n5\n6", "7"), blocks.map { it.text })
        assertEquals(listOf(0, 1, 2), blocks.map { it.index })
    }

    @Test
    fun aLaterSliceExtendsTheLastBlockAndLeavesTheEarlierOnesAlone() {
        val a = TextAssembler(linesPerBlock = 2)
        a.append("a\nb\nc")
        val before = a.snapshot()
        a.append("d\ne\nf\n")
        val after = a.snapshot()
        assertSame("the first block is the same object", before[0], after[0])
        assertEquals(listOf("a\nb", "cd\ne", "f\n"), after.map { it.text })
    }

    @Test
    fun anEmptySliceChangesNothing() {
        val a = TextAssembler()
        a.append("x")
        a.append("")
        assertEquals("x", a.text())
    }

    @Test
    fun aSliceEndingInANewlineStartsTheNextSliceOnANewLine() {
        val a = TextAssembler()
        a.append("a\n")
        a.append("b\n")
        assertEquals("a\nb\n", a.text())
    }
}
