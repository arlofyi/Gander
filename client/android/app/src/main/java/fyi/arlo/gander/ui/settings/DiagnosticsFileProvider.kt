package fyi.arlo.gander.ui.settings

import androidx.core.content.FileProvider

/**
 * The provider behind [DiagnosticsShare]. A subclass only so that the manifest can declare a second
 * `FileProvider` (Android wants one class name per provider) with its own authority and paths.
 */
class DiagnosticsFileProvider : FileProvider()
