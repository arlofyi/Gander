package fyi.arlo.gander.ui.filesheet

import java.time.DateTimeException
import java.time.Instant
import java.time.LocalDateTime
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle
import java.util.Locale

// Formatting of the file sheet's detail rows. Plain functions, covered by JVM unit tests.

/** A localised date and time of an ISO-8601 [iso] instant in [zone]; empty if [iso] is null or unreadable. */
fun formatDateTime(
    iso: String?,
    zone: ZoneId = ZoneId.systemDefault(),
    locale: Locale = Locale.getDefault(),
): String {
    if (iso == null) return ""
    return try {
        DateTimeFormatter.ofLocalizedDateTime(FormatStyle.MEDIUM, FormatStyle.SHORT).withLocale(locale).withZone(zone)
            .format(Instant.parse(iso))
    } catch (_: DateTimeException) {
        ""
    }
}

private val EXIF_TIME: DateTimeFormatter = DateTimeFormatter.ofPattern("yyyy:MM:dd HH:mm:ss", Locale.ROOT)

/**
 * The EXIF `taken_at` as the phone's locale shows dates (`2026:10:28 18:40:01` becomes a medium
 * date and short time). EXIF times carry no zone, so none is applied. Text that is not in the EXIF
 * layout is returned as it came.
 */
fun formatExifTime(raw: String?, locale: Locale = Locale.getDefault()): String {
    if (raw.isNullOrBlank()) return ""
    return try {
        DateTimeFormatter.ofLocalizedDateTime(FormatStyle.MEDIUM, FormatStyle.SHORT).withLocale(locale)
            .format(LocalDateTime.parse(raw.trim(), EXIF_TIME))
    } catch (_: DateTimeException) {
        raw.trim()
    }
}

/** `3:05` for 185 seconds, `1:02:03` from an hour up. Negative values read as zero. */
fun formatDuration(durationMs: Long): String {
    val totalSeconds = (durationMs.coerceAtLeast(0) + 500) / 1000
    val hours = totalSeconds / 3600
    val minutes = totalSeconds % 3600 / 60
    val seconds = totalSeconds % 60
    return if (hours > 0) {
        String.format(Locale.ROOT, "%d:%02d:%02d", hours, minutes, seconds)
    } else {
        String.format(Locale.ROOT, "%d:%02d", minutes, seconds)
    }
}
