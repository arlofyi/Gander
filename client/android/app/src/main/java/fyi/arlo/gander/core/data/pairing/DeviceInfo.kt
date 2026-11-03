package fyi.arlo.gander.core.data.pairing

import android.content.Context
import android.os.Build
import android.provider.Settings
import fyi.arlo.gander.core.net.protocol.PairingDevice

/**
 * What this phone tells the computer about itself in `POST /v1/pair`; the computer shows it in
 * the approval dialog ("Pixel 9 wants to pair"). The host accepts 1..64 characters, no control
 * characters, per field.
 */
object DeviceInfo {
    /** Longest field the host accepts (`MAX_DEVICE_FIELD` on the host). */
    const val MAX_FIELD_CHARS = 64

    /**
     * [raw] without control characters and surrounding whitespace, cut to [MAX_FIELD_CHARS]
     * characters (never in the middle of a surrogate pair); [fallback] when nothing is left.
     */
    fun sanitize(raw: String?, fallback: String): String {
        val cleaned = raw.orEmpty().filterNot { it.isISOControl() }.trim()
        if (cleaned.isEmpty()) return fallback
        if (cleaned.codePointCount(0, cleaned.length) <= MAX_FIELD_CHARS) return cleaned
        val end = cleaned.offsetByCodePoints(0, MAX_FIELD_CHARS)
        return cleaned.substring(0, end).trim().ifEmpty { fallback }
    }

    /** The device as the host should see it. [userName] is the name the owner gave the phone, if any. */
    fun describe(userName: String?, model: String?, appVersion: String): PairingDevice {
        val cleanModel = sanitize(model, "Android device")
        return PairingDevice(
            name = sanitize(userName, cleanModel),
            // Same value as the protocol examples; the host has no OS version field.
            platform = "android",
            model = cleanModel,
            appVersion = sanitize(appVersion, "unknown"),
        )
    }

    /** Reads the owner's device name (Settings, "About phone") and the build model. */
    fun fromSystem(context: Context, appVersion: String): PairingDevice {
        val userName = try {
            Settings.Global.getString(context.contentResolver, Settings.Global.DEVICE_NAME)
        } catch (_: SecurityException) {
            null
        }
        return describe(userName, Build.MODEL, appVersion)
    }
}
