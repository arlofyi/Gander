package fyi.arlo.gander.ui.preview

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import fyi.arlo.gander.core.data.actions.ActionRefusals
import fyi.arlo.gander.core.data.actions.FileAction
import fyi.arlo.gander.core.data.browse.EntryTarget
import fyi.arlo.gander.core.data.preview.PreviewFailure
import fyi.arlo.gander.core.data.preview.PreviewSource
import fyi.arlo.gander.core.data.preview.TextChunk
import fyi.arlo.gander.core.data.preview.shouldReportRefusal
import fyi.arlo.gander.core.data.preview.toPreviewFailure
import fyi.arlo.gander.core.net.GanderError
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import java.util.Locale

/** The encoding the host detected, as shown in the viewer ("UTF-16 LE"). */
data class TextEncoding(
    /** `X-Gander-Text-Encoding` as sent. */
    val label: String,
    /** What to show the user. */
    val name: String,
    /** The file is not UTF-8, so what is shown was converted by the computer. */
    val converted: Boolean,
) {
    companion object {
        /** Null when the host sent no encoding. Unknown labels (a newer host) are shown as sent. */
        fun parse(label: String?): TextEncoding? {
            val raw = label?.trim()?.takeIf { it.isNotEmpty() } ?: return null
            val lower = raw.lowercase(Locale.ROOT)
            val name = when (lower) {
                "utf-8", "utf8" -> "UTF-8"
                "utf-16le" -> "UTF-16 LE"
                "utf-16be" -> "UTF-16 BE"
                "windows-1252" -> "Windows-1252"
                else -> raw.uppercase(Locale.ROOT)
            }
            return TextEncoding(raw, name, converted = lower != "utf-8" && lower != "utf8")
        }
    }
}

/** A run of whole lines, one list item. [text] has no trailing newline. */
data class TextBlock(val index: Int, val text: String)

/**
 * Joins slices into lines. A slice ends wherever the byte count says, usually in the middle of a
 * line, so the start of the next slice continues the last line instead of starting a new one.
 * Lines are grouped into blocks of [linesPerBlock] (one lazy list item each) so a long file stays
 * cheap to lay out; a trailing `\r` of a CRLF line is dropped.
 */
class TextAssembler(private val linesPerBlock: Int = DEFAULT_LINES_PER_BLOCK) {
    init {
        require(linesPerBlock > 0) { "a block needs at least one line" }
    }

    private val blocks = ArrayList<ArrayList<String>>()
    private var cache = ArrayList<TextBlock?>()

    fun append(chunk: String) {
        if (chunk.isEmpty()) return
        val pieces = chunk.split('\n').map { it.removeSuffix("\r") }
        var first = true
        for (piece in pieces) {
            if (first) {
                first = false
                val lastBlock = blocks.lastOrNull()
                if (lastBlock != null && lastBlock.isNotEmpty()) {
                    lastBlock[lastBlock.lastIndex] = lastBlock.last() + piece
                    invalidate(blocks.lastIndex)
                    continue
                }
            }
            addLine(piece)
        }
    }

    private fun addLine(line: String) {
        var block = blocks.lastOrNull()
        if (block == null || block.size >= linesPerBlock) {
            block = ArrayList()
            blocks += block
            cache += null
        }
        block += line
        invalidate(blocks.lastIndex)
    }

    private fun invalidate(index: Int) {
        while (cache.size <= index) cache += null
        cache[index] = null
    }

    fun snapshot(): List<TextBlock> = blocks.indices.map { i ->
        cache[i] ?: TextBlock(i, blocks[i].joinToString("\n")).also { cache[i] = it }
    }

    companion object {
        const val DEFAULT_LINES_PER_BLOCK = 100
    }
}

sealed interface TextPhase {
    data object Loading : TextPhase

    data object Ready : TextPhase

    data class Failed(val failure: PreviewFailure) : TextPhase
}

