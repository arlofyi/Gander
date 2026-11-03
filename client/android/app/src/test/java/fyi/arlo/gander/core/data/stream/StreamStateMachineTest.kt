package fyi.arlo.gander.core.data.stream

import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.protocol.NetworkAdapter
import fyi.arlo.gander.core.net.protocol.RelayPath
import fyi.arlo.gander.core.net.protocol.StreamReason
import fyi.arlo.gander.core.net.protocol.StreamState
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The viewer's phase and the liveness rule: ping every second, a quiet host (more than 5 s
 * without a pong or state) is "reconnecting", pause on background and resume on return, and what
 * happens when the link fails.
 */
class StreamStateMachineTest {
    private val m = StreamStateMachine()

    /** A session that is streaming, with the control channel open at [at]. */
    private fun streaming(at: Long = 0L) {
        m.onStarting()
        m.onHostState(StreamState.REQUESTED, null, null, at)
        m.onHostState(StreamState.WINDOW_FOUND, null, null, at)
        m.onOffer(at)
        m.onHostState(StreamState.STREAMING, null, null, at)
        m.onPeer(PeerLink.UP, at)
        m.onControlOpen(at)
        assertEquals(StreamPhase.Streaming, m.phase)
    }

    private fun pings(effects: List<StreamEffect>) = effects.filterIsInstance<StreamEffect.SendPing>().map { it.seq }

    // The phases of the host ------------------------------------------------------------------------

    @Test
    fun thePhaseFollowsWhatTheHostSays() {
        assertEquals(StreamPhase.Preparing, m.phase)
        m.onStarting()
        assertEquals(StreamPhase.Starting, m.phase)
        m.onHostState(StreamState.REQUESTED, null, null, 0)
        assertEquals(StreamPhase.Requested, m.phase)
        m.onHostState(StreamState.APPROVING, null, null, 0)
        assertEquals(StreamPhase.Approving, m.phase)
        m.onHostState(StreamState.LAUNCHING, null, null, 0)
        assertEquals(StreamPhase.Launching, m.phase)
        m.onHostState(StreamState.WINDOW_FOUND, null, null, 0)
        assertEquals(StreamPhase.WindowFound, m.phase)
        m.onOffer(10)
        m.onHostState(StreamState.WINDOW_FOUND, null, null, 10)
        assertEquals("the offer arrived: the link is being set up", StreamPhase.Connecting, m.phase)
    }

    @Test
    fun theHostStreamingIsOnlyStreamingOnceTheLinkIsUp() {
        m.onStarting()
        m.onOffer(0)
        m.onHostState(StreamState.STREAMING, null, null, 10)
        assertEquals(StreamPhase.Connecting, m.phase)
        m.onPeer(PeerLink.UP, 20)
        assertEquals(StreamPhase.Streaming, m.phase)
    }

    @Test
    fun aPickCarriesTheCandidateWindows() {
        m.onStarting()
        m.onHostState(StreamState.PICK, null, listOf("Deck.pptx - PowerPoint", "PowerPoint"), 0)
        assertEquals(StreamPhase.PickWindow(listOf("Deck.pptx - PowerPoint", "PowerPoint")), m.phase)
    }

    private val titles = listOf("Deck.pptx - PowerPoint", "PowerPoint")

    private fun picking() {
        m.onStarting()
        m.onHostState(StreamState.PICK, null, titles, 0)
    }

    @Test
    fun aChoiceInThePickIsMarkedUntilTheHostMovesOn() {
        picking()
        assertTrue(m.onPickChosen(1, 5))
        assertEquals(StreamPhase.PickWindow(titles, chosen = 1), m.phase)
        m.onHostState(StreamState.WINDOW_FOUND, null, null, 10)
        assertEquals(StreamPhase.WindowFound, m.phase)
    }

