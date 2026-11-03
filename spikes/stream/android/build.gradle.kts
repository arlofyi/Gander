// Top-level build file for the WP-44 spike. Same plugin set the client uses to put the Kotlin Gradle
// plugin (version from the shared catalogue) on the classpath; AGP 9 compiles Kotlin by itself.

plugins {
    alias(libs.plugins.android.application) apply false
    alias(libs.plugins.kotlin.compose) apply false
}
