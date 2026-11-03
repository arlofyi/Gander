plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.compose)
    alias(libs.plugins.kotlin.serialization)
    alias(libs.plugins.ksp)
}

// The protocol source of truth (WP-10), two levels above client/android.
val openApiSpec: File = rootProject.file("../../protocol/openapi.yaml")

// The shared SAS vectors (WP-13), read by the SAS unit test so drift between the Rust host and the
// Kotlin client fails the build.
val sasVectors: File = rootProject.file("../../protocol/test-vectors/sas.json")

// The shared deep-zoom tile pyramid vectors (WP-67), read by TilePyramidTest so the Kotlin geometry
// cannot drift from the Rust host's (host/gander-core/src/tiles.rs runs the same file).
val tileVectors: File = rootProject.file("../../protocol/test-vectors/tiles.json")

android {
    namespace = "fyi.arlo.gander"
    compileSdk = libs.versions.compileSdk.get().toInt()

    // NDK r28+ (16 KB page-size aware). The app has no native code of its own; the NDK strips and
    // links the libraries that dependencies (org.jitsi:webrtc) bring. Install it with
    // `sdkmanager "ndk;<version from libs.versions.toml>"`.
    ndkVersion = libs.versions.ndk.get()

    defaultConfig {
        applicationId = "fyi.arlo.gander"
        minSdk = libs.versions.minSdk.get().toInt()
        targetSdk = libs.versions.targetSdk.get().toInt()
        versionCode = 1
        versionName = "0.1.0"
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro",
            )
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    buildFeatures {
        compose = true
        buildConfig = true
    }

    packaging {
        // Store native libraries uncompressed and page-aligned (16 KB with AGP 8.5.1+). Required
        // for the Android 15+ 16 KB page-size support that `cargo xtask check-elf-alignment` checks.
        jniLibs.useLegacyPackaging = false
        resources.excludes += "/META-INF/{AL2.0,LGPL2.1}"
    }

    lint {
        baseline = file("lint-baseline.xml")
        abortOnError = true
        // Versions are pinned on purpose (gradle/libs.versions.toml); "newer version available"
        // must not turn CI red the day a library publishes.
        disable += setOf("GradleDependency", "NewerVersionAvailable", "AndroidGradlePluginVersion")
        // targetSdk stays at 36 until the Android 17 (API 37) bump scheduled for spec Phase 3.
        disable += "OldTargetApi"
    }

    sourceSets {
        // Fakes shared by the JVM unit tests and the instrumented tests (scripted host, fake
        // repositories, entry builders). They depend on app classes only, no test library.
        getByName("test").kotlin.directories.add("src/sharedTest/java")
        getByName("androidTest").kotlin.directories.add("src/sharedTest/java")
    }

    testOptions {
        unitTests.all { test ->
            // DTO tests decode every example in the hand-written protocol document straight from
            // the repository, so a change to protocol/openapi.yaml that the Kotlin DTOs do not
            // follow fails `testDebugUnitTest`. Declared as an input so the test re-runs on change.
            test.systemProperty("gander.openapi", openApiSpec.absolutePath)
            test.inputs.file(openApiSpec).withPathSensitivity(PathSensitivity.NONE)
            test.systemProperty("gander.sasVectors", sasVectors.absolutePath)
            test.inputs.file(sasVectors).withPathSensitivity(PathSensitivity.NONE)
            test.systemProperty("gander.tileVectors", tileVectors.absolutePath)
            test.inputs.file(tileVectors).withPathSensitivity(PathSensitivity.NONE)
        }
    }
}

// Room schema export (used by migration tests once the database exists, WP-23).
ksp {
    arg("room.schemaLocation", "$projectDir/schemas")
}

dependencies {
    // AndroidX and Compose
    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.activity.compose)
    implementation(libs.androidx.lifecycle.runtime.compose)
    implementation(libs.androidx.lifecycle.viewmodel.compose)
    implementation(libs.androidx.navigation.compose)
    implementation(platform(libs.androidx.compose.bom))
    implementation(libs.androidx.compose.ui)
    implementation(libs.androidx.compose.ui.graphics)
    implementation(libs.androidx.compose.ui.tooling.preview)
    implementation(libs.androidx.compose.material3)
    implementation(libs.androidx.compose.material3.windowsizeclass) // phone versus tablet layouts
    implementation(libs.androidx.compose.material.icons.extended) // file-type glyphs; R8 drops the unused ones

    // Persistence: Room (listing snapshots, cached files, fetch jobs), DataStore (settings, tokens)
    implementation(libs.androidx.room.runtime)
    ksp(libs.androidx.room.compiler)
    implementation(libs.androidx.datastore.preferences)

    // App lock: BiometricPrompt with device-credential fallback (needs a FragmentActivity)
    implementation(libs.androidx.biometric)

    // Networking and images
    implementation(libs.okhttp)
    implementation(libs.coil.compose)
    implementation(libs.coil.network.okhttp)

    // Media preview
    implementation(libs.androidx.media3.exoplayer)
    implementation(libs.androidx.media3.ui)
    implementation(libs.androidx.media3.datasource.okhttp)

    // Pairing scan: CameraX preview + ZXing decode
    implementation(libs.androidx.camera.core)
    implementation(libs.androidx.camera.camera2)
    implementation(libs.androidx.camera.lifecycle)
    implementation(libs.androidx.camera.view)
    implementation(libs.zxing.core)

    // Stream mode (native libraries, 16 KB check applies)
    implementation(libs.webrtc)

    // Kotlin
    implementation(libs.kotlinx.coroutines.android)
    implementation(libs.kotlinx.serialization.json)

    // Unit tests (src/test, JVM)
    testImplementation(libs.junit)
    testImplementation(libs.kotlinx.coroutines.test)
    testImplementation(libs.okhttp.mockwebserver)
    testImplementation(libs.okhttp.tls) // HeldCertificate: test certificates generated per run
    testImplementation(libs.snakeyaml) // reads protocol/openapi.yaml examples in DTO tests

    // Instrumented tests (src/androidTest, emulator or device)
    androidTestImplementation(libs.androidx.test.ext.junit)
    androidTestImplementation(libs.androidx.test.runner)
    androidTestImplementation(libs.androidx.test.espresso.core)
    androidTestImplementation(platform(libs.androidx.compose.bom))
    androidTestImplementation(libs.androidx.compose.ui.test.junit4)

    debugImplementation(libs.androidx.compose.ui.tooling)
    debugImplementation(libs.androidx.compose.ui.test.manifest)
}
