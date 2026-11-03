package fyi.arlo.gander.core.data.stream

import fyi.arlo.gander.core.net.protocol.ClientEvent
import fyi.arlo.gander.core.net.protocol.InputMessage
import fyi.arlo.gander.core.net.protocol.KeyModifier
import fyi.arlo.gander.core.net.protocol.MouseButton
import fyi.arlo.gander.core.net.protocol.StreamReason
import fyi.arlo.gander.core.net.protocol.StreamState
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The wire shapes of the stream viewer. The control strings are the ones the host crate's
 * `gander-stream/src/control.rs` tests pin (`wire_shapes`), so a drift on either side fails here
 * or there.
 */
class StreamWireTest {

    // /v1/events frames, phone to host ---------------------------------------------------------

    @Test
    fun theAnswerCarriesTheSessionAndTheSdp() {
        assertEquals(
            """{"type":"stream.answer","session":"ses_1","sdp":"v=0\r\n"}""",
            StreamWire.encode(ClientEvent.StreamAnswer("ses_1", "v=0\r\n")),
        )
    }

    @Test
    fun aTrickledCandidateHasNoSdpMid() {
        val line = "candidate:1 1 udp 2122260223 10.0.2.15 54321 typ host generation 0"
        assertEquals(
            """{"type":"stream.ice","session":"ses_1","candidate":"$line"}""",
            StreamWire.encode(ClientEvent.StreamIce("ses_1", IceCandidates.toWire("a=$line"))),
        )
    }

    @Test
    fun endOfCandidatesIsAnEmptyCandidate() {
        assertEquals(
            """{"type":"stream.ice","session":"ses_1","candidate":""}""",
            StreamWire.encode(ClientEvent.StreamIce("ses_1", IceCandidates.END_OF_CANDIDATES)),
        )
    }

    @Test
    fun pauseResumeAndResizeAreSessionScoped() {
        assertEquals("""{"type":"stream.pause","session":"ses_1"}""", StreamWire.encode(ClientEvent.StreamPause("ses_1")))
        assertEquals("""{"type":"stream.resume","session":"ses_1"}""", StreamWire.encode(ClientEvent.StreamResume("ses_1")))
        assertEquals(
            """{"type":"stream.resize","session":"ses_1","width":1080,"height":2280}""",
            StreamWire.encode(ClientEvent.StreamResize("ses_1", 1080, 2280)),
        )
    }

    // control channel ------------------------------------------------------------------------------

    @Test
    fun controlMessagesFromThePhoneMatchTheHost() {
        assertEquals("""{"t":"ping","seq":7}""", StreamWire.encode(ControlMessage.Ping(7)))
        assertEquals("""{"t":"pause"}""", StreamWire.encode(ControlMessage.Pause))
        assertEquals("""{"t":"resume"}""", StreamWire.encode(ControlMessage.Resume))
        assertEquals("""{"t":"keyframe"}""", StreamWire.encode(ControlMessage.Keyframe))
    }

    @Test
    fun bringToFrontMatchesTheHost() {
        // Host: `gander-stream/src/input.rs`, `is_front_request` and `front_result_text`.
        assertEquals("""{"t":"front"}""", StreamWire.encode(ControlMessage.Front))
        assertEquals(ControlMessage.FrontResult(true), StreamWire.parseControl("""{"acquired":true,"t":"front.result"}"""))
        assertEquals(ControlMessage.FrontResult(false), StreamWire.parseControl("""{"t":"front.result","acquired":false}"""))
    }

    @Test
    fun controlMessagesFromTheHostAreParsed() {
        assertEquals(ControlMessage.Pong(7), StreamWire.parseControl("""{"t":"pong","seq":7}"""))
        assertEquals(
            ControlMessage.State(StreamState.PAUSED, StreamReason.NETWORK),
            StreamWire.parseControl("""{"t":"state","state":"paused","reason":"network"}"""),
        )
        assertEquals(
            ControlMessage.State(StreamState.STREAMING, null),
            StreamWire.parseControl("""{"t":"state","state":"streaming","reason":null}"""),
        )
        assertEquals(
            "a state without a reason (older hosts) is fine",
            ControlMessage.State(StreamState.STREAMING, null),
            StreamWire.parseControl("""{"t":"state","state":"streaming"}"""),
        )
    }

    @Test
    fun anUnknownReasonDoesNotBreakTheMessage() {
        val parsed = StreamWire.parseControl("""{"t":"state","state":"paused","reason":"battery_low"}""")
        assertEquals(ControlMessage.State(StreamState.PAUSED, StreamReason.UNKNOWN), parsed)
    }