    @Test
    fun onlyAnExistingCandidateOfAPickCanBeChosen() {
        m.onStarting()
        assertFalse("no pick yet", m.onPickChosen(0, 0))
        picking()
        assertFalse(m.onPickChosen(2, 1))
        assertFalse(m.onPickChosen(-1, 1))
        assertEquals(StreamPhase.PickWindow(titles), m.phase)
        m.onHostState(StreamState.WINDOW_FOUND, null, null, 2)
        assertFalse("the host has its window", m.onPickChosen(0, 3))
    }

    @Test
    fun aWindowThatIsGoneStartsThePickOver() {
        picking()
        m.onPickChosen(0, 1)
        m.onHostAppUnavailable(2)
        assertEquals(StreamPhase.PickWindow(titles, chosen = null, windowGone = true), m.phase)
        assertTrue("the user can choose again", m.onPickChosen(1, 3))
        assertEquals("the notice is gone once they do", StreamPhase.PickWindow(titles, chosen = 1), m.phase)
    }

    @Test
    fun aRefusedPickCanBeChosenAgain() {
        picking()
        m.onPickChosen(0, 1)
        m.onPickRefused(2)
        assertEquals(StreamPhase.PickWindow(titles), m.phase)
    }

    @Test
    fun aPickTheHostGivesUpOnEndsAsNoWindow() {
        picking()
        m.onHostAppUnavailable(30_000)
        m.onHostState(StreamState.ENDED, StreamReason.TIMEOUT, null, 30_001)
        assertEquals(StreamPhase.Ended(EndCause.APP_UNAVAILABLE), m.phase)
    }

    @Test
    fun anAppThatShowedNoWindowEndsAsNoWindowToo() {
        m.onStarting()
        m.onHostState(StreamState.LAUNCHING, null, null, 0)
        m.onHostAppUnavailable(1)
        m.onHostState(StreamState.ENDED, StreamReason.TIMEOUT, null, 2)
        assertEquals(StreamPhase.Ended(EndCause.APP_UNAVAILABLE), m.phase)
    }

    @Test
    fun aTimeoutAfterAPickThatWorkedIsStillATimeout() {
        picking()
        m.onHostAppUnavailable(1)
        m.onHostState(StreamState.WINDOW_FOUND, null, null, 2)
        m.onHostState(StreamState.ENDED, StreamReason.TIMEOUT, null, 3)
        assertEquals(StreamPhase.Ended(EndCause.TIMEOUT), m.phase)
    }

    @Test
    fun theHostsPauseReasonsAreKept() {
        streaming()
        for (reason in listOf(StreamReason.FOCUS_LOST, StreamReason.LOCAL_USER_ACTIVE, StreamReason.NETWORK)) {
            m.onHostState(StreamState.PAUSED, reason, null, 10)
            assertEquals(StreamPhase.Paused(reason), m.phase)
            m.onHostState(StreamState.STREAMING, null, null, 20)
            assertEquals(StreamPhase.Streaming, m.phase)
        }
    }

    @Test
    fun theControlChannelMirrorsTheHostsState() {
        streaming()
        m.onControlMessage(ControlMessage.State(StreamState.PAUSED, StreamReason.FOCUS_LOST), 100)
        assertEquals(StreamPhase.Paused(StreamReason.FOCUS_LOST), m.phase)
        m.onControlMessage(ControlMessage.State(StreamState.STREAMING, null), 200)
        assertEquals(StreamPhase.Streaming, m.phase)
    }

    // Liveness -----------------------------------------------------------------------------------------

    @Test
    fun theViewerPingsEverySecondOnceTheControlChannelIsOpen() {
        m.onStarting()
        m.onOffer(0)
        assertTrue("nothing to ping before the channel opens", m.tick(500).isEmpty())
        m.onPeer(PeerLink.UP, 1000)
        m.onHostState(StreamState.STREAMING, null, null, 1000)
        m.onControlOpen(1000)
        assertEquals(listOf(1L), pings(m.tick(1000)))
        assertTrue(m.tick(1500).isEmpty())
        assertTrue(m.tick(1999).isEmpty())
        assertEquals(listOf(2L), pings(m.tick(2000)))
        assertEquals(listOf(3L), pings(m.tick(3050)))
        assertTrue("a late tick does not send two", m.tick(3060).isEmpty())
    }

