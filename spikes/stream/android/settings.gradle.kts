// THROWAWAY spike (WP-44): a minimal Android viewer for the stream spike. It receives the host's
// WebRTC H.264 video track with org.jitsi:webrtc, shows it full screen with a frame counter
// overlay, and sends taps to the host as normalised coordinates on the "input" data channel. Not
// part of the shipping client; findings live in docs/specs/2026-11-01-stream-spike-findings.md.
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
            from(files("../../../client/android/gradle/libs.versions.toml"))
        }
    }
}

rootProject.name = "gander-stream-spike"

include(":app")
