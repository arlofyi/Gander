# Overnight report 2026-11-02 (Night 1)

## Outcome in one paragraph

Gander went from a planning corpus to working code on the integration branch `night-1`. It has
the Rust host: path resolver, pinned-TLS listener, pairing, Browse/Fetch/Preview API, events,
tray UI and installer sources. It has the Android client: pairing, browser, thumbnails, previews,
resumable verified fetch, settings and app lock. All five Phase 0 stream spikes are done, and
Phase 2 is built through the stream engine, launch, input, lifecycle and the Android viewer
(WP-50..54).

- **Gates:** G0-G4 all pass. G3 is the emulator end-to-end suite: Phase 1 is fully green; see the
  G3 row for the Phase 2 viewer tests. G5 (e2e-stream, WP-57) was not attempted.
- **Size:** 191 commits, 763 files, about 175k added lines.
- **Rust:** 856 tests pass. Host conformance (`cargo xtask e2e-host`) is 124-129 passed / 0 failed.
- **Android:** 799 JVM tests pass.
- **Deferred:** WP-55 (audio), WP-56 (clipboard) and WP-57 (e2e-stream) were not started. The MSI
  build is deferred (WiX not installed), and so are the live input tests (the session was locked).
- **Two security-relevant measurements** need your decision on `docs/security.md`.
  - Windows Graphics Capture keeps capturing, and `PostMessage` keeps injecting, while the session
    is locked. "No Stream while locked" is therefore enforced by Gander itself, fail-closed through
    `session::watch`, not by the OS as `security.md` currently says.

## Run notes (read first)

Why this run differed from a normal overnight run.

- **No Conduit `fleet_*` tools in this session.** Workers ran as Claude subagents on git worktrees
  (`.worktrees/`, gitignored). The Conductor integrated `night-1` itself. All worktrees have been
  removed; the feature branches remain.
- **Usage limit, twice.** The 5-hour usage limit stopped every worker at once, around 00:50 and
  around 07:50 local. Each was resumed from where it stopped when you said "continue". The run
  finished in the afternoon of 2026-11-02, not overnight.
- **The laptop locked** for long stretches despite the power plan. This blocked live input tests
  only.

## Gates

| Gate | Result | Evidence |
| --- | --- | --- |
| G0 | PASS | `cargo xtask preflight` passed (all hard checks); workspace builds; decision defaults recorded (ADR flips `aeb8d25`) |
| G1 | PASS | `cargo test --workspace` 856 passed / 0 failed (final, `8aa8420`); `cargo xtask e2e-host`: 124 passed, 0 failed, 12 not_reachable of 136; error codes 22 covered, 0 missing. With an attended, unlocked session WP-53 measured 129 / 0 / 7: the stream cases fail closed when the host is locked or unattended, as designed |
| G2 | PASS | `./gradlew assembleDebug testDebugUnitTest lintDebug assembleDebugAndroidTest` green; 799 JVM tests; `check-elf-alignment`: 20 libraries, none fails 16 KB |
| G3 | PASS | `cargo xtask e2e-android` on AVD `gander` (API 36), final run on the `fix/stream-viewer-tests` tip (merged as `d36de3b`): 136 passed, 0 failed, 2 skipped across 22 classes. The 2 skips are the named deferral `StreamViewerFixtureTest`, which needs WP-57 `e2e-stream`. Covers real-host pairing, list, thumbs, PDF/text/video preview, 50 MiB fetch with forced reconnect and BLAKE3, revoke giving 401, and the stream viewer screens. The first final run (`8aa8420`) found 5 failures in WP-54's `StreamViewerTest`, never run on an emulator before. Two were real app bugs, now fixed: a tap read as a long press (right click) because the gesture timer mixed time bases, and the end dialog's "Keep streaming" button overlapped "Close". Three were test bugs |
| G4 | PASS | `docs/specs/2026-11-01-stream-spike-findings.md`: 22 of 22 matrix rows (pass 8, partial 6, deferred-device 1, deferred-human 2, skipped 5, fail 0). Stack decided: ADR-0001 streaming line and ADR-0005 Accepted |
| G5 | not attempted | WP-57 `cargo xtask e2e-stream` not started |
| G6 | human | release readiness |

