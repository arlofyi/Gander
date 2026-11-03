package fyi.arlo.gander.ui.fetch

import org.junit.Assert.assertEquals
import org.junit.Test

class DeviceSaveTest {
    @Test
    fun theSavedCopyKeepsTheFileNameAndNeverAPath() {
        assertEquals("photo.png", DeviceSave.displayName("photo.png"))
        assertEquals("photo.png", DeviceSave.displayName("a/b/photo.png"))
        assertEquals("photo.png", DeviceSave.displayName("..\\photo.png"))
        assertEquals("download", DeviceSave.displayName("  "))
    }

    @Test
    fun copiesGoToTheirOwnFolderInDownloads() {
        assertEquals("Download/Gander", DeviceSave.relativePath)
    }
}
