# CLAUDE.md

Guidance for Claude Code (and other AI agents) working in this repo. Human-facing overview
is in [README.md](./README.md).

## What this is

A standalone, open-source app that lets you reach the files on your own computers from your
phone or tablet, over a private network you already run (Tailscale, Headscale, ZeroTier,
plain WireGuard). For any file you choose WHERE it is handled:

1. **Preview** -- the computer renders a lightweight preview (thumbnail, page images, text).
2. **Fetch** -- the file's bytes are streamed to the phone and opened with a phone app.
3. **Stream** -- the file is opened in the desktop app ON the computer and only that app's
   window is streamed to the phone, with touch/keyboard input sent back.

The user decides which device's resources are spent. Access is restricted to folders the
user explicitly shares, per device.

Name: **Gander** (as in "have a gander"). Decided in `docs/decisions/0004-name.md`; the project
folder is still named `OTG View`.

## Status

**Phase 0 and Phase 1 are built and gated; Phase 2 is mostly built.** Night 1 (2026-11-01 to 02)
ran on the integration branch `night-1`; nothing is pushed, there is no remote, `main` has not been
merged into. Details, evidence and the owner's open items are in `docs/context.md`.

- **Phase 0 spikes: done** (WP-40..47): test window, WGC capture, Media Foundation encode, WebRTC,
  Android viewer and input, Tailscale identity, Files-app provider, and the consolidated
  findings with the streaming-stack decision (webrtc-rs with conditions).
- **Phase 1: host and Android client built, gates G0-G4 pass.** Host (WP-10..1C): core types and
  config, path resolver, listener and TLS, pairing, browse/fetch API, render worker and thumbnails,
  previews, events, tray and settings UI, stream launch policy, installer sources, `e2e-host`
  harness. Client (WP-20..29): scaffold, network layer, pairing screens, browser, thumbnails,
  file sheet, preview viewers, fetch engine, cache/settings/app lock, `e2e-android`.
- **Phase 2: stream engine, launch, input, lifecycle and viewer built** (WP-50..54): WGC capture,
  Media Foundation encode, webrtc-rs track, window discovery and fit-to-phone, `SendInput` mapping
  and gating, the session lifecycle with host visibility and the kill switch, and the Android
  viewer. **WP-55 (audio), WP-56 (clipboard text) and WP-57 (`e2e-stream` plus the real-app
  checklist) are not started.**
- **Deferred or human-gated:** installing and live-testing the MSI (it builds, validates and
  extracts; it has never been installed), code signing, the live input tests (they need an
  unlocked, attended session), the instrumented stream viewer run (waits for WP-57), anything
  needing a physical phone, the real-application run (PowerPoint, Edge PDF), the
  `docs/security.md` lock-wording correction and ADR-0003 (licence, still Proposed).

Read in this order before doing anything:

1. `docs/context.md` -- current state, decision defaults in force, owner approvals, next actions
2. `docs/plans/2026-11-01-orchestrator-kickoff.md` (if you are the Conductor) then
   `docs/plans/2026-11-01-master-plan.md` -- how an autonomous run proceeds: preflight, defaults,
   work packages, gates, fallbacks, human-gated items, morning report
3. `docs/specs/2026-11-01-*.md` -- the spec for the package you are working on (protocol, pairing,
   path resolution, renderers, stream launch policy, stream spike, Android client)
4. `docs/security.md` -- threat model and the non-negotiables
5. `docs/architecture.md` -- the map of components and the three modes
6. `docs/decisions/` -- ADRs; "Accepted by default" means the owner may still override in
   `docs/context.md` before a run; anything `Proposed` is not settled
7. `docs/vision.md`, `docs/roadmap.md`, `docs/research/` -- why, and what was found out

An autonomous session follows the master plan's run protocol literally: defaults applied unless
overridden, no pushes or remote creation, machine state restored, morning report written.

## Workflow

- **Non-trivial work gets a spec, then a plan, before code**: `docs/specs/YYYY-MM-DD-<topic>.md`
  then `docs/plans/YYYY-MM-DD-<topic>.md`. Keep them as the record of WHY.
- **Architecture choices are ADRs** in `docs/decisions/NNNN-<slug>.md` (template: `0000-template.md`).
  When a `Proposed` ADR is settled, change its status and update `docs/architecture.md`.
