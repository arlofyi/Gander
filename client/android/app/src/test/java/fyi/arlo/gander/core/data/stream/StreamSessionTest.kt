package fyi.arlo.gander.core.data.stream

import fyi.arlo.gander.core.data.actions.ActionReason
import fyi.arlo.gander.core.data.actions.FileAction
import fyi.arlo.gander.core.data.actions.InMemoryActionRefusals
import fyi.arlo.gander.core.data.browse.EntryTarget
import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.protocol.ClientEvent
import fyi.arlo.gander.core.net.protocol.ClipboardMode
import fyi.arlo.gander.core.net.protocol.Entry
import fyi.arlo.gander.core.net.protocol.ErrorCode
import fyi.arlo.gander.core.net.protocol.InputMessage
import fyi.arlo.gander.core.net.protocol.MeResponse
import fyi.arlo.gander.core.net.protocol.MouseButton
import fyi.arlo.gander.core.net.protocol.RelayPath
import fyi.arlo.gander.core.net.protocol.ServerEvent
import fyi.arlo.gander.core.net.protocol.StreamApp
import fyi.arlo.gander.core.net.protocol.StreamCreated
import fyi.arlo.gander.core.net.protocol.StreamPreference
import fyi.arlo.gander.core.net.protocol.StreamReason
import fyi.arlo.gander.core.net.protocol.StreamRequest
import fyi.arlo.gander.core.net.protocol.StreamState
import fyi.arlo.gander.core.net.protocol.StreamWindow
import fyi.arlo.gander.core.net.protocol.VideoCodec
import fyi.arlo.gander.core.net.protocol.Viewport
import fyi.arlo.gander.testing.SampleData
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.receiveAsFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.yield
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.IOException
import java.util.Collections

