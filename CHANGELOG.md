# Changelog

## Unreleased

### Build, CI and tooling

- MSI: the Windows-version launch condition read `WindowsBuild`, which Windows Installer caps at
  9600 (it reports Windows 8.1 to every package), so the 0.1.0 MSI refused to install on Windows 11.
  It now reads `CurrentBuildNumber` from the registry (`RegistrySearch`, read-only) and still
  requires build 17763 (Windows 10 1809) or later.

## 0.1.0 - 2026-11-03

The first installable build: `Gander-0.1.0-x64.msi` (per-user, unsigned, 8.2 MiB) from
`cargo xtask installer-build`. Grouped by area; newest work first within each group.

### Host (Windows, Rust)

- Stream sound is encoded by the reference libopus 1.6.1 (BSD-3-Clause) through `opusic-c`, which
  builds the C source it bundles with CMake and MSVC, instead of the pure-Rust `rusty-opus`
  (removed). **CMake 3.16 or newer is now a build requirement** for `gander-stream` (measured with
  4.4.4; no NASM, clang or bindgen; the `windows-latest` CI runner has it). Format and settings are
  unchanged: 48 kHz stereo, 20 ms, VBR 64 kbit/s, in-band FEC. The loopback test viewer decodes
  with the same binding, and a test wraps the host's packets in Ogg/Opus and checks that ffmpeg's
  independent decoder returns the 1 kHz tone (skipped without ffmpeg on `PATH`).
- Settings window on the integrated GPU: the host still exports `NvOptimusEnablement` (and AMD's
  `AmdPowerXpressRequestHighPerformance`) but with the value 0, raised to 1 only while the Stream
  engine sets up an NVIDIA or AMD encoder (the driver reads the value at run time; measured,
  `docs/specs/2026-11-03-igpu-ui-findings.md`). New `[ui] gpu = "integrated" | "discrete" | "auto"`
  (default `integrated`) picks the GPU for the windows' OpenGL context. With the default the open
  settings window loads only the Intel GL driver, is not in `nvidia-smi`, and uses 63 MB private
  memory and 24 threads instead of 191 MB and 83; NVENC still activates and streams.
- Stream sound (WP-55): the streamed window's own sound on an Opus audio track next to the video
  (one BUNDLE, payload type 111, 48 kHz stereo, 20 ms, about 64 kbit/s). Capture is WASAPI process
  loopback of the window's process tree (`gander_stream::win::ProcessLoopback`, `wasapi` 0.26),
  never the system mix: when it cannot start the session has no sound and says
  `{"t":"audio","on":false,"reason":"unavailable"}` on `control`. Encoding is libopus (see
  the entry above). The capture runs only while the
  session streams and the phone has not muted it (`{"t":"mute","on":true}`); paused, backgrounded,
  muted or ended stops the WASAPI stream. Silence is padded while the app plays nothing, so RTP time
  follows the clock. `[stream] audio = false` turns it off (no audio track, `capabilities.stream.audio`
  false); the tray tooltip, status line, live band and viewing pill add "with sound" while it flows.
  `gander-testwin`'s `tone [HZ [MS]]` now plays a quiet sine (-30 dBFS, at most 1 s); tests: the
  synthetic tone through Opus and WebRTC (1000.0 Hz detected), the gate (connect, mute, pause,
  resume), an unavailable capture, no audio track without a source, and the real process-loopback
  tone (`--ignored`) plus `engine.audio_tone` in `cargo xtask e2e-stream`. Process loopback was
  measured to capture and play while the session is locked.
- Deep-zoom tiles for large images and document pages (WP-67): `GET /v1/shares/{share}/preview/tile
  ?path=&page=&level=&x=&y=` serves 512 x 512 WebP tiles of a pyramid whose top level is the
  natural resolution (an image's own pixels after EXIF orientation, a PDF page at 300 dpi, longest
  side at most 32768) and whose level 0 is the whole page fitted in 512; each level doubles the one
  below, tiles on the right and bottom edge are smaller (never padded). The geometry is
  `gander_core::tiles` (checked against `protocol/test-vectors/tiles.json`, which the Android client
  reads too). The worker gets `RenderOp::Tile`: images are cut from the decoded frame with a WIC
  clipper and scaler so a 6000 x 4500 photo is not decoded in full per tile (the EXIF orientation
  is mapped back to stored pixels, the `image` crate is the fallback for formats WIC lacks, RAW
  previews are tiled from the embedded JPEG); PDF pages render only the tile's `SourceRect` at the
  tile size with `Windows.Data.Pdf` (the oversize quirk of the OS renderer is corrected). Word,
  Excel and LibreOffice documents are tiled from their cached PDF export; PowerPoint slides are
  pictures and have no tiles. Tiles are cached in the preview cache (1 GiB cap unchanged) by file
  identity, page, level, x and y. The preview queue gets a low-priority class (limits unchanged:
  1 per device, 2 overall): tiles start only when no page, info or metadata job is waiting, are
  dropped after 15 s without being asked again, and are evicted before pages when the queue is
  full. `preview/info` gains `tile_size`, `tile_levels` and `tile_pages` (`{width, height, levels}`
  per page, at most 512 pages; absent when no page needs a second level). `openapi.yaml` has the
  new path, schema and examples; the e2e-host suite has 22 new cases (`preview_info.tiles.*`,
  `preview_tile.*`). Measured on the development machine (Windows 11, debug worker): top-level
  tile of a 50 MP JPEG 95-115 ms, of a 27 MP PNG 85-160 ms, an A1 PDF tile 290-570 ms.

- Windows Search index for search (WP-66): `GET /v1/shares/{share}/search` is answered from the
  index when the searched folder is in its crawl scope (`ISearchCrawlScopeManager::
  IncludedInCrawlScope`, read only; Gander adds no scope and changes no indexer setting) and falls
  back to the bounded walk when it is not, when the Windows Search service is off, or on any COM or
  provider error. The statement runs through the `Search.CollatorDSO` OLE DB provider (connection
  string from `ISearchQueryHelper`, `IDataInitialize`, `IRowset`; `gander_win::search`, every call
  inside a `ComScope`). The answer says `via: "index" | "walk"`, `/shares` carries a per-share
  `search: true | "indexed"` and `/me` `capabilities.search` is `"indexed"` when any share the
  device can list is (cached 30 s). New optional `scope=name|content` query parameter: `content`
  also matches the text inside files (every word typed, the last as a prefix) through `CONTAINS`;
  on a share searched by the walk it is answered as a name search and `via: "walk"` says so. Every
  path the index returns is untrusted: the tail below the searched folder's real path (from the
  open handle) is split into segments and resolved by `ShareRoot::resolve` like any phone path
  (no `..`, ADS, 8.3 aliases, junctions or symlinks, hidden patterns, final handle inside the
  share), each ancestor and the item must pass the listing filter (hidden and system, `desktop.ini`),
  and size, times and flags come from the file, not the index; a name search also checks the
  stored name. User text never becomes SQL: control characters or more than 128 characters go to
  the walk, quotes are doubled inside the one literal, `%`, `_` and `[` are written as one-character
  classes (the dialect has no `ESCAPE`), and the full-text words are quoted prefix terms with `"`
  and `*` removed. `System.ItemUrl` is read, not `System.ItemPathDisplay`: for `.eml` files the
  latter is the mail subject, not a path (measured). Paging uses `i1.<tag>.<offset>` cursors
  (index rows consumed, bound to the query; another query is `bad_request`), 2,000 rows and 2 s
  per page, 20,000 rows deep; `partial` also when the indexer's incremental queue is not empty (its
  notification queue is ignored: it sat at a constant 114 on an idle machine). Tests: SQL builder
  and escaping unit tests, 14 integration tests with a fake index feeding hostile rows through the
  real gate and resolver (`..`, junction, ADS, 8.3, hidden file and folder, `.git`, other folders
  and schemes; paging, cursor binding, fallback, `partial`, permissions), conformance cases in
  `cargo xtask e2e-host`, and `#[ignore]`d live tests against the machine's real index
  (`host/gander-win/tests/search_index.rs`, `host/gander-host/tests/search_index.rs`). Measured
  here: a name search of a OneDrive-redirected Documents folder with thousands of matches takes 0.7 s (0.3 s without the path
  ordering), a content search 0.35 s.
- Stream: the host brings the streamed window to the front once when the phone connects, and
  once more on a resume (launch-policy step 7), through the WP-52 foreground procedure
  (`AttachThreadInput` fallback on, Alt-key trick off). Decided by
  `gander_core::lifecycle::front_on_connect`: never while locked; with someone at the computer
  only after the owner's Allow on the host prompt. It runs in the input worker before the gate
  leaves its client pause, so a window that comes forward never shows `focus_lost`; afterwards
  only the phone's `{"t":"front"}` runs it again. The outcome is logged and readable through
  `InputLink::start_front`; nothing unsolicited is sent on `control`.
- Folder download as a streamed zip (WP-68): `GET /v1/zip?share=&path=` (`fetch` permission,
  explicit request only). A bounded walk through the path resolver (listing rules, hidden patterns,
  links and junctions not followed and named in `_gander-skipped.txt` at the end) measures the
  folder and answers `413 too_large` before streaming when it is over the `[zip]` limits in
  `config.toml` (4 GiB and 20,000 entries by default, plus a 30 s budget). The archive is written
  by a new streaming ZIP writer (`gander_core::zipstream`: stored entries, data descriptors, ZIP64
  past 4 GiB or 65,535 entries, UTF-8 names, zip-slip guard) through a bounded channel, so nothing
  is buffered or staged; chunked, no Range or resume, and the task stops when the client
  disconnects. `dry_run=1` returns `{entries, bytes, skipped, cloud_only}` for the client's
  confirmation, HEAD returns the headers, `capabilities.fetch.zip` advertises the limits, two zips
  run at once at most (`429`), and one `fetch` audit event (`kind: "zip"`, counts, never file
  names) is written when the download ends. Tests: writer round trips with the `zip` reader
  (including forced ZIP64 and a 4.5 GiB `#[ignore]` run), the walk and its limits, the integration
  test `host/gander-host/tests/zip.rs` (unzip and compare with the fixture tree, limits,
  permissions, audit, the client going away) and the `zip.*` cases of `cargo xtask e2e-host`.