- **Spike before committing to anything risky** (per-window capture, input injection, locked-session
  behaviour, mobile WebRTC decode). Write findings to `docs/specs/YYYY-MM-DD-<topic>-findings.md`.
  Measured facts beat docs; record what was measured, on which OS/version.
- Keep `docs/context.md` current at the end of each session: what changed, what is next.
- Commits: Conventional Commits, scoped (`feat(host): ...`, `docs(arch): ...`).
- Branches: `feat/<topic>`; never push or merge to `main` without explicit human approval.
- Changelog: `CHANGELOG.md`, every change under `## Unreleased`; a version number is stamped only
  when an installable artifact is actually built.

## Non-negotiables (security)

These hold regardless of which stack is chosen. Full reasoning in `docs/security.md`.

- **Never listen on `0.0.0.0` / `::`.** Bind loopback plus the private-network interface address
  only. Re-check the binding when the network changes.
- **Deny by default.** Nothing is reachable until the user adds a Share. Shares are per device,
  read-only by default.
- **A phone path is never joined onto a root as a string.** Resolve per segment, refuse `..`,
  refuse symlinks/junctions that leave the share, refuse alternate data streams and 8.3 short
  names on Windows, compare case-insensitively on case-insensitive filesystems.
- **The private network is the first gate, not the only one.** Every device is also paired with
  the host (explicit approval on the host screen) and holds its own revocable token.
- **Stream mode never executes arbitrary files.** It opens a file with an allowlisted desktop
  application. Executables, scripts, shortcuts, installers and macro-enabled docs are refused.
- **The host always shows when it is being viewed** (tray indicator + notification) and has a
  one-click kill switch that drops every session.
- **No secrets in logs.** Tokens, pairing secrets and file contents are never logged.
- **No cloud relay, no telemetry by default.** Traffic stays on the user's private network.

## Platform reality

- First targets: **Windows host, Android client.** Then iPad/iOS client, then macOS and Linux hosts.
- Any `#[cfg(windows)]` / platform-specific branch must be compiled and tested on that platform in
  CI. A green build on one OS proves nothing about another.

## Commands

Run from the repository root. `cargo xtask <task>` is plain Rust (`xtask/`), so it behaves the
same on Windows and Linux and needs no shell scripts.