## Decision defaults applied (D1..D14) and ADRs flipped

- **Defaults:** D1-D14 applied as approved by the owner on 2026-11-01. No overrides block was
  present.
- **First docs commit:** ADR-0001 (host and client lines), 0002, 0005, 0006 and 0007 flipped to
  Accepted. ADR-0003 stays Proposed, with Apache-2.0 as the working default (`LICENSE` written,
  nothing published).
- **After G4 (WP-45):** ADR-0001's streaming line and ADR-0005 (D12) flipped to Accepted.
  - webrtc-rs 0.21 is accepted with conditions: a delay-based estimator, a PLI interceptor,
    data-channel liveness, and trickle ICE or tolerance of empty answers. All four are now built in
    WP-50/53/54.
  - Capture uses our own WGC wrapper. Encode uses the Media Foundation hardware MFT. Input uses
    SendInput only when unlocked, uncovered and foreground.
  - The str0m fallback met every threshold, but `cargo deny` blocks it (RUSTSEC-2026-0387 via
    dimpl 0.7). Re-check it when str0m moves to dimpl 0.9 or later.
- **Evidence lines** added to ADR-0002 and ADR-0007.

### Decisions the Conductor made during the run (override if you disagree)

- **Scroll sign:** the Windows wheel convention. Positive `dy` means the wheel turns away from the
  user and the view moves up. The host and the client disagreed; both are now pinned by tests.
- **New protocol message:** `stream.pick {session, index}`, so the phone can answer a window pick.
- **Other protocol additions:** `X-Gander-Text-Next`, thumb `priority`, server `error` event, and
  control-channel messages (`ping`/`pong`/`pause`/`resume`/`keyframe`/`front`/`input.refused`).
  These are recorded in `openapi.yaml` and in code docs, but not yet in the protocol spec.
- **Cache caps:** thumbnails and previews get 1 GiB each.
- **Stream UDP:** bound only inside `[firewall] udp_ports` (default 50000-50099).

## Work packages

Branch for each package: `feat/<wp>-*`. All of them are merged into `night-1`.

| WP | Status | Verify / notes |
| --- | --- | --- |
| 00 + 01 | done | preflight report; workspace, CI, LICENSE, SECURITY.md (draft), xtask |
| 10 | done | DTOs, config, `openapi.yaml`; `protocol_examples_roundtrip` |
| 11 | done | 62 hostile vectors: 57 pass, 5 SKIPPED (symlinks need privilege; OneDrive placeholder; appexeclink). Real `NtCreateFile` relative walk, no fallback |
| 12 | done | ring-based TLS 1.3, SPKI pin; 2048-case "never wildcard" proptest; OS listening-table check |
| 13 | done | flows A and B, MITM gives a different SAS, 429 / 404, revoke closes the WebSocket in under 1 s; `sas.json` (SAS input is the DER SPKI bytes) |
| 14 | done | Browse / Fetch / hash; `/list` of 10k entries in 25 ms cold, 14 ms warm (release) |
| 15 | done | render worker, thumbnails, PNG bomb and malformed PDF contained |
| 16 | done | previews; PowerPoint slide 1 in 3.3-3.6 s, Word 4.7 s, Excel 3.5 s; no LibreOffice |
| 17 | done | `/v1/events`; `share.changed` in 511 ms; session lock watcher, fail-closed |
| 18 | done | tray plus 6 windows, `--ui-smoke` OK, offscreen screenshots in `docs/specs/assets/ui/` |
| 19 | done | stream policy; 59 vectors; real associations refuse scripts and executables |
| 1A | partial | autostart, firewall scripts (parse plus dry run only), notices, `installer-check`. **MSI build deferred** (WiX not installed or approved) |
| 1B | done | e2e-host harness. Found and fixed 2 host bugs: thumbnails and previews always returned 500 (handle access mask), and some errors came back as plain text |
| 1C | done | `cargo xtask docs-check` (0 broken), CLAUDE.md, README relay note, CHANGELOG regrouped |
| 20 | done | AGP 9.5.0, Gradle 9.6.0, Kotlin 2.4.21; libraries pinned to the last versions that build against compileSdk 36 |
| 21 | done | pinned OkHttp; every `openapi.yaml` example decodes |
| 22 | done | QR and manual pairing, SAS against vectors, Keystore token store |
| 23 | done | hosts, shares, browser, offline snapshots |
| 24 | done | Coil thumbnails, retry on 503, visible-first ordering |
| 25 | done | file sheet and default-action rules |
| 26 | done | pages pager, image, text, video (Media3 on the pinned client) |
| 27 | done | resumable fetch, 412 restart, BLAKE3 (35 official vectors), FileProvider |
| 28 | done | LRU cache, settings, biometric app lock, redacted diagnostics |
| 29 | done | `cargo xtask e2e-android` |
| 40 | done | `gander-testwin` |
| 41 | done | capture spike (own WGC wrapper) |
| 42 | done | encode spike: dGPU 2.4 ms per frame, iGPU 8.7 ms (misses the < 6 ms threshold) |
| 43 | done | WebRTC spike (webrtc-rs vs str0m) |
| 44 | done (partial live) | Android receiver at 28 fps, 65 of 66 taps hit, 118-142 ms warm; glass-to-glass deferred (no device) |
| 45 | done | findings and ADR flips |
| 46 | done | identity spike |
| 47 | done | Files-app spike |
| 50 | done | stream engine: keepalive gap 454 ms or less; adapts to the cap in 1.1-1.5 s with both a 200 ms and a 2 s bottleneck buffer |
| 51 | done | launch, discovery, fit-to-phone; 8 testwin scenarios |
| 52 | done (live deferred) | input; the session was locked, so nothing was injected. 6 live tests are `#[ignore]` |
| 53 | done | lifecycle; `POST`/`DELETE /stream`, busy / locked / unattended now covered in e2e-host |
| 54 | done (instrumented pending) | Android viewer; see G3 |
| fix-flaky | done | root cause of the gander-win crash: COM MTA teardown unloaded shell DLLs (a product bug). Office tests made robust |
| 55, 56, 57 | not started | Night 2 |