- Clipboard text bridge (WP-56): `clipboard.get` and `clipboard` on the stream's `control` channel,
  text only, at most 64 KiB of UTF-8 and never truncated, off unless the owner sets `[stream]
  clipboard` to `to_phone`, `from_phone` or `both` (Settings > Streaming > Clipboard text,
  in force at once). The input worker serves it behind the same gate as a click (session unlocked
  by the fail-closed lock probe, phone looking, window in front, nobody using the computer's own
  input); the answers are `clipboard`, `clipboard.ack {bytes}` and `clipboard.refused {op, reason}`
  (`disabled`, `host_locked`, `focus_lost`, `too_large`, `no_text`, `busy`, ...). The Windows
  clipboard (`gander_win::clipboard`: `OpenClipboard`, `CF_UNICODETEXT`, a message-only owner
  window, bounded retries while another application holds it, `\r\n` converted both ways). Every
  request is audited (`clipboard`: direction, session, size or reason, never the text) and a
  transfer queues a tray notification ("Pixel 9 copied text from this computer"). The setting is
  advertised as `capabilities.stream.clipboard` (omitted when off). The data-channel message
  ceiling is now 256 KiB (was 64 KiB) so a full-size text fits once escaped. Tests: policy, caps,
  codec and service unit tests, the loopback viewer set/get round trip, the settings row, and
  `host/gander-win/tests/clipboard.rs` for the real clipboard (`#[ignore]`: it saves and restores
  the user's text clipboard and skips while the session is locked).
- UI redesign (host tray and settings app, `host/gander-host/src/ui/`), from the approved design
  `docs/design/2026-11-02-ui-redesign.md` and its mockups: a theme module with the Gander colour
  tokens for light and dark, the Fluent compact type ramp (Segoe UI, Segoe UI Semibold and
  Cascadia Mono read from `C:/Windows/Fonts`, egui fonts as fallback) and the egui
  `Visuals`/`Spacing` mapping; the flat Mica approximation (mica window and nav, `layer-2` page
  with a 1 px stroke and 8 px top-left corner, `layer` cards). Follows the Windows light/dark
  setting, with a Theme setting (Use system setting / Light / Dark, saved in `ui-theme.txt` next
  to `config.toml`). One set of line icons drawn as egui shapes (`ui/glyphs.rs`, no icon font);
  the Lens mark is the committed app icon. Every window rebuilt: NavigationView with the status
  block and Add a device / Disconnect everyone at the bottom; Shares master-detail with the
  per-device permission grid (Stream column tinted when on) and the Share a folder dialog (every
  device unticked, Stream never set, whole drive or profile warns); Devices cards with identity,
  address and path, shares, Stop stream and Revoke; Add a device with the QR, three steps and the
  addresses, then the request with the large code and Deny / Allow; Activity table with a
  segmented filter; Network with listening addresses, connected paths and the connectivity
  check; Settings as Fluent setting rows. Owner decisions: the kill switch reads "Disconnect
  everyone" (window, flyout and tray menu); Allow on a pairing request stays disabled for 2 s
  after the request appears, with a countdown on the button. Being viewed: the orange live band
  in every window, an always-on-top orange pill at the top centre of the screen with Stop, and
  the kill-switch result band ("Everyone was disconnected at 14:07", Resume now while paused).
  The tray's left click opens a flyout (a borderless always-on-top egui window above the
  notification area, as `tray-icon` only offers a native menu): status, who is connected, Add a
  device, Pause 10 min, Open Gander, Disconnect everyone, Quit; double click opens Gander. Plain
  notices (device connected, is viewing, pause, kill switch) are native notifications of the tray
  icon (`Shell_NotifyIconW` `NIF_INFO`) and start no egui session; egui toasts remain only for
  what needs a button (pairing request, Stream prompts). `--ui-smoke` writes every screen in both
  themes (`<screen>.png`, `<screen>-dark.png`) and opens the flyout and one native notification
  in the native pass.
- Stream engine fix (found by `e2e-stream`): the capture pump dropped a captured frame that came
  sooner than 0.8 frame periods after the previous one (or while the encoder ring was full). WGC
  sends a frame only when the window changes, so the last repaint of a burst was lost and the
  phone kept the stale picture, repeated by keepalive, until the window changed again: measured
  with `gander-testwin park` right after an animated frame, the parked picture had not arrived
  10 s later with keyframe requests every 600 ms. The newest early frame is now held (one at a
  time, a fresh frame replaces it) and encoded as soon as it is due; the parked picture arrives in
  the first decode, under 0.75 s. Pacing is measured against now, not the capture time.
- Test host `kill` on stdin (`--test-approve` only): presses the kill switch as the tray does (every
  stream session ends, the pairing window closes, every connection drops, `kill_switch` audited)
  and answers `killed`. `gander-testwin` gains `park` (freeze with the box at its frame-0
  position, whatever repaints the window) for harnesses that must know where the box is.
- Loopback viewer (`gander_stream::testing`): `ViewerOptions::keep_data` keeps each access unit's
  bytes (`FrameRx::data`) for a decoder.

- Lens icon (host): `gander-host.exe` carries the icon and a version block (`build.rs`,
  `embed-resource`, `rc.exe` from the Windows SDK). The tray shows the lens in four states that
  differ in shape and colour (hollow grey idle, green lens with tinted glass connected, solid
  orange lens with the goose cut out streaming, grey lens with two bars paused), at the size
  Windows asks for (`SM_CXSMICON`: 16, 20, 24 or 32 px) and re-picked when the DPI changes; the
  drawn-shape code in `tray_state.rs` is gone. The Gander and Add a device windows use the app icon
  (decoded once instead of redrawn every frame).
- `gander-host --open` and one host per data directory: `--open` opens the settings window as soon
  as the tray starts (the Start menu entry). A named mutex (`gander_host::single_instance`, keyed by
  a hash of the data directory) stops a second copy from running a second tray icon beside the
  first; a second start prints "Gander is already running" and exits, and with `--open` it first
  asks the running host to open or focus its window. Checked end to end with a throwaway data
  directory and port.
- Host footprint (owner request: one process, slimmed): the tray now runs on a plain Win32
  message loop and the eframe/egui UI, its GL context and AccessKit exist only while a window or a
  toast shows (one `run_native` session each, eframe reuses its event loop); the tokio runtime has
  two workers instead of one per CPU; OpenGL, D3D11, DXGI, Media Foundation, UI Automation and
  DWM are delay-loaded. Idle with no window: 100 MB -> 32 MB working set, 96 MB -> 3.9 MB private,
  74 -> 15 threads, 3.3 % -> 0.1 % CPU (`docs/specs/2026-11-02-host-footprint.md`, ADR-0007
  evidence). `--ui-smoke` also checks that a window opens again in a second session.
- Device-test fixes (an Android tablet against the real host, 2026-11-02). Stream of packaged
  apps works: the `.png` handler Microsoft Photos (a packaged desktop app) refused
  `ActivateForFile` (`0x80270254`) and its `WinUIDesktopWin32WindowClass` window was never
  matched, so every Stream of a `.png` ended `app_unavailable`; Notepad for `.txt` failed with
  access denied (its exe is in `WindowsApps`). Packaged apps are now opened through the shell
  association handler with exactly the approved AppUserModelID, executables inside a package are
  activated rather than spawned, discovery matches the package family's own windows (preferring
  the one titled with the file) and its timeout starts after the activation call
  (`gander-win/tests/packaged_launch.rs`, ignored live test). A missing `gander-render.exe` (a
  bare `cargo run -p gander-host` builds none) is no longer silent: the host warns on the console
  and in `logs/host.log`, turns previews and thumbnails off, reports
  `capabilities.previews_unavailable` (`render_worker_missing`, `cache_unavailable`) in `/v1/me`
  and shows the reason in the tray's Settings window. The test-hooks host takes `qr <file>` on
  stdin (writes a flow A code for tests, never printed).

- Android stream window picker (`client/android`, after WP-53): the client follows the host's new
  `stream.pick`. `ClientEvent.StreamPick` (`{"type":"stream.pick","session":..,"index":n}`) is in
  the DTOs (the spec-example round trip, `ProtocolDtoTest`, passes again), and the viewer's
  window list for `stream.state pick` is now interactive: each candidate is a button that sends
  the pick on `/v1/events`, the sent row shows a spinner, and a picked window the host reports
  gone (`error app_unavailable`) says so and lets the user choose again. A pick the host gives
  up on after its 30 s (`error app_unavailable`, then `stream.state ended` reason `timeout`) now
  ends with its own message, "no window to stream", and Try again, instead of a bare timeout
  (`EndCause.APP_UNAVAILABLE`). The scroll sign was already the Windows wheel convention
  (fingers up = negative `dy`, fingers left = positive `dx`); it is now pinned by
  `StreamGesturesTest.scrollSignFollowsTheWindowsWheelConvention`, documented on
  `InputMessage.Scroll` and `StreamGestureRecognizer`, and guarded against a spec change by a
  `ProtocolDtoTest` check of the `openapi.yaml` scroll descriptions.
- Stream scroll sign (WP-54 integration, Conductor decision): `scroll` follows the Windows
  wheel convention. Positive `dy` is the wheel turned away from the user (content scrolls up,
  `+WHEEL_DELTA`), positive `dx` a right tilt; the host now passes both through unchanged
  (`gander_core::input::mapping::wheel_data`, pinned test). Before, the host negated `dy`.
- Stream session lifecycle and host visibility (WP-53): `POST /v1/stream` now starts a real
  session (201) on a host with Stream mode, and `DELETE /v1/stream/{session}?close_app=` ends it
  (204). The pure state machine (`gander_core::lifecycle`: requested -> approving -> launching ->
  window_found | pick -> streaming <-> paused -> ended) checks the preconditions in order
  (`host_locked`, `stream_busy` for the one session per host, `host_unattended` for an `ask`
  decision with nobody at the computer), shows the owner's prompts as toasts (approval: Allow
  once / Always / Deny, 60 s; someone using the computer: Allow / Not now, 10 s, unanswered per
  `[stream] idle_prompt_default`), lets the phone pick a window with the new `stream.pick`
  client message (30 s, else `app_unavailable`), holds a session whose phone dropped off for the
  120 s resume grace (`POST /v1/stream { resume }` re-offers on a new peer connection without
  relaunching), and decides `close_on_end` / `close_app` (`WM_CLOSE` only for an app Gander
  started with no other windows; never `TerminateProcess`; "left open: unsaved changes" when a
  dialog stays). `gander_host::lifecycle::StreamSessions` drives it with WP-51 launch, the WP-50
  engine and WP-52 input, ends the session on lock in every phase (fail closed), on the kill
  switch, listener pause, revocation and window close, keeps the computer awake while streaming
  (`SetThreadExecutionState`, cleared at the end; power settings untouched), restores the window
  placement, reports every `stream.state ended` exactly once (the engine gains
  `EngineConfig::owner_reports_end` and `EndReason::NetworkLost` for ICE failure), and audits
  `stream.refused`, `stream.prompt`, `stream.started`, `stream.resumed` and `stream.ended`. The
  tray shows the streaming state, the "Pixel 9 is viewing Deck.pptx in PowerPoint" toast has a
  Stop button, `host.state stream_available` and `capabilities.stream` turn on with the engine.
  The test host gains `--test-stream-app <gander-testwin>` (an in-memory `.gtw` catalogue entry;
  every other launch refused) and the stdin commands `lock`, `unlock` and `attended on|off|real`;
  `cargo xtask e2e-host` now covers 201, 204, `stream_busy`, `host_locked` (simulated lock, also
  ending a running session) and `host_unattended` against a real `gander-testwin` window.
- Stream input (WP-52): `gander_core::input` maps normalised picture coordinates onto the live
  window (DWM frame bounds by default, or the client area) and to `SendInput` absolute
  coordinates over the virtual desktop (unit-tested 0 px at 100/125/150/200 % on synthetic
  multi-monitor desktops, negative origins included), translates UI Events `code` values to scan
  codes, refuses desktop shortcuts (Windows key, Alt+Tab, Alt+Esc, Ctrl+Esc, Ctrl+Shift+Esc),
  turns `text` into `KEYEVENTF_UNICODE` events, decodes data-channel messages (keys and text only
  on the `control` channel), and decides pauses (`InputGate`: locked, fail closed > phone
  background > `focus_lost` > `local_user_active` for 2 s after local input). `InputSink` is the
  boundary the stream engine implements (`state_changed`, `refused`; `stream_state_event` builds
  `stream.state`). `gander_win::input::InputController` re-reads lock, foreground, local activity
  (`GetLastInputInfo` against its own injection ticks) and live geometry before every message,
  runs the pre-click check (`WindowFromPoint` + `GA_ROOT`, the target, its owned windows or its
  popup menu) and injects with `SendInput` (per-monitor v2 thread); foreground acquisition tries
  `SetForegroundWindow`, `AllowSetForegroundWindow`, `AttachThreadInput` and, opt-in, the Alt-key
  trick, and reports each step. `host.state` `attended` now ignores Gander's own injections
  (`local_idle_duration`). `gander-testwin` gains `typed?` and `--dpi-unaware`;
  `tests/input.rs` covers the locked refusal and live geometry at 150 % and 100 % (also a
  bitmap-scaled DPI-unaware window), plus ignored live tests for clicks, focus loss, local
  activity, covered windows, keys and text, and the foreground methods.
