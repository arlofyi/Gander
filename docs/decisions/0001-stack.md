# 0001. Host and client stack

- Status: Accepted on 2026-11-01 (owner approved defaults D1-D14) for the host (Rust) and the
  Android client (Kotlin). The streaming-library line was Proposed until the Phase 0 spike reported;
  it is **Accepted on 2026-11-02** (WP-45, by the Conductor under the master plan's D12 default
  rule): `webrtc` (webrtc-rs) 0.21.x for WebRTC, a Media Foundation hardware MFT for encode, and
  an own thin WGC wrapper over the `windows` crate for capture (not `windows-capture`), with the
  conditions listed under Evidence.
- Date: 2026-11-01

## Context

The host needs: low-level window capture, hardware video encode, input injection, a tray UI, a
small footprint, and good Windows support first. The client needs: hardware video decode over
WebRTC, smooth touch input, file handoff to other apps, a Files-app provider, pinning of a
self-signed per-host certificate, and Android first with iOS later.

Research on 2026-11-01 (`docs/research/2026-11-01-plan-review.md`) established:

- Host side: Rust has everything needed. `windows` 0.62 covers WGC, Media Foundation, SendInput
  and WinRT PDF rendering; `windows-capture` 2.0 exposes zero-copy D3D11 textures; `moq-video`
  (MIT/Apache) is a working reference for a low-latency Media Foundation H.264 encoder fed from
  DXGI surfaces; `webrtc` 0.21 and `str0m` 0.24 both do host-only ICE and H.264; `wasapi` 0.26 does
  per-process audio loopback; `tray-icon` 0.26 + `egui` 0.37 give a pure-Rust tray and settings UI.
  Sunshine (GPL-3.0) cannot be copied.
- Client side: a self-signed cert pinned at pairing cannot be handled by OkHttp `CertificatePinner`
  or by the React Native pinning libraries (they pin on top of system trust); Expo's file-system
  downloads bypass any custom OkHttp client; Android `DownloadManager` only trusts static config.
  A `DocumentsProvider` (Files-app integration) runs without the JS or Dart engine and needs its
  own native pinned HTTP client. All three WebRTC distributions (`react-native-webrtc` 155,
  `flutter_webrtc` 1.6 on M150, Maven `org.jitsi:webrtc` 155 / `io.github.webrtc-sdk:android` 150)
  are 16 KB-page aligned. Play requires targetSdk 36 now and 16 KB support for updates by
  2027-02-01.
- Autonomous build consideration: the first version is built by an AI session on a Windows machine
  with the Android SDK but no Android Studio; one toolchain (Gradle + emulator) with deterministic
  logs beats Metro or Dart plus a bridge.

## Options

Host:
1. **Rust** + `windows` crate + Media Foundation + `webrtc`/`str0m`; tray via `tray-icon` + `egui`.
2. **Go** + `pion/webrtc` + `tsnet`. Easiest Tailscale embedding; Windows capture/encode via cgo.
3. **C++** with libwebrtc directly. Most capable, highest cost and risk.
4. **C# / .NET** (Baton's choice: WPF + Media Foundation). Good Windows access, larger runtime,
   weaker story for a later macOS/Linux host.

Client:
1. **Native Kotlin + Jetpack Compose** (Android), later **SwiftUI** (iOS), sharing the protocol
   spec and test vectors, optionally Kotlin Multiplatform for the protocol layer later.
2. **Flutter** + `flutter_webrtc`: pinning is pure Dart; provider, download job and open-with
   still need Kotlin.
3. **React Native (Expo dev build)**: needs a custom OkHttp factory module, its own download
   module, and config plugins; the weakest fit for this app's native surface.

## Decision

- **Host: Rust.** Workspace crates: `gander-core` (protocol types, policy, path resolution, pairing
  state machine: platform-neutral, heavily tested), `gander-host` (binary: listener, tray, config,
  audit), `gander-win` (WGC, Media Foundation, SendInput, WinRT PDF, shell thumbnails, LocalAPI
  pipe), `gander-render` (worker binary), `gander-stream` (WebRTC session engine), `gander-testwin`
  (test window). Pinned: `windows` 0.62, `tokio`, `axum`, `rustls`/`tokio-rustls`, `webrtc` 0.21
  (fallback `str0m` 0.24), `windows-capture` 2.0 (or an own thin WGC wrapper if its D3D device
  ownership prevents sharing the encoder device, decided in the spike), `wasapi` 0.26, `tray-icon`
  0.26 + `eframe` 0.37, `redb`, `tracing`, `serde`/`toml`, `blake3`, `ulid`, `image` (fallback only).
