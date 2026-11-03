# Context -- current state

_Last updated: 2026-11-03 (0.1.1 installed and pairing verified on real devices; public history rewritten)_

## Session wrap-up (2026-11-03, afternoon)

- **Public GitHub history rewritten** with owner approval: `main` on GitHub is one commit
  (`6ced65c`, "feat: Gander 0.1.0", authored by the owner). The old history is only in the
  gitignored `docs/local/history-backup-2026-11-03.bundle` and must never be pushed. Dependabot's
  stale branches were deleted; GitHub keeps closed PR refs (`refs/pull/1-7`) until Support purges
  them. Commits use the owner's identity (repo-local git config) and carry no AI attribution lines.
- **0.1.1** (host only; the Android app stays 0.1.0), `dist/Gander-0.1.1-x64.msi`, upgrades 0.1.0:
  - MSI launch condition read `WindowsBuild`, which Windows Installer caps at 9600, so 0.1.0
    refused to install on Windows 11; it now reads `CurrentBuildNumber` from the registry.
  - The pairing QR addresses were read once at start; a host started before Tailscale showed a QR
    with no address ("pairing code is damaged" on the phone, only the port in the manual card).
    They now follow the listener plan, and the window explains when there is no address.
  - Release builds trim local paths from C code too (`/d1trimfile:`), 0 local paths in the exes.
- **Verified by the owner on real hardware:** the MSI installs on this laptop (Windows 11 build
  26200); a phone and the tablet run the 0.1.0 debug app and pair with the installed host.
- **Local commits not yet pushed:** `5cc0576` (installer launch condition) and `5bdbc4a` (QR
  addresses, 0.1.1) on top of the public `6ced65c`. Push only when the owner asks.
- **Next for the owner:** Preview, Fetch and Stream through the installed host from the phone and
  tablet; the unlocked attended runs (`cargo xtask e2e-stream --android`, the ignored input and
  clipboard tests, `docs/specs/2026-11-02-stream-real-app-checklist.md`); code signing; ADR-0003.

## After Night 2 (2026-11-03) -- owner decisions applied

- **Version 0.1.0** (Cargo workspace, Android `versionName`); `CHANGELOG.md` stamped `## 0.1.0`
  (superseded for the host by 0.1.1 the same day, see above).
- **`docs/security.md` approved:** rule 13 (clipboard), rule 14 (stream sound), the two residual
  risks, and the locked-session wording (the host enforces it, Windows does not).
- **"with sound"** wording in the indicators.
- **libopus** via `opusic-c` 1.6.1 replaces `rusty-opus`; CMake 4.4.4 installed machine-wide and
  required to build.
- **Settings window on the integrated GPU** by default, `[ui] gpu = "integrated" | "discrete" |
  "auto"`; the NVIDIA export is raised only while the stream encoder is set up. The owner expects
  heavier future UI work may need the discrete GPU: the setting is the switch.
- **Release hygiene:** `--remap-path-prefix`, `/Brepro`, and MSVC `/d1trimfile:` for C code; the
  release exes and the MSI contain no local folder.
- **Public repo privacy:** nothing tracked names a real user profile, device, address or account;
  machine paths live in the gitignored `docs/local/machine.md`. The owner approved rewriting the
  public GitHub history so the earlier commits (which did name them) are gone.
- **Repo assets:** the Lens logo PNGs in `brand/png/` and the 96 finalized mockup images in
  `docs/design/mockups/png/` (`cargo xtask mockups`).
- **Still owner-gated:** the unlocked attended runs (`cargo xtask e2e-stream --android`, the ignored
  input and clipboard tests, the real-app checklist), code signing, ADR-0003.

## Night 2 (2026-11-02 to 03) -- read `reports/2026-11-03-night2-report.md` first

Local `main` (at the time not pushed) had the device bug fixes, Lens logo, slim host,
WiX 5 MSI (then 0.0.0), the full UI redesign of both apps, WP-55 audio, WP-56
clipboard, WP-57 e2e-stream, bring-to-front, WP-60 Files-app provider, WP-66 Windows Search, WP-67
deep zoom, WP-68 folder zip and WP-82 supply chain. Gates on 8104e68: 1055 Rust tests, e2e-host
166/0, e2e-android 160/0/0. Owner decisions pending: security.md text (clipboard, sound, lock
wording), version number, "with sound" wording, settings window on iGPU, release path remapping.
Unverified until an unlocked attended run: live input, host stream session, real clipboard,
`e2e-stream --android`.

