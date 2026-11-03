plugins {
    alias(libs.plugins.android.application)
}

android {
    namespace = "fyi.arlo.gander.spike.stream"
    compileSdk = libs.versions.compileSdk.get().toInt()

    // Same NDK the client pins: the native libraries come from org.jitsi:webrtc, the spike has
    // none of its own, but AGP needs the NDK to strip them.
    ndkVersion = libs.versions.ndk.get()

    defaultConfig {
        applicationId = "fyi.arlo.gander.spike.stream"
        minSdk = libs.versions.minSdk.get().toInt()
        targetSdk = libs.versions.targetSdk.get().toInt()
        versionCode = 1
        versionName = "0.0.0-spike"
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    packaging {
        // Same as the client: native libraries stored uncompressed and page-aligned (16 KB).
        jniLibs.useLegacyPackaging = false
    }
}

dependencies {
    implementation(libs.webrtc)
    // WebSocket client for the signalling channel (plain ws:// to the spike host).
    implementation(libs.okhttp)
}