/**
 * The whole viewer session against fakes of the host (HTTP and `/v1/events`) and of the peer
 * connection: the order of the signalling, the trickled candidates, the liveness pings, pause on
 * background, reconnecting, and every way a session ends.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class StreamSessionTest {

    private val entry = SampleData.file("Deck.pptx", size = 4096, mtime = "2026-10-30T09:12:44Z")
    private val target = EntryTarget("host-1", "shr_projects", "Clients/Deck.pptx", entry)
    private val viewport = Viewport(1080, 2280, 2.625)
    private val app = StreamApp("powerpoint", "Microsoft PowerPoint")
    private val pickTitles = listOf("Deck.pptx - PowerPoint", "Deck (2).pptx - PowerPoint")

    // Fakes --------------------------------------------------------------------------------------

    /** Everything that happened, in order, for the tests that care about it. */
    private val log: MutableList<String> = Collections.synchronizedList(ArrayList())

    private inner class FakeApi : StreamHostApi {
        var me: MeResponse = SampleData.me()
        var startError: GanderError? = null
        var created = StreamCreated("ses_1", StreamState.REQUESTED, app, needsApproval = false)

        /** When set, `POST /stream` waits for it (events can overtake the answer). */
        var gate: CompletableDeferred<Unit>? = null
        val started = Collections.synchronizedList(ArrayList<StreamRequest>())
        val ended = Collections.synchronizedList(ArrayList<Pair<String, Boolean>>())
        var stat: () -> Entry = { entry }

        override suspend fun me(): MeResponse = me

        override suspend fun startStream(request: StreamRequest): StreamCreated {
            log += "POST /stream"
            started += request
            gate?.await()
            startError?.let { throw it }
            return created
        }

        override suspend fun endStream(session: String, closeApp: Boolean) {
            log += "DELETE /stream"
            ended += session to closeApp
        }

        override suspend fun stat(share: String, path: String): Entry = stat()
    }

    private inner class FakeConnection : StreamEventsConnection {
        private val channel = Channel<ServerEvent>(Channel.UNLIMITED)
        override val incoming: Flow<ServerEvent> = channel.receiveAsFlow()
        val sent = Collections.synchronizedList(ArrayList<ClientEvent>())

        @Volatile
        var closed = false

        fun push(event: ServerEvent) {
            channel.trySend(event)
        }

        fun fail(error: GanderError) {
            channel.close(error)
        }

        override fun send(event: ClientEvent): Boolean {
            sent += event
            log += "ws:${event::class.simpleName}"
            return true
        }

        override fun close() {
            closed = true
        }
    }

    private inner class FakeEvents : StreamEventsSource {
        val connections = Collections.synchronizedList(ArrayList<FakeConnection>())
        var sayHello = true

        override fun open(): StreamEventsConnection {
            log += "events.open"
            return FakeConnection().also {
                connections += it
                if (sayHello) it.push(ServerEvent.Hello(1, "2026-11-02T10:00:00Z"))
            }
        }

        val last: FakeConnection get() = connections.last()
    }

    private inner class FakeTransport : StreamTransport {
        private val channel = Channel<TransportEvent>(Channel.UNLIMITED)
        override val events: Flow<TransportEvent> = channel.receiveAsFlow()
        val offers = Collections.synchronizedList(ArrayList<String>())
        val remote = Collections.synchronizedList(ArrayList<String>())
        val sent = Collections.synchronizedList(ArrayList<Pair<InputChannel, String>>())

        /** Candidates the transport finds while it makes the answer: they must not overtake it. */
        var candidatesDuringAnswer: List<String> = emptyList()
        var answerError: Exception? = null

        @Volatile
        var closed = false

        fun emit(event: TransportEvent) {
            channel.trySend(event)
        }

        override suspend fun acceptOffer(offerSdp: String): String {
            offers += offerSdp
            answerError?.let { throw it }
            candidatesDuringAnswer.forEach { emit(TransportEvent.LocalCandidate(it)) }
            if (candidatesDuringAnswer.isNotEmpty()) yield()
            return "ANSWER"
        }

        override fun addRemoteCandidate(candidate: String) {
            remote += candidate
        }

        override fun send(channel: InputChannel, text: String): Boolean {
            sent += channel to text
            return !closed
        }

        /** The local sound switch, as the controller last set it (WP-55). */
        @Volatile
        var audioOn = true

        override fun setAudioEnabled(enabled: Boolean) {
            audioOn = enabled
        }

        override fun close() {
            closed = true
        }

        fun sentOn(channel: InputChannel): List<String> = sent.filter { it.first == channel }.map { it.second }
    }

    private inner class Rig(scope: CoroutineScope, private val testScope: TestScope) {
        val api = FakeApi()
        val events = FakeEvents()
        val transports = Collections.synchronizedList(ArrayList<FakeTransport>())
        val refusals = InMemoryActionRefusals()
        var decoder = true
        var configureTransport: (FakeTransport) -> Unit = {}
        val controller = StreamSessionController(
            target = target,
            api = api,
            eventsSource = events,
            transportFactory = { FakeTransport().also { configureTransport(it); transports += it } },
            decoderAvailable = { decoder },
            refusals = refusals,
            scope = scope,
            clock = { testScope.testScheduler.currentTime },
        )

        val phase: StreamPhase get() = controller.state.value.phase
        val transport: FakeTransport get() = transports.last()

        fun advance(ms: Long) {
            testScope.testScheduler.advanceTimeBy(ms)
            testScope.testScheduler.runCurrent()
        }

        fun settle() = testScope.testScheduler.runCurrent()

        fun start() {
            controller.start(viewport)
            settle()
        }

        fun hostState(
            state: StreamState,
            reason: StreamReason? = null,
            window: StreamWindow? = null,
            candidates: List<String>? = null,
        ) {
            events.last.push(ServerEvent.StreamStateChanged("ses_1", state, reason, window, candidates))
            settle()
        }

        /** An ambiguous window: the host asks the phone to pick one of [pickTitles]. */
        fun picking() {
            start()
            hostState(StreamState.LAUNCHING)
            hostState(StreamState.PICK, candidates = pickTitles)
            assertEquals(StreamPhase.PickWindow(pickTitles), phase)
        }

        fun hostError(code: ErrorCode, message: String) {
            events.last.push(ServerEvent.Error(code, message, "ses_1"))
            settle()
        }

        /** Offer, answer, a link that is up, an open control channel and a host that says streaming. */
        fun streaming() {
            start()
            events.last.push(ServerEvent.StreamOffer("ses_1", "OFFER"))
            settle()
            transport.emit(TransportEvent.Link(PeerLink.UP))
            transport.emit(TransportEvent.ChannelOpen(InputChannel.INPUT))
            transport.emit(TransportEvent.ChannelOpen(InputChannel.CONTROL))
            hostState(StreamState.STREAMING)
            assertEquals(StreamPhase.Streaming, phase)
        }

        fun pongAll() {
            transport.sentOn(InputChannel.CONTROL).forEach { text ->
                (StreamWire.parseControl(text) as? ControlMessage.Ping)?.let {
                    transport.emit(TransportEvent.ControlText(StreamWire.encode(ControlMessage.Pong(it.seq))))
                }
            }
            settle()
        }
    }

    private fun runRig(block: suspend TestScope.(Rig) -> Unit) = runTest {
        block(Rig(backgroundScope, this))
    }

    // The request ----------------------------------------------------------------------------------

    @Test
    fun theEventsSocketIsOpenBeforeTheStreamIsRequested() = runRig { rig ->
        rig.start()
        assertEquals(listOf("events.open", "POST /stream"), log)
        assertEquals(StreamPhase.Requested, rig.phase)
        assertEquals("ses_1", rig.controller.state.value.session)
        assertEquals(app, rig.controller.state.value.app)
    }

    @Test
    fun theRequestNamesTheFileTheViewportAndH264() = runRig { rig ->
        rig.start()
        assertEquals(
            StreamRequest(
                share = "shr_projects",
                path = "Clients/Deck.pptx",
                app = null,
                viewport = viewport,
                prefer = StreamPreference(VideoCodec.H264, 8_000),
                resume = null,
            ),
            rig.api.started.single(),
        )
    }

    @Test
    fun hostStateEventsDriveTheBannerPhases() = runRig { rig ->
        rig.start()
        rig.hostState(StreamState.APPROVING)
        assertEquals(StreamPhase.Approving, rig.phase)
        rig.hostState(StreamState.LAUNCHING)
        assertEquals(StreamPhase.Launching, rig.phase)
        rig.hostState(StreamState.WINDOW_FOUND, window = StreamWindow("Deck.pptx - PowerPoint", 1028, 2172, 144))
        assertEquals(StreamPhase.WindowFound, rig.phase)
        assertEquals("the window's size seeds the picture", 1028, rig.controller.state.value.window?.width)
    }

    @Test
    fun eventsForAnotherSessionAreIgnored() = runRig { rig ->
        rig.start()
        rig.events.last.push(ServerEvent.StreamStateChanged("ses_other", StreamState.ENDED, StreamReason.HOST_KILL_SWITCH, null))
        rig.settle()
        assertEquals(StreamPhase.Requested, rig.phase)
    }

    @Test
    fun eventsThatOvertakeTheAnswerToThePostAreKeptAndHandled() = runRig { rig ->
        rig.api.gate = CompletableDeferred()
        rig.start()
        assertEquals("the POST is still in flight", StreamPhase.Starting, rig.phase)
        rig.events.last.push(ServerEvent.StreamOffer("ses_1", "OFFER"))
        rig.events.last.push(ServerEvent.StreamStateChanged("ses_1", StreamState.WINDOW_FOUND, null, null))
        rig.settle()
        assertTrue("no peer connection before the session is known", rig.transports.isEmpty())
        rig.api.gate!!.complete(Unit)
        rig.settle()
        assertEquals(listOf("OFFER"), rig.transport.offers)
        assertEquals(StreamPhase.Connecting, rig.phase)
    }

    // Clipboard text (WP-56) ------------------------------------------------------------------------------

    private fun TestScope.collectClipboard(rig: Rig): List<ClipboardOutcome> {
        val outcomes = Collections.synchronizedList(ArrayList<ClipboardOutcome>())
        backgroundScope.launch { rig.controller.clipboard.collect { outcomes += it } }
        rig.settle()
        return outcomes
    }

    private fun Rig.streamingWithClipboard(mode: ClipboardMode) {
        api.me = SampleData.me(clipboard = mode)
        streaming()
    }

    private fun Rig.hostSays(message: ControlMessage) {
        transport.emit(TransportEvent.ControlText(StreamWire.encode(message)))
        settle()
    }

    private fun Rig.controlSent(): List<String> = transport.sentOn(InputChannel.CONTROL)

    // Sound (WP-55) -------------------------------------------------------------------------------------

    @Test
    fun theMuteButtonAppearsOnlyWhenTheSessionCarriesSound() = runRig { rig ->
        rig.streaming()
        assertFalse("nothing said yet", rig.controller.state.value.audio.available)
        rig.hostSays(ControlMessage.Audio(on = false, reason = AudioReason.NONE))
        assertFalse(rig.controller.state.value.audio.available)
        rig.hostSays(ControlMessage.Audio(on = false, reason = AudioReason.UNAVAILABLE))
        assertFalse(rig.controller.state.value.audio.available)
        rig.hostSays(ControlMessage.Audio(on = true))
        assertTrue(rig.controller.state.value.audio.available)
        rig.hostSays(ControlMessage.Audio(on = false, reason = AudioReason.PAUSED))
        assertTrue("paused sound comes back on resume", rig.controller.state.value.audio.available)
    }

    @Test
    fun muteSilencesHereTellsTheHostAndSurvivesAReconnect() = runRig { rig ->
        rig.streaming()
        rig.hostSays(ControlMessage.Audio(on = true))
        rig.controller.setMuted(true)
        rig.settle()
        assertTrue(rig.controller.state.value.audio.muted)
        assertFalse(rig.transport.audioOn)
        assertEquals(listOf("""{"t":"mute","on":true}"""), rig.controlSent().filter { it.contains("mute") })
        // The host confirms; nothing more is sent.
        rig.hostSays(ControlMessage.Audio(on = false, reason = AudioReason.MUTED))
        assertEquals(1, rig.controlSent().count { it.contains("mute") })
        // A host that starts with sound again (a new engine session after a reconnect) is muted again.
        rig.hostSays(ControlMessage.Audio(on = true))
        assertEquals(2, rig.controlSent().count { it == """{"t":"mute","on":true}""" })
        rig.controller.setMuted(false)
        rig.settle()
        assertFalse(rig.controller.state.value.audio.muted)
        assertTrue(rig.transport.audioOn)
        assertEquals("""{"t":"mute","on":false}""", rig.controlSent().last())
    }

    @Test
    fun theViewerLearnsWhichClipboardDirectionsTheOwnerAllowed() = runRig { rig ->
        rig.streamingWithClipboard(ClipboardMode.TO_PHONE)
        assertEquals(ClipboardMode.TO_PHONE, rig.controller.state.value.clipboard)
    }

    @Test
    fun pasteToComputerSendsThePhonesTextOnTheControlChannelAndReportsTheAck() = runRig { rig ->
        rig.streamingWithClipboard(ClipboardMode.BOTH)
        val outcomes = collectClipboard(rig)
        rig.controller.pasteToComputer("two\nlines")
        rig.settle()
        assertEquals(
            "on the reliable channel, as the host requires",
            """{"t":"clipboard","s":"two\nlines"}""",
            rig.controlSent().last(),
        )
        assertTrue("no answer yet", outcomes.isEmpty())
        rig.hostSays(ControlMessage.ClipboardAck(9))
        assertEquals(listOf<ClipboardOutcome>(ClipboardOutcome.Pasted(9)), outcomes)
    }

    @Test
    fun copyFromComputerAsksAndHandsBackTheText() = runRig { rig ->
        rig.streamingWithClipboard(ClipboardMode.TO_PHONE)
        val outcomes = collectClipboard(rig)
        rig.controller.copyFromComputer()
        rig.settle()
        assertEquals("""{"t":"clipboard.get"}""", rig.controlSent().last())
        rig.hostSays(ControlMessage.ClipboardText("from the computer"))
        assertEquals(listOf<ClipboardOutcome>(ClipboardOutcome.Copied("from the computer")), outcomes)
    }

    @Test
    fun textTheHostPushesWithoutBeingAskedIsDropped() = runRig { rig ->
        rig.streamingWithClipboard(ClipboardMode.BOTH)
        val outcomes = collectClipboard(rig)
        rig.hostSays(ControlMessage.ClipboardText("surprise"))
        rig.hostSays(ControlMessage.ClipboardAck(3))
        rig.hostSays(ControlMessage.ClipboardRefused(ClipboardOp.GET, ClipboardRefusal.BUSY))
        assertTrue("the computer's clipboard never reaches the phone unasked: $outcomes", outcomes.isEmpty())
        // Nor does an answer to the other kind of request.
        rig.controller.copyFromComputer()
        rig.settle()
        rig.hostSays(ControlMessage.ClipboardAck(3))
        assertTrue(outcomes.isEmpty())
    }

    @Test
    fun aRefusalFromTheHostIsReportedWithItsReason() = runRig { rig ->
        rig.streamingWithClipboard(ClipboardMode.BOTH)
        val outcomes = collectClipboard(rig)
        rig.controller.copyFromComputer()
        rig.settle()
        rig.hostSays(ControlMessage.ClipboardRefused(ClipboardOp.GET, ClipboardRefusal.LOCAL_USER_ACTIVE))
        rig.controller.pasteToComputer("x")
        rig.settle()
        rig.hostSays(ControlMessage.ClipboardRefused(ClipboardOp.SET, ClipboardRefusal.HOST_LOCKED))
        assertEquals(
            listOf<ClipboardOutcome>(
                ClipboardOutcome.Refused(ClipboardOp.GET, ClipboardRefusal.LOCAL_USER_ACTIVE),
                ClipboardOutcome.Refused(ClipboardOp.SET, ClipboardRefusal.HOST_LOCKED),
            ),
            outcomes,
        )
    }

    @Test
    fun aDirectionTheOwnerDidNotAllowIsNotEvenSent() = runRig { rig ->
        rig.streamingWithClipboard(ClipboardMode.TO_PHONE)
        val outcomes = collectClipboard(rig)
        val before = rig.controlSent().size
        rig.controller.pasteToComputer("not allowed")
        rig.settle()
        assertEquals(listOf<ClipboardOutcome>(ClipboardOutcome.Refused(ClipboardOp.SET, ClipboardRefusal.DISABLED)), outcomes)
        assertEquals("nothing went out", before, rig.controlSent().size)
    }

    @Test
    fun aHostWithTheClipboardOffGetsNoRequestsAtAll() = runRig { rig ->
        rig.streamingWithClipboard(ClipboardMode.OFF)
        val outcomes = collectClipboard(rig)
        val before = rig.controlSent().size
        rig.controller.copyFromComputer()
        rig.controller.pasteToComputer("x")
        rig.settle()
        assertEquals(
            listOf<ClipboardOutcome>(
                ClipboardOutcome.Refused(ClipboardOp.GET, ClipboardRefusal.DISABLED),
                ClipboardOutcome.Refused(ClipboardOp.SET, ClipboardRefusal.DISABLED),
            ),
            outcomes,
        )
        assertEquals(before, rig.controlSent().size)
    }

    @Test
    fun textOverTheCapOrEmptyIsNotSent() = runRig { rig ->
        rig.streamingWithClipboard(ClipboardMode.BOTH)
        val outcomes = collectClipboard(rig)
        val before = rig.controlSent().size
        rig.controller.pasteToComputer("a".repeat(CLIPBOARD_MAX_TEXT_BYTES + 1))
        rig.controller.pasteToComputer("")
        rig.settle()
        assertEquals(
            listOf<ClipboardOutcome>(
                ClipboardOutcome.Refused(ClipboardOp.SET, ClipboardRefusal.TOO_LARGE),
                ClipboardOutcome.Refused(ClipboardOp.SET, ClipboardRefusal.NO_TEXT),
            ),
            outcomes,
        )
        assertEquals(before, rig.controlSent().size)
        // Exactly the cap is sent.
        rig.controller.pasteToComputer("a".repeat(CLIPBOARD_MAX_TEXT_BYTES))
        rig.settle()
        assertEquals(before + 1, rig.controlSent().size)
    }

    @Test
    fun aSecondTapWhileTheFirstIsInFlightDoesNothing() = runRig { rig ->
        rig.streamingWithClipboard(ClipboardMode.BOTH)
        rig.controller.copyFromComputer()
        rig.controller.copyFromComputer()
        rig.settle()
        assertEquals(1, rig.controlSent().count { it == """{"t":"clipboard.get"}""" })
        // Once answered, the next tap asks again.
        rig.hostSays(ControlMessage.ClipboardText("x"))
        rig.controller.copyFromComputer()
        rig.settle()
        assertEquals(2, rig.controlSent().count { it == """{"t":"clipboard.get"}""" })
    }

    @Test
    fun aHostThatDoesNotAnswerIsGivenUpOnAfterTenSeconds() = runRig { rig ->
        rig.streamingWithClipboard(ClipboardMode.BOTH)
        val outcomes = collectClipboard(rig)
        rig.controller.copyFromComputer()
        rig.advance(CLIPBOARD_TIMEOUT_MS - 1)
        assertTrue(outcomes.isEmpty())
        rig.advance(1)
        assertEquals(listOf<ClipboardOutcome>(ClipboardOutcome.NoAnswer(ClipboardOp.GET)), outcomes)
        // A late answer is not a surprise copy.
        rig.hostSays(ControlMessage.ClipboardText("too late"))
        assertEquals(1, outcomes.size)
    }

    @Test
    fun nothingIsSentWhileTheStreamIsNotRunning() = runRig { rig ->
        rig.streamingWithClipboard(ClipboardMode.BOTH)
        val outcomes = collectClipboard(rig)
        rig.controller.onBackground()
        rig.settle()
        val before = rig.controlSent().size
        rig.controller.copyFromComputer()
        rig.settle()
        assertEquals(
            listOf<ClipboardOutcome>(ClipboardOutcome.Refused(ClipboardOp.GET, ClipboardRefusal.UNAVAILABLE)),
            outcomes,
        )
        assertEquals(before, rig.controlSent().size)
    }

    // Window pick (stream.state pick, stream.pick) -----------------------------------------------------

    @Test
    fun tappingACandidateSendsStreamPickAndTheHostMovingOnClearsIt() = runRig { rig ->
        rig.picking()
        assertTrue(rig.controller.pickWindow(1))
        assertEquals(
            "the choice goes out on /v1/events with the session and the candidate's index",
            ClientEvent.StreamPick("ses_1", 1),
            rig.events.last.sent.last(),
        )
        assertEquals("the viewer shows which row was sent", StreamPhase.PickWindow(pickTitles, chosen = 1), rig.phase)
        rig.hostState(StreamState.WINDOW_FOUND, window = StreamWindow("Deck (2).pptx - PowerPoint", 1028, 2172, 144))
        assertEquals(StreamPhase.WindowFound, rig.phase)
        // The stream carries on as any other: offer, answer.
        rig.events.last.push(ServerEvent.StreamOffer("ses_1", "OFFER"))
        rig.settle()
        assertEquals(StreamPhase.Connecting, rig.phase)
        assertTrue(rig.events.last.sent.contains(ClientEvent.StreamAnswer("ses_1", "ANSWER")))
    }

    @Test
    fun aPickNobodyAskedForOrOutsideTheListIsNotSent() = runRig { rig ->
        rig.start()
        assertFalse("the host has not asked yet", rig.controller.pickWindow(0))
        rig.hostState(StreamState.PICK, candidates = pickTitles)
        assertFalse(rig.controller.pickWindow(2))
        assertFalse(rig.controller.pickWindow(-1))
        assertTrue(rig.events.last.sent.none { it is ClientEvent.StreamPick })
        rig.hostState(StreamState.WINDOW_FOUND)
        assertFalse("the host has its window", rig.controller.pickWindow(0))
        assertTrue(rig.events.last.sent.none { it is ClientEvent.StreamPick })
    }

    @Test
    fun aPickCannotBeSentWithoutAnEventsSocketAndCanBeTriedAgain() = runRig { rig ->
        rig.picking()
        rig.events.last.fail(GanderError.Network(IOException("reset")))
        rig.settle()
        assertFalse(rig.controller.pickWindow(0))
        assertEquals("nothing is marked as sent", StreamPhase.PickWindow(pickTitles), rig.phase)
        rig.advance(StreamSessionController.REOPEN_DELAY_MS)
        assertEquals("the socket is back", 2, rig.events.connections.size)
        assertTrue(rig.controller.pickWindow(0))
        assertEquals(ClientEvent.StreamPick("ses_1", 0), rig.events.last.sent.last())
    }

    @Test
    fun aPickedWindowThatIsGoneIsSaidSoAndTheUserChoosesAgain() = runRig { rig ->
        rig.picking()
        assertTrue(rig.controller.pickWindow(0))
        rig.hostError(ErrorCode.APP_UNAVAILABLE, "That window is no longer open.")
        assertEquals(StreamPhase.PickWindow(pickTitles, chosen = null, windowGone = true), rig.phase)
        assertTrue(rig.controller.pickWindow(1))
        assertEquals(
            listOf(ClientEvent.StreamPick("ses_1", 0), ClientEvent.StreamPick("ses_1", 1)),
            rig.events.last.sent.filterIsInstance<ClientEvent.StreamPick>(),
        )
        rig.hostState(StreamState.WINDOW_FOUND)
        assertEquals(StreamPhase.WindowFound, rig.phase)
    }

    @Test
    fun aPickTheHostRefusesAsBadRequestCanBeSentAgain() = runRig { rig ->
        rig.picking()
        assertTrue(rig.controller.pickWindow(1))
        rig.hostError(ErrorCode.BAD_REQUEST, "No such window in the list.")
        assertEquals(StreamPhase.PickWindow(pickTitles), rig.phase)
    }

    @Test
    fun noChoiceInThirtySecondsEndsAsNoWindowWithoutADelete() = runRig { rig ->
        rig.picking()
        // What the host sends when its 30 s run out: the error, then the end with reason timeout.
        rig.hostError(ErrorCode.APP_UNAVAILABLE, "The app did not show a window that could be streamed.")
        rig.hostState(StreamState.ENDED, StreamReason.TIMEOUT)
        assertEquals(StreamPhase.Ended(EndCause.APP_UNAVAILABLE), rig.phase)
        assertTrue("the host ended it itself, so there is nothing to delete", rig.api.ended.isEmpty())
        assertTrue(rig.events.last.closed)
    }

    @Test
    fun anOrdinaryTimeoutIsNotMistakenForNoWindow() = runRig { rig ->
        rig.streaming()
        rig.hostState(StreamState.ENDED, StreamReason.TIMEOUT)
        assertEquals(StreamPhase.Ended(EndCause.TIMEOUT), rig.phase)
    }

    // Offer, answer and candidates ------------------------------------------------------------------

    @Test
    fun candidatesFoundWhileMakingTheAnswerAreSentAfterIt() = runRig { rig ->
        // The transport finds a candidate (and lets the collector see it) before the answer is ready.
        rig.configureTransport = { it.candidatesDuringAnswer = listOf("a=candidate:7 1 udp 2122260223 10.0.2.15 54321 typ host generation 0") }
        rig.start()
        rig.events.last.push(ServerEvent.StreamOffer("ses_1", "OFFER"))
        rig.settle()
        assertEquals(
            listOf(
                ClientEvent.StreamAnswer("ses_1", "ANSWER"),
                ClientEvent.StreamIce("ses_1", "candidate:7 1 udp 2122260223 10.0.2.15 54321 typ host generation 0"),
            ),
            rig.events.last.sent.toList(),
        )
    }

    @Test
    fun laterCandidatesAreTrickledAtOnceAndGatheringEndsWithAnEmptyCandidate() = runRig { rig ->
        rig.start()
        rig.events.last.push(ServerEvent.StreamOffer("ses_1", "OFFER"))
        rig.settle()
        rig.transport.emit(TransportEvent.LocalCandidate("a=candidate:1 1 udp 2122260223 10.0.2.15 54321 typ host generation 0"))
        rig.transport.emit(TransportEvent.GatheringComplete)
        rig.settle()
        assertEquals(
            listOf(
                ClientEvent.StreamAnswer("ses_1", "ANSWER"),
                ClientEvent.StreamIce("ses_1", "candidate:1 1 udp 2122260223 10.0.2.15 54321 typ host generation 0"),
                ClientEvent.StreamIce("ses_1", ""),
            ),
            rig.events.last.sent.toList(),
        )
    }

    @Test
    fun theHostsTrickledCandidatesReachThePeerConnectionAndBlanksDoNot() = runRig { rig ->
        rig.start()
        rig.events.last.push(ServerEvent.StreamOffer("ses_1", "OFFER"))
        rig.events.last.push(ServerEvent.StreamIce("ses_1", "candidate:1 1 udp 2130706431 100.64.1.2 50000 typ host"))
        rig.events.last.push(ServerEvent.StreamIce("ses_1", ""))
        rig.settle()
        assertEquals(listOf("candidate:1 1 udp 2130706431 100.64.1.2 50000 typ host"), rig.transport.remote)
    }

    @Test
    fun aCandidateThatArrivesBeforeTheOfferWaitsForThePeerConnection() = runRig { rig ->
        rig.start()
        rig.events.last.push(ServerEvent.StreamIce("ses_1", "candidate:1 1 udp 2130706431 100.64.1.2 50000 typ host"))
        rig.settle()
        assertTrue(rig.transports.isEmpty())
        rig.events.last.push(ServerEvent.StreamOffer("ses_1", "OFFER"))
        rig.settle()
        assertEquals(listOf("candidate:1 1 udp 2130706431 100.64.1.2 50000 typ host"), rig.transport.remote)
    }

    @Test
    fun anOfferThePeerConnectionCannotApplyEndsWithNoVideoLink() = runRig { rig ->
        rig.configureTransport = { it.answerError = IOException("bad sdp") }
        rig.start()
        rig.events.last.push(ServerEvent.StreamOffer("ses_1", "OFFER"))
        rig.settle()
        assertEquals(StreamPhase.Ended(EndCause.NO_VIDEO_LINK), rig.phase)
        assertEquals("a local end is reported to the host", listOf("ses_1" to false), rig.api.ended)
        assertTrue(rig.transport.closed)
    }
    // Liveness --------------------------------------------------------------------------------------------

    @Test
    fun theViewerPingsOnTheControlChannelEverySecond() = runRig { rig ->
        rig.streaming()
        rig.advance(250)
        rig.advance(1000)
        rig.advance(1000)
        val pings = rig.transport.sentOn(InputChannel.CONTROL).filter { it.startsWith("""{"t":"ping"""") }
        assertEquals(listOf("""{"t":"ping","seq":1}""", """{"t":"ping","seq":2}""", """{"t":"ping","seq":3}"""), pings)
        assertTrue("never on the lossy channel", rig.transport.sentOn(InputChannel.INPUT).isEmpty())
    }

    @Test
    fun aHostThatStopsAnsweringIsReconnectingAfterMoreThanFiveSeconds() = runRig { rig ->
        rig.streaming()
        rig.advance(5000)
        assertEquals(StreamPhase.Streaming, rig.phase)
        rig.advance(1000)
        assertEquals(StreamPhase.Reconnecting, rig.phase)
        rig.transport.emit(TransportEvent.ControlText("""{"t":"pong","seq":1}"""))
        rig.settle()
        assertEquals(StreamPhase.Streaming, rig.phase)
        assertTrue(
            "a fresh picture is asked for after the gap",
            rig.transport.sentOn(InputChannel.CONTROL).contains("""{"t":"keyframe"}"""),
        )
    }

    @Test
    fun theHostsControlStateMirrorsIntoThePhase() = runRig { rig ->
        rig.streaming()
        rig.transport.emit(TransportEvent.ControlText("""{"t":"state","state":"paused","reason":"local_user_active"}"""))
        rig.settle()
        assertEquals(StreamPhase.Paused(StreamReason.LOCAL_USER_ACTIVE), rig.phase)
        rig.transport.emit(TransportEvent.ControlText("""{"t":"state","state":"streaming","reason":null}"""))
        rig.settle()
        assertEquals(StreamPhase.Streaming, rig.phase)
    }

    @Test
    fun focusLostAtConnectSendsFrontOnceAndShowsTheAnswer() = runRig { rig ->
        rig.streaming()
        rig.transport.emit(TransportEvent.ControlText("""{"t":"state","state":"paused","reason":"focus_lost"}"""))
        rig.settle()
        rig.advance(StreamSessionController.TICK_MS)
        val fronts = { rig.transport.sentOn(InputChannel.CONTROL).count { it == """{"t":"front"}""" } }
        assertEquals(1, fronts())
        assertEquals(FrontStatus.SENDING, rig.controller.state.value.front)
        rig.transport.emit(TransportEvent.ControlText("""{"t":"front.result","acquired":false}"""))
        rig.settle()
        assertEquals(FrontStatus.REFUSED, rig.controller.state.value.front)
        // The banner's button asks again.
        assertTrue(rig.controller.bringToFront())
        assertEquals(2, fronts())
        rig.transport.emit(TransportEvent.ControlText("""{"t":"state","state":"streaming","reason":null}"""))
        rig.transport.emit(TransportEvent.ControlText("""{"t":"front.result","acquired":true}"""))
        rig.settle()
        assertEquals(StreamPhase.Streaming, rig.phase)
        assertEquals(FrontStatus.IDLE, rig.controller.state.value.front)
        assertFalse("nothing to bring forward while streaming", rig.controller.bringToFront())
    }

    @Test
    fun controlMessagesThisClientDoesNotKnowAreIgnored() = runRig { rig ->
        rig.streaming()
        rig.transport.emit(TransportEvent.ControlText("""{"t":"future","x":1}"""))
        rig.transport.emit(TransportEvent.ControlText("garbage"))
        rig.settle()
        assertEquals(StreamPhase.Streaming, rig.phase)
    }

    // Background ---------------------------------------------------------------------------------------------

    @Test
    fun theBackgroundPausesTheHostOnBothChannelsAndStopsThePings() = runRig { rig ->
        rig.streaming()
        rig.advance(1000)
        val before = rig.transport.sentOn(InputChannel.CONTROL).size
        rig.controller.onBackground()
        assertEquals(StreamPhase.Backgrounded, rig.phase)
        assertTrue(rig.transport.sentOn(InputChannel.CONTROL).contains("""{"t":"pause"}"""))
        assertTrue(rig.events.last.sent.contains(ClientEvent.StreamPause("ses_1")))
        rig.advance(30_000)
        assertEquals("no pings while away", before + 1, rig.transport.sentOn(InputChannel.CONTROL).size)
        assertEquals("and no reconnecting for a host nobody is listening to", StreamPhase.Backgrounded, rig.phase)
    }

    @Test
    fun comingBackResumesTheHostAndThePingsStartAgain() = runRig { rig ->
        rig.streaming()
        rig.advance(250)
        rig.controller.onBackground()
        rig.advance(60_000)
        rig.controller.onForeground()
        rig.settle()
        assertTrue(rig.transport.sentOn(InputChannel.CONTROL).contains("""{"t":"resume"}"""))
        assertTrue(rig.events.last.sent.contains(ClientEvent.StreamResume("ses_1")))
        assertEquals(StreamPhase.Streaming, rig.phase)
        rig.advance(250)
        val pings = rig.transport.sentOn(InputChannel.CONTROL).filter { it.contains(""""t":"ping"""") }
        assertTrue("a ping right away", pings.size >= 2)
    }

    @Test
    fun anEventsSocketThatDiedInTheBackgroundIsReopenedAndTheHostToldAgain() = runRig { rig ->
        rig.streaming()
        rig.controller.onBackground()
        rig.events.last.fail(GanderError.Network(IOException("reset")))
        rig.settle()
        rig.advance(StreamSessionController.REOPEN_DELAY_MS)
        assertEquals("a new socket", 2, rig.events.connections.size)
        assertTrue("it says again that this phone is away", rig.events.last.sent.contains(ClientEvent.StreamPause("ses_1")))
    }

    // Input -----------------------------------------------------------------------------------------------------

    @Test
    fun pointerMotionGoesOnTheLossyChannelAndClicksOnTheReliableOne() = runRig { rig ->
        rig.streaming()
        assertTrue(rig.controller.send(InputMessage.Move(0.5, 0.25)))
        assertTrue(rig.controller.send(InputMessage.Down(MouseButton.LEFT, 0.5, 0.25)))
        assertEquals(listOf("""{"t":"move","x":0.5,"y":0.25}"""), rig.transport.sentOn(InputChannel.INPUT))
        assertTrue(rig.transport.sentOn(InputChannel.CONTROL).contains("""{"t":"down","b":"left","x":0.5,"y":0.25}"""))
    }

    @Test
    fun nothingIsSentWhileTheStreamIsPausedOrNotThereYet() = runRig { rig ->
        assertFalse(rig.controller.send(InputMessage.Move(0.5, 0.5)))
        rig.streaming()
        rig.hostState(StreamState.PAUSED, StreamReason.FOCUS_LOST)
        assertFalse("the host is not injecting while paused", rig.controller.send(InputMessage.Move(0.5, 0.5)))
    }

    @Test
    fun fitToPhoneSendsTheNewViewport() = runRig { rig ->
        rig.streaming()
        rig.controller.resize(2400, 1080)
        assertTrue(rig.events.last.sent.contains(ClientEvent.StreamResize("ses_1", 2400, 1080)))
    }

    // Ending -----------------------------------------------------------------------------------------------------

    @Test
    fun leavingTellsTheHostWhetherToCloseTheAppAndTearsEverythingDown() = runRig { rig ->
        rig.streaming()
        rig.controller.end(closeApp = true)
        rig.settle()
        assertEquals(listOf("ses_1" to true), rig.api.ended)
        assertEquals(StreamPhase.Ended(EndCause.YOU_LEFT), rig.phase)
        assertTrue(rig.transport.closed)
        assertTrue(rig.events.last.closed)
    }

    @Test
    fun leavingTheAppOpenAsksTheHostNotToCloseIt() = runRig { rig ->
        rig.streaming()
        rig.controller.end(closeApp = false)
        assertEquals(listOf("ses_1" to false), rig.api.ended)
    }

    @Test
    fun aHostThatEndedTheSessionNeedsNoDelete() = runRig { rig ->
        rig.streaming()
        rig.hostState(StreamState.ENDED, StreamReason.WINDOW_CLOSED)
        assertEquals(StreamPhase.Ended(EndCause.WINDOW_CLOSED), rig.phase)
        rig.controller.end(closeApp = false)
        assertTrue(rig.api.ended.isEmpty())
        assertTrue(rig.transport.closed)
    }

    @Test
    fun aLockThatEndsTheStreamIsReportedAsALock() = runRig { rig ->
        rig.streaming()
        rig.events.last.push(ServerEvent.Error(ErrorCode.HOST_LOCKED, "The computer was locked; the stream ended.", "ses_1"))
        rig.settle()
        rig.hostState(StreamState.ENDED, StreamReason.ERROR)
        assertEquals(StreamPhase.Ended(EndCause.HOST_LOCKED), rig.phase)
    }

    @Test
    fun aRevokedTokenOnTheEventsSocketEndsTheStream() = runRig { rig ->
        rig.streaming()
        rig.events.last.fail(GanderError.Host.TokenRevoked(401, "revoked"))
        rig.settle()
        assertEquals(StreamPhase.Ended(EndCause.REVOKED), rig.phase)
    }

    @Test
    fun aViewerThatIsClearedWhileStreamingEndsTheStreamOnTheHost() = runRig { rig ->
        rig.streaming()
        rig.controller.release(this)
        rig.settle()
        assertEquals(listOf("ses_1" to false), rig.api.ended)
        assertTrue(rig.transport.closed)
    }

    @Test
    fun aViewerThatIsClearedAfterTheEndSendsNothing() = runRig { rig ->
        rig.streaming()
        rig.hostState(StreamState.ENDED, StreamReason.HOST_KILL_SWITCH)
        rig.controller.release(this)
        rig.settle()
        assertTrue(rig.api.ended.isEmpty())
    }

    // Reconnecting -------------------------------------------------------------------------------------------------

    @Test
    fun aFailedLinkIsRenegotiatedForTheSameSession() = runRig { rig ->
        rig.streaming()
        val old = rig.transport
        old.emit(TransportEvent.Link(PeerLink.FAILED))
        rig.settle()
        assertEquals(StreamPhase.Reconnecting, rig.phase)
        assertTrue(old.closed)
        val resumed = rig.api.started.last()
        assertEquals("ses_1", resumed.resume)
        assertEquals(2, rig.api.started.size)
        // The host re-offers over a fresh peer connection.
        rig.events.last.push(ServerEvent.StreamOffer("ses_1", "OFFER 2"))
        rig.settle()
        assertEquals(2, rig.transports.size)
        assertEquals(listOf("OFFER 2"), rig.transport.offers)
        rig.transport.emit(TransportEvent.Link(PeerLink.UP))
        rig.transport.emit(TransportEvent.ChannelOpen(InputChannel.CONTROL))
        rig.settle()
        assertEquals(StreamPhase.Streaming, rig.phase)
    }

    @Test
    fun aRefusedResumeEndsTheStreamAndTellsTheHost() = runRig { rig ->
        rig.streaming()
        rig.api.startError = GanderError.Host.NotFound(404, "gone")
        rig.transport.emit(TransportEvent.Link(PeerLink.FAILED))
        rig.settle()
        assertEquals(StreamPhase.Ended(EndCause.CONNECTION_LOST), rig.phase)
        assertEquals(listOf("ses_1" to false), rig.api.ended)
    }

    @Test
    fun aVideoLinkThatNeverComesUpEndsTheStream() = runRig { rig ->
        rig.start()
        rig.events.last.push(ServerEvent.StreamOffer("ses_1", "OFFER"))
        rig.settle()
        rig.hostState(StreamState.STREAMING)
        rig.advance(21_000)
        assertEquals(StreamPhase.Ended(EndCause.NO_VIDEO_LINK), rig.phase)
        assertEquals(listOf("ses_1" to false), rig.api.ended)
    }

    // Refusals and preconditions -----------------------------------------------------------------------------------

    @Test
    fun aRefusedRequestIsAFailureAndTheSheetLearnsWhy() = runRig { rig ->
        rig.api.startError = GanderError.Host.HostLocked(412, "studio-pc is locked")
        rig.start()
        val failed = rig.phase as StreamPhase.Failed
        assertTrue(failed.error is GanderError.Host.HostLocked)
        assertEquals(ActionReason.HostLocked, rig.refusals.observe(target).first()[FileAction.STREAM])
        assertTrue(rig.events.last.closed)
    }

    @Test
    fun aHostWithoutStreamModeYetIsHandledGracefully() = runRig { rig ->
        rig.api.startError = GanderError.Host.StreamUnavailable(503, "Stream mode is not available on this computer yet.")
        rig.start()
        assertTrue((rig.phase as StreamPhase.Failed).error is GanderError.Host.StreamUnavailable)
        assertEquals(ActionReason.StreamNotEnabled, rig.refusals.observe(target).first()[FileAction.STREAM])
    }

    @Test
    fun otherRefusalsAreReportedToo() = runRig { rig ->
        rig.api.startError = GanderError.Host.StreamBusy(409, "busy")
        rig.start()
        assertEquals(ActionReason.StreamBusy, rig.refusals.observe(target).first()[FileAction.STREAM])
    }

    @Test
    fun streamModeTurnedOffOnTheHostIsReportedWithoutARequest() = runRig { rig ->
        rig.api.me = SampleData.me().let { me -> me.copy(capabilities = me.capabilities.copy(stream = me.capabilities.stream.copy(enabled = false))) }
        rig.start()
        assertTrue((rig.phase as StreamPhase.Failed).error is GanderError.Host.StreamUnavailable)
        assertTrue("no request, no socket", rig.api.started.isEmpty() && rig.events.connections.isEmpty())
        assertEquals(ActionReason.StreamNotEnabled, rig.refusals.observe(target).first()[FileAction.STREAM])
    }

    @Test
    fun aHostThatDoesNotStreamH264IsUnsupported() = runRig { rig ->
        rig.api.me = SampleData.me().let { me -> me.copy(capabilities = me.capabilities.copy(stream = me.capabilities.stream.copy(codecs = listOf(VideoCodec.HEVC)))) }
        rig.start()
        assertEquals(StreamPhase.Unsupported(StreamPhase.Unsupported.Why.HOST_DOES_NOT_STREAM_H264), rig.phase)
        assertTrue(rig.api.started.isEmpty())
    }

    @Test
    fun aPhoneWithoutAnH264DecoderRefusesBeforeAskingTheHost() = runRig { rig ->
        rig.decoder = false
        rig.start()
        assertEquals(StreamPhase.Unsupported(StreamPhase.Unsupported.Why.PHONE_HAS_NO_H264_DECODER), rig.phase)
        assertTrue(rig.api.started.isEmpty())
    }

    @Test
    fun anUnreachableHostFailsTheStartWithThatError() = runRig { rig ->
        rig.events.sayHello = false
        rig.start()
        rig.events.last.fail(GanderError.Unreachable(listOf("100.64.1.2"), null))
        rig.settle()
        assertTrue((rig.phase as StreamPhase.Failed).error is GanderError.Unreachable)
        assertTrue("no request was made", rig.api.started.isEmpty())
    }

    @Test
    fun aHostThatNeverSaysHelloTimesOut() = runRig { rig ->
        rig.events.sayHello = false
        rig.start()
        rig.advance(StreamSessionController.HELLO_TIMEOUT_MS + 1)
        assertTrue((rig.phase as StreamPhase.Failed).error is GanderError.Timeout)
    }

    @Test
    fun aRelayedPathAsksBeforeAnythingIsRequested() = runRig { rig ->
        rig.api.me = SampleData.me(relayed = RelayPath.DERP)
        rig.start()
        val phase = rig.phase as StreamPhase.ConfirmRelay
        assertEquals(RelayPath.DERP, phase.path)
        assertTrue("nothing started yet", rig.api.started.isEmpty() && rig.events.connections.isEmpty())
        rig.controller.answerRelayWarning(true)
        rig.settle()
        assertEquals(1, rig.api.started.size)
        assertEquals(StreamPhase.Requested, rig.phase)
    }

    @Test
    fun decliningTheRelayWarningLeavesWithoutARequest() = runRig { rig ->
        rig.api.me = SampleData.me(relayed = RelayPath.PEER_RELAY)
        rig.start()
        rig.controller.answerRelayWarning(false)
        rig.settle()
        assertEquals(StreamPhase.Ended(EndCause.YOU_LEFT), rig.phase)
        assertTrue(rig.api.started.isEmpty())
    }

    @Test
    fun aDirectPathNeedsNoWarning() = runRig { rig ->
        rig.api.me = SampleData.me(relayed = RelayPath.DIRECT)
        rig.start()
        assertEquals(1, rig.api.started.size)
    }

    @Test
    fun tryingAgainAfterAFailureStartsANewSession() = runRig { rig ->
        rig.api.startError = GanderError.Host.StreamBusy(409, "busy")
        rig.start()
        assertTrue(rig.phase is StreamPhase.Failed)
        rig.api.startError = null
        rig.controller.retry(viewport)
        rig.settle()
        assertEquals(StreamPhase.Requested, rig.phase)
        assertEquals(2, rig.api.started.size)
        assertNull("a fresh session, not a resume", rig.api.started.last().resume)
    }

    // "Fetch the saved file" -----------------------------------------------------------------------------------------

    @Test
    fun aFileThatChangedDuringTheSessionIsOfferedForFetching() = runRig { rig ->
        val saved = SampleData.file("Deck.pptx", size = 5000, mtime = "2026-11-02T10:30:00Z")
        rig.api.stat = { saved }
        assertEquals(saved, rig.controller.changedFile())
    }

    @Test
    fun aFileWithTheSameSizeAndTimeIsNotOffered() = runRig { rig ->
        assertNull(rig.controller.changedFile())
    }

    @Test
    fun aNewerModificationTimeAloneCountsAsChanged() = runRig { rig ->
        rig.api.stat = { SampleData.file("Deck.pptx", size = 4096, mtime = "2026-11-02T10:30:00Z") }
        assertNotNull(rig.controller.changedFile())
    }

    @Test
    fun aHostThatCannotBeAskedOffersNothing() = runRig { rig ->
        rig.api.stat = { throw GanderError.Unreachable(emptyList(), null) }
        assertNull(rig.controller.changedFile())
    }
}