    @Test
    fun aHostThatKeepsAnsweringIsNeverQuiet() {
        streaming(0)
        var seq = 0L
        for (t in 0..60_000L step 250) {
            for (ping in pings(m.tick(t))) {
                seq = ping
                m.onControlMessage(ControlMessage.Pong(ping), t + 20)
            }
        }
        assertTrue(seq >= 60)
        assertEquals(StreamPhase.Streaming, m.phase)
    }

    @Test
    fun moreThanFiveSecondsOfSilenceIsReconnecting() {
        streaming(1000)
        m.tick(1000)
        m.tick(6000) // exactly five seconds: still fine
        assertEquals(StreamPhase.Streaming, m.phase)
        m.tick(6001)
        assertEquals(StreamPhase.Reconnecting, m.phase)
    }

    @Test
    fun theViewerKeepsPingingWhileTheHostIsQuietSoItCanRecover() {
        streaming(1000)
        m.tick(1000)
        assertEquals(listOf(2L), pings(m.tick(6000)))
        assertEquals(listOf(3L), pings(m.tick(7000)))
        assertEquals(StreamPhase.Reconnecting, m.phase)
    }

    @Test
    fun anyMessageFromTheHostEndsTheQuietAndAsksForAFreshPicture() {
        streaming(1000)
        m.tick(1000)
        m.tick(7000)
        assertEquals(StreamPhase.Reconnecting, m.phase)
        val effects = m.onControlMessage(ControlMessage.Pong(1), 7500)
        assertEquals(StreamPhase.Streaming, m.phase)
        assertEquals("the picture may be broken after the gap", listOf(StreamEffect.RequestKeyframe), effects)
    }

    // Background -----------------------------------------------------------------------------------------

    @Test
    fun goingToTheBackgroundPausesTheHostAndStopsThePings() {
        streaming(0)
        m.tick(0)
        val effects = m.onBackground(1000)
        assertEquals(listOf<StreamEffect>(StreamEffect.SendPause), effects)
        assertEquals(StreamPhase.Backgrounded, m.phase)
        assertTrue("no pings while away", m.tick(2000).isEmpty())
        assertTrue(m.tick(30_000).isEmpty())
        assertEquals("a host that cannot answer is not news while nobody looks", StreamPhase.Backgrounded, m.phase)
        assertTrue("saying it twice does nothing", m.onBackground(31_000).isEmpty())
    }

    @Test
    fun comingBackResumesTheHostAndDoesNotCountTheTimeAwayAsSilence() {
        streaming(0)
        m.onBackground(1000)
        val effects = m.onForeground(90_000)
        assertEquals(listOf<StreamEffect>(StreamEffect.SendResume), effects)
        assertEquals(StreamPhase.Streaming, m.phase)
        assertEquals("the pings start again at once", listOf(1L), pings(m.tick(90_000)))
        assertEquals(StreamPhase.Streaming, m.phase)
        m.tick(95_000)
        assertEquals("five seconds of quiet are counted from the return", StreamPhase.Streaming, m.phase)
        m.tick(95_001)
        assertEquals(StreamPhase.Reconnecting, m.phase)
    }

    @Test
    fun comingBackWithoutHavingLeftDoesNothing() {
        streaming(0)
        assertTrue(m.onForeground(500).isEmpty())
    }

    @Test
    fun theHostsPhoneBackgroundEchoDoesNotLeaveTheViewerPausedAfterTheReturn() {
        streaming(0)
        m.onBackground(1000)
        m.onHostState(StreamState.PAUSED, StreamReason.PHONE_BACKGROUND, null, 1100)
        assertEquals("this phone knows why", StreamPhase.Backgrounded, m.phase)
        m.onForeground(5000)
        assertEquals(StreamPhase.Paused(StreamReason.PHONE_BACKGROUND), m.phase)
        m.onControlMessage(ControlMessage.State(StreamState.STREAMING, null), 5100)
        assertEquals(StreamPhase.Streaming, m.phase)
    }

    // The link -----------------------------------------------------------------------------------------------