    @Test
    fun whatTheViewerDoesNotKnowIsIgnoredNotFatal() {
        assertNull(StreamWire.parseControl("""{"t":"future.thing","x":1}"""))
        assertNull(StreamWire.parseControl("not json"))
        assertNull(StreamWire.parseControl("""{"seq":1}"""))
        assertNull(StreamWire.parseControl(""))
    }

    // input messages ----------------------------------------------------------------------------------

    @Test
    fun inputMessagesHaveTheProtocolShapes() {
        assertEquals("""{"t":"move","x":0.4321,"y":0.7788}""", StreamWire.encode(InputMessage.Move(0.4321, 0.7788)))
        assertEquals(
            """{"t":"down","b":"left","x":0.5,"y":0.25}""",
            StreamWire.encode(InputMessage.Down(MouseButton.LEFT, 0.5, 0.25)),
        )
        assertEquals(
            """{"t":"up","b":"right","x":0.5,"y":0.25}""",
            StreamWire.encode(InputMessage.Up(MouseButton.RIGHT, 0.5, 0.25)),
        )
        assertEquals(
            """{"t":"scroll","dx":0,"dy":-120,"x":0.5,"y":0.5}""",
            StreamWire.encode(InputMessage.Scroll(0, -120, 0.5, 0.5)),
        )
        assertEquals(
            """{"t":"key","code":"KeyZ","down":true,"mods":["ctrl"]}""",
            StreamWire.encode(InputMessage.Key("KeyZ", true, listOf(KeyModifier.CTRL))),
        )
        assertEquals(
            """{"t":"key","code":"Escape","down":false,"mods":[]}""",
            StreamWire.encode(InputMessage.Key("Escape", false, emptyList())),
        )
        assertEquals("""{"t":"text","s":"typed locally"}""", StreamWire.encode(InputMessage.Text("typed locally")))
    }

    @Test
    fun clicksAndKeysTravelOnTheReliableChannelAndPointerMotionOnTheLossyOne() {
        assertEquals(InputChannel.INPUT, StreamWire.channelFor(InputMessage.Move(0.1, 0.1)))
        assertEquals(InputChannel.INPUT, StreamWire.channelFor(InputMessage.Scroll(0, 120, 0.1, 0.1)))
        // A lost `up` is a stuck button: everything discrete is reliable and ordered.
        assertEquals(InputChannel.CONTROL, StreamWire.channelFor(InputMessage.Down(MouseButton.LEFT, 0.1, 0.1)))
        assertEquals(InputChannel.CONTROL, StreamWire.channelFor(InputMessage.Up(MouseButton.LEFT, 0.1, 0.1)))
        assertEquals(InputChannel.CONTROL, StreamWire.channelFor(InputMessage.Key("KeyA", true, emptyList())))
        assertEquals(InputChannel.CONTROL, StreamWire.channelFor(InputMessage.Text("a")))
        assertEquals(InputChannel.CONTROL, StreamWire.channelFor(InputMessage.ClipboardGet))
        assertEquals(InputChannel.CONTROL, StreamWire.channelFor(InputMessage.Clipboard("x")))
        assertEquals("input", InputChannel.INPUT.label)
        assertEquals("control", InputChannel.CONTROL.label)
    }

    // Clipboard (WP-56) ----------------------------------------------------------------------------------

    @Test
    fun theClipboardRequestsHaveTheShapesTheHostCrateParses() {
        // Pinned by `gander-core/src/clipboard.rs` (`requests_decode_and_everything_else_is_not_ours`).
        assertEquals("""{"t":"clipboard.get"}""", StreamWire.encode(InputMessage.ClipboardGet))
        assertEquals(
            """{"t":"clipboard","s":"line one\nline \"two\""}""",
            StreamWire.encode(InputMessage.Clipboard("line one\nline \"two\"")),
        )
    }

    @Test
    fun theHostsClipboardAnswersParse() {
        // The strings `Reply::to_text` produces (`replies_have_the_documented_shapes`).
        assertEquals(
            ControlMessage.ClipboardText("a\"b\n"),
            StreamWire.parseControl("""{"t":"clipboard","s":"a\"b\n"}"""),
        )
        assertEquals(ControlMessage.ClipboardAck(12), StreamWire.parseControl("""{"t":"clipboard.ack","bytes":12}"""))
        assertEquals(
            ControlMessage.ClipboardRefused(ClipboardOp.SET, ClipboardRefusal.TOO_LARGE),
            StreamWire.parseControl("""{"t":"clipboard.refused","op":"set","reason":"too_large"}"""),
        )
        assertEquals(
            ControlMessage.ClipboardRefused(ClipboardOp.GET, ClipboardRefusal.HOST_LOCKED),
            StreamWire.parseControl("""{"t":"clipboard.refused","op":"get","reason":"host_locked"}"""),
        )
    }