- Stream input wired to the engine (WP-52 on WP-50): `gander_stream::input::attach` installs the
  session's input handler, runs one worker thread per session that owns the injector, turns
  `focus_lost` and `local_user_active` into the engine's new input-only `PauseReason`s
  (`Session::set_input_paused`; `stream.state paused` while video keeps flowing), mirrors the
  engine's phone and network pauses into the gate, answers `{"t":"front"}` with the foreground
  procedure and `front.result`, and tells the phone `input.refused` with a reason code. Keys and
  text travel on `control` only (no sequencing layer). The loopback test viewer can send raw text
  on both channels and keeps every control text (`tests/input_link.rs`).
- Stream launch, window discovery and fit-to-phone (WP-51): `gander_core::launch` builds the
  launch command from a catalogue template (must start with `{exe}`), the handler's registered
  command or the isolated browser template (per-browser profile under
  `<data dir>\stream-browser\`), always with the policy-checked handler as the program and the
  file as one quoted argument; refused or unanswered `ask` decisions plan nothing. Discovery diffs
  `EnumWindows` snapshots against S0, matches the launched process tree, the file name in the
  title or the packaged app behind `ApplicationFrameWindow`, skips `MsoSplash` and untitled
  `Chrome_WidgetWin_0`, waits 400 ms for a second match, and answers found, pick (several, or
  any rule at the timeout) or timed out, with owned dialogs listed per candidate. Fit math gives
  one window pixel per phone pixel at the monitor's dpi, at least 640x480 logical (oriented like
  the phone), shrunk to the work area. `gander_win::launch` does the Win32 side
  (`CreateProcessW` without a shell, `IApplicationActivationManager` for packaged apps,
  `SetWindowPos`, `Get/SetWindowPlacement` restore, all per-monitor v2). `gander-testwin` gains
  `--splash`, `--reuse` and `--twin` scenario flags; `tests/discovery.rs` covers new process,
  re-used process via pipe, splash-then-main, two candidates and fit/restore at 150 % and 100 %.
- Stream engine (WP-50, `host/gander-stream`): the spike productionised as `StreamEngine` /
  `Session` (`start`, `stop`, `request_keyframe`, `stats`, input handler, `handle_client_event`).
  WGC capture on the encoder's adapter chosen by LUID, D3D11 video processor to NV12, Media
  Foundation hardware H.264 (NVIDIA rate changes at run time, Intel by re-creating the encoder),
  `timeBeginPeriod(1)` while a source runs, `NvOptimusEnablement` / `AmdPowerXpress...` exported
  by `gander-host`. webrtc-rs 0.21 track with H.264 constrained baseline level 5.0 (`42e032`, then
  `42e01f`) and `level-asymmetry-allowed=1`, playout-delay min = max = 0 on every packet, a
  `sendonly` transceiver, `input` (unordered, 0 retransmits) and `control` (ordered) data channels.
  A delay-based bandwidth estimator replaces GCC (encoded output under a 1.5 Mbit/s cap in about
  1.1-1.5 s with both a 200 ms and a 2 s bottleneck buffer, real encoder and synthetic source), the
  PLI/FIR interceptor, keyframes on PLI, reconnect and resume, a 450 ms repeat-last-frame keepalive
  (a static window gets a frame at least every 500 ms), battery cap, liveness/pause messages on the
  `control` channel, host-only ICE on the listener's addresses with one port each inside
  `[firewall] udp_ports` (refused with `stream_unavailable` when the range is exhausted), answers
  without candidates and trickled candidates, and the session-lock guard (ends the session within
  1 s, fail closed). `gander-host::stream` installs the engine as the `/v1/events` stream
  signalling. `gander-testwin` gains `freeze` / `animate` pipe commands (a static window for the
  keepalive test). Real-pipeline tests: `cargo test -p gander-stream --test pipeline -- --ignored
  --test-threads=1`.
- Installer, autostart and firewall rules (WP-1A, `installer/`): a per-user WiX v3 MSI definition
  (`installer/main.wxs`, `[package.metadata.wix]` in `gander-host`; no UAC to install; the MSI build
  itself is deferred until the owner installs the WiX Toolset and cargo-wix, exact commands in
  `installer/README.md`, signing documented as a human step). The Settings "start when I sign in"
  switch is now real: `TaskSchedulerAutostart` registers a Task Scheduler logon task for the
  current user (`installer/autostart-task.xml`, RunLevel LeastPrivilege, no time limit) through
  `schtasks /Create /XML`, behind a `TaskScheduler` trait with a fake for all tests and one
  `#[ignore]` live test; `gander-host --autostart on|off|status` runs the same code (the MSI uses
  it). Firewall: `firewall-rules.ps1` (Install, Remove, Plan; re-launches elevated; idempotent;
  rule group `Gander`), `firewall-common.ps1` and read-only `verify-rules.ps1`, all Windows
  PowerShell 5.1 compatible: inbound Allow for TCP (the listener port) and UDP (WebRTC range) from
  the tailnet CIDRs plus `[listener.generic] cidrs`, Private and Domain profiles (Public only with
  `[firewall] allow_public_profile`), and a hard refusal of RemoteAddress Any, `/0` and anything
  broader than a private network. New `[firewall]` section in `config.toml` (`udp_ports`, default
  `50000-50099`; `allow_public_profile`). The release build of `gander-host` is now a GUI-subsystem
  program that keeps redirected output, attaches to a parent console, and otherwise logs to
  `%LOCALAPPDATA%/Gander/logs/host.log` (`gander_host::diagnostics`). New xtasks: `notices`
  (THIRD-PARTY-NOTICES.txt from `cargo metadata`, including the egui font OFL-1.1/Ubuntu-font-1.0
  and Boost texts, plus the licence RTF), `installer-check` (WiX source, task XML, PowerShell
  parse under 5.1 and 7 plus a pure self-test, notices; `--binary` checks the release host) and
  `installer-build`; `test-all` runs `installer-check`.
- Flaky Windows tests fixed (stabilisation, not a WP). `gander-win` lib tests crashed about 1 run in
  15 with `STATUS_ACCESS_VIOLATION`: `AssocQueryStringW` (handler lookup for the Stream policy and
  the shell-thumbnail check) was called from threads with no COM apartment, so its WinRT activations
  ran in the implicit MTA; when that MTA was torn down COM unloaded the servers the shell still
  held, and the next query executed code in an unloaded DLL. The new crate-private `com::ComScope`
  (`host/gander-win/src/com.rs`) joins the thread to the MTA per query and pins the MTA for the life
  of the process (`CoIncrementMTAUsage`). Also fixed while looping: a watch callback could still run
  after its `WatchGuard` was dropped (the drop now waits for a callback in flight), the LibreOffice
  job kill did not wait for the processes to exit so the scratch folder could leak, and three tests
  were timing- or pid-reuse-sensitive. Office COM: the quit deadline now starts before
  `Close`/`Quit` (under load `Quit` blocked for 60 s after a 0.5 s render, so the watchdog fired on
  a finished job); the worker's job and watchdog now race for the exit through one claim, so a
  watchdog that killed Office mid-call is reported as a timeout (exit 3) and not as `0x800706BE`
  (exit 4); an activation the worker's watchdog interrupts is now swept by `emergency_cleanup`
  (`procs::sweep_automation`), an activation that fails at the RPC layer (`0x800706BE` and kin: a
  just-quit instance still registered) is retried once after its strays are stopped, and the
  `office_com` tests hold a session-wide named mutex (serial across processes and worktrees, not
  just inside one binary) and a drop guard that stops every automation instance started during the
  test (never one without the automation switch), wait for a quitting instance before deciding to
  skip, print a `SKIPPED` line naming whose instance is running, and use a test-only worker budget
  (the product's 30 s Office timeout and the plan's 30 s Verify assertion are unchanged).