    @Test
    fun aDroppedLinkIsReconnectingAndComesBackOnItsOwn() {
        streaming(0)
        assertTrue(m.onPeer(PeerLink.DOWN, 1000).isEmpty())
        assertEquals(StreamPhase.Reconnecting, m.phase)
        val back = m.onPeer(PeerLink.UP, 2000)
        assertEquals(StreamPhase.Streaming, m.phase)
        assertEquals(listOf<StreamEffect>(StreamEffect.RequestKeyframe), back)
    }

    @Test
    fun aFailedLinkAsksForANewNegotiationAtOnceAndAgainEveryThirtySeconds() {
        streaming(0)
        val failed = m.onPeer(PeerLink.FAILED, 1000)
        assertEquals(listOf<StreamEffect>(StreamEffect.Reconnect), failed)
        assertEquals(StreamPhase.Reconnecting, m.phase)
        assertTrue(m.tick(30_999).none { it is StreamEffect.Reconnect })
        assertTrue(m.tick(31_000).any { it is StreamEffect.Reconnect })
        // The new peer connection replaces the old one: the clock for the next try restarts.
        m.onTransportReplaced(31_100)
        assertEquals(StreamPhase.Reconnecting, m.phase)
        assertTrue(m.tick(60_000).none { it is StreamEffect.Reconnect })
        assertTrue(m.tick(61_100).any { it is StreamEffect.Reconnect })
    }

    @Test
    fun aLinkThatNeverComesBackEndsTheStreamWhenTheHostGivesUpTheSession() {
        streaming(0)
        m.onPeer(PeerLink.FAILED, 1000)
        m.tick(120_999)
        assertEquals(StreamPhase.Reconnecting, m.phase)
        m.tick(121_000)
        assertEquals(StreamPhase.Ended(EndCause.CONNECTION_LOST), m.phase)
        assertTrue(m.isTerminal)
    }

    @Test
    fun aBackgroundedPhoneDoesNotTryToReconnect() {
        streaming(0)
        m.onBackground(100)
        m.onPeer(PeerLink.FAILED, 200)
        assertEquals(StreamPhase.Backgrounded, m.phase)
        assertTrue(m.tick(100_000).isEmpty())
    }

    @Test
    fun aLinkThatFailsBeforeEverConnectingEndsTheStreamWithNoVideoLink() {
        m.onStarting()
        m.onOffer(0)
        assertTrue(m.onPeer(PeerLink.FAILED, 500).isEmpty())
        assertEquals(StreamPhase.Ended(EndCause.NO_VIDEO_LINK), m.phase)
    }

    @Test
    fun aLinkThatNeverComesUpTimesOutTwentySecondsAfterTheOffer() {
        m.onStarting()
        m.onOffer(1000)
        m.onHostState(StreamState.STREAMING, null, null, 1500)
        m.tick(21_000)
        assertEquals(StreamPhase.Connecting, m.phase)
        m.tick(21_001)
        assertEquals(StreamPhase.Ended(EndCause.NO_VIDEO_LINK), m.phase)
    }

    // The end ---------------------------------------------------------------------------------------------------

    @Test
    fun theHostsEndReasonsBecomeTheCauseOfTheEnd() {
        val table = mapOf(
            StreamReason.CLIENT_CLOSED to EndCause.YOU_LEFT,
            StreamReason.HOST_KILL_SWITCH to EndCause.HOST_KILL_SWITCH,
            StreamReason.WINDOW_CLOSED to EndCause.WINDOW_CLOSED,
            StreamReason.REVOKED to EndCause.REVOKED,
            StreamReason.TIMEOUT to EndCause.TIMEOUT,
            StreamReason.ERROR to EndCause.ERROR,
            StreamReason.NETWORK to EndCause.CONNECTION_LOST,
            StreamReason.UNKNOWN to EndCause.ERROR,
        )
        for ((reason, cause) in table) {
            assertEquals(cause, EndCause.fromReason(reason))
            val machine = StreamStateMachine()
            machine.onHostState(StreamState.ENDED, reason, null, 0)
            assertEquals(StreamPhase.Ended(cause), machine.phase)
        }
        assertEquals(EndCause.ERROR, EndCause.fromReason(null))
    }

