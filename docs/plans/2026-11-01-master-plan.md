# Master plan: Gander from empty folder to production-ready, executable by an autonomous session

- Date: 2026-11-01
- Specs: everything in `docs/specs/2026-11-01-*.md`; decisions in `docs/decisions/`
- Review that produced this plan: `docs/research/2026-11-01-plan-review.md`

## 0. How to use this plan

This plan is written for a Claude session (a Conductor with workers, or a single session) that
runs unattended for a night, possibly several nights. It assumes no human answers questions during
the run.

**Read in this order before touching anything:** `CLAUDE.md`, `docs/context.md`, this file
(sections 1-3 fully), then the spec for the work package you are about to do. Do not re-read the
whole corpus for every package.

**Run protocol**
1. Run the automated preflight (WP-00). If a hard requirement fails, stop and write the morning
   report with the failure; do not improvise around missing toolchains.
2. Apply the decision defaults (section 2) unless `docs/context.md` contains an "Owner overrides"
   block dated after this plan. Record each applied default in the morning report.
3. Work through packages in dependency order (section 4). Within a phase, packages marked `‖` may
   run in parallel on separate worktrees/workers; packages marked `→` depend on the one before.
4. A package is done only when its **Verify** command(s) pass and its **Done** checklist is met.
   Commit per package on a `feat/<wp-id>-<slug>` branch (Conventional Commits, scoped). Never push,
   never create a remote, never publish, never install software outside the preflight list.
5. On a failing verify: fix up to three times; then apply the package's **Fallback**; then mark it
   `deferred` with the exact error in the morning report and continue with packages that do not
   depend on it.
