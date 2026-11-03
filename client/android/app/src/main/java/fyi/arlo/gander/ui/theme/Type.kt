package fyi.arlo.gander.ui.theme

import androidx.compose.material3.Typography
import androidx.compose.runtime.Immutable
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.sp

// Type scale from the design document, section 4.1. The system sans (Roboto on most devices) and
// FontFamily.Monospace; nothing is bundled.

private val Base = Typography()

val GanderMaterialTypography = Typography(
    headlineMedium = Base.headlineMedium.copy(fontSize = 28.sp, lineHeight = 36.sp, fontWeight = FontWeight.Normal),
    titleLarge = Base.titleLarge.copy(fontSize = 22.sp, lineHeight = 28.sp, fontWeight = FontWeight.Medium),
    titleMedium = Base.titleMedium.copy(fontSize = 16.sp, lineHeight = 24.sp, fontWeight = FontWeight.SemiBold),
    titleSmall = Base.titleSmall.copy(fontSize = 14.sp, lineHeight = 20.sp, fontWeight = FontWeight.SemiBold),
    bodyLarge = Base.bodyLarge.copy(fontSize = 16.sp, lineHeight = 24.sp, fontWeight = FontWeight.Normal),
    bodyMedium = Base.bodyMedium.copy(fontSize = 14.sp, lineHeight = 20.sp, fontWeight = FontWeight.Normal),
    bodySmall = Base.bodySmall.copy(fontSize = 12.sp, lineHeight = 16.sp, fontWeight = FontWeight.Normal),
    labelLarge = Base.labelLarge.copy(fontSize = 14.sp, lineHeight = 20.sp, fontWeight = FontWeight.Medium),
    labelMedium = Base.labelMedium.copy(fontSize = 12.sp, lineHeight = 16.sp, fontWeight = FontWeight.Medium),
)

/** The styles Material 3 has no role for. */
@Immutable
data class GanderTypography(
    /** The pairing code: two groups of three digits, mono 52 sp. */
    val sas: TextStyle,
    /** The smaller code on the "waiting for approval" screen (flow A, identity already checked). */
    val sasSmall: TextStyle,
    /** Paths, the text preview and other monospace content. */
    val mono: TextStyle,
    /** Section labels above lists ("Shared with this phone"). */
    val section: TextStyle,
    /** Tags such as "Default": small caps-like uppercase label. */
    val tag: TextStyle,
)

val DefaultGanderTypography = GanderTypography(
    sas = TextStyle(
        fontFamily = FontFamily.Monospace,
        fontWeight = FontWeight.Medium,
        fontSize = 52.sp,
        lineHeight = 60.sp,
        letterSpacing = 4.sp,
    ),
    sasSmall = TextStyle(
        fontFamily = FontFamily.Monospace,
        fontWeight = FontWeight.Medium,
        fontSize = 40.sp,
        lineHeight = 48.sp,
        letterSpacing = 4.sp,
    ),
    mono = TextStyle(fontFamily = FontFamily.Monospace, fontSize = 12.5.sp, lineHeight = 20.sp),
    section = TextStyle(fontSize = 13.sp, lineHeight = 18.sp, fontWeight = FontWeight.SemiBold, letterSpacing = 0.1.sp),
    tag = TextStyle(fontSize = 11.sp, lineHeight = 14.sp, fontWeight = FontWeight.Bold, letterSpacing = 0.4.sp),
)