    @Test
    fun aLockThatEndsTheStreamIsToldApartFromAGenericError() {
        streaming(0)
        m.onHostLocked()
        m.onHostState(StreamState.ENDED, StreamReason.ERROR, null, 100)
        assertEquals(StreamPhase.Ended(EndCause.HOST_LOCKED), m.phase)
    }

    @Test
    fun anEndedStreamStaysEnded() {
        streaming(0)
        m.onHostState(StreamState.ENDED, StreamReason.WINDOW_CLOSED, null, 100)
        m.onHostState(StreamState.STREAMING, null, null, 200)
        m.onPeer(PeerLink.UP, 300)
        assertEquals(StreamPhase.Ended(EndCause.WINDOW_CLOSED), m.phase)
        assertTrue(m.tick(10_000).isEmpty())
        assertTrue(m.onBackground(11_000).isEmpty())
        assertTrue(m.onControlMessage(ControlMessage.Pong(1), 12_000).isEmpty())
    }

    @Test
    fun leavingEndsTheStreamAndStopsThePings() {
        streaming(0)
        m.onUserEnd()
        assertEquals(StreamPhase.Ended(EndCause.YOU_LEFT), m.phase)
        assertTrue(m.tick(5000).isEmpty())
    }

    @Test
    fun aRefusedRequestIsAFailureWithItsError() {
        val refusal = GanderError.Host.HostLocked(412, "locked")
        m.onStarting()
        m.onFailed(refusal)
        assertEquals(StreamPhase.Failed(refusal), m.phase)
        assertTrue(m.isTerminal)
    }

    @Test
    fun aStreamThatCannotWorkHereIsUnsupportedNotFailed() {
        m.onUnsupported(StreamPhase.Unsupported.Why.PHONE_HAS_NO_H264_DECODER)
        assertEquals(StreamPhase.Unsupported(StreamPhase.Unsupported.Why.PHONE_HAS_NO_H264_DECODER), m.phase)
    }

    @Test
    fun theRelayWarningIsAPhaseOfItsOwn() {
        m.onConfirmRelay(NetworkAdapter.TAILSCALE, RelayPath.DERP)
        assertEquals(StreamPhase.ConfirmRelay(NetworkAdapter.TAILSCALE, RelayPath.DERP), m.phase)
        m.onStarting()
        assertEquals(StreamPhase.Starting, m.phase)
    }

    // The banner ---------------------------------------------------------------------------------------------------

    @Test
    fun theBannerSaysWhatTheHostIsDoing() {
        assertEquals(StreamBanner.Requested, StreamPhase.Requested.banner())
        assertEquals(StreamBanner.Requested, StreamPhase.Starting.banner())
        assertEquals(StreamBanner.Approving, StreamPhase.Approving.banner())
        assertEquals(StreamBanner.Launching, StreamPhase.Launching.banner())
        assertEquals(StreamBanner.SettingUp, StreamPhase.WindowFound.banner())
        assertEquals(StreamBanner.SettingUp, StreamPhase.Connecting.banner())
        assertEquals(StreamBanner.Paused(StreamReason.FOCUS_LOST), StreamPhase.Paused(StreamReason.FOCUS_LOST).banner())
        assertEquals(StreamBanner.Backgrounded, StreamPhase.Backgrounded.banner())
        assertEquals(StreamBanner.Reconnecting, StreamPhase.Reconnecting.banner())
        assertEquals(StreamBanner.PickWindow(listOf("a")), StreamPhase.PickWindow(listOf("a")).banner())
    }

    @Test
    fun noBannerWhileThePictureSpeaksForItselfOrAScreenOfItsOwnIsUp() {
        assertNull(StreamPhase.Streaming.banner())
        assertNull(StreamPhase.Preparing.banner())
        assertNull(StreamPhase.Ended(EndCause.ERROR).banner())
        assertNull(StreamPhase.Failed(null).banner())
        assertNull(StreamPhase.ConfirmRelay(NetworkAdapter.GENERIC, RelayPath.PEER_RELAY).banner())
        assertNull(StreamPhase.Unsupported(StreamPhase.Unsupported.Why.HOST_DOES_NOT_STREAM_H264).banner())
    }