Sound is now encoded by the reference libopus (`opusic-c` 1.6.1, bundled libopus built with CMake
4.4.4 and MSVC) on `feat/libopus`, replacing `rusty-opus`; this closes the "rusty-opus review"
decision. CMake is a build requirement from that branch on; the owner approved installing it.

## State

- Planning complete to implementation depth. Night 1 (2026-11-01 to 02) built the host core, the
  Android client and the Phase 0 spikes on branch `night-1`; gates G0-G4 passed, then the Phase 2
  attempt built the stream engine, launch, input, lifecycle and the Android viewer (WP-50..54);
  WP-55 audio and WP-57 `e2e-stream` are not started (see "Night 1 results"). WP-56 (clipboard
  text bridge: opt-in `[stream] clipboard`, text only, 64 KiB, gated like input, audited, with a
  notification; keyboard-bar actions on the phone) is built on `feat/wp-56-clipboard`; its real
  Windows clipboard test (`cargo test -p gander-win --test clipboard -- --ignored --test-threads 1`)
  needs an unlocked session and has not been run yet. No repo remote yet; `main` has not been
  merged into.
- 2026-11-01: full plan review with web research and machine measurements
  (`research/2026-11-01-plan-review.md`). Seven implementation specs written in `specs/`, a shared
  hostile path corpus in `protocol/test-vectors/paths.json`, three new ADRs (0005 transport, 0006
  stream launch policy, 0007 process model), ADR-0001/0002 accepted by default, and an executable
  master plan in `plans/2026-11-01-master-plan.md` with preflight, decision defaults, work packages,
  gates, fallbacks and a morning-report template.
- Name settled: **Gander** (ADR 0004). Folder still named `Gander` on disk (was `OTG View`).
- Intended home: a public GitHub repo under the owner's org (`<org>/gander`), not yet created.

## Decision defaults in force (override here before a run if you disagree)

Rust host with `tray-icon` + `egui`; native Kotlin + Compose Android client; OS VPN client plus
`tailscale`/`generic` trust adapters; Apache-2.0 (not published yet); pinned TLS + HTTP/1.1 + WS
on port 42637 with WebRTC host-only candidates; stream launch policy by handler executable with
owner approval; user-session tray app with isolated render workers; read-only v1; no Stream while
locked; one stream at a time; Windows 11 minimum for Stream. Full table: master plan section 2.

## Owner approvals (2026-11-01, evening)

- **Decision defaults D1-D14: all accepted.** The run flips ADR-0001, 0002, 0005, 0006, 0007 to
  Accepted at kickoff; ADR-0003 stays Proposed with Apache-2.0 as the working default.
- **Installs pre-approved:** Android emulator + `system-images;android-36;google_apis;x86_64`
  (+ `platform-tools`, AVD `gander`), and PowerShell 7 via winget. LibreOffice NOT approved.
  Nothing else may be installed.
- **Night 1 scope: everything, including a Phase 2 attempt** after gates G0-G4 pass in order.
- **Device: emulator only for now.** A physical phone will be used later; device-gated packages
  are deferred, and the run must not `adb connect` to tailnet devices.
- Repo: local git on `main` with the planning corpus committed on 2026-11-01. No remote. Workers
  integrate on `night-1`; `main` is not merged into without the owner.
- **Machine: the laptop stays on and awake overnight** (owner, 2026-11-01). Capture tests may run
  at any time; the run should still restore any power setting it changes.

<!-- Owner overrides: add a dated block here if anything above changes before a run. -->

## Night 1 results (2026-11-02)

Workers integrated on `night-1`; nothing pushed, no remote created.

