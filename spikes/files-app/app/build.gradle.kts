plugins {
    alias(libs.plugins.android.application)
}

android {
    namespace = "fyi.arlo.gander.spike.files"
    compileSdk = libs.versions.compileSdk.get().toInt()

    defaultConfig {
        applicationId = "fyi.arlo.gander.spike.files"
        minSdk = libs.versions.minSdk.get().toInt()
        targetSdk = libs.versions.targetSdk.get().toInt()
        versionCode = 1
        versionName = "0.0.0-spike"
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

// No dependencies on purpose: platform classes only (StorageManager, VideoView, HttpURLConnection).
