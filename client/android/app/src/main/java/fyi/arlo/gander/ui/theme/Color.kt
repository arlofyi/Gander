package fyi.arlo.gander.ui.theme

import androidx.compose.runtime.Immutable
import androidx.compose.ui.graphics.Color

// Design tokens from docs/design/2026-11-02-ui-redesign.md section 3.2. Same names in both themes,
// only the values change. Contrast ratios are in the design document; do not change a value here
// without re-checking them.

/** The brand colours (section 3.1). The beak is the same in both themes. */
object Brand {
    val SlateInk = Color(0xFF1D2A30)
    val GreylagBeak = Color(0xFFE2682A)
    val Paper = Color(0xFFF4F2EC)
}

/** Every Gander token for one theme. */
@Immutable
data class GanderColors(
    val bg: Color,
    val surface: Color,
    val surface2: Color,
    val surface3: Color,
    val line: Color,
    val outline: Color,
    val text: Color,
    val text2: Color,
    val text3: Color,
    val primary: Color,
    val onPrimary: Color,
    /** Live dot, default outline, selection, focus ring. Non-text only (3.25:1 on light surfaces). */
    val accent: Color,
    val onAccent: Color,
    /** Orange-coloured text (LIVE label, links); always use this, never [accent], for text. */
    val accentText: Color,
    val accentContainer: Color,
    val onAccentContainer: Color,
    val ok: Color,
    val warn: Color,
    val bad: Color,
    /** Fill of destructive buttons. */
    val danger: Color,
    val onDanger: Color,
    val okContainer: Color,
    val warnContainer: Color,
    val badContainer: Color,
    val scrim: Color,
    val isDark: Boolean,
)

val LightGanderColors = GanderColors(
    bg = Color(0xFFF3F4F2),
    surface = Color(0xFFFBFBFA),
    surface2 = Color(0xFFECEEEC),
    surface3 = Color(0xFFE3E7E5),
    line = Color(0xFFD6DBDA),
    outline = Color(0xFF7E8B91),
    text = Color(0xFF1D2A30),
    text2 = Color(0xFF46555C),
    text3 = Color(0xFF5F6E75),
    primary = Color(0xFF1D2A30),
    onPrimary = Color(0xFFFFFFFF),
    accent = Color(0xFFE2682A),
    onAccent = Color(0xFF170D06),
    accentText = Color(0xFFA8460F),
    accentContainer = Color(0xFFFBE3D4),
    onAccentContainer = Color(0xFF5A2206),
    ok = Color(0xFF2E7D4F),
    warn = Color(0xFF8A6100),
    bad = Color(0xFFB3261E),
    danger = Color(0xFFB3261E),
    onDanger = Color(0xFFFFFFFF),
    okContainer = Color(0xFFDCEFE2),
    warnContainer = Color(0xFFF6E7C4),
    badContainer = Color(0xFFF9DEDC),
    // Ink at 42 %.
    scrim = Color(0x6B111719),
    isDark = false,
)

val DarkGanderColors = GanderColors(
    bg = Color(0xFF111719),
    surface = Color(0xFF172023),
    surface2 = Color(0xFF1E282C),
    surface3 = Color(0xFF263236),
    line = Color(0xFF33424A),
    outline = Color(0xFF6F7D83),
    text = Color(0xFFE7ECEC),
    text2 = Color(0xFFB3C0C4),
    text3 = Color(0xFF8D9BA0),
    primary = Color(0xFFE7ECEC),
    onPrimary = Color(0xFF1D2A30),
    accent = Color(0xFFEE7A3C),
    onAccent = Color(0xFF170D06),
    accentText = Color(0xFFF59A62),
    accentContainer = Color(0xFF4A2412),
    onAccentContainer = Color(0xFFFFDCCB),
    ok = Color(0xFF6FCF97),
    warn = Color(0xFFE3B341),
    bad = Color(0xFFF2B8B5),
    danger = Color(0xFFC4302B),
    onDanger = Color(0xFFFFFFFF),
    okContainer = Color(0xFF1F3A2A),
    warnContainer = Color(0xFF3D3012),
    badContainer = Color(0xFF4A1D1A),
    // Black at 55 %.
    scrim = Color(0x8C000000),
    isDark = true,
)

/** Colours of the immersive (always dark) viewers: preview pages, image, video and the camera. */
object Immersive {
    val Background = Color(0xFF0B0E0F)
    val Content = Color(0xFFE9EEEE)
    val ContentMuted = Color(0xFFB9C4C6)
    val StreamBackdrop = Color(0xFF1A1F21)
}
