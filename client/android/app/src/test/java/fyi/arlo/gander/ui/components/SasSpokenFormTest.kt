package fyi.arlo.gander.ui.components

import org.junit.Assert.assertEquals
import org.junit.Test

/** The pairing code is announced digit by digit with a pause between groups (design document section 11). */
class SasSpokenFormTest {
    @Test
    fun groupsAreSpokenDigitByDigitWithAPause() {
        assertEquals("4 8 2, 9 1 3", sasSpokenForm("482 913"))
    }

    @Test
    fun extraSpacesDoNotAddGroups() {
        assertEquals("0 7 9, 5 1 5", sasSpokenForm("  079   515 "))
    }

    @Test
    fun aSingleGroupHasNoPause() {
        assertEquals("1 2 3 4 5 6", sasSpokenForm("123456"))
    }
}