| Gate | Result | Evidence |
| --- | --- | --- |
| G0 | PASS | Preflight passed and the workspace was scaffolded (`reports/2026-11-01-preflight.md`) |
| G1 | PASS | `cargo test --workspace`: 516+ tests pass. `cargo xtask e2e-host`: 123 passed, 0 failed, 11 not_reachable (each listed with its reason) |
| G2 | PASS | `./gradlew assembleDebug testDebugUnitTest lintDebug` green; `cargo xtask check-elf-alignment` on the debug APK: aligned (16 KB pages) |
| G3 | PASS | `cargo xtask e2e-android`: 117 passed, 0 failed, 0 skipped |
| G4 | PASS | `specs/2026-11-01-stream-spike-findings.md`: 22 matrix rows (one per C-item), thresholds scored, decisions recorded. ADR-0001's streaming-library line and ADR-0005's D12 line are Accepted |

Streaming stack, decided by the Conductor under the D12 default rule (override here if you
disagree):

- **WebRTC: `webrtc` (webrtc-rs) 0.21.x, accepted with conditions**: a delay-based bandwidth
  estimator (its GCC reacted to loss only: 5.5 s to adapt with a 2 s bottleneck buffer, 2.5 s with
  200 ms), the RTCP/PLI interceptor, liveness and pause messages over the data channel, and trickle
  ICE or empty-candidate tolerance. `str0m` 0.24.1 met every threshold but is blocked by
  `cargo deny` (RUSTSEC-2026-0387 via dimpl 0.7); re-evaluate when it ships on dimpl >= 0.9. Raw
  H.264 over TLS is not needed.
- **Capture:** own thin WGC wrapper over the `windows` crate (`windows-capture` cannot share a
  device). **Encode:** Media Foundation hardware MFT, one device on the encoder's adapter by LUID,
  `NvOptimusEnablement`, `timeBeginPeriod(1)`, re-create the Intel encoder to change bitrate,
  H.264 level 5.0 (`42e032`). **Input:** `SendInput` when the target is uncovered and foreground,
  never while locked.
- **Security finding (needs your decision; `docs/security.md` was not edited):** WGC keeps
  capturing and `PostMessage` keeps injecting while the session is locked, so the OS does not
  enforce "Stream only works on an unlocked session"; the host does, through `session::watch`
  (WP-17), failing closed. The findings file proposes replacement wording.
- Measured on one machine (build 26200) with an emulator only: glass-to-glass, cellular and every
  real-application item are open (Next actions below and the findings file).

### Phase 2 and the rest of the night (after G4)

Same rules: integrated on `night-1`, nothing pushed. Each line is what exists and what it does not
yet prove; the CHANGELOG has the detail.

- **WP-50 stream engine (`host/gander-stream`):** `StreamEngine` / `Session`. WGC capture on the
  encoder's adapter, D3D11 video processor to NV12, Media Foundation hardware H.264, webrtc-rs 0.21
  track with the accepted conditions built in: a delay-based estimator (output under a 1.5 Mbit/s
  cap in about 1.1-1.5 s with 200 ms and 2 s bottleneck buffers), PLI/FIR interceptor, 450 ms
  keepalive for a static window, liveness and pause messages, trickle ICE, and the session-lock
  guard (ends the session within 1 s, fail closed). Real-pipeline tests are `--ignored`.
- **WP-51 launch and discovery:** the launch command comes only from a policy-checked handler plus
  the file as one argument; window discovery diffs `EnumWindows` snapshots (new process, re-used
  process, splash then main, several candidates) and fit-to-phone math restores the placement.
  `host/gander-testwin/tests/discovery.rs` covers those cases and fit/restore at 150 % and 100 %.
- **WP-52 input:** UI-Events-to-scan-code mapping, normalised-to-`SendInput` coordinates (0 px error
  at 100/125/150/200 % on synthetic multi-monitor desktops), desktop shortcuts refused, pause
  gate (locked, phone background, focus lost, local user active). **Live tests are deferred:** the
  session was locked, and `SendInput` is not injected while locked by design. Run them unlocked and
  attended: `cargo test -p gander-testwin --test input -- --ignored --nocapture --test-threads 1`.
