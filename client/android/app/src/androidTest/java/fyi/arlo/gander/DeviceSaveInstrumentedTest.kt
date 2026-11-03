package fyi.arlo.gander

import android.os.Build
import android.provider.MediaStore
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import fyi.arlo.gander.ui.fetch.DeviceSave
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File
import java.util.UUID

/**
 * "Save to device" puts a fetched file where the rest of the phone can see it (found on a real
 * tablet: a fetched file stayed in app-private storage and "never downloaded to the device").
 */
@RunWith(AndroidJUnit4::class)
class DeviceSaveInstrumentedTest {
    private val context = InstrumentationRegistry.getInstrumentation().targetContext

    @Test
    fun aFetchedFileIsCopiedIntoDownloadsGander() = runBlocking {
        val bytes = ByteArray(4096) { (it * 31).toByte() }
        val name = "gander-save-test-${UUID.randomUUID()}.bin"
        val source = File(context.cacheDir, name).apply { writeBytes(bytes) }
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) {
            // Android 8 and 9 ask where to save (ACTION_CREATE_DOCUMENT); nothing to check here.
            source.delete()
            return@runBlocking
        }
        val resolver = context.contentResolver
        val uri = DeviceSave.toDownloads(resolver, source, name, "application/octet-stream")
        try {
            val copied = resolver.openInputStream(uri)!!.use { it.readBytes() }
            assertArrayEquals(bytes, copied)
            resolver.query(uri, arrayOf(MediaStore.Downloads.RELATIVE_PATH, MediaStore.Downloads.IS_PENDING), null, null, null)!!.use { c ->
                c.moveToFirst()
                assertEquals("Download/Gander/", c.getString(0))
                assertEquals("published, not pending", 0, c.getInt(1))
            }
        } finally {
            resolver.delete(uri, null, null)
            source.delete()
        }
    }
}