6. Gates (section 5) are checkpoints; the run stops at the first gate it cannot pass after
   fallbacks and reports. Night 1 (owner's scope, 2026-11-01): gates G0-G4 in order, then a Phase 2
   attempt (WP-50 onward) as time allows; see `2026-11-01-orchestrator-kickoff.md`.
7. End of run, always: update `docs/context.md`, `CHANGELOG.md` (Unreleased), flip ADR statuses the
   evidence supports, write `docs/reports/<date>-overnight-report.md` from the template in
   section 8, and leave no spike processes or power-setting changes behind.

**Hard rules during the run (restating CLAUDE.md where it matters most)**
- Nothing listens on `0.0.0.0` or `::`, not even in tests. Tests assert it.
- No secrets in logs, fixtures or commits. Pairing secrets and tokens in tests are generated per run.
- No code copied from GPL or unlicensed projects (Sunshine, Baton). Patterns, yes; code, no.
- The machine's lock screen, sleep and display settings are restored if a spike changes them.
- Do not call any external service except crates.io, Maven Central, Google's Android repos and
  GitHub for dependency downloads.

## 1. Preflight

### 1a. Human, before leaving (10 minutes)

_Status 2026-11-01: the owner answered these (see `docs/context.md`, "Owner approvals"). The
emulator and pwsh installs are delegated to WP-00; the display and Office items remain for the
owner to do physically before leaving._

| Item | Why | How |
| --- | --- | --- |
| Confirm or override the decision defaults (section 2) | the run will otherwise take them | add an "Owner overrides" block to `docs/context.md` |
| Install the Android emulator and a system image | enables the Phase 1 end-to-end gate | `sdkmanager "emulator" "system-images;android-36;google_apis;x86_64" "platform-tools"` then `avdmanager create avd -n gander -k "system-images;android-36;google_apis;x86_64"` (about 2 GB). Hardware acceleration: Hyper-V is present on this machine; enable "Windows Hypervisor Platform" if the emulator refuses to start |
| Optionally plug in an Android device with USB debugging, or enable wireless debugging on the tablet | real-device tests and Tailscale identity binding | `adb devices` must list it |
| Keep the display on for the night, or accept that capture tests run first | WGC capture tests need a composed desktop | `powercfg /change monitor-timeout-ac 0` (the run restores 60) |
| Decide whether the run may install `pwsh` 7 | the owner's Claude Code profile requires it; it is absent | `winget install Microsoft.PowerShell`, or allow Bash-only scripting for repo tasks |
| Leave Office closed | Office COM render tests create their own hidden instance | |

### 1b. Automated (WP-00), must pass before anything else

Checks and expected results: `cargo --version` (>= 1.90), `cl.exe` reachable through the MSVC
toolchain (`cargo build` of a hello-world), `java -version` (17 or 21), `%ANDROID_HOME%` with
`platforms/android-36`, `build-tools/36.*`, `cmdline-tools/latest`; `tailscale status` reports
`Running`; `git` configured; free disk >= 20 GB; Windows build >= 22000 reported; optional:
emulator present (`emulator -list-avds` lists `gander`), `adb devices` has a device, Office
detected (`POWERPNT.EXE` exists), `ffmpeg` on PATH. The result is written to
`docs/reports/<date>-preflight.md`.

## 2. Decision defaults

Applied automatically if not overridden. Each has an ADR or spec that already documents the reasoning.

| # | Decision | Default | Record |
| --- | --- | --- | --- |
| D1 | Host language and UI | Rust; `tray-icon` + `egui` | ADR-0001, ADR-0007 |
| D2 | Android client | native Kotlin + Jetpack Compose, minSdk 26, targetSdk 36 | ADR-0001 |
| D3 | Network trust | OS VPN client + `tailscale`/`generic` adapters; LocalAPI pipe, CLI fallback | ADR-0002 |
| D4 | Licence | Apache-2.0 (LICENSE written; nothing published) | ADR-0003 |
| D5 | Transport | pinned TLS 1.3 + HTTP/1.1 + WS on port 42637; WebRTC host-only candidates | ADR-0005 |
| D6 | Stream launch policy | handler-executable allowlist, owner approval, browsers isolated | ADR-0006 |
| D7 | Process model | user-session tray app, worker per render job, no service | ADR-0007 |
| D8 | Write access in v1 | none; `write: false` capability | protocol spec |
| D9 | Stream while locked | no; `host_locked` | security.md |
| D10 | Concurrent streams | one per host | stream-launch spec |
| D11 | Minimum Windows | 11 (22000) for Stream; 10 2004 best-effort for Browse/Preview/Fetch | ADR-0001 |
| D12 | WebRTC crate | `webrtc` 0.21 first, `str0m` fallback, raw-H.264-over-TLS last resort | ADR-0005, spike spec |
| D13 | Fetch handoff | built-in viewers for preview types; system resolver otherwise | client spec |
| D14 | Repo visibility | local git only; remote creation is human-gated | this plan |

## 3. Repository layout (created by WP-01)

```
gander/
  Cargo.toml                 workspace
  host/
    gander-core/             protocol types, policy, path resolution, pairing state machine (no OS calls)
    gander-win/              WGC, Media Foundation, SendInput, WinRT PDF, shell thumbs, LocalAPI pipe
    gander-stream/           WebRTC session engine (Phase 2)
    gander-render/           worker binary
    gander-host/             the tray application binary
    gander-testwin/          deterministic test window (spike + tests)
  client/android/            Gradle project, package fyi.arlo.gander
  protocol/
    openapi.yaml             generated from the protocol spec in WP-10
    test-vectors/            paths.json, sas.json, policy.json
  spikes/stream/             throwaway Phase 0 code (deleted or archived after findings)
  installer/                 cargo-wix config, firewall rule script, autostart task XML
  scripts/                   xtask-style helpers (Rust `cargo xtask`), no shell dependency
  .github/workflows/         host-windows.yml, client-android.yml, docs.yml
  docs/                      as today
  LICENSE, SECURITY.md, CHANGELOG.md, README.md, CLAUDE.md
```

## 4. Work packages

Fields: **Goal**, **Inputs** (specs), **Outputs** (files), **Verify** (commands that must pass),
**Done** (checklist), **Deps**, **Size** (S < 2 h, M 2-5 h, L > 5 h of worker time), **Route**
(hint for a Conductor: `hard` = Claude Opus-class reasoning, `std` = standard, `cheap` = bulk or
mechanical), **Fallback**.

### Night 1 scope (owner-approved 2026-11-01)

WP-00, WP-01, WP-10..WP-1C (host), WP-20..WP-29 (client), WP-40..WP-47 (spikes), gates G0-G4 in
order, then the Phase 2 attempt (WP-50, WP-51, then 52-54 as time allows), then the morning
report. Emulator only; device-gated items deferred. Installs allowed: emulator + API 36 image,
pwsh 7. Wave ordering and worker briefs: `2026-11-01-orchestrator-kickoff.md`.

### Phase 0: spikes (findings are the deliverable)

**WP-40 Test window** ‖  
Goal: `gander-testwin` paints a 60 Hz frame counter, moving box, clock; title from argv; `Ctrl+P`
opens a popup menu and tooltip; `--self-test` exits after 20 s; a named pipe lets tests ask it to
open a second window or minimise. Inputs: stream-spike spec. Outputs: `host/gander-testwin/`.
Verify: `cargo run -p gander-testwin -- --self-test` exits 0 within 25 s. Size S. Route cheap.

**WP-41 Capture spike** → WP-40  
Goal: capture the test window via WGC to D3D11 textures with `windows-capture` (and, if its device
ownership blocks encoder sharing, an own thin wrapper); log fps and frame arrival under matrix
items C1-C5, C9-C11, C15-C17 (automatable ones). Outputs: `spikes/stream/capture/`, numbers in the
findings doc. Verify: `cargo run -p spike-capture -- --matrix auto` prints a table with no panics;
C2 shows no occluder pixels (compare a known colour patch). Size M. Route hard.
Fallback: own WGC wrapper via `windows` crate directly.

**WP-42 Encode spike** → WP-41  
Goal: Media Foundation H.264 hardware encoder fed from the capture texture (pattern from
`moq-video`: hardware MFT, DXGI device manager, low-latency CBR, GOP, forced IDR), on the iGPU and
the dGPU; measure ms/frame and bitrate at 1080x2280 and 1920x1080; HEVC if available. Outputs:
`spikes/stream/encode/`, Annex-B dump file for WP-43. Verify: encoder produces decodable output
(`ffmpeg -i dump.h264 -f null -` exits 0; ffmpeg is on PATH here); ms/frame table printed. Size M.
Route hard. Fallback: software encoder (`openh264` crate) for functional continuation, flagged.

**WP-43 WebRTC spike** → WP-42  
Goal: `webrtc` 0.21 sends the encoded stream as a track with host candidates only on explicit
addresses (loopback + tailnet), GCC enabled; data channel echoes input JSON; signalling over a
plain WebSocket. Repeat with `str0m` if latency/adaptation fails thresholds. Outputs:
`spikes/stream/webrtc/`. Verify: a loopback receiver (same crate) decodes frames; throttling with a
Windows QoS policy or `clumsy` shows bitrate adaptation within 5 s (recorded). Size M. Route hard.

**WP-44 Android spike + input** → WP-43 ‖ WP-20  
Goal: minimal Kotlin activity with `org.jitsi:webrtc` rendering the track, overlay frame counter,
taps sent as normalised coordinates; host maps to the window and injects with `SendInput`; also
try `PostMessage` for comparison (C19, C20). Verify: on the emulator, frames render and a tap on
the moving box changes its colour (testwin reacts to clicks); on a real device if present, measure
glass-to-glass by photographing both screens. Size M. Route std.

**WP-45 Findings and ADR flip** → WP-44  
Goal: write `docs/specs/<date>-stream-spike-findings.md` per the spike spec's "Recording
findings" section, including skipped items (C6 needs a human; C7/C8/C13 as possible); set
ADR-0001's streaming line and ADR-0005 D12 to Accepted or record the fallback. Verify: file
exists with one row per matrix item; `docs/context.md` updated. Size S. Route std.

**WP-46 Identity spike** ‖  
Goal: from an unelevated Rust process, call the Tailscale LocalAPI pipe `whois` for the host's own
address and for each peer in `status --json`; record `Node.Tags`, `UserProfile.LoginName`,
`Sharer`, `CapMap`; classify each peer's path (`CurAddr`/`Relay`/`PeerRelay`); try the CLI
fallback. Outputs: `spikes/identity/`, findings appended to the stream findings doc or its own
`<date>-identity-spike-findings.md`. Verify: JSON parsed for self; at least one peer classified.
Size S. Route std.

**WP-47 Files-app spike** ‖ (optional Night 1)  
Goal: a throwaway `DocumentsProvider` serving one file from a local HTTP server via
`openProxyFileDescriptor`; log read offsets/sizes requested by the Files app and a video player.
Verify: emulator Files app opens the document; read chunk sizes recorded. Size S. Route cheap.

### Phase 1: host (Browse, Preview, Fetch)

**WP-01 Repository scaffold** → WP-00  
Goal: workspace per section 3; `LICENSE` (Apache-2.0), `SECURITY.md` (disclosure address
placeholder, supported versions, no bug bounty), `.editorconfig`, `rustfmt.toml`, `clippy` config
(`-D warnings`), `deny.toml` (licences: permissive only; advisories), `.github/workflows` for host
(windows-latest: fmt, clippy, test, deny) and client (ubuntu + Android SDK: unit tests, lint, ELF
16 KB check), `cargo xtask` with `preflight`, `test-all`, `vectors-check`. Update `CLAUDE.md`
Commands section. Verify: `cargo xtask preflight` passes; `cargo build --workspace` succeeds with
empty crates; `cargo deny check` passes. Size S. Route cheap.

**WP-10 Core types, config, protocol schema** → WP-01  
Goal: `gander-core`: protocol DTOs (serde) for every response in the protocol spec; error codes
enum; config model (`config.toml`, `devices.toml`, `apps.toml`) with load/save and owner-only
ACL on Windows; `protocol/openapi.yaml` written by hand from the spec and validated by a
schema test (DTOs round-trip the examples in the spec). Verify: `cargo test -p gander-core`
includes `protocol_examples_roundtrip`. Size M. Route std.

**WP-11 Path resolution** → WP-10  
Goal: implement the path-resolution spec: segment validation, Windows handle-relative
`NtCreateFile` walk with reparse-tag policy and short-name check, step-7 final-path check, listing
via handle with flags, hidden patterns, paging and sorting; load `protocol/test-vectors/paths.json`
with the template tokens; fuzz target with `cargo fuzz` or a proptest invariant. Verify:
`cargo test -p gander-core -p gander-win path::` passes with every non-privileged vector green and
privileged ones explicitly `SKIPPED`; `cargo test ... escaped_check_catches_broken_resolver`.
Size L. Route hard. Fallback: string-join + `GetFinalPathNameByHandle` double-check as an interim
implementation, flagged in the report as not meeting the spec.

**WP-12 Listener, TLS, trust adapters, firewall check** → WP-10 ‖ WP-11  
Goal: host identity key + self-signed cert (DPAPI-protected key file), rustls server with TLS 1.3
only; listener plan from the trust adapter (tailscale auto-detect by adapter GUID/description and
ranges; generic by config); bind loopback + explicit addresses, exclusive; re-plan on
`NotifyUnicastIpAddressChange` and a 30 s poll; WhoIs via LocalAPI pipe with CLI fallback and 60 s
cache; `status --json` relay classification; browser-marker and `Host` header refusal; per-device
concurrency limiter; firewall self-check (enumerate Block rules naming the exe; detect Shields Up
from status). Verify: property test "listener plan never contains a wildcard address" over random
interface tables; integration test: start on loopback with a temp config, TLS handshake succeeds
only with the pinned SPKI (client with a different cert fails); fake WhoIs adapter unit tests for
tagged/shared/not-found semantics. Size L. Route hard.

**WP-13 Pairing, tokens, audit log** → WP-12  
Goal: pairing window state machine (open/closed/expired/attempts), QR payload builder, flow A and
B endpoints, SAS derivation (test vectors in `protocol/test-vectors/sas.json`, written here),
token issue/hash/verify/rotate/revoke, device records, identity capture at approval, audit JSONL
with rotation and redaction, `device.revoked` push hook. A test hook (`--test-approve`) lets
integration tests approve pairings without a UI. Verify: unit tests per the pairing spec "Tests";
integration: full flow A and B over loopback; MITM simulation yields different SAS; 6th attempt
429; closed window 404; revocation closes an open WS within 1 s. Size M. Route hard.

**WP-14 Browse and Fetch API** → WP-11, WP-13  
Goal: `/shares`, `/list` (cursor, sort, kinds, ext, ETag/304), `/stat` (+meta via worker, WP-15),
`/search` (bounded BFS, 2 s), `/recent` (bounded walk, 60 s cache), `/file` (Range, If-Range, ETag,
412 on change, Content-Disposition), `/hash` (blake3 and sha256, cached by ETag, 503 while
computing), `/me`, `/host`, `/token/rotate`. Verify: integration tests against a generated fixture
tree (10k files directory, unicode names, hidden files, a junction); Range tests: full, partial,
suffix, multipart -> 416, changed file -> 412; `/list` 10k entries under 300 ms in release; search
partial flag. Size L. Route std.

**WP-15 Render worker and thumbnails** → WP-10 ‖ WP-14  
Goal: `gander-render` worker (handle inheritance, stdin params, stdout image, exit codes), job
object limits, below-normal priority; thumbnail renderers: WIC images (+orientation, ICC), RAW
embedded preview, shell `IShellItemImageFactory` fallback (reject icon-only), video poster via
shell, Office embedded thumbnail from the zip; WebP encoding; cache (redb index, LRU, 2 GiB cap);
queue with concurrency 2 and visible-first priority; `/thumb` endpoint with 503/retry. Verify:
golden tests (dimensions + perceptual hash) on fixtures; malformed PDF and PNG bomb fixtures exit
3/4 inside caps without affecting the host test process; cache rename/touch/evict tests. Size L.
Route hard.

**WP-16 Previews** → WP-15  
Goal: `/preview/info`, `/preview` (pdf via `Windows.Data.Pdf`, image resize, video poster,
office via Office COM export then LibreOffice if found, else none), `/preview/text` with encoding
detection and binary sniff; `estimated_ms`; preview queue (1 per device, 2 total). Verify: golden
tests for a 3-page PDF, rotated JPEG, UTF-16 text, pptx with/without embedded thumbnail; Office
COM test runs when PowerPoint is detected (it is, on this machine) and renders slide 1 of a
generated deck within 30 s; skip message otherwise. Size M. Route std.

**WP-17 Events channel** → WP-13 ‖ WP-14  
Goal: `/events` WebSocket with bearer auth; `hello`, `shares.updated`, `device.revoked`,
`host.state` (locked via `WTSRegisterSessionNotification`, attended via `GetLastInputInfo`,
on_battery via power status), `share.changed` from `ReadDirectoryChangesW` debounced for
recently-listed directories; ping/pong; stream signalling message types defined but returning
`stream_unavailable` until Phase 2. Verify: integration test receives `share.changed` within 2 s
of touching a file; revocation closes the socket. Size M. Route std.

**WP-18 Tray and settings UI** → WP-13, WP-17  
Goal: `tray-icon` + `eframe` app: tray states (idle/connected/streaming), menu (Open, Pause
listener 10 min, Kill switch, Quit); windows: Shares (add folder picker, name, per-device
permissions, show-hidden, hidden patterns, show-path toggle), Devices (name, platform, identity,
last seen, address, revoke), Add device (QR render + SAS + approve/deny + attempts count),
Activity (audit log tail), Network (adapter, bound addresses, relay info, firewall self-check
results, Shields Up warning), Settings (port, cache cap, purge, autostart toggle). Toasts for
pairing requests and session starts. Verify: `cargo build -p gander-host` with the UI feature;
headless smoke test launches the app with `--ui-smoke` which opens each window once and exits 0;
screenshots saved to `docs/specs/assets/ui/` by the smoke test for the report. Size L. Route std.
Fallback: minimal tray + a settings window with only Shares, Devices and Add device.

**WP-19 Stream policy engine (no streaming yet)** → WP-10 ‖ WP-14  
Goal: implement the policy section of the stream-launch spec against a fake association table on
tests and `AssocQueryString` in production: hard-refused extensions, handler resolution and
signature read, never-list, catalogue load/save, browser detection, outcome `allowed | refused |
ask`; `list` entries carry the outcome; `POST /stream` returns `stream_unavailable` (capability
false) until Phase 2 but validates policy first. Test vectors in `protocol/test-vectors/policy.json`.
Verify: unit tests from the spec's "Tests" list all green. Size M. Route hard.

**WP-1A Installer, autostart, firewall rule** → WP-18  
Goal: cargo-wix MSI (per-user install), Task Scheduler autostart at logon (no highest privileges),
firewall allow rules (TCP port, UDP range) scoped to tailnet CIDRs and generic CIDRs from config,
uninstall removes rules and task; unsigned build; `installer/README.md` documents signing as a
human step. Verify: `cargo wix` produces an MSI; install/uninstall in a Windows Sandbox or on this
machine under a test prefix is human-gated, so the automated verify is: MSI builds and
`installer/verify-rules.ps1` (Windows PowerShell 5.1 compatible) parses. Size M. Route cheap.

**WP-1B Integration harness** → WP-14  
Goal: `cargo xtask e2e-host` starts a host on loopback with a temp share (fixture tree), performs
pairing via the test hook, and runs the HTTP conformance suite from `protocol/openapi.yaml`
examples (every endpoint, every documented error code). Verify: `cargo xtask e2e-host` exits 0 and
writes `target/e2e/report.json`. Size M. Route std.

**WP-1C Docs sync** → all Phase 1  
Goal: `docs/context.md`, `CHANGELOG.md`, `CLAUDE.md` Commands, ADR status flips with evidence
lines; `README.md` honesty note about relays. Verify: links resolve (`cargo xtask docs-check`
checks relative links). Size S. Route cheap.

### Phase 1: Android client

**WP-20 Project scaffold** → WP-00 ‖ WP-01  
Goal: Gradle project (`client/android/`), Kotlin, Compose, minSdk 26, targetSdk/compileSdk 36,
AGP 8.5.1+ (latest stable), NDK r28+ set in Gradle (download via sdkmanager if needed), package
`fyi.arlo.gander`, dependency catalogue (OkHttp 5, Media3, Coil, `org.jitsi:webrtc`, CameraX,
ZXing core, DataStore, Room, Navigation), `dataExtractionRules`, lint baseline, unit test and
instrumented test source sets, CI workflow, ELF alignment check script. Verify:
`./gradlew assembleDebug testDebugUnitTest lintDebug` passes; `check-elf-alignment` reports all
`.so` aligned. Size M. Route cheap. Note: first Gradle run downloads ~1 GB; allow 20 minutes.

**WP-21 Network layer** → WP-20  
Goal: `GanderTrustManager` (SPKI pin), hostname verifier, per-host OkHttp client, bearer
interceptor, `HostEndpoint` with address fallback, error-code mapping to a sealed `GanderError`,
WebSocket client for `/events`, DTOs mirroring `protocol/openapi.yaml` (kotlinx.serialization).
Verify: unit tests with MockWebServer and two generated self-signed certs: pinned accepted, other
rejected, system-trusted rejected; DTO decoding of every example in the spec. Size M. Route hard.

**WP-22 Pairing screens** → WP-21  
Goal: Add host: QR scan (CameraX + ZXing) and manual entry; SAS screen; approval wait with
polling; success stores host + pin + token in the encrypted store; `FLAG_SECURE` while secrets
visible; copy per the pairing spec. Verify: unit tests for `gander://pair` parsing and SAS against
`protocol/test-vectors/sas.json`; instrumented test on the emulator pairs against the real host
(WP-13 test hook) via `10.0.2.2`. Size M. Route std.

**WP-23 Hosts, shares, browser** → WP-21  
Goal: Hosts screen with online ping and relay chip; Shares; Browser with list/grid, sort, kinds
filter, breadcrumb, cursor paging, search with partial indicator, Recent tab, multi-select;
listing snapshots in Room for offline display with "cached" chip. Verify: unit tests for paging
and sort persistence; Compose UI tests for list rendering with a fake repository; instrumented
listing against the host fixture tree. Size L. Route std.

**WP-24 Thumbnails** → WP-23  
Goal: Coil fetcher on the pinned client with visible-first priority, cancellation on scroll,
503-retry handling, type glyph fallback, density-based size. Verify: unit test of the fetcher's
retry logic; instrumented: thumbnails appear for fixture images within 5 s on the emulator.
Size S. Route cheap.

**WP-25 File sheet and default-action rules** → WP-23  
Goal: sheet with metadata (`stat?meta=1`), flags, three actions with hints and reasons from
capability/error codes, remembered default per extension, network-aware rule table from the
client spec. Verify: unit tests for the rule table (metered/unmetered, size thresholds,
stream-only types, missing capability). Size M. Route std.

**WP-26 Preview viewers** → WP-25  
Goal: pages pager with zoom and prefetch, image viewer, text viewer with encoding display, video
via Media3 on the pinned OkHttp data source. Verify: instrumented: PDF page 1 renders from the host;
video fixture plays 3 s. Size M. Route std.

**WP-27 Fetch engine and open-with** → WP-25  
Goal: foreground `dataSync` service, Room-persisted jobs, Range resume with `If-Range`, 412
restart, blake3/sha256 verification (pure-Kotlin BLAKE3 or sha256 fallback), notification
progress, `onTimeout` handling, `FileProvider` + `ACTION_VIEW` open-with, `ACTION_SEND` share,
cellular warning dialog, Keep offline (pin). Verify: unit tests with MockWebServer for resume,
412 and verification mismatch; instrumented: 50 MiB fixture downloads from the host with a forced
reconnect mid-way and verifies. Size L. Route hard.

**WP-28 Cache, offline, settings, app lock** → WP-27  
Goal: LRU cache with cap and pinned exemption, Storage screen, Settings (cellular rule, verify,
cap, default actions, biometric lock), diagnostics export (redacted), host details (rotate token,
forget). Verify: unit tests for eviction; Compose tests for settings persistence. Size M. Route cheap.

**WP-29 Emulator end-to-end** → WP-22..WP-28, WP-1B  
Goal: `cargo xtask e2e-android` starts the host on loopback, boots the `gander` AVD (if present),
installs the debug APK, runs the instrumented suite (pair, list, thumb, preview, fetch+resume,
revoke -> 401 handling). Verify: suite green; JUnit XML saved under `target/e2e/android/`. If no
AVD: mark `deferred: needs emulator` and run the unit suites only. Size M. Route std.

### Phase 2: Stream (Night 2+)

**WP-50 Stream engine** → WP-45, WP-17  
Goal: productionise the spike in `gander-stream`: capture -> encode -> WebRTC track with
repeat-last-frame keepalive (every 500 ms when no new frame), keyframe on PLI/FIR and on
reconnect, playout-delay extension min=max=0, bitrate from the estimator with battery cap, codec
negotiation (H.264 first), host-only candidates on bound addresses. Verify: loopback receiver
test measures frame cadence with a static window (keepalive proves itself) and adaptation under
throttling. Size L. Route hard.

**WP-51 Launch, window discovery, fit-to-phone** → WP-19, WP-50  
Goal: catalogue launch templates, `CreateProcessW`, window snapshot/diff, title match,
ApplicationFrameHost mapping, pick fallback, fit-to-phone math and `SetWindowPos`, restore
placement at end, splash-class skip list, browser isolated profile templates. Verify: tests with
`gander-testwin` scenarios (new process, re-used process via pipe, splash-then-main, two
candidates); fit math unit tests at 100/125/150/200 %. Size L. Route hard.

**WP-52 Input** → WP-51  
Goal: data channel receivers, normalised-to-screen mapping with live rect tracking, `SendInput`
mouse/keyboard/unicode, foreground acquisition with documented fallbacks, pause on focus loss and
on local user activity, `host.state` and `stream.state` events. Verify: testwin reacts to injected
clicks at 4 DPI settings; focus-loss pause test (another window brought to front by the test).
Size M. Route hard.

**WP-53 Session lifecycle and host visibility** → WP-52  
Goal: state machine with resume grace (120 s), `DELETE` semantics and `close_on_end` policies,
one-session rule, kill switch integration, tray streaming state, toasts, idle-user prompt,
`host_locked`/`host_unattended` preconditions, keep-awake during sessions, audit entries.
Verify: state-machine unit tests; integration: lock event simulated via the test hook ends/pauses
correctly. Size M. Route std.

**WP-54 Android stream viewer** → WP-21, WP-50  
Goal: `SurfaceViewRenderer`, local pinch-zoom/pan, Direct touch and Trackpad modes with the
documented gesture map, keyboard bar, local text box, state banner, pause on background, end
dialog, relay warning, "fetch the saved file" shortcut. Verify: Compose/gesture unit tests for
the mapping; instrumented: stream from the host's testwin renders and a tap changes the box colour.
Size L. Route std.

**WP-55 Audio** → WP-50  
Goal: per-process loopback capture via `wasapi`, Opus encode, WebRTC audio track, mute control,
silence when no stream. Verify: testwin plays a tone on command; the loopback receiver detects it.
Size M. Route std.

**WP-56 Clipboard text bridge** → WP-52  
Goal: `clipboard.get`/set over the control channel, host opt-in setting, text only. Verify: unit
and integration tests. Size S. Route cheap.

**WP-57 Phase 2 end-to-end and findings** → WP-53..WP-56  
Goal: `cargo xtask e2e-stream` with testwin, real-app checklist for a human (PowerPoint, Edge PDF
isolated window, Blender if installed), docs sync, ADR flips. Size M. Route std.

### Phase 3: breadth (later nights; design inputs already in the specs)

WP-60 SAF `DocumentsProvider` (read-only, proxy FD, read-ahead) · WP-61 Write shares (upload via
share sheet, rename, delete to host trash, SAF write modes, conflict = last writer wins + backup)
· WP-62 iOS client (SwiftUI; Files + Preview + Fetch first; Stream behind a review-risk flag) ·
WP-63 macOS host (ScreenCaptureKit, QuickLook thumbnails, `openat` resolver) · WP-64 Linux host
(portal/PipeWire attended capture, `openat2` resolver) · WP-65 Preview/Fetch service for headless
or pre-login hosts (new ADR; LocalSystem LocalAPI access) · WP-66 Windows Search index for
`search: "indexed"` · WP-67 deep-zoom tiles for large images and pages · WP-68 folder download as a
streamed zip on explicit request · WP-69 watch-only Stream and pen/stylus pointer injection.

### Phase 4: later / maybe

WP-70 Unattended Stream threat review and backend evaluation (indirect display driver vs RDP
RemoteApp/RAIL via FreeRDP) · WP-71 guest shares with expiry and away mode · WP-72 desktop-to-desktop
client · WP-73 Wake-on-LAN relay through another Gander host.

### Release engineering (starts in Phase 1, finishes before the first public release)

**WP-80 CI matrix** → WP-01: Windows host build/test/clippy/deny; Android unit/lint/ELF check;
docs link check; artifact upload of MSI and APK on tags. **WP-81 Signing** (human: Azure Trusted
Signing or OV cert; Play upload key). **WP-82 Supply chain**: `cargo auditable`, CycloneDX SBOM for
both, Dependabot, reproducible build notes. **WP-83 Security docs**: SECURITY.md final, disclosure
address, opt-in update check design (GitHub releases API, explicit consent at first run), threat
model refresh. **WP-84 Store readiness**: Play data-safety ("no data collected"), foreground-service
declaration video, F-Droid metadata (WebRTC from Maven Central only), winget manifest. **WP-85
External security review** (human). **WP-86 Beta**: closed track, feedback template, crash
diagnostics export instructions.

## 5. Gates

| Gate | Means | Verify |
| --- | --- | --- |
| G0 | Preflight passed, repo scaffolded, decision defaults recorded | `cargo xtask preflight`; `cargo build --workspace`; report section exists |
| G1 | Host core passes hostile corpus, listener never wildcards, pairing flows pass, Browse/Fetch API conformance | `cargo test --workspace`; `cargo xtask e2e-host` |
| G2 | Client builds, unit tests pass, ELF aligned | `./gradlew assembleDebug testDebugUnitTest lintDebug`; alignment check |
| G3 | Emulator end-to-end (or explicitly deferred for lack of an AVD) | `cargo xtask e2e-android` |
| G4 | Spike findings written with measured numbers; streaming stack decided or fallback recorded | findings file; ADR lines flipped |
| G5 | Stream end-to-end on testwin; policy tests; lifecycle tests | `cargo xtask e2e-stream` |
| G6 | Release readiness: signed builds, SBOM, SECURITY.md, store metadata, external review scheduled | human-confirmed checklist |

## 6. Human-gated backlog (the run must not attempt these)

- Confirming the licence and creating the GitHub remote; any push or publication.
- Plugging in or enabling debugging on a physical Android device; installing Tailscale in the
  emulator or logging in anywhere.
- Unlocking the machine (spike C6) and any test that requires the lock screen or a password.
- Buying or configuring code signing; Play Console, App Store and F-Droid accounts; domain and
  trademark follow-ups from ADR-0004.
- Installing software outside the preflight list (emulator images and `pwsh` are pre-approved
  if the owner ticked them).
- Deciding on Phase 3/4 scope changes, hosted/pro plans, or anything that changes `security.md`
  non-negotiables.
- Running the real-app Stream checklist (PowerPoint, Edge, Blender) and judging perceived latency.

## 7. Fallback rules (summary)

| If | Then |
| --- | --- |
| `windows-capture` cannot share its device with the encoder | own thin WGC wrapper (WP-41 fallback) |
| hardware MFT fails | software encoder for continuity; flag as blocking for Phase 2 |
| `webrtc` fails latency/adaptation thresholds | repeat with `str0m`; then record raw-H.264-over-TLS design |
| Office COM hangs or is absent | embedded thumbnail only; `preview: none` for pages; Stream suggested |
| no AVD | unit suites only; G3 deferred with the sdkmanager command for the morning |
| Gradle download or AGP incompatibility | pin the previous stable AGP; record versions |
| `NtCreateFile` relative open is blocked by the Rust bindings | temporary string-join implementation with the step-7 check, flagged as non-compliant |
| LocalAPI pipe refuses the process | CLI fallback; record the account/user context in findings |

## 8. Morning report template (`docs/reports/<date>-overnight-report.md`)

```
# Overnight report <date>

## Outcome in one paragraph
## Gates: G0 .. G6 with pass / fail / deferred and the command output tail for failures
## Decision defaults applied (D1..D14) and ADRs flipped
## Work packages: table of id, status (done / deferred / skipped), commit, verify result, notes
## Measurements (from spikes): the matrix rows that ran, numbers, screenshots paths
## Problems hit and how they were resolved or worked around
## Human actions needed next, in order of value
## Files changed (summary), branches created
## Machine state restored? (power settings, processes) yes/no with evidence
```

## 9. Conductor routing hints

Rust core and security packages (WP-11, 12, 13, 15, 19, 21, 27, 41-43, 50-52): `hard`. UI and
screens (WP-18, 22-26, 54): `std`. Scaffolding, docs, CI, installer (WP-01, 1A, 1C, 20, 24, 28,
40, 47, 80): `cheap`. Give each worker exactly one package, its spec, the protocol spec, and the
verify command; require a structured result with the verify output. Workers must not edit specs;
spec problems are reported back and resolved by the Conductor in a docs-only commit.