## Measurements (from the spikes)

Full data: `docs/specs/2026-11-01-stream-spike-findings.md`,
`docs/specs/2026-11-01-identity-spike-findings.md` and
`docs/specs/2026-11-01-files-app-spike-findings.md`.

- **Capture (WGC):** about 56 fps on the Intel and NVIDIA adapters. No pixels from covering windows
  leak in.
- **Capture gaps:** modal dialogs are not captured, and nothing is captured while the window is
  minimised.
- **Capture while locked:** capture continues while the session is locked.
- **Encode (Media Foundation hardware), 1080x2280 H.264:** dGPU 2.4 ms p50; iGPU 8.7 ms.
- **Encode conditions:** NVIDIA needs the `NvOptimusEnablement` export, and Intel needs
  `timeBeginPeriod(1)`.
- **WebRTC on loopback:** capture to receive takes 7 ms p50.
- **Adaptation:** the production estimator meets the 5 s rule; the spike's congestion control took
  5.5 s with a 2 s buffer.
- **Android emulator:** 28.4 fps, 0 frames dropped. Tap to changed pixels takes 118-142 ms warm
  and 300-800 ms cold.
- **Input:** maps normalised coordinates to window pixels with 0 px error at 150 % and 100 %.
  `PostMessage` reaches the window while the session is locked or the window is covered.
- **Browse:** `/list` of 10k entries in 14-25 ms.
- **Previews:** a PowerPoint slide in about 3.5 s.
- **Tailscale LocalAPI:** works unelevated only with identification QoS. Its DACL allows
  BUILTIN\Users, not only the logged-in user as the specs say.
- **Files app:** playback reads about 60 times a second, 4-28 KiB each. A shared handler thread
  stalls `open` by 4 s.

## Problems hit and how they were resolved

- **Kotlin DTO drift.** A Rust-side `openapi.yaml` change broke the Kotlin `ProtocolDtoTest` twice
  (WP-19, WP-53). Fixed both times, and the Conductor now runs Gradle after every protocol merge.
- **Merge conflicts.** Shared files (`lib.rs`, `Cargo.toml`, `main.rs`, `GanderNavHost.kt`)
  conflicted because packages ran in parallel; each was resolved by hand. Duplicate workspace
  dependency keys broke cargo twice.
