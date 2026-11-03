package fyi.arlo.gander.ui.stream

import fyi.arlo.gander.core.data.stream.ClipboardOp
import fyi.arlo.gander.core.data.stream.ClipboardOutcome
import fyi.arlo.gander.core.data.stream.ClipboardRefusal
import fyi.arlo.gander.core.net.protocol.ClipboardMode
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** What the viewer says after a clipboard action, and which actions the keyboard bar offers (WP-56). */
class ClipboardNoticeTest {
    private fun refused(op: ClipboardOp, reason: ClipboardRefusal) = ClipboardNotice.of(ClipboardOutcome.Refused(op, reason))

    @Test
    fun successIsSaidInOneShortMessage() {
        assertEquals(ClipboardNotice.COPIED, ClipboardNotice.of(ClipboardOutcome.Copied("text")))
        assertEquals(ClipboardNotice.PASTED, ClipboardNotice.of(ClipboardOutcome.Pasted(4)))
        assertEquals(ClipboardNotice.NO_ANSWER, ClipboardNotice.of(ClipboardOutcome.NoAnswer(ClipboardOp.GET)))
    }

    @Test
    fun everyRefusalHasAMessage() {
        for (op in listOf(ClipboardOp.GET, ClipboardOp.SET)) {
            for (reason in ClipboardRefusal.entries) {
                // `when` is exhaustive, so a new reason cannot go unexplained; this pins that none throws.
                refused(op, reason)
            }
        }
        assertEquals(ClipboardNotice.DISABLED, refused(ClipboardOp.SET, ClipboardRefusal.DISABLED))
        assertEquals(ClipboardNotice.LOCKED, refused(ClipboardOp.GET, ClipboardRefusal.HOST_LOCKED))
        assertEquals(ClipboardNotice.TOO_LARGE, refused(ClipboardOp.SET, ClipboardRefusal.TOO_LARGE))
        assertEquals(ClipboardNotice.BUSY, refused(ClipboardOp.GET, ClipboardRefusal.BUSY))
        assertEquals(ClipboardNotice.FAILED, refused(ClipboardOp.GET, ClipboardRefusal.UNAVAILABLE))
    }

    @Test
    fun aStreamThatIsNotActiveIsOneMessage() {
        for (reason in listOf(ClipboardRefusal.PHONE_BACKGROUND, ClipboardRefusal.FOCUS_LOST, ClipboardRefusal.LOCAL_USER_ACTIVE)) {
            assertEquals(ClipboardNotice.NOT_ACTIVE, refused(ClipboardOp.GET, reason))
        }
    }

    @Test
    fun noTextIsTheSideThatHadNone() {
        assertEquals(ClipboardNotice.PHONE_CLIPBOARD_EMPTY, refused(ClipboardOp.SET, ClipboardRefusal.NO_TEXT))
        assertEquals(ClipboardNotice.COMPUTER_HAS_NO_TEXT, refused(ClipboardOp.GET, ClipboardRefusal.NO_TEXT))
    }

    @Test
    fun theBarOffersOnlyTheActionsTheOwnerAllowed() {
        assertFalse(ClipboardMode.OFF.canPasteToComputer || ClipboardMode.OFF.canCopyFromComputer)
        assertTrue(ClipboardMode.TO_PHONE.canCopyFromComputer && !ClipboardMode.TO_PHONE.canPasteToComputer)
        assertTrue(ClipboardMode.FROM_PHONE.canPasteToComputer && !ClipboardMode.FROM_PHONE.canCopyFromComputer)
        assertTrue(ClipboardMode.BOTH.canPasteToComputer && ClipboardMode.BOTH.canCopyFromComputer)
    }
}