    // Bring to front ---------------------------------------------------------------------------------

    private fun focusLost(at: Long): List<StreamEffect> =
        m.onControlMessage(ControlMessage.State(StreamState.PAUSED, StreamReason.FOCUS_LOST), at)

    private fun fronts(effects: List<StreamEffect>) = effects.count { it == StreamEffect.SendFront }

    @Test
    fun focusLostRightAfterConnectAsksOnceByItself() {
        streaming(at = 0)
        focusLost(1_500)
        assertEquals(StreamPhase.Paused(StreamReason.FOCUS_LOST), m.phase)
        assertEquals(1, fronts(m.tick(1_750)))
        assertEquals(FrontStatus.SENDING, m.front)
        // Refused: said so, and not asked again by itself.
        m.onControlMessage(ControlMessage.FrontResult(false), 2_000)
        assertEquals(FrontStatus.REFUSED, m.front)
        assertEquals(0, fronts(m.tick(2_250)))
        m.onControlMessage(ControlMessage.State(StreamState.STREAMING, null), 2_500)
        focusLost(3_000)
        assertEquals("once per connection", 0, fronts(m.tick(3_250)))
    }

    @Test
    fun aLaterFocusLostIsLeftToTheUser() {
        streaming(at = 0)
        focusLost(StreamStateMachine.AUTO_FRONT_WINDOW_MS + 1)
        assertEquals(0, fronts(m.tick(StreamStateMachine.AUTO_FRONT_WINDOW_MS + 250)))
        assertEquals(FrontStatus.IDLE, m.front)
        // The user's tap asks.
        assertEquals(1, fronts(m.onFrontRequested(StreamStateMachine.AUTO_FRONT_WINDOW_MS + 500)))
    }

    @Test
    fun bringToFrontOnlyWhilePausedForFocusAndOneAtATime() {
        streaming(at = 0)
        assertTrue("streaming: nothing to bring forward", m.onFrontRequested(100).isEmpty())
        m.onControlMessage(ControlMessage.State(StreamState.PAUSED, StreamReason.LOCAL_USER_ACTIVE), 200)
        assertTrue(m.onFrontRequested(300).isEmpty())
        focusLost(400)
        assertEquals(1, fronts(m.onFrontRequested(500)))
        assertTrue("waiting for the answer", m.onFrontRequested(600).isEmpty())
        // The host brings it forward: the pause ends and the answer no longer matters.
        m.onControlMessage(ControlMessage.State(StreamState.STREAMING, null), 700)
        m.onControlMessage(ControlMessage.FrontResult(true), 710)
        assertEquals(StreamPhase.Streaming, m.phase)
        assertEquals(FrontStatus.IDLE, m.front)
    }

    @Test
    fun anUnansweredBringToFrontIsGivenUp() {
        streaming(at = 0)
        focusLost(StreamStateMachine.AUTO_FRONT_WINDOW_MS + 1)
        m.tick(StreamStateMachine.AUTO_FRONT_WINDOW_MS + 2)
        val at = StreamStateMachine.AUTO_FRONT_WINDOW_MS + 100
        assertEquals(1, fronts(m.onFrontRequested(at)))
        // The host is there (pongs), it just does not answer the front.
        m.onControlMessage(ControlMessage.Pong(1), at + 4_000)
        m.tick(at + StreamStateMachine.FRONT_TIMEOUT_MS - 1)
        assertEquals(FrontStatus.SENDING, m.front)
        m.tick(at + StreamStateMachine.FRONT_TIMEOUT_MS)
        assertEquals(FrontStatus.IDLE, m.front)
        assertEquals(1, fronts(m.onFrontRequested(at + StreamStateMachine.FRONT_TIMEOUT_MS + 1)))
    }
}