- **Flaky gander-win crash.** Found to be a real COM lifetime bug and fixed: 0 crashes in 200 runs
  afterwards.
- **Office tests and stray Office processes.** The Office tests hung or left automation PowerPoint
  instances running. The cleanup, the quit deadline and the serialisation are fixed. On a loaded
  machine, starting PowerPoint can still exceed the plan's 30 s budget.
- **Emulator HTTP 400 (`bad_host`).** The emulator's `Host: 10.0.2.2` header was refused. It is now
  allowed in test mode only.
- **Disk space.** Free space fell from 71 GB to 18 GB. Worktrees were removed after each merge and
  incremental build caches were cleared; 35 GB was free at the end.
- **Gradle daemons.** A shared daemon killed a parallel build. All builds now run with
  `--no-daemon`.
- **Stray PowerPoint.** A `taskkill` of a stray automation PowerPoint was denied by the permission
  classifier. It had exited, or you closed it, by 10:50.

## Human actions needed next, in order of value

1. **Review `night-1`.** If you are happy with it, merge it to `main` (I did not), then create the
   remote (human-gated).
2. **Run the live input tests unlocked and attended.**
   `cargo test -p gander-testwin --test input -- --ignored --nocapture --test-threads 1`.
   Optionally also run the GPU pipeline tests:
   `cargo test -p gander-stream --test pipeline -- --ignored --test-threads=1`.
3. **Decide on the `docs/security.md` correction** about locked sessions. Proposed wording is in the
   findings Summary.
4. **Approve or refuse the `deny.toml` exceptions:** OFL-1.1 and Ubuntu-font-1.0 (egui's bundled
   fonts), BSL-1.0 (`clipboard-win`, `error-code`), and four "unmaintained" advisory ignores.
5. **TLS 1.3 vs minSdk 26.** Android only has TLS 1.3 from API 29. Either bundle Conscrypt for API
   26-28 or raise minSdk to 29.
6. **`androidx.biometric` is pinned to `1.4.0-alpha05`.** Newer releases need compileSdk 36.1;
   accept it or bump compileSdk.
7. **MSI.** `winget install --id WiXToolset.WiXToolset -e`, then `cargo install cargo-wix --locked`,
   then `cargo xtask installer-build`. Then the install test in `installer/README.md`. Also decide
   the UpgradeCode GUID and the Manufacturer name.
8. **Lock-screen spike (C6), a real device and the C20 real-app checklist.** Glass-to-glass
   latency, the real hardware decoder, and Stream with PowerPoint, Edge and Notepad.
9. **Confirm the run's choices:** the phone shortcut filter (Windows key, Alt+Tab and others are
   refused), the Office 30 s budget under load, the share root behaviour with RedirectionGuard (a
   junction made by a non-admin cannot be shared), licence (ADR-0003), and SECURITY.md wording.
10. **Delete about 87 leftover `gander-lo-*` and `gander-wp17-watch-*` folders in `%TEMP%`.**
    Deletion was blocked for me.

## Files changed (summary), branches created

- **`host/`:** gander-core, gander-win, gander-stream, gander-render, gander-host, gander-testwin,
  gander-e2e.
- **`client/android/`:** the full Kotlin and Compose app.
- **`protocol/`:** `openapi.yaml` and test vectors (paths, policy, sas).
- **`spikes/`:** identity, files-app, and stream (capture, encode, webrtc, input, android).
- **Other:** `installer/`, `xtask/`, `.github/workflows/`, docs (findings, reports, context, ADR
  evidence), `LICENSE`, `SECURITY.md`.
- **Branches:** `night-1` (the deliverable, 191 commits from `main`; the range is
  `main..night-1`), 39 `feat/*`/`fix/*` branches, and no remote. `main` is untouched.

## Machine state restored?

- **Power settings:** not changed (no `powercfg` calls). The streaming keep-awake is per process
  and is cleared when a session ends.
- **Processes:** at the end, no emulator, qemu, gander, testwin or Office automation process was
  running (tasklist checked after the final runs).
- **Installed:** Android emulator, the API 36 system image, AVD `gander`, NDK 28.2, PowerShell 7.6.9
  and cargo-deny. Only approved items, plus NDK and cargo-deny as toolchain dependencies.
- **Firewall and scheduled tasks:** untouched.