data class TextUiState(
    val phase: TextPhase = TextPhase.Loading,
    val blocks: List<TextBlock> = emptyList(),
    val encoding: TextEncoding? = null,
    /** Offset the next slice starts at; also how far the text has been read. */
    val loadedBytes: Long = 0,
    /** Size of the decoded text (`X-Gander-Text-Total`), when the host said. */
    val totalBytes: Long? = null,
    val hasMore: Boolean = false,
    val loadingMore: Boolean = false,
    val loadMoreFailure: PreviewFailure? = null,
    /** The phone stopped at [TextViewModel]'s memory cap; the rest needs Fetch. */
    val capped: Boolean = false,
)

/**
 * Logic of the text viewer (client spec, "Preview: text"): reads `preview/text` in slices, shows
 * the encoding the host detected, and loads the next slice on demand ("load more", or when the
 * list scrolls to its end). A slice continues where the host's `X-Gander-Text-Next` says (or,
 * without that header, where the bytes received end), and the text stops growing at [maxBytes]
 * so a gigabyte log cannot fill the phone's memory.
 */
class TextViewModel(
    private val target: EntryTarget,
    private val source: PreviewSource,
    private val refusalLog: ActionRefusals,
    private val sliceBytes: Int = DEFAULT_SLICE_BYTES,
    private val maxBytes: Long = DEFAULT_MAX_BYTES,
) : ViewModel() {

    private val assembler = TextAssembler()
    private val _state = MutableStateFlow(TextUiState())
    val uiState: StateFlow<TextUiState> = _state.asStateFlow()

    private var job: Job? = null
    private var refusalCleared = false

    init {
        load(first = true)
    }

    /** Reads the next slice. Does nothing while one is loading, after the end, or at the memory cap. */
    fun loadMore() {
        val state = _state.value
        if (state.phase != TextPhase.Ready || !state.hasMore || state.capped || job?.isActive == true) return
        load(first = false)
    }

    /** Retry after a failed first read or a failed "load more". */
    fun retry() {
        if (job?.isActive == true) return
        when (_state.value.phase) {
            is TextPhase.Failed -> load(first = true)
            TextPhase.Ready -> if (_state.value.loadMoreFailure != null) load(first = false)
            TextPhase.Loading -> Unit
        }
    }

    private fun load(first: Boolean) {
        val offset = if (first) 0L else _state.value.loadedBytes
        _state.update {
            if (first) it.copy(phase = TextPhase.Loading) else it.copy(loadingMore = true, loadMoreFailure = null)
        }
        job = viewModelScope.launch {
            try {
                val chunk = source.text(target.hostId, target.shareId, target.path, offset, sliceBytes)
                apply(chunk, first)
            } catch (e: CancellationException) {
                throw e
            } catch (e: GanderError) {
                fail(e, first)
            }
        }
    }

    private fun apply(chunk: TextChunk, first: Boolean) {
        assembler.append(chunk.text)
        val capped = chunk.hasMore && chunk.nextOffset >= maxBytes
        _state.update { state ->
            state.copy(
                phase = TextPhase.Ready,
                blocks = assembler.snapshot(),
                // The first slice names the encoding; later ones repeat it.
                encoding = if (first) TextEncoding.parse(chunk.encoding) else state.encoding,
                loadedBytes = chunk.nextOffset,
                totalBytes = chunk.totalBytes ?: state.totalBytes,
                hasMore = chunk.hasMore,
                loadingMore = false,
                loadMoreFailure = null,
                capped = capped,
            )
        }
        if (!refusalCleared) {
            refusalCleared = true
            refusalLog.clear(target, FileAction.PREVIEW)
        }
    }

    private fun fail(error: GanderError, first: Boolean) {
        val failure = error.toPreviewFailure()
        _state.update {
            if (first) it.copy(phase = TextPhase.Failed(failure)) else it.copy(loadingMore = false, loadMoreFailure = failure)
        }
        if (shouldReportRefusal(target, error)) refusalLog.report(target, FileAction.PREVIEW, error)
    }

    companion object {
        /** Half a typical screen of source code is a few KB; 128 KiB keeps round trips few on a VPN. */
        const val DEFAULT_SLICE_BYTES = 128 * 1024

        /** Decoded text kept in memory (a Kotlin string takes up to twice that). */
        const val DEFAULT_MAX_BYTES = 16L * 1024 * 1024
    }
}
