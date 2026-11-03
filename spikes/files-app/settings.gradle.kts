// THROWAWAY spike (WP-47): a DocumentsProvider that serves files from a loopback HTTP server through
// StorageManager.openProxyFileDescriptor. Not part of the shipping client; findings live in
// docs/specs/2026-11-01-files-app-spike-findings.md.
//
// Versions come straight from the real client's catalogue so the spike never drifts from it.

pluginManagement {
    repositories {
        google {
            content {
                includeGroupAndSubgroups("com.android")
                includeGroupAndSubgroups("com.google")
                includeGroupAndSubgroups("androidx")
            }
        }
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
    }
    versionCatalogs {
        create("libs") {
            from(files("../../client/android/gradle/libs.versions.toml"))
        }
    }
}

rootProject.name = "gander-files-app-spike"

include(":app")
