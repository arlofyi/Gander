package fyi.arlo.gander.core.data.actions

import fyi.arlo.gander.core.data.browse.EntryTarget
import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.protocol.PreviewKind
import fyi.arlo.gander.testing.SampleData
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Test

class ActionRefusalsTest {
    private val log = InMemoryActionRefusals()
    private val pdf = SampleData.file("a.pdf", preview = PreviewKind.PDF)
    private val target = EntryTarget("host", "shr", "Clients/a.pdf", pdf)
    private val other = EntryTarget("host", "shr", "Clients/b.pdf", SampleData.file("b.pdf"))

    private fun error(status: Int, code: String, message: String = "refused") =
        GanderError.fromResponse(status, """{"error":{"code":"$code","message":"$message","retry_after_ms":0}}""")

    @Test
    fun nothingIsReportedAtFirst() = runTest {
        assertEquals(emptyMap<FileAction, ActionReason>(), log.observe(target).first())
    }

    @Test
    fun aReportBecomesTheProtocolReasonOfThatAction() = runTest {
        log.report(target, FileAction.STREAM, error(412, "host_locked"))
        log.report(target, FileAction.PREVIEW, error(415, "unsupported_type"))
        assertEquals(
            mapOf(
                FileAction.STREAM to ActionReason.HostLocked,
                FileAction.PREVIEW to ActionReason.NoRenderer("pdf"),
            ),
            log.observe(target).first(),
        )
    }

    @Test
    fun aLaterReportReplacesAnEarlierOneOfTheSameAction() = runTest {
        log.report(target, FileAction.STREAM, error(412, "host_locked"))
        log.report(target, FileAction.STREAM, error(422, "stream_refused", "Scripts are not opened."))
        assertEquals(
            mapOf(FileAction.STREAM to ActionReason.StreamRefused("Scripts are not opened.")),
            log.observe(target).first(),
        )
    }

    @Test
    fun reportsAreKeptPerFile() = runTest {
        log.report(target, FileAction.STREAM, error(409, "stream_busy"))
        assertEquals(emptyMap<FileAction, ActionReason>(), log.observe(other).first())
    }

    @Test
    fun clearingOneActionKeepsTheOthers() = runTest {
        log.report(target, FileAction.STREAM, error(409, "stream_busy"))
        log.report(target, FileAction.FETCH, error(404, "not_found"))
        log.clear(target, FileAction.STREAM)
        assertEquals(mapOf(FileAction.FETCH to ActionReason.FileGone), log.observe(target).first())
        log.clear(target)
        assertEquals(emptyMap<FileAction, ActionReason>(), log.observe(target).first())
    }

    @Test
    fun clearingWhatIsNotThereChangesNothing() = runTest {
        log.clear(target)
        log.clear(target, FileAction.FETCH)
        assertEquals(emptyMap<FileAction, ActionReason>(), log.observe(target).first())
    }
}