    @Test
    fun everyRefusalCodeTheHostSendsIsKnown() {
        val codes = listOf(
            "disabled" to ClipboardRefusal.DISABLED,
            "host_locked" to ClipboardRefusal.HOST_LOCKED,
            "phone_background" to ClipboardRefusal.PHONE_BACKGROUND,
            "focus_lost" to ClipboardRefusal.FOCUS_LOST,
            "local_user_active" to ClipboardRefusal.LOCAL_USER_ACTIVE,
            "too_large" to ClipboardRefusal.TOO_LARGE,
            "no_text" to ClipboardRefusal.NO_TEXT,
            "invalid_text" to ClipboardRefusal.INVALID_TEXT,
            "busy" to ClipboardRefusal.BUSY,
            "unavailable" to ClipboardRefusal.UNAVAILABLE,
        )
        for ((wire, expected) in codes) {
            val parsed = StreamWire.parseControl("""{"t":"clipboard.refused","op":"get","reason":"$wire"}""")
            assertEquals(wire, ControlMessage.ClipboardRefused(ClipboardOp.GET, expected), parsed)
        }
        // A newer host's code is tolerated, not a parse failure.
        assertEquals(
            ControlMessage.ClipboardRefused(ClipboardOp.GET, ClipboardRefusal.UNKNOWN),
            StreamWire.parseControl("""{"t":"clipboard.refused","op":"get","reason":"on_fire"}"""),
        )
    }

    // Sound (WP-55) -------------------------------------------------------------------------------------

    @Test
    fun muteAndAudioMatchTheHostsShapes() {
        assertEquals("""{"t":"mute","on":true}""", StreamWire.encode(ControlMessage.Mute(true)))
        assertEquals(ControlMessage.Audio(on = true), StreamWire.parseControl("""{"t":"audio","on":true,"reason":null}"""))
        assertEquals(
            ControlMessage.Audio(on = false, reason = "muted"),
            StreamWire.parseControl("""{"t":"audio","on":false,"reason":"muted"}"""),
        )
        assertTrue(AudioReason.hasSound(ControlMessage.Audio(on = false, reason = AudioReason.PAUSED)))
        assertFalse(AudioReason.hasSound(ControlMessage.Audio(on = false, reason = AudioReason.UNAVAILABLE)))
        assertFalse(AudioReason.hasSound(ControlMessage.Audio(on = false, reason = "something_new")))
    }

    // ICE ------------------------------------------------------------------------------------------------

    @Test
    fun candidatesLoseTheAttributePrefixOnTheWayOutAndGainTheirsOnTheWayIn() {
        assertEquals("candidate:1 1 udp 1 1.2.3.4 5 typ host", IceCandidates.toWire("  a=candidate:1 1 udp 1 1.2.3.4 5 typ host\r\n"))
        assertEquals("candidate:1 1 udp 1 1.2.3.4 5 typ host", IceCandidates.fromWire("candidate:1 1 udp 1 1.2.3.4 5 typ host"))
        assertEquals("candidate:1 1 udp 1 1.2.3.4 5 typ host", IceCandidates.fromWire("a=candidate:1 1 udp 1 1.2.3.4 5 typ host"))
        assertEquals("candidate:1 1 udp 1 1.2.3.4 5 typ host", IceCandidates.fromWire("1 1 udp 1 1.2.3.4 5 typ host"))
    }

    @Test
    fun endOfCandidatesAndBlanksAreNotCandidates() {
        assertNull(IceCandidates.fromWire(""))
        assertNull(IceCandidates.fromWire("   "))
        assertNull(IceCandidates.fromWire(IceCandidates.END_OF_CANDIDATES))
    }

    @Test
    fun trickledCandidatesBelongToTheFirstMediaSection() {
        val offer = "v=0\r\nm=video 9 UDP/TLS/RTP/SAVPF 102\r\na=mid:0\r\na=sendonly\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\na=mid:1\r\n"
        assertEquals("0", IceCandidates.firstMid(offer))
        assertEquals("video", IceCandidates.firstMid("v=0\r\nm=video 9 X\r\na=mid:video\r\n"))
        assertEquals("a missing mid defaults to 0 like the host assumes", "0", IceCandidates.firstMid("v=0\r\n"))
    }
}