- **Client: native Kotlin + Jetpack Compose.** minSdk 26 (needed for `openProxyFileDescriptor`),
  targetSdk/compileSdk 36, AGP 8.5.1+ and NDK r28+ for 16 KB alignment. Pinned: OkHttp 5.x with a
  custom SPKI-pinning `X509TrustManager`, Media3 (ExoPlayer) with an OkHttp data source, Coil with
  the same OkHttp client, `org.jitsi:webrtc` 155.x (alternative `io.github.webrtc-sdk:android`),
  CameraX + ZXing core for QR scanning (no Google proprietary ML Kit, keeping F-Droid possible),
  Jetpack DataStore + Keystore-backed encryption for the token store, Room for cache metadata,
  Kotlin coroutines/Flow, Compose Navigation. No Hilt; manual DI.
- **iOS later: SwiftUI** plus a native File Provider; nothing in the protocol assumes a shared codebase.
- Flutter is the named alternative if a single UI codebase becomes a requirement; React Native is
  not pursued.

## Consequences

- Easier: every hard native feature is first-class; one Gradle toolchain for the autonomous build;
  no WebView2, JS or Dart supply chain in a security-sensitive host.
- Harder: iOS UI is a second implementation; the host UI is written in egui rather than web tech
  the owner prefers (acceptable: it is small).
- Must do: spike the WebRTC crate choice and the WGC wrapper choice (Phase 0); keep the protocol
  spec and `protocol/test-vectors/` as the shared contract between platforms.

## Evidence (Phase 0 stream spike, 2026-11-02)

Findings: [`../specs/2026-11-01-stream-spike-findings.md`](../specs/2026-11-01-stream-spike-findings.md)
("Summary and decisions", "Consolidated matrix", then the WP-41 to WP-44 sections). This closes the
streaming-library line only; the host (Rust) and client (Kotlin) lines were accepted on 2026-11-01.

- **WebRTC: `webrtc` (webrtc-rs) 0.21.x, accepted with conditions.** Required Phase 2 work: a
  delay-based bandwidth estimator through its `BandwidthEstimator` interface (its GCC reacted to
  loss only: output under the cap after 5.5 s with a 2 s bottleneck buffer, 2.5 s with 200 ms), the
  RTCP/PLI interceptor (inbound RTCP is dropped unless marked `DeliverToApplication`), liveness and
  pause messages over the data channel (a silent phone is noticed only after about 30 s), and trickle
  ICE or tolerance of empty-candidate answers. See
  [WebRTC (WP-43)](../specs/2026-11-01-stream-spike-findings.md#webrtc-wp-43) and
  [Android + input (WP-44)](../specs/2026-11-01-stream-spike-findings.md#android--input-wp-44).
- **`str0m` 0.24.1 fallback: met every threshold but is blocked by `cargo deny`**
  (RUSTSEC-2026-0387 via `dimpl` 0.7 / `crypto-bigint` 0.5.5). Re-evaluate when it ships on
  `dimpl` >= 0.9. The raw-H.264-over-TLS option is not needed.
- **Capture: an own thin WGC wrapper over the `windows` crate, not `windows-capture`.** The
  "or an own thin WGC wrapper ... decided in the spike" branch of the Decision is the one taken:
  `windows-capture` 2.0.1 always creates its own device on the default adapter, without
  `VIDEO_SUPPORT`, and lends frames only inside its callback, so it cannot share the encoder's
  device. See [Encode (WP-42)](../specs/2026-11-01-stream-spike-findings.md#encode-wp-42) and the WP-41
  "Device ownership" section.
- **Encode: Media Foundation hardware MFT (H.264 baseline, HEVC available).** Conditions: the device
  on the encoder's adapter chosen by LUID; the `NvOptimusEnablement` export (the NVIDIA MFT does not
  activate without it); `timeBeginPeriod(1)` (Intel 15.5 ms against 8.7 ms per frame); on Intel,
  re-create the encoder to change bitrate (about 100-160 ms, one keyframe); H.264 level 5.0
  (`42e032`) for 1080x2280 with level-asymmetry-allowed (the phone advertises `42e01f`).
- **Input: `SendInput` when the target is uncovered and foreground; never while the session is
  locked.** `PostMessage` also works while locked or covered, so the host gates both paths on
  `session::watch` (WP-17), fail closed. `OpenInputDesktop` is not a lock check on build 26200.
- **Pinned-version delta against the Decision text:** patch level only (`webrtc` 0.21 measured as
  0.21.1, `str0m` 0.24 as 0.24.1, `windows` 0.62 as 0.62.2, `windows-capture` 2.0 as 2.0.1,
  `org.jitsi:webrtc` 155.x as 155.0.1).
