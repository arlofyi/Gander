# Night 2 report 2026-11-03

## Outcome in one paragraph

Local `main` now carries every package on the Night 2 queue. Nothing has been pushed; the last
push to GitHub was `006d645`. Since then there are 89 commits: 383 files, about 48k lines added.

- **Done:**
  - the real-device bug fixes from your tablet test
  - the Lens logo and icons
  - a lightweight host
  - a WiX 5 MSI
  - the full UI redesign of both apps (approved mockups)
  - all of Phase 2: stream end-to-end harness, audio, clipboard text, and bring-to-front
  - from Phase 3: the Android Files-app provider, folder download as zip, deep-zoom tiles and
    Windows Search
  - supply chain: SBOMs, auditable binaries, Dependabot
- **Final gates, all green on `8104e68`:**
  - `cargo test --workspace`: 1055 passed, 0 failed
  - `cargo xtask e2e-host`: 166 passed, 0 failed
  - `cargo xtask e2e-android`: 160 passed, 0 failed, 0 skipped
  - Gradle build and lint, `cargo deny`, and the docs check all clean
- **On the tablet:** the newest debug app is already installed.
- **To install:** a fresh MSI is at `dist/Gander-0.0.0-x64.msi`.
- **Not verified while you slept:** the PC stayed locked all night, so everything that needs an
  unlocked, attended desktop is still unverified: live input, the stream session through the host,
  the real clipboard, and the Android stream run. The commands to run them are below.

## What to do first (about 15 minutes)

1. Restart the host from the new code with `cargo xtask run-host --release`. It builds the host and
   its render worker together. Alternatively, install `dist/Gander-0.0.0-x64.msi`, a per-user
   install with no admin prompt. It is unsigned, so expect a SmartScreen warning.
2. On the tablet:
   - use **Pair again** or **Forget** on any card that says "Removed on the computer"
   - browse a share and preview a PDF and a photo; pinch in to see the deep-zoom tiles
   - fetch a file, then tap **Save to device**
   - open the **Files** app: your computer appears as a location
3. With the PC unlocked, at the desk, try Stream from the tablet on these files:
   - a `.txt`, which opens in Notepad
   - a `.png`, which opens in Photos
   - a `.pptx`, which opens in PowerPoint

   Then work through `docs/specs/2026-11-02-stream-real-app-checklist.md`.
4. While still unlocked and at the desk, run the checks that could not run overnight:
   - `cargo xtask e2e-stream --android` (host session path plus the emulator viewer)
   - `cargo test -p gander-testwin --test input -- --ignored --nocapture --test-threads 1`
   - `cargo test -p gander-win --test clipboard -- --ignored --test-threads 1` (saves and restores
     your clipboard)

## What was built (merged into local `main`)

| Package | Result |
| --- | --- |
| Device bug fixes | **Pairing:** "failed" after it succeeded, because TLS sockets were closed on the main thread; fixed, and flow A is now tested. **Revoked state:** "Removed on the computer" with Pair again / Forget. **Previews:** a missing render worker is now detected and explained; `xtask build-host`/`run-host` build both binaries. **Save to device:** a new button, since fetched files stayed app-private. **Stream of Store apps (Photos, new Notepad):** fixed in launch and discovery |
| Lens logo | `brand/` SVGs; exe, tray (4 states), Android adaptive and themed icon, notification icon; `cargo xtask icons` |
| Slim host | Idle with no window: 100 MB to 32 MB working set, 96 MB to 4 MB private, CPU about 0. The UI exists only while a window or prompt is shown; plain notices use native Windows notifications |
| MSI (WiX 5) | `cargo xtask installer-build`; per-user, validated, 8.3 MiB, ships the SBOM. Single-instance host and `--open` |
| UI redesign, Windows | Every window to the mockups, tray flyout, orange "being viewed" pill, 2 s Allow delay, light and dark themes; screenshots in `docs/specs/assets/ui/` |
| UI redesign, Android | Theme, components, bottom navigation, tablet two-pane, every screen; e2e-android green |
| WP-57 e2e-stream | `cargo xtask e2e-stream`; it found and fixed a capture-loop bug that dropped the last frame of a burst (stale picture after typing or a click) |
| Bring to front | The host brings the streamed window forward at connect (only when unlocked, and only after your approval if you are at the PC); the phone has a "Bring to front" button |
| WP-56 clipboard | Text only, off by default (`[stream] clipboard`), one direction at a time, gated like input, audited, announced with a notification |
| WP-55 audio | Sound from the streamed app's own process tree only (never system-wide), Opus over WebRTC, mute button on the phone |
| WP-60 Files app | Read-only DocumentsProvider. Sequential reads fetch each byte once; a player-like pattern costs 1.07x, against 2.4x in the spike |
| WP-68 folder zip | `GET /v1/zip`, streamed with ZIP64, limits (4 GiB and 20k entries by default), a dry-run size check, and "Download folder as zip" on the phone |
| WP-67 deep zoom | 512 px tile pyramid for images and PDFs (300 dpi); tiles load as you zoom in |
| WP-66 Windows Search | `/search` answers from the Windows Search index when the share's folder is indexed (a Documents folder usually is). Content search is available as `scope=content`. Every index path is re-resolved through the path resolver |
| WP-82 supply chain | `cargo xtask sbom` (CycloneDX 1.5, validated), Android SBOM, dependency lists embedded in the exes, Dependabot, `docs/release/reproducible-builds.md` |