| Command | What it does |
| --- | --- |
| `cargo xtask preflight [--report <path>]` | Re-runs the master plan 1b toolchain checks; exit 1 on a hard failure (cargo >= 1.90, MSVC build, java 17/21, Android SDK 36, Tailscale Running, git, disk, Windows build). Optional checks (AVD, adb, Office, ffmpeg, pwsh, cargo-deny) only warn |
| `cargo xtask test-all [--skip-deny] [--skip-android]` | fmt, clippy, test, deny, vectors-check, installer-check, docs-check and the Android unit tests (skipped when `client/android` has no wrapper), every step run even after a failure, with a summary |
| `cargo xtask vectors-check` | Parses every JSON file under `protocol/test-vectors/` (unique vector ids, no BOM) |
| `cargo xtask check-elf-alignment <apk\|dir\|.so>...` | Android 16 KB page-size check: 64-bit `PT_LOAD` alignment and zip alignment of stored libraries |
| `cargo xtask e2e-host [--no-build]` | Builds the test-hooks host, starts it on 127.0.0.1 (port 0) over a generated fixture share, pairs via the test hook and runs the HTTP conformance suite (`host/gander-e2e`) against `protocol/openapi.yaml`; writes `target/e2e/report.json` (host output in `target/e2e/host.log`), exit 1 on any failed case |
| `cargo xtask e2e-android [--no-build] [--address <a>] [--adb-reverse] [--class <c>]` | Builds the test-hooks host and both APKs (`gradlew --no-daemon`), starts the host on 127.0.0.1 over a generated fixture share (images, PDF, text, ffmpeg-made H.264 MP4, 50 MiB file), pairs two devices via `gander-e2e pair`, boots the `gander` AVD headless (`emulator-5584`), runs the whole androidTest suite with the fixture arguments (except `StreamViewerFixtureTest`, which `e2e-stream --android` runs), revokes one device via the host's stdin and runs the revoke case; writes `TEST-<class>.xml`, `summary.json`/`summary.txt`, raw instrumentation output, logcat and host log (tokens redacted) to `target/e2e/android/`, always kills the emulator. Exit 1 on any failure or skip, except a skip of `StreamViewerFixtureTest` (a `--class` run), reported as `deferred: run by cargo xtask e2e-stream --android (WP-57)`; `--class` runs only the named classes (debugging, never a gate result). Without the AVD: JVM unit suite only, `deferred: needs emulator`. Needs `GRADLE_USER_HOME` set as below and ffmpeg on PATH |
| `cargo xtask e2e-stream [--no-build] [--android] [--skip-engine]` | WP-57, Stream mode end to end. Builds the test-hooks host, `gander-e2e` and `gander-testwin`, starts the host on 127.0.0.1 with `--test-stream-app` over a share holding `stream/e2e.gtw`, and runs `gander-e2e stream` with the loopback viewer (`gander_stream::testing`) as the phone. Host stage: pair, `/v1/events`, `POST /v1/stream` 201, `window_found`, offer/answer, `streaming`, frames decoded (ffmpeg), the `park`ed box found in the decoded pixels, a tap that changes its colour (pipe and decoded pixels; the host must have brought the test window to the front at connect, else the harness sends `{"t":"front"}` as the client does), keepalive while static, pause/resume on `control`, a simulated lock (stdin `lock`) ends the session, `DELETE ?close_app=true` closes the test window, the kill switch (stdin `kill`) drops the socket and the session. Engine stage: the same capture, encode, WebRTC and decode path in process on a test window, the lock guard faked (runs while locked). A really locked session (`gander_win::session::query_session_state`) makes the host refuse Stream: then the host stage checks `412 host_locked` and the kill switch and reports the session steps `deferred: session locked`; input is never posted as a substitute. `--android` then runs `StreamViewerFixtureTest` on the `gander` AVD against the same host with the measured box position (`gander-e2e park-watch` parks the test windows, `--front` optional; waits while another emulator runs, always kills it; needs the host tap to have passed). Writes `target/e2e/stream/report.json` (and `host.log`, `work/`, `android/`); exit 1 on any failed step, deferrals allowed. Needs ffmpeg on PATH for the decode steps (deferred without it) |
| `cargo xtask notices [--out-dir <dir>]` | Writes `THIRD-PARTY-NOTICES.txt` (licence texts of every crate linked into `gander-host` and `gander-render`, read from `cargo metadata`; includes the OFL-1.1, Ubuntu-font-1.0 and Boost texts) and `License.rtf` (the `LICENSE` for the installer) to `target/installer/`. No cargo-about needed |
| `cargo xtask sbom [--out-dir <dir>]` / `cargo xtask sbom --check [<file>...]` | Writes `target/sbom/gander-host.cdx.json`, a CycloneDX 1.5 SBOM of everything linked into or built with `gander-host` and `gander-render` (from `cargo metadata` and the `Cargo.lock` checksums: purl, SPDX licence, SHA-256; build-only crates are `scope: excluded`), validated against the vendored CycloneDX schema in `xtask/schemas/cyclonedx-1.5/` before it is written; deterministic (`SOURCE_DATE_EPOCH` or the HEAD commit time). `--check` validates existing SBOMs (default `target/sbom/*.cdx.json`), e.g. the Android one. The same dependency walk (`host/build-support/auditable.rs`) is embedded in both exes as a `.dep-v0` section (cargo-auditable format) by their `build.rs`. The MSI installs the host SBOM as `sbom\gander-host.cdx.json`. Supply-chain and reproducible-build notes: `docs/release/reproducible-builds.md` |
| `cargo xtask icons [--check]` | Renders the brand SVGs in `brand/` (the Lens logo: master, small-size, reversed, one-colour, app-icon tile, four tray states, lockup) with `resvg` into committed files: `host/gander-host/assets/gander.ico` (16, 20, 24, 32, 40, 48, 64, 256 px), `gander-window-64.png`, `assets/tray/gander-tray-<idle,connected,streaming,paused>-<16,20,24,32>.png` and `brand/png/*` (the 512 px Lens `gander-lens-512.png` and `gander-lens-reversed-512.png`, the 1440 px lockups `gander-lockup-1440.png` and `gander-lockup-reversed-1440.png` for README and social use on light and dark backgrounds, transparent; `legibility.png` and `tray-states.png`, review sheets of the small sizes). Deterministic: a second run writes nothing. `--check` writes nothing and exits 1 when a committed file is stale (a unit test and `installer-build` enforce it). Run it after editing any SVG and commit the output; no build needs a rasteriser. The Android icon XML (`client/android/.../res/drawable/ic_launcher_*.xml`, `ic_stat_gander.xml`) is hand-converted from the same paths |
| `cargo xtask mockups [--browser <path>] [--only <id>[,<id>]] [--scale <n>] [--jobs <n>] [--allow-fallback-fonts]` | Renders every screen of `docs/design/mockups/index.html` (48 screens) to `docs/design/mockups/png/<group>/<id>-<light,dark>.png` (groups `android-phone`, `android-tablet`, `windows-host`, `design-system`; the device frame only, 1x, transparent corners) and rewrites the index `docs/design/mockups/README.md` from the page's own screen registry. Uses headless Microsoft Edge (found in Program Files, or `--browser <path>` / `GANDER_BROWSER`, or another Chromium) in the new headless mode with a throw-away `--user-data-dir` under `target/mockups/` (never your profile; deleted at the end, a hung browser is killed). The page's `?capture` mode hides the sidebar, header and notes; `?capture&manifest` logs each screen and its measured frame size to the browser log, so window sizes are exact and the registry is the single source. Needs a network on first load: the web fonts come from Google Fonts, and without them the task stops (`--allow-fallback-fonts` renders with system fonts anyway). The PNGs are re-encoded with the `png` crate (best compression, no ancillary chunks: no text, paths or profiles), so a second run with the same browser writes nothing. `--scale 2` makes 2x images (about 4x the size; the committed set is 1x, about 6 MiB). Run it after changing `index.html` and commit the output; a unit test fails when a screen has no image or index entry |
| `cargo xtask installer-check [--binary]` | Gate for WP-1A: `installer/main.wxs` (WiX 5, v4 schema) and `autostart-task.xml` are well-formed with the expected ids and per-user/least-privilege, fixed component GUIDs, only the bind paths the build passes, every committed source present, the UpgradeCode pinned; every `installer/*.ps1` is ASCII, parses under Windows PowerShell 5.1 (`powershell.exe`) and `pwsh`, and `verify-rules.ps1 -SelfTest` passes under both; the host embeds its icon and both exes' build scripts embed their dependency list; notices generate; the SBOM generates and validates (the MSI ships it). With the `wix` 5.0.x tool and `WixToolset.UI.wixext` present it also does a dry `wix build` against stand-in binaries, `wix msi validate` and an administrative extract; without them it prints `[DEFER] MSI build` (exit stays 0). `--binary` also builds the release host and checks it is a GUI-subsystem program that carries a `.dep-v0` list matching the SBOM and whose `--ui-smoke` output reaches a pipe and `logs/host.log`. Never touches the firewall, Task Scheduler or registry, installs nothing |
| `cargo xtask installer-build` | Builds `target/installer/Gander-<version>-x64.msi` (per-user, unsigned, about 7.7 MiB): `icons --check`, notices, the SBOM (shipped in the MSI), `cargo build --locked --release -p gander-host -p gander-render` (then both exes must carry a `.dep-v0` list that matches the SBOM), `wix build` with the bind paths, `wix msi validate`, then an administrative extract (`msiexec /a` into a temporary folder; installs nothing) that lists the files. Needs the .NET SDK 8, `wix` 5.0.2 and the UI extension (commands in `installer/README.md`, this laptop's paths in `docs/local/machine.md`); exits 1 with the install commands when they are missing. Never run the MSI on a development machine without the owner |
| `cargo xtask docs-check [--root <dir>]` | WP-1C gate: every relative link, image, reference definition and `href`/`src` in the repository's Markdown (not `target/`, `.worktrees/`, `node_modules/`, `build/`) must resolve, checked case-sensitively and without leaving the repository, and every `#anchor` into a Markdown file must match a heading (GitHub slug rules) or an `id`/`name`. Prints each broken link with file and line, exit 1 if any; external links are counted, never fetched. CI: `.github/workflows/docs.yml` |
| `cargo xtask build-host [--release]` / `cargo xtask run-host [--release] [-- <host args>]` | Builds `gander-host` AND `gander-render` in one cargo call (previews and thumbnails need the worker next to the host; a bare `cargo run -p gander-host` does not build it, and the host then warns and reports `previews_unavailable`), and for `run-host` starts the host with `<host args>` |
| `cargo run -p gander-host -- --ui-smoke [--ui-out <dir>] [--ui-no-native]` | The tray and settings UI smoke test (WP-18): renders every window offscreen to PNGs (default `docs/specs/assets/ui/`), then opens each window and the tray for real unless `--ui-no-native`; starts no server and reads no real config; exit 0 only if every step passed |
| `cargo run -p gander-host -- --autostart on\|off\|status` | Registers, removes or reports the Task Scheduler logon task that starts the host when the user signs in (WP-1A; the Settings switch and the MSI use the same code). Per-user, least privilege, no elevation; `status` is read-only |
| `cargo run -p gander-host -- --open` | Starts the host and opens the settings window at once (the Start menu entry uses it). The host keeps one copy per data directory (named mutex, `gander_host::single_instance`): a second start prints "Gander is already running" and exits, and with `--open` it first asks the running host to open or focus its window. Refused with `--no-ui` and `--test-approve` |
| `cargo run -p gander-testwin -- --self-test` | Opens the deterministic Win32 test window (`host/gander-testwin`, stream spike and tests) and exits 0 after 20 s |
| `cargo build --workspace` | Builds every crate (`host/*` and `xtask`) |
| `cargo test --workspace` | Unit and integration tests |
| `cargo lint` | Alias for `cargo clippy --workspace --all-targets -- -D warnings`, the CI lint gate |
| `cargo fmt --check` | Formatting gate (`rustfmt.toml`) |
| `cargo deny check` | Licences (permissive only), advisories, bans, sources (`deny.toml`) |
| `client/android: ./gradlew assembleDebug testDebugUnitTest lintDebug` | Android gate (use `gradlew.bat` in PowerShell); then `cargo xtask check-elf-alignment client/android/app/build/outputs/apk/debug/` for the 16 KB check |
| `client/android: ./gradlew --no-daemon cyclonedxSbom` | Writes `target/sbom/gander-android.cdx.json`, the CycloneDX 1.5 SBOM of the release APK's runtime classpath (CycloneDX Gradle plugin, version in `libs.versions.toml`); validate it with `cargo xtask sbom --check target/sbom/gander-android.cdx.json`. Needs `GRADLE_USER_HOME` as in the machine notes |
| `client/android: ./gradlew assembleDebugAndroidTest` | Compiles the instrumented tests (`src/androidTest`) without needing an emulator |

Stream engine, input and discovery tests that need an unlocked, attended session are
`#[ignore]`d; run them by hand (and `cargo xtask e2e-stream` again, unlocked), for example
`cargo test -p gander-testwin --test input -- --ignored --nocapture --test-threads 1` and
`cargo test -p gander-stream --test pipeline -- --ignored --test-threads=1`.

Shared dependency versions and lint levels live in the root `Cargo.toml`
(`[workspace.dependencies]`, `[workspace.lints]`); member crates opt in with
`name.workspace = true` and `[lints] workspace = true`. Add new shared dependencies there.

Machine notes (Windows dev laptop, measured 2026-11-01; versions in
`docs/reports/2026-11-01-preflight.md`). **Machine-specific paths: see `docs/local/machine.md`
(not committed; `docs/local/` is gitignored).** Tracked files never name a real user profile,
device, address or account; write paths with environment variables instead.

Environment variables the tooling reads (set them in the shell, never in a tracked file):

| Variable | Used by | Typical value |
| --- | --- | --- |
| `ANDROID_HOME` | preflight, `e2e-android`, `e2e-stream --android`, Gradle, the spike scripts (they stop if it is unset) | `%LOCALAPPDATA%/Android/Sdk` |
| `ANDROID_AVD_HOME` | `emulator`/`avdmanager`, the spike scripts (they stop if it is unset); the xtasks fall back to `ANDROID_USER_HOME/avd`, `%USERPROFILE%/.android/avd` and the profile above `%LOCALAPPDATA%` | `%USERPROFILE%/.android/avd` |
| `GRADLE_USER_HOME` | `gradlew` (the xtasks inherit it), the Files-app spike script (stops if unset) | `%USERPROFILE%/.gradle` |
| `DOTNET_CLI_HOME`, `DOTNET_ROOT` | `installer-check`/`installer-build` find `wix` on `PATH`, in `DOTNET_CLI_HOME`/`USERPROFILE`/`HOME` `.dotnet/tools`, or `%SystemDrive%/Users/%USERNAME%`; `dotnet` in `DOTNET_ROOT`, `PATH`, `%LOCALAPPDATA%/Microsoft/dotnet` or `%ProgramFiles%/dotnet` | the real profile |
| `CARGO_HOME` | `cargo deny` (cargo looks in `$CARGO_HOME/bin`) | `%USERPROFILE%/.cargo` |

When a launcher overrides `HOME`/`USERPROFILE`, `%USERPROFILE%` above means the real user profile.

- `cargo deny` is installed in `%CARGO_HOME%/bin`; cargo finds it there even if that directory is
  not on `PATH`.
- Android SDK tools are not on `PATH`; use `$ANDROID_HOME/emulator/emulator.exe`,
  `$ANDROID_HOME/platform-tools/adb.exe` and `$ANDROID_HOME/cmdline-tools/latest/bin/*.bat`.
- The `gander` AVD lives in the real user profile's `.android/avd`. A session launcher that
  overrides `HOME`/`USERPROFILE` (the owner's Claude profile does) will not see it by default:
  set `ANDROID_AVD_HOME` to that directory before running `emulator` or `avdmanager`. The
  preflight resolves it on its own.
- Emulator acceleration is WHPX (Windows Hypervisor Platform), already usable on this laptop.
- Android client toolchain (WP-20): AGP 9.5.0 needs Gradle 9.6.0 (the wrapper pins it) and JDK 17;
  versions live in `client/android/gradle/libs.versions.toml`. NDK `28.2.13676358` is installed in
  `$ANDROID_HOME/ndk`. The launcher's `HOME`/`USERPROFILE` override would send Gradle's cache to the
  profile folder, so set `GRADLE_USER_HOME=%USERPROFILE%/.gradle` (the real profile's cache,
  shared with Android Studio) before running `gradlew`. When several builds share that
  `GRADLE_USER_HOME` (parallel worktrees, Android Studio), run with `--no-daemon` rather than
  stopping a shared daemon afterwards with `./gradlew --stop`.
- libopus build (2026-11-03): Stream audio encodes with the reference libopus, built from the C
  source bundled in `opusic-sys` by CMake and MSVC (no NASM, clang or bindgen). CMake 4.4.4 was
  installed machine-wide with `winget install --id Kitware.CMake -e` (`C:\Program Files\CMake\bin`,
  on the system `PATH`); a shell that started before the install does not see it, so add that
  directory to `PATH` for `cargo build`/`cargo test`. CMake 3.16 or newer is enough. CI uses the
  CMake that `windows-latest` ships.
- MSI toolchain (2026-11-02): WiX 5.0.2 as a `dotnet` global tool plus `WixToolset.UI.wixext` 5.0.2
  (Util is installed, unused), on the .NET SDK 8.0.427 (winget, machine-wide, in
  `C:\Program Files\dotnet`); `wix.exe` is in `%USERPROFILE%\.dotnet\tools` and the extensions
  in `%USERPROFILE%\.wix\extensions` (the real profile, not the launcher's). A shell that
  started before the SDK install has no `dotnet` or `wix` on `PATH`; `cargo xtask installer-*` finds
  them anyway and sets `DOTNET_ROOT`. `rc.exe` for the exe icon (`build.rs`, `embed-resource`) is in the Windows SDK
  10.0.26100.0 (`C:\Program Files (x86)\Windows Kits\10\bin\10.0.26100.0\x64`), found through the
  registry. `pwsh` 7.6.9 is installed now.
