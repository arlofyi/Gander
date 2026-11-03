# R8 rules for the release build. Debug builds do not shrink.

# libwebrtc (org.jitsi:webrtc) is reached from native code through JNI; keep its classes and
# native method names intact.
-keep class org.webrtc.** { *; }
-dontwarn org.webrtc.**

# Keep line numbers for readable stack traces from a local, redacted diagnostics export.
-keepattributes SourceFile,LineNumberTable
-renamesourcefileattribute SourceFile
