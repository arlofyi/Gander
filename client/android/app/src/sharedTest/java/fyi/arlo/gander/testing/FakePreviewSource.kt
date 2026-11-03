package fyi.arlo.gander.testing

import fyi.arlo.gander.core.data.preview.PreviewPageData
import fyi.arlo.gander.core.data.preview.PreviewPageRef
import fyi.arlo.gander.core.data.preview.PreviewSource
import fyi.arlo.gander.core.data.preview.PreviewTileRef
import fyi.arlo.gander.core.data.preview.TextChunk
import fyi.arlo.gander.core.net.protocol.PreviewInfo
import fyi.arlo.gander.core.net.protocol.PreviewKind
import fyi.arlo.gander.core.net.protocol.TilePage
import okio.ByteString.Companion.encodeUtf8
import java.util.Collections

/** One recorded `preview/text` request. */
data class TextCall(val offset: Long, val limit: Int)

/**
 * A scripted host behind [PreviewSource]. Set [infoHandler], [pageHandler], [tileHandler] or [textHandler] to
 * answer or throw (`GanderError.Host.Generating`, `UnsupportedType`...); every call is recorded.
 */
class FakePreviewSource : PreviewSource {
    var infoHandler: suspend () -> PreviewInfo = { pdfInfo() }
    var pageHandler: suspend (PreviewPageRef) -> PreviewPageData = {
        PreviewPageData("page".encodeUtf8(), "image/webp", "\"p-1\"")
    }
    var tileHandler: suspend (PreviewTileRef) -> PreviewPageData = {
        PreviewPageData("tile".encodeUtf8(), "image/webp", "\"t-1\"")
    }
    var textHandler: suspend (TextCall) -> TextChunk = { call ->
        TextChunk("", call.offset, call.offset, "utf-8", 0, hasMore = false)
    }

    // Coil fetches pages on its own threads, so the call lists are synchronised.
    val infoCalls: MutableList<String> = Collections.synchronizedList(mutableListOf())
    val pageCalls: MutableList<PreviewPageRef> = Collections.synchronizedList(mutableListOf())
    val tileCalls: MutableList<PreviewTileRef> = Collections.synchronizedList(mutableListOf())
    val textCalls: MutableList<TextCall> = Collections.synchronizedList(mutableListOf())

    override suspend fun info(hostId: String, shareId: String, path: String): PreviewInfo {
        infoCalls += path
        return infoHandler()
    }

    override suspend fun page(ref: PreviewPageRef): PreviewPageData {
        pageCalls += ref
        return pageHandler(ref)
    }

    override suspend fun tile(ref: PreviewTileRef): PreviewPageData {
        tileCalls += ref
        return tileHandler(ref)
    }

    override suspend fun text(hostId: String, shareId: String, path: String, offset: Long, limit: Int): TextChunk {
        val call = TextCall(offset, limit)
        textCalls += call
        return textHandler(call)
    }

    companion object {
        fun pdfInfo(
            pages: Int? = 5,
            estimatedMs: Long = 400,
            kind: PreviewKind = PreviewKind.PDF,
            tilePages: List<TilePage>? = null,
        ) = PreviewInfo(
            kind = kind,
            pages = pages,
            width = 1240,
            height = 1754,
            textBytes = null,
            renderer = "windows_pdf",
            estimatedMs = estimatedMs,
            tileSize = tilePages?.let { 512 },
            tileLevels = tilePages?.maxOfOrNull { it.levels },
            tilePages = tilePages,
        )
    }
}
