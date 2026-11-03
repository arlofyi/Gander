package fyi.arlo.gander.ui.browser

import fyi.arlo.gander.core.net.protocol.Entry
import fyi.arlo.gander.core.net.protocol.EntryFlag
import fyi.arlo.gander.core.net.protocol.EntryKind
import fyi.arlo.gander.core.net.protocol.PreviewKind
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle
import java.util.Locale

/** The placeholder glyph of an entry until thumbnails arrive (WP-24 draws the thumbnail over it). */
enum class GlyphKind { FOLDER, IMAGE, PDF, TEXT, OFFICE, VIDEO, AUDIO, ARCHIVE, FILE }

private val IMAGE_EXT = setOf("jpg", "jpeg", "png", "gif", "webp", "bmp", "heic", "heif", "avif", "tif", "tiff", "svg", "raw", "dng", "cr2", "nef", "arw")
private val VIDEO_EXT = setOf("mp4", "mkv", "mov", "avi", "webm", "m4v", "wmv", "mpg", "mpeg")
private val AUDIO_EXT = setOf("mp3", "wav", "flac", "m4a", "aac", "ogg", "opus", "wma")
private val ARCHIVE_EXT = setOf("zip", "7z", "rar", "tar", "gz", "bz2", "xz", "tgz")
private val OFFICE_EXT = setOf("doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", "ods", "odp", "rtf")
private val TEXT_EXT = setOf("txt", "md", "csv", "json", "xml", "yaml", "yml", "log", "ini", "toml", "kt", "rs", "py", "js", "ts", "html", "css")

/** Picks the glyph from the host's preview kind first (it knows), then the extension. */
fun glyphKind(entry: Entry): GlyphKind {
    if (entry.kind == EntryKind.DIR) return GlyphKind.FOLDER
    when (entry.preview) {
        PreviewKind.IMAGE -> return GlyphKind.IMAGE
        PreviewKind.PDF -> return GlyphKind.PDF
        PreviewKind.TEXT -> return GlyphKind.TEXT
        PreviewKind.OFFICE -> return GlyphKind.OFFICE
        PreviewKind.VIDEO_POSTER -> return GlyphKind.VIDEO
        PreviewKind.NONE, PreviewKind.UNKNOWN -> Unit
    }
    return glyphKindForExtension(entry.ext ?: entry.name.substringAfterLast('.', ""))
}

/** The glyph of a file known only by its name (saved copies, downloads). */
fun glyphKindForName(name: String): GlyphKind = glyphKindForExtension(name.substringAfterLast('.', ""))

private fun glyphKindForExtension(extension: String): GlyphKind {
    val ext = extension.lowercase(Locale.ROOT)
    return when (ext) {
        in IMAGE_EXT -> GlyphKind.IMAGE
        in VIDEO_EXT -> GlyphKind.VIDEO
        in AUDIO_EXT -> GlyphKind.AUDIO
        in ARCHIVE_EXT -> GlyphKind.ARCHIVE
        in OFFICE_EXT -> GlyphKind.OFFICE
        "pdf" -> GlyphKind.PDF
        in TEXT_EXT -> GlyphKind.TEXT
        else -> GlyphKind.FILE
    }
}

/**
 * `1.8 GB`, `48.2 MB`, `12 KB`, `0 B`: 1024-based with the unit names Windows Explorer uses, so the
 * numbers match what the user sees on the computer. One decimal below 100 of a unit.
 */
fun formatFileSize(bytes: Long): String {
    if (bytes < 1024) return "${bytes.coerceAtLeast(0)} B"
    val units = arrayOf("KB", "MB", "GB", "TB", "PB")
    var value = bytes.toDouble()
    var unit = -1
    while (value >= 1024 && unit < units.lastIndex) {
        value /= 1024
        unit++
    }
    val tenths = Math.round(value * 10)
    return if (tenths >= 1000 || tenths % 10 == 0L) {
        String.format(Locale.ROOT, "%d %s", Math.round(value), units[unit])
    } else {
        String.format(Locale.ROOT, "%.1f %s", value, units[unit])
    }
}

/** A medium localised date of an ISO-8601 [mtime] in [zone], or an empty string if it cannot be read. */
fun formatModified(mtime: String, zone: ZoneId = ZoneId.systemDefault(), locale: Locale = Locale.getDefault()): String =
    try {
        DateTimeFormatter.ofLocalizedDate(FormatStyle.MEDIUM).withLocale(locale).withZone(zone)
            .format(Instant.parse(mtime))
    } catch (_: java.time.DateTimeException) {
        ""
    }

/** A `link` entry is shown greyed and does nothing; an unknown kind (newer host) is not acted on. */
fun Entry.isOpenable(): Boolean = kind != EntryKind.UNKNOWN && EntryFlag.LINK !in flags

/** Only files are selectable: folder fetch is not in v1 (client spec). */
fun Entry.isSelectable(): Boolean = kind == EntryKind.FILE && EntryFlag.LINK !in flags
