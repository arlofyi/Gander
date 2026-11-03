// Top-level build file. Plugins are declared here (apply false) so every module resolves the same
// versions from the catalogue; the app module applies them.
//
// AGP 9 ships built-in Kotlin, so org.jetbrains.kotlin.android is deliberately not applied. The
// Compose, serialization and KSP plugins pull the Kotlin Gradle plugin version listed in the
// catalogue onto the build classpath.

import com.android.build.api.dsl.ApplicationExtension
import org.cyclonedx.Version
import org.cyclonedx.gradle.CyclonedxDirectTask
import org.cyclonedx.model.Component

plugins {
    alias(libs.plugins.android.application) apply false
    alias(libs.plugins.kotlin.compose) apply false
    alias(libs.plugins.kotlin.serialization) apply false
    alias(libs.plugins.ksp) apply false
    // WP-82 supply chain: the CycloneDX SBOM of the APK's runtime dependencies. Applied here, at
    // the root, so the app module's own build file is untouched; the plugin registers
    // `cyclonedxDirectBom` in every project and the block below configures the app's one.
    alias(libs.plugins.cyclonedx)
}

// SBOM of the app (WP-82): `./gradlew --no-daemon cyclonedxSbom` writes
// `<repo>/target/sbom/gander-android.cdx.json`, a CycloneDX 1.5 document of what the release APK
// carries. `releaseRuntimeClasspath` is the resolved runtime closure of the release variant (the
// debug-only tooling and every test configuration are not in it); build-environment (plugin)
// dependencies are left out too: they do not ship. `cargo xtask sbom --check` validates the file.
val androidSbom: RegularFile = layout.projectDirectory.file("../../target/sbom/gander-android.cdx.json")

project(":app") {
    tasks.withType<CyclonedxDirectTask>().configureEach {
        includeConfigs.set(listOf("releaseRuntimeClasspath"))
        schemaVersion.set(Version.VERSION_15)
        projectType.set(Component.Type.APPLICATION)
        componentGroup.set("fyi.arlo")
        componentName.set("gander-android")
        // The app's own versionName, read when the task graph is built (the app module is configured by then).
        componentVersion.set(
            providers.provider {
                project(":app").extensions.getByType<ApplicationExtension>().defaultConfig.versionName ?: "0.0.0-dev"
            },
        )
        componentDescription.set("Gander Android client")
        includeBomSerialNumber.set(true)
        includeBuildSystem.set(false)
        includeBuildEnvironment.set(false)
        includeLicenseText.set(false)
        xmlOutput.unsetConvention()
        jsonOutput.set(androidSbom)
    }
}

tasks.register("cyclonedxSbom") {
    group = "verification"
    description = "Writes the CycloneDX 1.5 SBOM of the app to <repo>/target/sbom/gander-android.cdx.json."
    dependsOn(":app:cyclonedxDirectBom")
}
