package fyi.arlo.gander.ui.theme

import android.os.Build
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.ColorScheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.ReadOnlyComposable
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.platform.LocalContext

/** The Gander tokens that have no Material 3 role, read as `GanderTheme.colors.warn`. */
val LocalGanderColors = staticCompositionLocalOf { LightGanderColors }

/** The Gander type styles that have no Material 3 role, read as `GanderTheme.type.sas`. */
val LocalGanderTypography = staticCompositionLocalOf { DefaultGanderTypography }

/**
 * The tokens mapped onto Material 3 roles (design document 9.1). Orange is `tertiary` (the raw
 * accent) and `secondaryContainer` (selection: nav indicator, selected chips, segmented buttons);
 * commands are ink (`primary`).
 */
fun ganderColorScheme(c: GanderColors): ColorScheme {
    val base = if (c.isDark) darkColorScheme() else lightColorScheme()
    return base.copy(
        primary = c.primary,
        onPrimary = c.onPrimary,
        primaryContainer = c.surface3,
        onPrimaryContainer = c.text,
        inversePrimary = c.accent,
        secondary = c.accentText,
        onSecondary = c.surface,
        secondaryContainer = c.accentContainer,
        onSecondaryContainer = c.onAccentContainer,
        tertiary = c.accent,
        onTertiary = c.onAccent,
        tertiaryContainer = c.accentContainer,
        onTertiaryContainer = c.onAccentContainer,
        background = c.bg,
        onBackground = c.text,
        surface = c.bg,
        onSurface = c.text,
        surfaceVariant = c.surface2,
        onSurfaceVariant = c.text2,
        surfaceTint = c.primary,
        inverseSurface = c.text,
        inverseOnSurface = c.bg,
        error = c.bad,
        onError = c.surface,
        errorContainer = c.badContainer,
        onErrorContainer = c.text,
        outline = c.outline,
        outlineVariant = c.line,
        scrim = c.scrim,
        surfaceBright = c.surface,
        surfaceDim = c.surface2,
        surfaceContainerLowest = c.surface,
        surfaceContainerLow = c.surface,
        surfaceContainer = c.surface2,
        surfaceContainerHigh = c.surface3,
        surfaceContainerHighest = c.surface3,
    )
}

/**
 * Material 3 with the fixed Gander palette. Dynamic colour is off (owner decision, design
 * document open question 1): the "orange means looking" rules depend on a fixed palette.
 * [dynamicColor] stays for tests and experiments only.
 */
@Composable
fun GanderTheme(
    darkTheme: Boolean = isSystemInDarkTheme(),
    dynamicColor: Boolean = false,
    content: @Composable () -> Unit,
) {
    val gander = if (darkTheme) DarkGanderColors else LightGanderColors
    val colorScheme = if (dynamicColor && Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
        val context = LocalContext.current
        if (darkTheme) dynamicDarkColorScheme(context) else dynamicLightColorScheme(context)
    } else {
        ganderColorScheme(gander)
    }
    CompositionLocalProvider(
        LocalGanderColors provides gander,
        LocalGanderTypography provides DefaultGanderTypography,
    ) {
        MaterialTheme(
            colorScheme = colorScheme,
            typography = GanderMaterialTypography,
            shapes = GanderShapes,
            content = content,
        )
    }
}

/** Accessors next to `MaterialTheme`: `GanderTheme.colors.warn`, `GanderTheme.type.sas`. */
object GanderTheme {
    val colors: GanderColors
        @Composable
        @ReadOnlyComposable
        get() = LocalGanderColors.current

    val type: GanderTypography
        @Composable
        @ReadOnlyComposable
        get() = LocalGanderTypography.current
}
