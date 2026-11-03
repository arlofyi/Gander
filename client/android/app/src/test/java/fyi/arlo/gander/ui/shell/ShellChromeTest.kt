package fyi.arlo.gander.ui.shell

import fyi.arlo.gander.ui.navigation.Routes
import fyi.arlo.gander.ui.preview.PreviewRoutes
import fyi.arlo.gander.ui.stream.StreamRoutes
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** Which navigation chrome each screen gets (owner decision: bottom navigation, rail on tablets). */
class ShellChromeTest {
    @Test
    fun topLevelScreensShowTheBottomBar() {
        listOf(Routes.HOSTS, Routes.SHARES, Routes.DOWNLOADS, Routes.SETTINGS).forEach { route ->
            assertTrue(route, shellChrome(route).bottomBar)
        }
    }

    @Test
    fun deeperScreensHideIt() {
        listOf(Routes.BROWSER, Routes.ADD_HOST, Routes.STORAGE, Routes.HOST_DETAILS, PreviewRoutes.PREVIEW, StreamRoutes.STREAM)
            .forEach { route -> assertFalse(route, shellChrome(route).bottomBar) }
    }

    @Test
    fun theSelectedDestinationFollowsTheSection() {
        assertEquals(TopDestination.COMPUTERS, shellChrome(Routes.BROWSER).selected)
        assertEquals(TopDestination.COMPUTERS, shellChrome(Routes.HOSTS).selected)
        assertEquals(TopDestination.SETTINGS, shellChrome(Routes.STORAGE).selected)
        assertEquals(TopDestination.DOWNLOADS, shellChrome(Routes.DOWNLOADS).selected)
    }

    @Test
    fun viewersAndTheStreamAreImmersiveOnTablets() {
        listOf(PreviewRoutes.PREVIEW, StreamRoutes.STREAM).forEach { route ->
            val chrome = shellChrome(route)
            assertFalse(chrome.rail)
            assertNull(chrome.selected)
            assertFalse(chrome.readableWidth)
        }
        assertTrue(shellChrome(Routes.BROWSER).rail)
        assertFalse("the two-pane browser uses the full width", shellChrome(Routes.BROWSER).readableWidth)
        assertTrue(shellChrome(Routes.SETTINGS).readableWidth)
    }
}