- Host tray and settings UI (WP-18, `host/gander-host/src/ui`, cargo feature `ui`, on by default):
  a `tray-icon` + `eframe` app with the tray menu (Open, Pause listener 10 min, Kill switch, Quit),
  four tray states (idle ring, connected dot, streaming play, paused bars) and the windows Shares
  (folder picker, name, per-device List/Preview/Fetch/Stream, show hidden, hidden patterns, show
  path; edits are drafts until Save and a new share allows nobody), Devices (identity, last seen,
  address, two-step Revoke), Add a device (QR code, address/port/MagicDNS for manual pairing, the
  six-digit code, Allow/Deny, failed-attempt count; the pairing window exists exactly while this
  window is open), Activity (audit tail with filter), Network (adapter, interface, bound sockets,
  relay path per device, firewall Block rule and Shields Up self-check) and Settings (port, cache
  limit and purge, autostart switch). Toasts for pairing requests and session starts. The UI talks
  to the host through the `HostController` trait (`LiveController` for the real host,
  `DemoController` for tests and the smoke run). `gander-host --ui-smoke` renders every window
  offscreen with a CPU rasteriser to `docs/specs/assets/ui/` (no screen capture) and then opens
  each window and the tray for real; `--no-ui` runs headless, as does `--test-approve`.
  Host plumbing: `Server::handle()` (`ServerHandle`), `ListenerPause`/`PausablePlan`,
  `PairingService::audit()`, `[cache] cap_mib` in `config.toml` (default
  2048, read at start), audit kinds for shares, pause and settings. A share edit in the UI goes
  through `ApiState::set_config`, the same path as the test hook. `deny.toml` gains three
  per-crate licence exceptions (egui's bundled fonts, `clipboard-win`, `error-code`) and four
  reviewed "unmaintained" advisory ignores. Autostart is a stub until WP-1A.
- Browse and Fetch API (WP-14, `gander-host::api`): `GET /v1/me` (device, host with `os_version`,
  network, capabilities, limits), `/v1/shares`, `/list` (cursor, sort, order, kinds, ext; weak ETag
  over the whole listing with `If-None-Match` -> 304), `/stat` (`meta=1`: image header sniff, else
  the WP-16 worker `Info` op), `/search` (bounded breadth-first walk, 2 s, `partial`, resumable
  cursor), `/v1/recent` (top 3 levels or 20,000 entries per share, cached 60 s), `/file` (GET/HEAD,
  single `Range`, `If-Range`/`If-Match` -> 412 `file_changed`, multipart -> 416, strong ETag,
  `Content-Disposition`, streamed from the resolved handle), `/hash` (blake3 and sha256, cached per
  ETag, 503 `generating` while a large file is hashed), `/thumb` (WP-15), `/preview/info`,
  `/preview`, `/preview/text` (WP-16, adds `X-Gander-Text-Next`) and `POST /v1/stream` (WP-19
  policy, then `stream_unavailable`). `host_routes` is the single route assembly. Queries are
  percent-decoded exactly once (`+` stays a plus); refusals, fetches, hashes and stream requests
  are audited. `ThumbQuery` gains `priority=visible|prefetch`. Integration tests run against a
  generated tree (10,000-file directory, Unicode names, hidden files, a junction); `/list` of
  10,000 entries measured at 25 ms (first) / 14 ms (warm) in release.
- Events channel (WP-17): `GET /v1/events` WebSocket (bearer auth like every request) sends `hello`
  and the current `host.state`, then pushes `host.state` changes, `share.changed`, `shares.updated`,
  `device.revoked` (then closes, within 1 s of a revoke) and `pong`; WebSocket ping every 20 s, drop
  after two missed, close on the kill switch, at most 4 sockets per device. `stream.*` client
  messages are answered with an additive `error { code: stream_unavailable, message, session }`
  event until Phase 2 (`Events::send_to` and `StreamSignalling` are the Phase 2 hooks). `host.state`
  comes from `gander_win::session`: lock state from `WTSRegisterSessionNotification` on a
  message-only window (initial `WTSQuerySessionInformation` after registering, 15 s re-query, fail
  closed: unknown or disconnected counts as locked; the capture spike showed Windows keeps
  delivering window frames while locked, so the host enforces the rule itself), `attended` from
  `GetLastInputInfo` against `stream.idle_prompt_seconds`, `on_battery` from `GetSystemPowerStatus`.
  `share.changed` comes from `gander_win::watch` (one I/O completion port and thread,
  `ReadDirectoryChangesW` per directory opened relative to the resolved directory handle) for
  directories `/list` reported through `WatchRegistry::touch_armed` / `touch` within 10 min (cap
  64), coalesced in a 500 ms window, suppressed for hidden and always-omitted names, delivered only
  to devices holding `list` on the share. Integration tests cover `share.changed` within 2 s of
  writing a file, revocation closing the socket, the kill switch, heartbeat drops and the permission
  filter.
- Pairing, tokens and audit log (WP-13): `gander-core::pairing` holds the SAS derivation (DER SPKI
  and raw nonces, 20 bits mod 10^6; vectors in `protocol/test-vectors/sas.json`, generated by an
  independent Python implementation), token hashing with a constant-time lookup, the
  `gander://pair` QR payload and the pairing window state machine (5 min window, 5 failures per
  source then 429, one pending request, 2 min pending and collection timeouts). `gander-host`
  serves `POST /v1/pair`, `GET /v1/pair/{id}` and `POST /v1/token/rotate`, replaces `HelloOnly`
  with `TokenAuthenticator` (401 token_missing/invalid/revoked, Tailscale network binding with 403
  `network_identity_mismatch`), keeps devices in `devices.toml` (hash only, revoked kept 30 days),
  captures the WhoIs identity at approval, broadcasts `DeviceEvent::Revoked` and closes the
  device's connections through a new per-connection `ConnHandle`, and writes
  `logs/audit.jsonl` (10 MiB x 5, redaction of secret keys and token-shaped text). The binary's
  `--test-approve` hook needs the `test-hooks` feature and binds loopback only. Integration tests
  cover flows A and B over loopback TLS, a MITM relay giving a different SAS, 429 on the 6th
  attempt, 404 while closed, rotation, identity mismatch with a fake WhoIs and revocation closing
  a WebSocket within 1 s.
- Previews (WP-16): `gander_win::preview::PreviewService` is the transport-independent core of
  `/preview/info`, `/preview` and `/preview/text` for WP-14 to route (`info`, `page`, `text`,
  typed `PreviewError` with `http_status()`/`to_body()`, `503 generating` with `retry_after_ms`,
  `estimated_ms` from measured renderer timings). New worker ops `Page`, `Info` and `ExportPdf`:
  images and RAW at the requested width (orientation-aware, never upscaled), PDF pages through
  `Windows.Data.Pdf` (a probe measures the renderer's output scale so pages are exactly as wide
  as asked), video posters, and presentation metadata from the package. Office rendering: Office
  COM automation in the worker (PowerPoint `Slides(n).Export`; Word and Excel
  `ExportAsFixedFormat` to a cached PDF whose pages go through the PDF renderer) on a hidden
  instance of its own, never attaching to or quitting an Office the owner is running (PowerPoint
  is refused when any instance exists; for Word and Excel a missing new process means the
  worker attached and lets go), on a private copy of the document with macros force-disabled,
  quit on drop, killed on timeout (the worker's watchdog, the announced pid, and a host-side
  sweep of automation-launched Office processes that started during the job). LibreOffice is
  detected (registry, known folders, `PATH`) and used for conversion when Office is absent;
  with neither, `preview` is `none` and page 1 of a failed Office render falls back to the
  embedded thumbnail. Text previews detect BOM, BOM-less UTF-16, UTF-8 and Windows-1252, sniff
  binary files (415), and are offset-addressed in decoded UTF-8 bytes. Preview queue: 1 render
  per device, 2 overall, de-duplicated, stale jobs dropped. The preview cache reuses the
  thumbnail cache's LRU (`ThumbCache::open_in`, named keys) under `previews/`.
- Path resolution and listing (WP-11): `gander-core::path` validates protocol paths (steps 1-4:
  empty/dot segments, depth and length limits, invalid characters, trailing dot/space, reserved
  DOS names, invalid UTF-8), holds the reparse-tag policy, share `hidden_patterns` globs and the
  listing filter/sort/paging with an opaque base64url cursor. `gander-win::path::ShareRoot` walks
  each segment with `NtCreateFile` relative to the parent handle (`FILE_OPEN_REPARSE_POINT`),
  refuses junctions/symlinks and 8.3 aliases, re-checks the final handle against the canonical
  root (step 7) and lists directories through the handle. Tests run every
  `protocol/test-vectors/paths.json` vector (privileged fixtures print `SKIPPED`), a broken
  string-join resolver caught by step 7, and proptest fuzz invariants.
- Stream policy engine (WP-19): `gander_core::policy` implements the launch policy of the
  stream-launch spec (hard-refused final extensions, never list and `System32` rule, catalogue
  match by executable path or package family, `allowed | refused | ask` with the decision path,
  browser detection with the isolated-window launch, owner approvals that update `apps.toml`),
  `ListingPolicy` for the `stream` field of listing entries, and `handle_stream_request`, which
  validates share, permission, viewport and policy and then answers `stream_unavailable` (503)
  until Phase 2. `gander_win::assoc` resolves handlers with `AssocQueryStringW` (packaged apps by
  `AppUserModelID`) and reads Authenticode signatures read-only with `WinVerifyTrust` (embedded,
  then catalogue). New error code `stream_unavailable` in `ErrorCode` and `openapi.yaml`; vectors
  in `protocol/test-vectors/policy.json`.
- Listener, TLS, trust adapters, firewall check (WP-12): host identity (ECDSA P-256 key,
  DPAPI-protected `host.key` with owner-only ACL, self-signed cert re-issued from the key, host id =
  base64url SHA-256 of the SPKI); rustls server with the `ring` provider, TLS 1.3 only; listener
  plan from the trust adapter (`gander_core::net::plan`: tailscale by adapter GUID/description and
  ranges, generic by interface/CIDR, never a wildcard, link-local excluded, proptest over random
  interface tables); exclusive binding (`SO_EXCLUSIVEADDRUSE`), re-plan on
  `NotifyUnicastIpAddressChange` and every 30 s; WhoIs over the LocalAPI pipe (identification
  QoS, per WP-46) with CLI fallback and a 60 s cache; `status` relay classification using
  `Online`/`Active`; request gate (browser-marker 403, `Host` 400, pluggable authenticator,
  per-device concurrency limit with `Retry-After`, protocol headers); read-only firewall
  self-check (Block rules naming the exe, Shields Up from prefs). Routes plug in as axum
  `Router`s via `gander_host::server::Server::start`.
- Render worker and thumbnails (WP-15): `gander-render.exe` runs one job per process from a JSON
  job on stdin (a read-only file handle duplicated into the suspended worker, never a path),
  writes WebP/JPEG to stdout and exits 0/1/2/3/4. The host spawner
  (`gander_win::render::worker::run_job`) uses a job object (512 MiB, 20 s CPU, one process,
  below-normal priority, kill on close, UI limits), an explicit inherited-handle list, a
  low-integrity token, and host-side wall-clock and CPU-time polling. Renderers: WIC images
  (EXIF orientation, ICC to sRGB, downscaled decode, 300 MP guard) with an `image` crate
  fallback; RAW embedded previews (TIFF/IFD, RAF, CR3); PDF page 1 via `Windows.Data.Pdf`;
  Office/ODF embedded thumbnails (JPEG/PNG/EMF); shell `IShellItemImageFactory` for video,
  audio and registered providers with icon-only results refused. `gander_win::thumbs`: content
  identity cache (redb index, LRU, 2 GiB cap, evict 95 % to 80 %), render queue (concurrency 2,
  visible first, de-duplication, `retry_after_ms` = 200 ms x position), and the transport-
  independent `/thumb` service for WP-14 to route.
- Core types, config and protocol schema (WP-10): `gander-core` protocol v1 DTOs for every
  request, response, WebSocket event and input message (`protocol::`), the `ErrorCode` enum with
  HTTP statuses (`error::`), and the `config.toml` / `devices.toml` / `apps.toml` model with
  versioning, unknown-key rejection, validation (no wildcard CIDRs, deny-by-default grants) and
  load/save through a `SecureFileStore` trait (`config::`). `gander-win::config_store` implements
  it on Windows: files and the config directory are created with a protected owner-only DACL and
  replaced atomically. Hand-written `protocol/openapi.yaml`; the `protocol_examples_roundtrip`
  test validates every example against its schema and round-trips it through the Rust types.

### Client (Android)

- Deep-zoom tiles in the pages and image viewers (WP-67): once the zoom passes about 1.5x the base
  page (and the host's top level is sharper than the base picture) the viewers load the visible
  512 px tiles of the pyramid level that is sharp enough, draw them inside the zoomed layer over
  the base page, and cancel a tile's request when it leaves the viewport; the base page stays as
  the placeholder and a tile failure is silent. Tiles are Coil requests (`PreviewTileRef`) keyed by
  host, share, path, `mtime#size`, page, level, x and y, cached in memory and in the previews
  disk cache, loaded through the same per-host scheduler and retry policy as pages. Geometry
  (`TilePyramid`) is checked against `protocol/test-vectors/tiles.json`; level choice and the
  visible tile set (`DeepZoom`) are JVM-tested. The image viewer asks `preview/info` once after
  the picture has loaded; the pages viewer takes `tile_pages` from the info it already has (and
  from the re-fetch after the first render for Word and Excel). While tiles are available the
  pinch limit rises from 5x to natural resolution (capped at 12x). `PreviewInfo` carries
  `tileSize`, `tileLevels` and `tilePages`.

- Search inside files (WP-66): the search screen shows a "Search inside files" chip when the share
  advertises `search: "indexed"` (`ShareInfo.search`, new optional DTO field); turning it on
  re-runs the search with `scope=content`, and a cursor continues the mode it was made with. When
  the computer answers with its walk anyway (`via: "walk"`, for example while its search service is
  off) a note says only file names were searched. Off for every share the walk serves and after
  closing the search. `ProtocolDtoTest` covers the new openapi examples (`search` on shares, `via`
  on search results); view-model, repository and instrumented tests cover the switch.
- Files-app integration (WP-60): `GanderDocumentsProvider` (`fyi.arlo.gander.documents`,
  authority `<applicationId>.documents`, exported behind `MANAGE_DOCUMENTS`) makes every paired
  computer that has a token and was reached once (or has stored shares) a read-only SAF root of
  the system Files app and every document picker; shares and folders are documents. Document ids
  are `g1:<host>[:<share>:<path>]`, each part base64url, decoded strictly (no empty, `.` or `..`
  segment), built from host id, share id and path only, never the token. Listings come from the
  stored snapshots at once and refresh in the background (`EXTRA_LOADING`, then `notifyChange`;
  pages followed up to 2000 entries; not refreshed again for 30 s); offline shows the snapshot with
  an `EXTRA_INFO` note, a failure `EXTRA_ERROR`. Thumbnails go through the shared `ThumbLoader` at
  `priority=prefetch`. No document or root advertises write, delete, rename, move or create, and
  `openDocument` refuses every mode but `r`. Opening never touches the network beyond one cached
  `stat` for a document that was never listed: a saved copy of the current version is returned as
  a real fd (`OfflineCachedFileLookup.currentCopy`), anything else as
  `StorageManager.openProxyFileDescriptor` on a handler thread of its own per fd. Behind it,
  `ReadAheadCache` (pure Kotlin): the first read after a jump fetches exactly the requested range,
  a stream that continues (at most 64 KiB past its end) grows 64 KiB to 1 MiB, prefetches on a
  worker once continued twice, up to 4 streams in 4 MiB per fd; each read fails with `EIO` after
  20 s; the `CancellationSignal` cancels the fd. `HostRangeFetcher` sends `Range: bytes=a-b` and,
  after the first answer, `If-Range` with the ETag; 412, a 200 to an `If-Range` request, a new ETag
  or a different total size fail the read and every later one on that fd with `EIO` rather than
  mixing versions. App lock: while it is engaged (also in a process with no activity, and after the
  background timeout before the app returns: `AppLock.hidesContent`) the only root is "Unlock
  Gander to browse" and nothing opens; a host that answers `token_revoked`/`token_invalid` drops
  out of the roots. Settings > Opening files > "Show computers in the Files app" (default on;
  off returns no roots). Debug builds log per-fd counts only (`DebugLog.note`). Tests: JVM
  `DocumentIdsTest`, `ReadAheadCacheTest` (spike patterns: copy, two demux readers, random preads,
  seeks, timeouts, cancel, file changed), `HostRangeFetcherTest` (MockWebServer), `AppLockTest`,
  settings; instrumented `FilesAppProviderFixtureTest` (in `cargo xtask e2e-android`, which now
  passes `gander.fixture.large_pattern=e2e-android-v1`) and a Settings switch test. Measured on the
  `gander` AVD (API 36) against the e2e host over `10.0.2.2`, 52,429,200-byte fixture, every byte
  compared: sequential 1 MiB `read()` 401 kernel reads, 54 requests, over-fetch 1.000, 1.6 s, 2 MiB
  resident; 200 x 4 KiB + 50 x 64 KiB random `pread` 255 reads, 254 requests, over-fetch 1.078;
  player-like (two readers 2 MiB apart, 4 to 20 KiB reads, 8 to 20 KiB gaps, one seek) 1,544
  reads in 57 requests, 4 MiB resident, 2.08x of the bytes read (the skipped gaps are fetched with
  each block). `openFileDescriptor` returned in 6 to 26 ms. In an earlier run the kernel asked for
  about 440 KiB of the sequential file twice on one fd (answered from the cache). Not measured:
  real tailnet latency, ExoPlayer/VLC, OEM Files apps, the Files app UI itself (DocumentsUI was not
  driven), thumbnails through DocumentsUI.

- Stream sound (WP-55): the host's Opus track plays through libwebrtc's Java audio device module
  set up for media (`USAGE_MEDIA`, stereo out), and the session bar has a mute button while the
  session carries sound. Mute silences the track at once and sends `{"t":"mute","on":true}` so the
  host stops capturing; it holds across a reconnect. Unit tests for the wire shapes and the mute
  flow.
- Stream viewer: a `focus_lost` pause banner offers **Bring to front** (`{"t":"front"}` on
  `control`) and shows the host's `front.result` ("did not bring the window forward" with a
  retry). When `focus_lost` arrives within 10 s of the control channel opening (the host's own
  attempt failed) the viewer asks once by itself, per connection; later pauses are left to the
  user.
- Download folder as zip (WP-68): "Download folder as zip" in the open folder's overflow menu,
  offered only when the host advertises `capabilities.fetch.zip` and the share allows `fetch`. It
  asks `GET /v1/zip?dry_run=1`, shows the item count and size (a warning for cloud-only files, a
  note for skipped links, the host's own sentence for `413 too_large`) and only Download queues the
  zip. The zip is a fetch-engine job (key `zip:<folder>`, so no table change) with no `Range` and no
  verification: a broken connection starts over from zero. It then appears in Downloads with Save to
  device (MediaStore) like any fetched file. DTOs `ZipPlan` and `ZipCapability`
  (`ProtocolDtoTest` covers the new examples), `FolderZipTest` (dry run, no Range even after a
  dropped connection, no `/hash`, `too_large`) and `ZipFolderTest` (menu and dialog states).
- Clipboard text in the stream viewer (WP-56): the keyboard bar shows "Paste to computer" (sends
  the phone's clipboard text) and "Copy from computer" (asks for the computer's clipboard text and
  puts it on the phone's clipboard, marked sensitive on Android 13+) only for the directions the
  host advertises in `capabilities.stream.clipboard`. Text over 64 KiB or empty is not sent, one
  request of each kind at a time, a 10 s wait for the answer, one short message per action
  (never the text), and a `clipboard` message the phone did not ask for is dropped.
- UI redesign, phase 2 (every screen rebuilt to `docs/design/mockups/index.html`, behaviour and
  ViewModels unchanged): Computers as full-bleed host blocks with status dot and word, relay chip
  and sentence, the "Removed on the computer" state with Pair again / Forget; the Add a computer
  flow (chooser panels, immersive scanner with the orange reticle, address entry, both identities
  and the boxed code, success and failure result screens); Shares with crossed-out permission
  chips; the browser (crumbs, Files / Recent, sort and kind chips with a sort-and-filter sheet,
  skeleton rows, offline banner, grid cards, selection bar); the file sheet with action panels,
  the work rail and full-contrast reasons; dark immersive Pages / Image / Video viewers and the
  text viewer; Downloads as a bottom-navigation screen (was a sheet) with Open with / Share / Save
  to device / Keep offline; the mobile-data dialog; Storage with a three-part meter; Settings;
  App lock with the Lens mark; the stream session bar, banners, window picker and key-cap keyboard
  bar. Tablets (expanded width): navigation rail, a 440 dp folder pane beside an always-visible
  file detail pane (folder summary when nothing is chosen), one-row stream keyboard bar.
  Test tags added: `browser-fetch-selected`, `browser-sort-sheet`, `file-detail-pane`,
  `shell-*`; `stream-mode-toggle` now sits on the inactive Direct / Trackpad segment.
- UI redesign, phase 1 (design system, `docs/design/2026-11-02-ui-redesign.md`): the Gander
  tokens as light and dark `ColorScheme`s plus a `GanderColors` CompositionLocal
  (`GanderTheme.colors.warn`), the M3 type scale with a mono `sas` style, shapes and spacing;
  dynamic colour off by default (owner decision). The Lens mark as an `ImageVector` drawn from
  `brand/gander-lens.svg`. Shared components in `ui/components`: work rail, status dot and LIVE
  indicator, banners (info, warn, bad, ok, live), chips (including crossed-out "not allowed"),
  action panels (default and unavailable states), the pairing code (read digit by digit) and
  identity pair, progress bar (static with reduced motion), empty and result states, keyboard keys
  (sticky modifiers are toggles), top bar, list rows and buttons. App shell: bottom navigation
  Computers / Downloads / Settings on phones; a navigation rail with the Lens mark, and a 600 dp
  reading width, on expanded widths (`material3-window-size-class`). The window background uses
  the `bg` token. `ComponentsTest` (instrumented), `SasSpokenFormTest` and `ShellChromeTest` (JVM).
- Lens launcher icon (client): the placeholder "G" is replaced by the Gander lens: an adaptive icon
  (paper lens and orange beak on the slate background colour, inside the 66 dp safe zone), a
  monochrome layer for Android 13 themed icons, and `ic_stat_gander`, a white 24 dp silhouette now
  used by the fetch progress, result and paused notifications (there are no stream notifications
  yet; failed fetches are told apart by their text, no longer by the system error icon). No round
  icon: minSdk is 26, so every launcher masks the adaptive icon itself.
- Device-test fixes (an Android 13 tablet, against the real host, 2026-11-02). Pairing
  no longer ends on "Pairing failed." after it worked: the pairing connections were closed on the
  main thread (`NetworkOnMainThreadException` from TLS close_notify) after the token was stored;
  they now close on `Dispatchers.IO`, and any unexpected exception maps to a kind (`APP_ERROR`
  with the class name only, or `UNREACHABLE` / `INCOMPATIBLE` / `STORAGE`). A phone removed on the
  computer shows "Removed on the computer" with Pair again and Forget instead of a red error. The
  file sheet says "Previews are unavailable on <computer>" when the host reports
  `previews_unavailable`. The fetch panel has "Save to device": a copy in `Download/Gander`
  (MediaStore, Android 10+; a save-as picker on 8 and 9), since a fetched file otherwise stays in
  app-private storage and never shows up in Files or Downloads.

- Stream viewer fixes from its first emulator run (`StreamViewerTest`, after WP-54): the gesture
  timers now run on the pointer events' own clock (`GestureTimer`), not `SystemClock`, so a tap
  is no longer read as a long press (right click) when the two time bases differ; and the end
  dialog's "Keep streaming" no longer sits over the right half of "Close <app>" (all three choices
  are one column). Three tests that drove the screen wrongly were fixed (keyboard-bar keys are
  scrolled into view, the first-size test starts from Preparing) and the live-phase resize wait
  and the dialog layout have tests of their own.
- Android stream viewer (WP-54, `client/android`): Stream on the file sheet (and "Stream instead" in
  the cellular dialog) opens the viewer. `StreamSessionController` opens its own `/v1/events` socket
  first, asks `GET /me` (stream enabled, H.264 offered by the host and decodable on the phone,
  relayed path: a warning before anything starts), requests `POST /stream` (h264, 8 Mbit/s ceiling,
  the video area as viewport) and answers the host's offer with libwebrtc (`org.jitsi:webrtc`,
  MediaCodec H.264 only) receive-only, trickling its candidates after the answer (not waiting for
  gathering) and taking the host's trickled ones; an `error host_locked` before `ended` is told
  apart from a generic error (`ServerEvent.Error`, new DTO). Liveness on the host's `control`
  channel: ping every 1 s, a host quiet for more than 5 s is "reconnecting", `pause`/`resume` plus
  `stream.pause`/`stream.resume` on `onStop`/return (not on rotation), a failed link is renegotiated
  with `POST /stream` + `resume` and given up after the host's 120 s. The picture is a
  `SurfaceViewRenderer` laid out at its aspect-fit rectangle with pinch zoom and pan kept locally
  until reset (three-finger drag also pans). Direct touch (tap = click, long press = right click,
  two-finger drag = scroll, tap then hold-drag = drag) and Trackpad (one finger moves the viewer's
  own pointer, tap = click, two-finger tap = right click, two-finger drag = scroll, tap-drag)
  remembered per host; normalised points account for letterboxing and zoom, and a touch on a bar is
  not a touch on the window. Moves and scroll go on the lossy `input` channel, button presses, keys
  and text on the reliable `control` channel. Keyboard bar (sticky Ctrl/Alt/Shift, Esc, Tab, arrows,
  Fn row, Ctrl+Z/Y, Ctrl+S, Enter, Backspace, Delete), local text box sending `text`, an
  on-screen-keyboard button typing live; state banner for
  requested/approving/launching/pick/connecting/paused (with the reason)/backgrounded/ reconnecting;
  end dialog "leave the app open / close it" (`DELETE ?close_app=`); "Fetch the saved file" when
  `stat` shows a size or time different from the sheet's; a refused or unavailable stream is
  reported to the file sheet through `ActionRefusals`. JVM tests: wire shapes (pinned against the
  host's `control.rs`), geometry, the whole gesture map, keyboard bar, the liveness and pause state
  machine and the session controller against fakes. `StreamViewerTest` (Compose, chrome and
  gestures) and `StreamViewerFixtureTest` (real stream from the host's testwin, tap changes the box
  colour; arguments `gander.stream.share`, `gander.stream.path`, `gander.stream.box_x`,
  `gander.stream.box_y` beside the `gander.host.*` ones) compile and skip until WP-53 provides `POST
  /v1/stream`; running them is deferred to WP-53/57.
- Android cache, offline files, settings, app lock and host details (WP-28, `client/android`):
  fetched files are now kept under a cap (default 2 GiB, 512 MiB to 10 GiB in Settings) by an LRU
  rule: least recently used first (opening or sharing a copy counts as use), files with Keep offline
  are never evicted even when they alone exceed the cap, and a copy used in the last 10 minutes is
  held back so one just handed to another app is not deleted under it; it runs by itself whenever a
  fetch finishes or the cap changes. The Storage screen shows usage against the cap per computer,
  opens any saved copy without the computer, toggles Keep offline, deletes, clears everything not
  kept, clears the thumbnail and preview caches, and is the permanent entry point to the fetch list.
  Settings (one DataStore file, saved as you tap): the cellular warning threshold (shared by the
  fetch dialog and the file sheet's rule table), download verification, the cap, remembered default
  actions per file type with reset, the Stream-first type list, app lock, diagnostics export and the
  list of paired computers. App lock is `BiometricPrompt` (`androidx.biometric` 1.4.0-alpha05, the
  newest line that compiles against compileSdk 36) with the screen lock as fallback: locked at
  start and after 0, 1, 5 or 15 minutes in the background, nothing behind it is composed until the
  first unlock, `FLAG_SECURE` while locked, the recents thumbnail hidden while the lock is on
  (Android 13+), and Settings asks for the device credential to turn the lock on or off. Host
  details show the certificate fingerprint, addresses and "paired as", replace the access token
  (`POST /token/rotate`; the new token is stored even if the screen closes mid-call) and forget the
  computer, which now also stops its downloads and deletes its fetched files, thumbnails, previews
  and remembered sort order. Export diagnostics writes a plain-text report to a share sheet through
  its own `FileProvider`: tokens, pairing secrets, file names and paths, host and device names and
  logins never appear (names become per-export hashes, addresses are masked, host ids are kept), and
  the finished text goes through a second redaction pass. JVM tests cover eviction (order, pinned,
  grace, race with Keep offline, trigger and grace expiry on virtual time), the settings
  repository, the redactor and report with planted secrets, the lock's state machine, forgetting,
  token rotation and the three view models; the Compose tests (settings persistence on the real
  DataStore file, Storage, host details, lock screen) compile only until WP-29.
- Android pairing screens (WP-22, `client/android`): Add host with QR scan (CameraX + ZXing) and
  manual address entry, the six-digit code derived on the phone from the certificate it saw
  (`protocol/test-vectors/sas.json` is read by the unit test), approval wait with polling, and
  success storing host, pin and token. Flow A pins the handshake to the code's host id before
  sending the secret; flow B trusts the first contact for `GET /v1/host` only and pins everything
  after it. The host's own code is cross-checked, never shown, and a mismatch stops pairing.
  Tokens now live in `KeystoreTokenStore` (AES-256-GCM under an Android Keystore key, host id as
  associated data, DataStore file, no deprecated `EncryptedSharedPreferences`), replacing the
  in-memory placeholder. `FLAG_SECURE` is set while the camera, the code in flight or the code is
  visible; copy carries the "if someone told you to do this, stop" warning. `ObservingTrustManager`
  gained an optional expected pin. The instrumented pairing test against the real host
  (`--test-approve`) compiles; WP-29 runs it with the host port and id as instrumentation arguments.
- Android fetch engine and open-with (WP-27, `client/android`): Fetch on the file sheet and the
  browser's multi-select queue downloads (2 files per host, one sequential stream each) whose state
  lives in Room (`fetch_jobs`, `cached_files`; database version 2 by auto-migration). Downloads
  resume with `Range` + `If-Range` after a dropped connection, a pause or a dead process; `412
  file_changed` (or a changed ETag) starts over from zero and the panel says so; transient failures
  back off and wait for the network. Finished files are checked against `/hash` (pure-Kotlin BLAKE3
  verified against the official test vectors, SHA-256 fallback; on for files from 50 MiB by
  default): a mismatch deletes the copy and offers Retry. A foreground service of type `dataSync`
  shows progress and results as notifications, handles `onTimeout` (API 34/35) by pausing every
  job, and the next launch resumes them. Files land in `filesDir/fetched/` and leave the app only
  through a `FileProvider` with a read grant: Open with (`ACTION_VIEW`, system resolver), Share
  (`ACTION_SEND` chooser), Keep offline (pin, exempt from WP-28's eviction). Fetches of 50 MiB or
  more on a metered network ask first; a current saved copy opens without fetching, and the file
  sheet now shows it. JVM tests (MockWebServer over pinned TLS) cover resume, 412 restart,
  verification mismatch and 503 polling, refusals, offline waiting, pause/resume and the per-host
  limit; the 50 MiB host fixture test with a forced reconnect compiles and runs in WP-29.
- Android preview viewers (WP-26, `client/android`): Preview from the file sheet now opens a viewer
  chosen by the entry's preview kind. **Pages** (PDF, Office): a horizontal pager of page images at
  the screen's pixel width (clamped to the host's `preview_max_width`), pinch zoom with double tap,
  a page counter, and the next page prefetched once the visible one has loaded (the previous page
  too, nearest first; requests that leave the window are cancelled). A Word or Excel document shows
  page 1 until the host, which learns the count from its first render, reports it. **Image**: the
  same page request for one picture with zoom and "View original (Fetch)". **Text**: monospace, the
  encoding the host detected (`X-Gander-Text-Encoding`, shown as "UTF-16 LE, shown as UTF-8" when
  converted), read in 128 KiB slices that follow `X-Gander-Text-Next` (or the bytes received when
  the header is absent), loaded when the list reaches its end or with "Load more", capped at 16 MiB
  in memory. **Video**: the poster frame first, ExoPlayer only after the user taps play, reading
  `/file` through `OkHttpDataSource` on the host's pinned `transfer` client (Range for every open
  and seek; the token is a header added by the client's interceptor, never in the URL). Page images
  are Coil requests on the app's one loader with their own 256 MiB disk cache, keyed by host, share,
  file version (mtime and size), page and width, never by token; Office pages are kept off disk
  because the host may answer page 1 with a low-fidelity thumbnail under the same URL before the
  real render. `503 generating` is retried for up to 90 s (the host's `retry_after_ms`, clamped
  0.25-3 s) instead of three times, since a cold Office render takes 10-20 s; `preview/info` is
  retried the same way. A preview the host refuses for good (415, 413, 403, 404) is reported to the
  file sheet through `ActionRefusals` (outages and busy answers are not), and a viewer that loads
  clears it. JVM tests cover the page window and prefetch, retries, the disk cache, text paging and
  encoding, playback error mapping and zoom arithmetic; the Compose tests and the host fixture test
  (PDF page 1, 3 s of video, text slices) compile only until WP-29.
- Android file sheet and default-action rules (WP-25, `client/android`): tapping a file opens a
  bottom sheet with name, size, dates, flags, the host's `stat?meta=1` metadata (image, video, PDF,
  audio, Office) and the three actions Preview / Fetch / Stream, each with a one-line cost hint and,
  when off, the reason from the share's permissions, the host's capabilities, `host.state` or a
  protocol error code ("Preview: no renderer for .blend on studio-pc. Try Stream.", "Stream:
  studio-pc is locked"). `ActionRules` (pure Kotlin) is the spec's rule table: stream-only types,
  pdf/image/text/video/audio previews, phone handler with the 25 MiB metered threshold, cellular
  warning from 50 MiB, a remembered choice per extension (DataStore) that beats the table, and a
  default that is never a disabled action. Metered/unmetered comes from `ConnectivityManager` behind
  `NetworkStateMonitor`; the phone-handler check from `PackageManager` behind `PhoneHandlerLookup`
  (adds a `<queries>` entry). `GanderNavHost` gains `onPreview`/`onFetch`/`onStream` hooks for
  WP-26/27/54 (a "not available yet" dialog until then) and `onOpenFile` is gone; those screens tell
  the sheet why an action was refused through `AppGraph.actionRefusals`. JVM tests cover the rule
  table (metered/unmetered, size thresholds, stream-only types, missing capability, permissions,
  offline), the ViewModel, the preferences and formatting; Compose tests compile only until WP-29.
- Android client: the `stream_unavailable` error code (503, `POST /stream` before stream mode
  exists), added to `protocol/openapi.yaml` by WP-19, is now an `ErrorCode` and a
  `GanderError.Host.StreamUnavailable`; `ProtocolDtoTest` had been failing on the drift.
- Android thumbnails (WP-24, `client/android`): a Coil fetcher for `ThumbRef(host, share, path,
  mtime#size, size)` over each host's pinned `transfer` client, drawn over the type glyph in list
  rows and grid cells (the glyph stays when the host cannot make one). Cache key and disk cache
  entry carry the file's mtime and size, so an edited file refreshes, and never the token. `503
  generating` (and `429`) is retried after `retry_after_ms`, clamped to 0.1-5 s, up to 3 times,
  then the glyph stays; permanent refusals (415, 413, 404...) are remembered per file version.
  Requests carry `priority=visible|prefetch`; a per-host scheduler (4 slots) serves visible rows
  first, rows that scroll away cancel their request on the wire, and the 12 rows after the
  viewport are prefetched. Size is density based, from the host's `thumb_sizes`, capped at 256 px
  on phones and 512 px on tablets. JVM unit tests cover retry, scheduling, the disk cache,
  remembered refusals and the HTTP request against a pinned MockWebServer; the Compose test
  (compile only until WP-29) checks thumbnails appear within 5 s, and the host fixture test is
  deferred to WP-29.
- Android hosts, shares and browser (WP-23, `client/android`): Hosts screen with a 20 s online ping
  (`GET /v1/host` then `/v1/me`), relay chip, last seen, "paired as", rename and forget, pull to
  refresh and the "Private network" hint; Shares screen refreshed by `shares.updated`; Browser with
  list/grid, sort (directories first, persisted per share in DataStore), kinds filter, breadcrumb,
  cursor paging, debounced search with a partial-results indicator, Recent tab and multi-select.
  Listings and shares are stored in Room (`hosts`, `shares`, `listing_snapshots`, schema exported)
  for instant back-navigation, `If-None-Match` revalidation and offline display with a "cached"
  chip; stale data is shown only when the computer is unreachable, never for a refusal. Defines the
  `HostRepository`/`TokenStore` contract WP-22 completes, `BrowseRepository`, `EntryTarget` and the
  `onOpenFile`/`onFetchSelected` navigation hooks for WP-25/WP-27. JVM unit tests cover paging,
  ETag/snapshots, offline rules, sort persistence and the ViewModels; Compose tests (compile only
  until WP-29) cover list rendering against a fake repository.
- Android network layer (WP-21, `client/android`, `fyi.arlo.gander.core.net`): `GanderTrustManager`
  accepts a host only if the leaf certificate's SPKI SHA-256 equals the pin from pairing (no system
  trust store, no CA, no date check), `PinnedHostnameVerifier`, TLS 1.3 only; `GanderHttpClients`
  builds the per-host `api` (connect 5 s, read 30 s, WebSocket ping 20 s) and `transfer` (60 s idle)
  clients with an 8-request dispatcher, no redirects, cookies, cache or logging; bearer,
  `X-Gander-Protocol` and `User-Agent` interceptor (`BearerToken` never prints itself);
  `HostEndpoint` address fallback that remembers the working address and refreshes from `/me`;
  `GanderError` with one subclass per protocol error code plus transport failures; typed
  `GanderApi`; `/v1/events` WebSocket client; kotlinx.serialization DTOs for every schema in
  `protocol/openapi.yaml` with forward-compatible enums; `ObservingTrustManager` for pairing flow
  B's first contact. Unit tests use MockWebServer with per-run generated certificates and decode and
  round-trip every example in `protocol/openapi.yaml` (read from the repo, so drift fails the
  build).
- Android client scaffold (WP-20, `client/android/`): Gradle 9.6.0 wrapper, AGP 9.5.0, Kotlin
  2.4.21, Compose (BOM 2026.06.01), NDK 28.2.13676358, package `fyi.arlo.gander`, minSdk 26, compile
  and target SDK 36; version catalogue with OkHttp 5, Media3, Coil 3, `org.jitsi:webrtc`, CameraX,
  ZXing core, DataStore, Room, Navigation; `dataExtractionRules` and `fullBackupContent` excluding
  all app data; lint baseline; unit and instrumented test source sets; placeholder Hosts screen,
  `AppGraph` and `core/net`, `core/data` package markers. `client-android.yml` installs the NDK.
  Libraries are the newest releases that compile against API 36 (compileSdk 37 lines wait for the
  target bump).

### Spikes and findings

- Stream spike findings and ADR flip (WP-45, gate G4):
  `docs/specs/2026-11-01-stream-spike-findings.md` gains a "Summary and decisions" section, one
  consolidated matrix with a row for each of the 22 spike items (8 pass, 6 partial, 1
  deferred-device, 2 deferred-human, 5 skipped, 0 fail) and the spike spec's thresholds scored
  against the measurements (encode under 6 ms on the iGPU fails as written, webrtc-rs adapts in 5.5
  s with a 2 s buffer, glass-to-glass and tap-to-effect are device-gated). Decisions: webrtc-rs
  0.21.x accepted with conditions (delay-based estimator, PLI interceptor, liveness messages,
  trickle ICE; `str0m` is blocked by RUSTSEC-2026-0387 until it ships on `dimpl` >= 0.9), an own WGC
  wrapper, a Media Foundation hardware encoder, and `SendInput` never while locked. ADR-0001's
  streaming-library line and ADR-0005's D12 line are Accepted with an Evidence section each;
  `docs/architecture.md` and `docs/context.md` (Night 1 results, next actions, owner questions) are
  updated. Security finding for the owner, not edited into `docs/security.md`: WGC captures and
  `PostMessage` injects while the session is locked, so the host must enforce "no Stream while
  locked" itself.
- Stream spike, Android and input (WP-44): `spikes/stream/android/` (throwaway Gradle project,
  package `fyi.arlo.gander.spike.stream`, versions from the client catalogue, `org.jitsi:webrtc`
  155.0.1) answers the host's WebRTC offer, renders the H.264 track full screen with a frame counter
  overlay and sends taps as normalised coordinates; `spikes/stream/input/` (`spike-input`) streams a
  `gander-testwin` window, maps taps through the window frame and injects them with `SendInput` and
  `PostMessage`, and `spike-input matrix` measures matrix items C19, C20 and the DPI mapping without
  a phone. `spike-webrtc` is now a library plus its binary. Findings:
  `docs/specs/2026-11-01-stream-spike-findings.md`, "Android + input (WP-44)".
- WebRTC spike (WP-43, `spikes/stream/webrtc`, crate `spike-webrtc`): the WP-42 hardware H.264
  stream of the test window sent as a webrtc-rs 0.21.1 video track (ring crypto, host candidates
  on 127.0.0.1 only, no STUN/TURN/mDNS, TWCC + GCC + pacer), received, depacketised and decoded
  (ffmpeg) by a loopback viewer in the same process; `input` data channel echoing JSON; plain
  WebSocket signalling on 127.0.0.1; an in-process UDP relay throttles the link on a schedule
  (substitute for clumsy/QoS policy). GCC drives the encoder bitrate: runtime `MeanBitRate` on
  NVIDIA (`Encoder::set_bitrate`, new), encoder re-creation on Intel (its MFT ignores the runtime
  change and refuses `MEEncodingParameters`); a PLI/FIR forwarding interceptor makes keyframe
  requests reach the host. The host was repeated with str0m 0.24.1 per ADR-0005 D12
  (`spikes/stream/webrtc/str0m/`, not compiled by default: str0m pulls RUSTSEC-2026-0387 through
  `dimpl` 0.7, so `cargo deny` fails with it): webrtc-rs adapted in 2.5 s behind a 200 ms buffer
  but 5.5 s behind a 2 s one (its GCC reacted to loss only), str0m in 1.0-1.5 s. Results under
  `spikes/stream/webrtc/results/`; findings appended to
  `docs/specs/2026-11-01-stream-spike-findings.md`.
- Encode spike (WP-42, `spikes/stream/encode`, crate `spike-encode`): Media Foundation hardware
  H.264/HEVC encoders per adapter (`MFTEnum2` + `MFT_ENUM_ADAPTER_LUID`), fed zero-copy from WGC
  textures through a D3D11 video-processor BGRA->NV12 step, low-latency CBR, GOP and forced IDR;
  measures per-frame latency, sustained ms/frame, bitrate and the cross-adapter copy (C12) on the
  Intel iGPU and NVIDIA dGPU at 1080x2280 and 1920x1080, and commits an Annex-B dump for WP-43
  (`spikes/stream/encode/results/wp43-h264-1080x2280.h264`). Findings appended to
  `docs/specs/2026-11-01-stream-spike-findings.md` (the NVIDIA MFT needs the Optimus export; the
  Intel MFT needs a 1 ms timer resolution).
- Files-app spike (WP-47, `spikes/files-app`, throwaway, own Gradle project): a `DocumentsProvider`
  that serves files from an in-process loopback (127.0.0.1) HTTP Range server through
  `StorageManager.openProxyFileDescriptor`, logging every `onRead` offset and size. A PowerShell
  driver boots the `gander` AVD headless, drives the system Files app (browse, open, copy) and a
  platform `MediaPlayer`, runs controlled sequential, random, parallel and slow-read probes, and
  writes logs and tables to `spikes/files-app/results/`. Findings (read pattern table, threading,
  timeouts, WP-60 implications): `docs/specs/2026-11-01-files-app-spike-findings.md`.
- Capture spike (WP-41): `spikes/stream/capture` (crate `spike-capture`) captures one HWND with
  Windows Graphics Capture into BGRA8 D3D11 textures on a caller-created device (any adapter,
  video support, multithread protected), zero-copy over a bounded channel with QPC timestamps; the
  `spike-capture --matrix auto` runner measures C1-C5, C9-C11, C15 against `gander-testwin`.
  `windows-capture` 2.0.1 (MIT) is kept as a comparison backend only: it owns its device on the
  default adapter without video support. Findings in
  `docs/specs/2026-11-01-stream-spike-findings.md` (no occluder leakage; minimised windows deliver
  no frames; menus need `IncludeSecondaryWindows`, modal dialogs are never included; WGC keeps
  capturing while the session is locked, so the host must enforce the unlocked-only rule).
- Identity spike (WP-46): `spikes/identity` (crate `spike-identity`) asks the local Tailscale
  LocalAPI pipe and CLI for `status` and `whois` from an unelevated process, classifies each peer's
  path, and has a `--redact` mode for anything written to disk; findings in
  `docs/specs/2026-11-01-identity-spike-findings.md` (the pipe needs identification-level QoS, the
  relay rule needs `Online` and `Active`).
- Test window (WP-40): `gander-testwin` (`host/gander-testwin/`) is a per-monitor-DPI-aware Win32
  window that paints a 60 Hz frame counter (decimal digits plus a 24-cell binary strip), a moving
  box that changes colour when clicked, the local clock and two exact colour patches (magenta
  `RGB(255,0,255)` top-left, green `RGB(0,255,0)` bottom-right); title from argv; Ctrl+P popup menu
  and tooltip, Ctrl+D modal dialog; `--self-test` exits 0 after 20 s; a per-process,
  current-user-only named pipe (`\\.\pipe\gander-testwin-<pid>`) opens a second window, minimises,
  restores, reports state and pixel layout (protocol in `host/gander-testwin/README.md`); audio
  `tone` command is a stub hook. The library half (`layout`, `render`, `protocol`) lets later spikes
  decode the counter and check the patches from captured pixels. Adds `windows` 0.62 to the
  workspace dependencies.

### Build, CI and tooling

- Release builds also strip local folders from C code: `cargo xtask installer-build` and
  `installer-check --binary` set `CFLAGS_x86_64_pc_windows_msvc` to an MSVC `/d1trimfile:` per
  remapped folder, so libopus's assertion messages no longer carry the cargo registry path (260
  strings in the 0.1.0 host before the fix, 0 after).

- `cargo xtask mockups [--browser <path>] [--only <id>] [--scale <n>] [--jobs <n>]
  [--allow-fallback-fonts]`: renders every screen of the clickable UI mockups to PNG in the light
  and dark themes (`docs/design/mockups/png/<group>/<id>-<theme>.png`, 96 images) and writes the
  index `docs/design/mockups/README.md` from the page's own screen registry. Headless Microsoft
  Edge (or another Chromium) with a throw-away profile under `target/mockups/`; the window of each
  shot is the measured device frame, so the image is the frame and nothing else. Output is
  deterministic (a second run writes nothing), re-encoded with the `png` crate with every
  ancillary chunk dropped, and stops when the web fonts did not load. A unit test fails when a
  screen has no image or index entry. `cargo xtask icons` also writes
  `brand/png/gander-lockup-reversed-1440.png` (the lockup for dark backgrounds).
- Reproducible, path-free release exes: `cargo xtask installer-build` and `installer-check
  --binary` remap the workspace, cargo home, rustup home and sysroot (`--remap-path-prefix`,
  computed from the environment, `xtask/src/repro.rs`) and fail if an exe still names a local
  folder; release links of `gander-host` and `gander-render` add `/Brepro`. User-profile path
  strings in the exes: 960 and 121 before, 0 after; two builds in different directories are
  byte-identical (`docs/release/reproducible-builds.md`).
- `cargo xtask e2e-stream`: the host stage no longer sends `front` before the tap; it checks the
  host brought the test window forward at connect and falls back to `front` as the client does.
  The Android stage runs `park-watch` without `--front` (the flag stays, optional).
- `cargo xtask e2e-stream [--no-build] [--android] [--skip-engine]` (WP-57): Stream mode end to end
  against the loopback test host with `gander-testwin` as the only application and the loopback
  viewer as the phone (`gander-e2e stream`). Host stage: pair, events socket, `POST /v1/stream`,
  `window_found`, offer/answer, `streaming`, frames decoded with ffmpeg, the parked box found in
  the decoded pixels in the colour the window reports, a tap on it (after `{"t":"front"}`) that
  changes the colour on the pipe and in the decoded pixels, keepalive while static, pause/resume
  on `control`, a simulated lock ends the session, `DELETE ?close_app=true` closes the window, the
  kill switch. Engine stage: the same capture, encode, WebRTC and decode path in process (lock
  guard faked), which also runs while the laptop is locked. A really locked session makes the
  host refuse Stream: the harness then checks `412 host_locked` and the kill switch and reports
  the session steps `deferred: session locked` (input is never posted instead). `--android` runs
  `StreamViewerFixtureTest` on the `gander` AVD with the box position the host stage measured
  (`gander-e2e park-watch` parks and fronts each test window the phone opens), waits while
  another emulator runs and always kills its own. Report: `target/e2e/stream/report.json`.
  First run (2026-11-02, session locked): 10 passed, 0 failed, 11 deferred.
- `cargo xtask e2e-android` leaves `StreamViewerFixtureTest` out of its run (it needs the stream
  harness); its deferral note now reads `deferred: run by cargo xtask e2e-stream --android
  (WP-57)`.

- Supply chain (WP-82): a CycloneDX 1.5 SBOM for each app, an embedded dependency list in the host
  executables, Dependabot, and reproducible-build notes (`docs/release/reproducible-builds.md`).
  `cargo xtask sbom` writes `target/sbom/gander-host.cdx.json` by hand from `cargo metadata` and the
  `Cargo.lock` checksums (no cargo-cyclonedx): 349 third-party crates plus 4 first-party, each with
  a `pkg:cargo` purl, SPDX licence and SHA-256; build-only crates (build scripts, proc macros) are
  `scope: excluded`. It is validated against the vendored CycloneDX 1.5 JSON schema
  (`xtask/schemas/cyclonedx-1.5/`, checked by a small validator in `xtask/src/jsonschema.rs`, no
  new crate) and is deterministic (`SOURCE_DATE_EPOCH` or the HEAD commit time);
  `cargo xtask sbom --check <file>` validates any SBOM. The Android SBOM is
  `./gradlew --no-daemon cyclonedxSbom` (CycloneDX Gradle plugin 3.5.1, pinned in the version
  catalogue, configured in the root `build.gradle.kts` only): the 228 components of
  `releaseRuntimeClasspath` in `target/sbom/gander-android.cdx.json`. Without `cargo auditable`
  (not installed), `gander-host.exe` and `gander-render.exe` carry their dependency list in a
  `.dep-v0` section in the `cargo auditable` format (zlib JSON): both `build.rs` files include
  `host/build-support/auditable.rs`, which writes a small COFF object and hands it to `link.exe`;
  the same code walks the graph for the SBOM. The MSI installs the SBOM as
  `sbom\gander-host.cdx.json` (new component `Cmp_Sbom`, bind path `sbom`), `installer-build`
  generates it first, builds with `--locked`, and fails unless both exes carry a list that matches
  it; `installer-check` validates the SBOM and `--binary` checks the exe. CI: an SBOM step and
  artefact in `host-windows.yml` and `client-android.yml`, plus a Gradle wrapper checksum check
  (jar and distribution against gradle.org). `.github/dependabot.yml`: weekly grouped updates for
  cargo, the three Gradle projects and GitHub Actions. No `rust-toolchain.toml` (it would force a
  toolchain download); the doc says how to add it.
- WiX 5 MSI (WP-1A follow-up): `installer/main.wxs` is ported from WiX v3 to WiX 5 (the v4 schema:
  `Package Scope="perUser"`, `StandardDirectory`, `FileRef` custom actions with `Condition`
  attributes, fixed component GUIDs, `WixUI_Minimal` from `WixToolset.UI.wixext`, the lens icon as
  `ARPPRODUCTICON`, and a Start menu entry that runs `gander-host.exe --open`). The design is
  unchanged: per-user, autostart through `gander-host --autostart on`, the separate elevated
  firewall step, uninstall removes the task and the rules. `cargo xtask installer-build` replaces
  the cargo-wix path (`icons --check`, notices, release build of `gander-host` and `gander-render`,
  `wix build` with named bind paths into `target/installer/Gander-<version>-x64.msi`,
  `wix msi validate`, and an administrative extract that installs nothing) and
  `cargo xtask installer-check` is rewritten for the v4 schema and, with the `wix` tool present,
  does a dry `wix build`, `msi validate` and extract against stand-in binaries. Built here:
  `Gander-0.0.0-x64.msi`, 7.7 MiB, unsigned, `wix msi validate` clean, seven files in the
  administrative extract. Toolchain: the .NET SDK 8.0.427 (winget, machine-wide), `wix` 5.0.2 and
  `WixToolset.UI.wixext` 5.0.2; paths in `installer/README.md`. The
  `[package.metadata.wix]` table and `GANDER_REPO_ROOT` are gone. The MSI has not been installed.
- `cargo xtask icons [--check]` (Lens logo): rasterises `brand/*.svg` with `resvg` (Apache-2.0 OR
  MIT, tiny-skia BSD-3-Clause; passes `cargo deny`) into committed files, so no build needs a
  rasteriser: `gander.ico` (16, 20, 24, 32, 40, 48, 64, 256 px), the 64 px window icon, the tray
  PNGs (4 states x 16/20/24/32 px), brand PNGs and two legibility review sheets. Deterministic;
  `--check` and a unit test fail when a committed file is stale.
- `cargo xtask build-host [--release]` and `cargo xtask run-host [--release] [-- <args>]` build
  `gander-host` and `gander-render` in one cargo call, so the render worker is always next to the
  host. `cargo xtask e2e-android` runs `PairingInstrumentedTest#qrPairingThroughTheAddHostLogicFinishes`
  (flow A through the Add host logic) with a code from the test host's `qr` command, redacted from
  the logs.

- `cargo xtask e2e-android`: the skips of `StreamViewerFixtureTest` (it needs a host with a real
  stream and the frozen box position) are a named deferral, `deferred: needs cargo xtask e2e-stream
  (WP-57)`, in `summary.txt` and `summary.json`, and no longer fail the gate. The list holds exact
  class names (`DEFERRED` in `xtask/src/e2e_android.rs`); any other skip, and any failure in that
  class, still fails.
- `cargo xtask docs-check` (WP-1C, `xtask/src/docs.rs`): walks every Markdown file (not `target/`,
  `.worktrees/`, `node_modules/`, `build/`) and checks that each relative link, image, reference
  definition and `href`/`src` target exists with exactly the spelling on disk (GitHub is
  case-sensitive) and stays inside the repository, and that each `#anchor` into a Markdown file
  matches a heading (GitHub slug rules, repeats get `-1`, `-2`) or an `id`/`name` attribute. Code
  blocks, code spans and HTML comments are ignored; external links are counted and never fetched.
  Exit 1 lists each broken link with file and line. The link parser, the slugger and the resolver
  are unit-tested, one test runs the check over the repository's own docs, `test-all` runs it, and
  `.github/workflows/docs.yml` runs it on Markdown changes.
- Emulator end-to-end (WP-29, gate G3): `cargo xtask e2e-android` starts the test-hooks host on
  127.0.0.1 over a generated fixture share, pairs two test devices from the machine
  (`gander-e2e pair`, token handed over in a temp file, never printed), boots the `gander` AVD
  headless, installs the debug and androidTest APKs and runs the whole instrumented suite against
  the real host through `10.0.2.2` (pair, list, thumbnails, PDF page, text slices, video playback,
  50 MiB fetch with forced reconnect and resume), then revokes one device through the host's stdin
  and checks the client reports `TokenRevoked` (401). New `HostSessionFixtureTest` (list and
  revoke). JUnit XML per class and a summary in `target/e2e/android/`; tokens are redacted from
  every file written; the emulator is always killed. Without an AVD the JVM unit suite runs and
  G3 is reported `deferred: needs emulator`.
- Host integration harness (WP-1B): `cargo xtask e2e-host` builds the test-hooks host, generates a
  fixture share (images, a 3-page PDF, UTF-16 text, nested, unicode and hidden names, a 24 MiB
  file, a deny-ACL folder), starts the host on 127.0.0.1 port 0, pairs through the test hook and
  runs the conformance suite (`host/gander-e2e`, pinned TLS client): every endpoint of
  `protocol/openapi.yaml` with a schema-validated success case, every error code reachable from
  outside (the rest listed as `not_reachable` with a reason), and `/v1/events` (hello, host.state,
  share.changed, ping, device.revoked). Report in `target/e2e/report.json`; the host is always
  killed (also through a kill-on-close job object). The schema validator moved to
  `gander_core::protocol::schema`. Host wiring done with it: events are part of the API state
  (`/list` arms the directory watch before listing, a config change replaces the events' share view
  and sends `shares.updated`, `capabilities.events` is true), `/me.network.relayed` comes from the
  trust network, `--test-approve` also accepts `Host: 10.0.2.2` on loopback (Android emulator),
  `--test-grant <share>=<perms>` grants approved test devices, and the test host reads
  `revoke <device-id>` on stdin. Bugs it found and fixed: thumbnails and previews always failed with
  500 because the render worker's handle duplicate asked for more access than the resolver's handle
  has; axum's plain-text rejections (oversized body, method not allowed) are now protocol error
  bodies (`too_large`, `bad_request`).
- `cargo xtask check-elf-alignment <dir>` now also checks APKs and AABs found inside the directory
  (it used to look only for loose `.so` files and passed vacuously on a build output folder).
- Repository scaffold (WP-01): Cargo workspace with empty crates `gander-core`, `gander-win`,
  `gander-stream`, `gander-render`, `gander-host`, `gander-testwin`; shared
  `[workspace.dependencies]` and `[workspace.lints]`; `rustfmt.toml`, `clippy.toml`,
  `.editorconfig`, `deny.toml` (permissive licences only, advisories, crates.io only).
- `cargo xtask` (`xtask/`): `preflight` (master plan 1b checks, optional markdown report),
  `test-all`, `vectors-check`, `check-elf-alignment` (Android 16 KB page-size check for APKs,
  directories and `.so` files, runs on Windows and Linux). Aliases `cargo xtask` and `cargo lint`.
- CI workflows: `host-windows.yml` (fmt, clippy, build, test, vectors, cargo-deny on
  windows-latest) and `client-android.yml` (Gradle unit tests, lint, ELF 16 KB check on Ubuntu;
  Gradle steps activate when `client/android/` exists). Actions pinned to commit SHAs.
- `LICENSE` (Apache-2.0, the working default of ADR-0003) with SPDX headers on Rust sources, and a
  draft `SECURITY.md` (disclosure address placeholder, supported versions, no bug bounty).
- Preflight report for the first autonomous run (`docs/reports/2026-11-01-preflight.md`);
  Android emulator 37.2.13, `system-images;android-36;google_apis;x86_64` and the `gander` AVD
  installed; PowerShell 7.6.9 installed; `cargo-deny` 0.20.2 installed.

### Docs and decisions

- UI mockup images: `docs/design/mockups/png/` holds a PNG of every one of the 48 mockup screens in
  the light and dark themes (about 6 MiB in all, device frame only, transparent corners, no
  metadata), indexed with each screen's purpose in `docs/design/mockups/README.md`. The page gained
  a `?capture` mode (frame only, scale 1) for the renders; normal viewing is unchanged. The sample
  profile path in the Shares mockup is now `C:/Users/alice/Documents` (was `robin`), matching the
  sample account alice@example.com. Regenerate with `cargo xtask mockups`.
- Real-application Stream checklist for the owner (WP-57):
  `docs/specs/2026-11-02-stream-real-app-checklist.md` (PowerPoint, Edge PDF in the isolated
  profile, Photos, Notepad, Blender if installed; steps from the tablet, what to observe and a
  results table to fill in).

- UI redesign proposal (design only, no code changed): `docs/design/2026-11-02-ui-redesign.md`
  defines the design system (goose-down neutrals, slate ink, greylag-beak orange as the single
  accent used only for live, default and selection; light and dark tokens with contrast ratios;
  Android and Windows type scales; the "work rail" glyph that shows which device does the work),
  the screen inventory with required states, and the mapping onto Compose `MaterialTheme` and
  egui `Visuals` (Mica approximated without blur). `docs/design/mockups/index.html` is a
  self-contained clickable mockup of 48 screens: Android phone, the tablet two-pane browser and
  stream, and the Windows tray, windows, toasts and kill switch. Open questions for the owner
  are listed at the end of the document.
- Docs sync (WP-1C): `CLAUDE.md` Status says what is built (Phase 0 spikes; the Phase 1 host and
  Android client with gates G0-G4 passing; Phase 2 WP-50 to WP-54; WP-55 to WP-57 not started) and
  its Commands table lists every xtask plus `gander-host --ui-smoke` and `--autostart` (the Commands
  section first listed the real commands at WP-01); `README.md` gains "A note on relays" and a
  status that matches the repository; `docs/context.md` records the Phase 2 results, the deferred
  runs and the Night 2 next actions; ADR-0002 and ADR-0007 gain Evidence sections (LocalAPI pipe
  DACL and QoS; the job-object CPU limit, Office COM outside the job object, the egui root
  viewport); this changelog is grouped by area.
- Plan review 2026-11-01: gaps, risks and research synthesis
  (`docs/research/2026-11-01-plan-review.md`).
- Implementation specs: protocol v1, pairing and trust, path resolution, preview renderers,
  stream launch policy, stream spike, Android client v1 (`docs/specs/2026-11-01-*.md`).
- Shared hostile path corpus for every host implementation (`protocol/test-vectors/paths.json`).
- New ADRs: 0005 transport and protocol, 0006 stream launch policy, 0007 process model. ADR-0001
  (stack) and ADR-0002 (network trust) accepted by default; ADR-0003 licence default Apache-2.0.
- Executable master plan for autonomous implementation runs
  (`docs/plans/2026-11-01-master-plan.md`).
- Architecture, security, roadmap and context updated to match the decisions.
- Name the project Gander (ADR 0004), replacing the "OTG View" codename.
- Project planning docs: vision, landscape research, architecture draft, threat model, roadmap,
  ADRs.
