package fyi.arlo.gander

import org.junit.Assert.assertEquals
import org.junit.Test

/** Smoke test that the unit-test source set is wired and the package id is the agreed one. */
class ApplicationIdTest {
    @Test
    fun applicationIdIsFyiArloGander() {
        assertEquals("fyi.arlo.gander", BuildConfig.APPLICATION_ID)
    }
}