- **WP-53 session lifecycle and host visibility:** `POST /v1/stream` now returns 201 and
  `DELETE /v1/stream/{session}` 204. State machine with the one-session, locked and unattended
  checks, approval and "someone is using the computer" prompts, window pick, 120 s resume grace,
  `close_on_end`, keep-awake while streaming, audit events, tray state and a Stop toast.
  `cargo xtask e2e-host` is now **129 passed, 0 failed, 7 not_reachable of 136** (it covers 201,
  204, `stream_busy`, `host_locked` and `host_unattended` against a real `gander-testwin` window).
- **WP-54 Android stream viewer:** viewer screen, libwebrtc receive path with liveness and
  renegotiation, pinch zoom and pan, Direct touch and Trackpad modes, keyboard bar, end dialog.
  JVM tests pass. `StreamViewerTest` and `StreamViewerFixtureTest` compile; **the instrumented run
  is deferred to WP-57.** Conductor decision at integration: scroll follows the Windows wheel
  convention end to end (positive `dy` is the wheel turned away from the user).
- **WP-1A installer, autostart, firewall:** per-user WiX 5 MSI source, Task Scheduler logon task
  behind `gander-host --autostart on|off|status`, firewall scripts (tailnet CIDRs only, refuse
  `Any` and `/0`), third-party notices, `cargo xtask installer-check` (in `test-all`). **The MSI is
  built** (`cargo xtask installer-build` -> `target/installer/Gander-0.0.0-x64.msi`, 7.7 MiB,
  unsigned, `wix msi validate` clean, files listed by an administrative extract); **it has not been
  installed anywhere** (owner step, see `installer/README.md`). The Start menu entry runs
  `gander-host --open`; the host keeps one copy per data directory.
- **Lens logo (2026-11-02):** the owner's pick (a goose head inside a magnifying-glass ring) is
  `brand/gander-lens.svg` with one-colour, reversed, small-size, app-icon, tray-state and lockup
  variants; `cargo xtask icons` renders the committed `.ico`, window and tray PNGs; the host exe,
  tray and windows and the Android launcher and notification icons use it.
- **Flaky Windows tests fixed:** `gander-win` lib tests crashed about 1 run in 15 with
  `STATUS_ACCESS_VIOLATION` (`AssocQueryStringW` called from threads with no COM apartment; COM
  unloaded a server the shell still held). Fixed with a crate-private `ComScope` that pins the MTA
  for the process; Office COM cleanup and its tests were hardened in the same pass.
- **WP-1C docs sync:** this file, `CHANGELOG.md` regrouped, `CLAUDE.md` status and commands,
  README relay note, ADR-0002 and ADR-0007 evidence lines, and `cargo xtask docs-check` (relative
  links and anchors; in `test-all` and CI as `docs.yml`).

## Next actions (in order)

_The current next steps are in "Session wrap-up" at the top; the list below is the Night 1 plan, kept
for the record (items 1 and 2 are done)._

1. **Owner, morning after:** read `reports/2026-11-02-overnight-report.md`, then decide the
   human-gated items under "Open questions", starting with the `security.md` correction (it changes
   what Phase 2 may promise).
2. **Night 2 (autonomous):** WP-55 audio (per-process loopback capture, Opus track, mute, silence
   when idle), WP-57 `cargo xtask e2e-stream` with
   `gander-testwin` plus the human real-app checklist (PowerPoint, Edge PDF in an isolated window,
   Blender if installed), docs sync and ADR flips. Also run what was deferred: the live input tests
   (`cargo test -p gander-testwin --test input -- --ignored --nocapture --test-threads 1`, needs an
   unlocked, attended session) and the instrumented `StreamViewerTest` / `StreamViewerFixtureTest`
   against the host (`POST /v1/stream` exists now).
3. **Owner at the machine (human-gated spike items):** C6 lock-screen spike (lock with a stream
   running, then unlock; also run `spike-input matrix` while locked to see what `SendInput`
   returns); C8 lid closed; C7 and C21 display and power state (read `powercfg` before, restore
   after); the on-screen border half of C10; C22's Task Scheduler half.
4. **Owner with a physical phone on the tailnet (device-gated):** real-device glass-to-glass (the
   two-screen photograph; counter and clock are already on both screens), C13 on 5G and on a DERP
   path, C14 on a real phone, hardware decode and jitter-buffer time. A run must not `adb connect`
   to tailnet devices on its own.