## Decisions I need from you

1. **`docs/security.md`.** WP-56 proposed a rule 13 for the clipboard and a residual-risk line;
   WP-55 proposed a line for sound. I kept the file unchanged. Proposed clipboard text:
   - Rule 13: "**Clipboard text is opt-in, one direction at a time, and gated like a click.** Off by
     default (`[stream] clipboard`); text only, 64 KiB of UTF-8, never truncated. The clipboard is read
     or written only while the stream passes the input gate (session unlocked by the same
     fail-closed lock probe, phone looking, window in front, nobody using the computer's own input),
     and the setting is re-read on every request, so switching it off applies at once. The phone only
     receives text it asked for. Every request is audited by direction and size (never content), the
     computer shows a notification when text is read or written, and nothing about the content is
     logged."
   - Residual risk: "**The clipboard can hold secrets.** With `clipboard` on, a paired device with
     Stream access can read whatever text is on the computer's clipboard (a password just copied)
     while a stream is active, and can replace it. That is why it is off by default, split by
     direction, gated like input, audited and announced."

   Proposed sound text: "Stream sound captures only the streamed application's own process tree
   (WASAPI process loopback); it never captures the system mix, and there is no fallback when process
   loopback is unavailable. Sound follows the stream's approval and visibility, is captured only
   while the stream is live and unmuted, and `[stream] audio = false` disables it."

   Also still open from Night 1: the "the OS enforces no Stream while locked" wording. The host
   enforces it, not Windows.
2. **Version number.** The MSI is 0.0.0 because the project has no version. Is the first one 0.1.0?
3. **Opus library.** Audio uses `rusty-opus` 1.0.2, a pure-Rust Opus encoder (BSD-3), because every
   libopus binding needs CMake, which isn't installed. It is small but not widely vetted. Either
   review it, or install CMake so the reference libopus can be used.
4. **Wording.** The being-viewed text now says "…is viewing PowerPoint and sound". I suggest
   "with sound".
5. **Settings window GPU.** The settings window wakes the NVIDIA GPU, which costs battery. Should it
   run on the Intel iGPU? Stream encoding stays on NVIDIA.
6. **Release hygiene.** The release exes embed your user-profile paths (about 900 times). Add
   `--remap-path-prefix` and `/Brepro` before any public release. Recommended: yes.
7. **Leftover design questions** (defaults applied; say if you disagree): bottom navigation, the
   "Disconnect everyone" label, the 2 s Allow delay, dynamic colour off, no global hotkey for the
   kill switch.

## Known issues and flakes

- **Timing tests under load.** `gander-render` Office COM and PDF preview tests can exceed their
  30 s timing assertions when the machine is busy; run alone, they pass. `e2e-stream`'s
  `engine.keepalive` (700 ms limit) also failed under load and passed when the machine was quiet.
  Consider relaxing these thresholds for CI.
- **Shared process tree.** Stream sound includes other windows of the same process tree, for
  example two documents in one PowerPoint process.
- **Mono audio on the phone.** The phone plays sound in mono (no `stereo=1` in its answer).
- **PowerPoint has no deep-zoom tiles.** Its slides are exported as pictures.
- **Windows Search scope.** Indexed search covers only crawled folders; a drive-root share walks.
- **Untested on real hardware.** Live input, the host stream session, the real clipboard and the
  Android stream viewer on a real device all need you to be unlocked and at the PC.

## Run notes

- **Agents.** Workers were Claude subagents on git worktrees, and the Conductor integrated on local
  `main`.
- **Overnight.** A check-in every 35 minutes resumed workers after the 5-hour usage limit, which
  hit once more at about 01:00 PKT.
- **Installs.** The .NET SDK 8 and WiX 5.0.2 were installed for the MSI, with your approval.
  Nothing else was installed.
- **Disk.** Free space fell to 6-8 GB twice and was recovered by `cargo clean`; 54 GB was free at
  the end.
- **Machine state.** No power, lock, firewall or scheduled-task changes. No Gander, emulator or
  test processes are left running. The tablet stayed connected over USB, with the newest debug
  APK installed.
- **Records.** Full run log (Conductor scratchpad) is summarised here; the per-package detail is in
  `CHANGELOG.md` under Unreleased.