5. **Human-attended real-application run:** C20 on Notepad, PowerPoint and Edge (the C20 real-app
   run), C15 with PowerPoint, C16 Edge PDF, C17 Photos, C18 DRM, document-content bitrate.
6. **Installer, owner:** install the MSI from `target/installer/` (built by
   `cargo xtask installer-build`; the human-gated install test is in `installer/README.md`), then
   the live autostart and firewall checks. The install/uninstall run under a test prefix and code
   signing stay human steps.
7. Name follow-ups still open: domain, trademark search (classes 9/42), GitHub repo creation.

## Open questions for the owner (defaults are taken if unanswered)

- Fetch handoff: built-in viewers for preview types plus the system resolver otherwise. (default)
- Write access in v1: no. (default)
- Stream while locked: no until a Phase 4 threat review. (default)
- Multi-user: paired guest devices are already possible via per-device shares; expiry/away mode
  in Phase 4. (default)
- Licence: Apache-2.0 unless a hosted/pro plan exists, then AGPL for the host. (default Apache)

Needs the owner (no default is taken; each blocks something named here):

- **`docs/security.md` correction (human-gated):** it says the OS ensures Stream only works on an
  unlocked session and that injection is dropped while locked. Measured on build 26200: WGC keeps
  capturing and posted messages keep arriving while locked; the host enforces the rule itself,
  failing closed. Proposed wording is in the findings file, "Security-relevant findings".
- **C6 lock-screen spike, real-device glass-to-glass and the C20 real-app run:** Next actions 3 to
  5; the findings file lists exactly what each one closes.
- **Licence confirmation:** ADR-0003 stays Proposed; Apache-2.0 is the working default.
- **Remote creation:** the GitHub repository under the org; nothing has been pushed.
- **MSI toolchain installed (2026-11-02):** the .NET SDK 8.0.427 (winget `Microsoft.DotNet.SDK.8`,
  machine-wide in `C:\Program Files\dotnet`, after the owner's UAC click), WiX 5.0.2 and its UI and
  Util extensions (`dotnet tool install --global wix`, `wix extension add -g`, under
  `%USERPROFILE%\.dotnet` and `.wix` of the real profile). Nothing else was installed.
- **`deny.toml` licence exceptions (WP-18):** per-crate exceptions for egui's bundled fonts
  (`epaint_default_fonts`: OFL-1.1, Ubuntu-font-1.0) and for `clipboard-win` and `error-code`
  (BSL-1.0, the Boost licence). Confirm them; shipping the fonts needs their licence texts in the
  installer's third-party notices.
- **`androidx.biometric` alpha pin:** `1.4.0-alpha05` is the newest line that compiles against
  compileSdk 36 (alpha06 and later need 36.1). Accept an alpha for the app lock, or wait for stable.
- **TLS 1.3 against minSdk 26:** the host is TLS 1.3 only, and the Android platform provides
  TLS 1.3 from Android 10 (API 29). Either bundle Conscrypt (APK size and an update burden) or raise
  minSdk to 29 (drops Android 8 and 9). The emulator runs API 36, so the gap stays invisible until
  an older device connects.
- **Minimum Windows build for Stream:** Windows 11 (22000+) is the default; only build 26200 was
  measured. Accept the default, or ask for a run on a second build.

## Environment notes for whoever runs this next

Measured 2026-11-01: Windows 11 build 26200, hybrid Intel + RTX 5060 laptop, two monitors; Rust
1.96, JDK 17, Node 24, Android SDK with platform 36 but no emulator images; no `pwsh` 7; Office
installed; Tailscale 1.104 logged in with two Android devices on the tailnet (the tablet is
usually DERP-relayed); `.pdf` opens in Edge. Details: plan review section 8. Installed during
Night 1 from the pre-approved list: Android emulator 37.2.13, the `android-36` Google APIs image and
the `gander` AVD, PowerShell 7.6.9, `cargo-deny` 0.20.2. Installed later (2026-11-02) for the MSI:
.NET SDK 8.0.427, WiX 5.0.2 and its UI and Util extensions. Not installed: cargo-wix (no
longer used), LibreOffice.
