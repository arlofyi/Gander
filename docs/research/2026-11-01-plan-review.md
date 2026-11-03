# Plan review 2026-11-01: gaps, risks, research, and what changed

_A re-analysis of the planning corpus (vision, landscape, architecture, security, roadmap, ADRs)
before any code exists. Research was done on 2026-11-01 with web sources plus measurements on the
development machine; confidence per fact is noted where it matters. Facts with "(verified here)"
were checked on this machine._

## 1. Verdict

The thesis holds: nothing file-first with a per-file choice of where compute happens exists, and
the two nearest projects (Baton, FileApex, both tiny and 2026) confirm the gap rather than fill it.
The original plan was right to spike Stream first and to make security non-negotiable.

What the plan missed falls into four groups:

1. **Stream mode's real security boundary was mis-stated.** It is not share-scoped: any streamed
   app exposes the whole filesystem through its own dialogs, input injection is screen-coordinate
   based, and file associations resolve to interpreters and browsers. The fix is a handler-executable
   allowlist with owner approval and honest "trusted device" framing (ADR-0006).
2. **The host's platform constraints were under-specified.** WGC and input only work in the
   interactive session (so no Windows service), Tailscale's LocalAPI only answers the logged-in
   user, windows that do not repaint emit no frames, popups need Windows 11 24H2, and relayed
   Tailscale paths run at a few Mbit/s. All now in ADR-0005/0007 and the stream specs.
3. **The client stack argument was wrong.** "One codebase" does not hold because pinning,
   downloads, the Files-app provider and WebRTC are native code regardless; Expo's download path
   cannot even use a custom trust manager. Native Kotlin is the default (ADR-0001).
4. **Nothing was executable by an autonomous session.** Open decisions had no defaults, no spec
   defined the wire protocol or path rules precisely, there were no test vectors, and "done" was
   not machine-checkable. Seven specs, a shared test corpus and a master plan with gates fix that.

## 2. What changed (index)

| Artifact | Change |
| --- | --- |
| `decisions/0001-stack.md` | Rust host confirmed; Kotlin + Compose client chosen; libraries pinned; Flutter named as alternative, RN dropped |
| `decisions/0002-network-trust.md` | Accepted by default: OS VPN + adapters; LocalAPI pipe primary, CLI fallback; tagged/shared-node policy; relay classification |
| `decisions/0003-license.md` | Apache-2.0 default; GPL (Sunshine) and FFmpeg constraints recorded |
| `decisions/0005-transport-and-protocol.md` | New: pinned TLS + HTTP/1.1 + WS; WebRTC host-only; browser requests refused |
| `decisions/0006-stream-launch-policy.md` | New: handler-executable allowlist, owner approval, browsers isolated, trusted-device framing |
| `decisions/0007-process-model.md` | New: user-session tray app, egui UI, isolated render workers, no service in v1 |
| `specs/2026-11-01-protocol-v1.md` | New: full REST + WS + signalling spec with error codes and limits |
| `specs/2026-11-01-pairing-and-trust.md` | New: QR and manual flows, SAS, tokens, firewall, identity binding |
| `specs/2026-11-01-path-resolution.md` + `protocol/test-vectors/paths.json` | New: handle-relative algorithm, reparse policy, 60+ hostile vectors |
| `specs/2026-11-01-preview-renderers.md` | New: renderer catalogue, worker isolation, cache, budgets |
| `specs/2026-11-01-stream-launch-policy.md` | New: policy, window discovery, fit-to-phone, input rules, lifecycle |
| `specs/2026-11-01-stream-spike.md` | New: what to build, 22-item measurement matrix, thresholds |
| `specs/2026-11-01-android-client-v1.md` | New: screens, net layer, cache, fetch engine, stream viewer, SAF design |
| `plans/2026-11-01-master-plan.md` | New: preflight, decision defaults, work packages, gates, fallbacks, human-gated list, morning report |
| `architecture.md`, `security.md`, `roadmap.md`, `context.md` | Updated to match |

## 3. Cross-cutting gaps found

Each item: the gap, why it would have hurt, and where the fix now lives.

1. **Interpreters and browsers as file handlers.** `.py` opens python, `.js` opens wscript, and on
   this machine `.pdf` opens Microsoft Edge (verified here), which would have streamed the owner's
   whole browser. Fix: resolve the handler executable without executing, never-list, owner-approved
   catalogue, isolated browser windows (ADR-0006, stream-launch-policy spec).
2. **Stream is app-level access.** Open/Save dialogs of a streamed app reach everything; no v1
   sandbox exists. Fix: Stream is a per-share, per-device, off-by-default permission described as
   trusted-device access; residual risk stated in `security.md`.
3. **No audio in Stream.** A deck with a video, or any media app, was silent by omission. Fix:
   per-process loopback capture (Windows build 20348+, `wasapi` crate supports it) as Phase 2.1.
4. **Windows that do not repaint produce no WGC frames**, minimised windows produce none, and
   apps may stop drawing when off-screen. The stream would freeze or go blank. Fix: repeat-last-frame
   keepalive, keyframe-on-request, restore-if-minimised, spike items C4/C9.
5. **Popup menus and tooltips** are separate windows; `IncludeSecondaryWindows` exists only on
   Windows 11 24H2+, clips to the main window and vanishes when an ordinary window overlaps.
   Fix: minimum Windows 11 for Stream, documented limitation on older builds, spike item C5.
6. **Locked session, display off, headless.** WGC is "not supposed to" capture the secure
   desktop (and a 2026 update that changed it was rolled back), injection is silently dropped when
   locked, a sleeping monitor yields no frames. Fix: `host_locked` precondition with honest text,
   session lock tracking, keep-awake during sessions, Phase 4 virtual display evaluation.
7. **Already-running apps.** PowerPoint re-uses its process; "find the new window of the launched
   pid" fails. Fix: title-match strategy plus pick fallback; `ApplicationFrameHost` mapping for
   packaged apps.
8. **Input needs the foreground**, and `SetForegroundWindow` is restricted with no stable
   workaround; posted messages are dropped by Chromium/Electron. Fix: inject only while our window
   is foreground, pause with reason otherwise, "bring to front" action, local-user-activity pause.
9. **Windows Firewall.** Tailscale's own `Tailscale-In` rule normally allows inbound on tailnet
   addresses, but a dismissed first-listen prompt creates Block rules that silently win, Public
   category disables it, and Shields Up blocks everything. Fix: installer rule scoped to tailnet
   ranges, tray self-check that lists Block rules naming Gander.
10. **Tailscale identity edge cases.** Tagged nodes report the pseudo-user `tagged-devices`;
    shared-in nodes resolve to an external user; LAN/loopback sources are "not found"; the LocalAPI
    pipe only admits the logged-in user. Fix: pairing spec semantics, ADR-0002, ADR-0007.
11. **DERP relays are slow** (measured 2.2 Mbit/s intercontinental; 5-10 Mbit/s reported). The
    owner's own tablet is currently relayed (verified here). "No relay" as a principle needs the
    honest footnote that Tailscale may relay end-to-end-encrypted traffic through its servers. Fix:
    relay classification in `/me`, client warning before Stream, README wording.
12. **Tailnet addresses change** (admin reassignment, ipPool, IPv6-only option) and the adapter
    comes up slowly after start. Fix: re-plan bindings on address-change notifications plus a 30 s
    safety poll; retry-bind.
13. **OneDrive and other cloud placeholders are reparse points.** A naive "refuse all reparse
    points" rule would have made every OneDrive folder unshareable. Fix: allow the cloud-files,
    WOF and dedup tag families, deny the rest; expose `cloud_only` so fetch can warn.
14. **Windows name games beyond `..`**: trailing dots and spaces are stripped by Win32, reserved
    device names, 8.3 aliases, unpaired-surrogate names that are not UTF-8. Fix: per-segment rules,
    short-name check, `unlistable` count, 60+ vectors in `paths.json`.
15. **Preview renderers are attack surface.** Image/PDF decoders and third-party shell thumbnail
    handlers crash routinely. Fix: separate worker process per job, job-object caps, handle passed
    not path.
16. **Office previews are not free.** Embedded thumbnails are often absent; the "pptx first page
    in under 3 s" success metric is only achievable with Office COM export (Office is installed
    here) or LibreOffice, else Stream. Fix: renderer ladder with `estimated_ms`, metric caveated.
17. **Self-signed pinning on the phone.** OkHttp `CertificatePinner` and the RN pinning libraries
    pin on top of system trust; Expo downloads bypass custom clients; `DownloadManager` trusts only
    static config. Fix: custom `X509TrustManager`, own download engine (ADR-0001, client spec).
18. **Android background limits.** `dataSync` foreground services get 6 h per day; Android 16
    tightened job quotas; Android 17 adds a local-network permission that lists 100.64.0.0/10 as
    local. Fix: resumable downloads that survive `onTimeout`, permission plan in the client spec.
19. **Play and 16 KB.** targetSdk 36 required now; 16 KB page support required for updates from
    2027-02-01; all candidate WebRTC AARs measure aligned but a transitive `libyuv` once broke it.
    Fix: AGP 8.5.1+/NDK r28+, ELF alignment check in CI.
20. **Apple 4.2.7** ("mirror of specific software" must be on a "local and LAN-based network").
    Stream on iOS is exactly the targeted shape; enforcement against VPN clients is unknown. Fix:
    iOS phase leads with Files/Preview/Fetch, Stream flagged as a review risk with notes prepared.
21. **Token hygiene on the phone**: Android auto-backup would have copied the token store to the
    cloud. Fix: extraction rules exclude it; rotation after restore.
22. **Pairing scam patterns have evolved**: device-linking proxies (WhatsApp "GhostPairing"), QR
    lures, device-code phishing (FBI PSA 2026-05-21). Fix: pairing exists only while the owner's
    dialog is open, approval on host names the device and shows a code, explicit "if someone asked
    you to do this, stop" text, one pending request at a time.
23. **No update or disclosure story** for a remote-access tool that will need security patches.
    Fix: SECURITY.md, opt-in update check, signed builds, SBOM, in the release-engineering work
    packages.
24. **No telemetry means no crash insight.** Fix: local diagnostics export, never automatic.
25. **Laptop realities**: battery, thermal, lid closed, Modern Standby. Fix: `on_battery` state,
    bitrate cap, keep-awake, spike items C7/C8.
26. **Hybrid GPUs** (this machine: Intel iGPU + RTX 5060): WGC textures come from one adapter,
    the preferred encoder may be on the other. Fix: spike item C12, encoder-on-capture-device rule.
27. **Fast user switching / second user** would collide on the port. Fix: exclusive bind, loud
    failure, documented limit.
28. **Licence contamination risk**: Sunshine is GPL-3.0, Baton shows no licence, FFmpeg GPL builds.
    Fix: ADR-0003 constraints; OS codecs only; ffmpeg only if already on PATH.

## 4. Research facts that changed assumptions

Primary sources are listed in the four research reports summarised here; the most decision-relevant:

**Windows capture, encode, input**
- WGC `CreateForWindow` needs Windows 10 1903; cursor flag 2004; `IsBorderRequired=false` is
  documented via `GraphicsCaptureAccess.RequestAccessAsync(Borderless)` (consent prompt) and the
  border is drawn on the host display, not into frames (one report). `IncludeSecondaryWindows`
  needs 24H2 (build 26100; this machine is 26200).
- Media Foundation hardware H.264 from Rust: `moq-video` (MIT/Apache) shows the working pattern
  (hardware MFT, DXGI device manager on the capture device, `MFCreateDXGISurfaceBuffer`,
  `CODECAPI_AVLowLatencyMode`, CBR, GOP). A C++ report measures about 1.6 ms P50 per 1080p frame on
  an RTX 5060 Laptop GPU, the same class as this machine.
- `windows-capture` 2.0.1 exposes zero-copy textures but creates its own D3D device (reported) and
  its frames are `!Send`; a thin own wrapper is a realistic alternative.
- `SendInput` is UIPI-bound and screen-coordinate based; `SetForegroundWindow` rules are strict;
  `AllowSetForegroundWindow` is one-shot. Pointer injection (`InjectSyntheticPointerInput`, 1809+)
  can deliver real touch/pen semantics later.
- Per-process audio loopback: build 20348+, `wasapi` 0.26 supports it.
- `SetThreadExecutionState` is the keep-awake; Modern Standby keeps the system running with the
  screen off; a sleeping monitor still yields no frames.
- Tauri 2.12 is stable but needs WebView2 and a JS toolchain; `tray-icon` 0.26 + `egui` 0.37 is
  the smaller-surface choice.

**WebRTC in Rust**
- `webrtc` 0.21.1 (Sept 2026): "1.0-rc in everything but version"; H.264 packetizer; TWCC; GCC
  estimator and pacer landed recently, opt-in, simulator-tested. Wildcard binds expand to every
  interface, so explicit addresses are mandatory for our no-wildcard rule.
- `str0m` 0.24.1: built-in BWE, API churn, P2P less tested; SPS/PPS silently dropped if STAP-A
  exceeds MTU.

**Tailscale**
- `tailscale whois --json` since v1.56; works unelevated (verified here: returns `Node`,
  `UserProfile`, `CapMap`). LocalAPI pipe `\\.\pipe\ProtectedPrefix\Administrators\Tailscale\tailscaled`
  admits the logged-in user and LocalSystem only. `WhoIs` and `Status` are the only calls marked
  stable; `status --json` is "subject to change".
- `Tailscale-In` firewall rule: inbound allow to tailnet addresses, Private/Domain profiles, no
  program restriction (the adapter is Private here, verified).
- DERP: 2.2 Mbit/s measured intercontinental; peer relays (1.86+) help but Android cannot be one.
- Android Tailscale calls `setMetered(false)` and `setUnderlyingNetworks`, so metered detection
  should reflect the underlying network; open bugs exist; the Android app exposes no peer API.

**Android**
- Self-signed pinning: custom `X509TrustManager` in Kotlin is the only clean path; RN libraries
  and Expo downloads do not work for this case; `DownloadManager` cannot pin.
- `openProxyFileDescriptor` supports on-demand random access for a `DocumentsProvider`, with reads
  in 64-128 KiB FUSE chunks (read-ahead needed).
- iOS File Provider: whole-file fetch on iOS (partial content is macOS-only); ~20 MB extension
  memory reports.
- Play: targetSdk 36 required since 2026-08-31; 16 KB support required for updates from
  2027-02-01; `dataSync` 6 h/day; user-initiated transfer jobs are the Play-preferred path.
- libwebrtc Android: no Java API for playout delay; sender-side playout-delay extension is the
  lever; decoder latency 3-30 ms by SoC; HEVC has a reported crash in one distribution, so H.264 first.

**Prior art and UX**
- Baton (C#/.NET 9 WPF + Kotlin, no licence shown): WGC + Media Foundation H.264 + pinned TLS over
  TCP, per-app audio loopback, QR-pinned pairing, read-only shares with path hardening, fetch only,
  no preview; states it cannot work while locked and protected video captures black. Read for
  ideas only.
- FileApex (MIT, 66 stars): browse/send/sync with embedded tsnet, QR or 6-digit pairing, no
  preview or streaming.
- Sunshine cannot capture a single window and WGC does not work from its service model (open since 2024).
- Windows App's touch model (direct touch vs pointer, two-finger tap/hold = right click,
  two-finger drag = scroll, local pinch) is the convention to adopt; Moonlight's most requested
  gap is pinch-zoom of the stream; its hard-coded three-finger tap blocked Windows multi-touch.
- Phone file managers that thumbnail by reading remote bytes stall listings (Material Files).
- Taildrive is still alpha with many open mobile bugs; nothing announced makes Gander redundant.
- OneDrive "Fetch files" was retired in 2020 for low usage: demand is an enthusiast niche until
  proven otherwise.

## 5. Per-phase pre-plan: what will go wrong, and improvements

### Phase 0: spikes
- Likely trouble: `windows-capture`'s D3D device ownership versus the encoder device; async MFT
  unlock and SPS/PPS repetition quirks; webrtc-rs GCC being opt-in and young; hybrid-GPU copies;
  no frames from a static test window until it repaints; C6 (lock) cannot run unattended.
- Improvements: a deterministic test window with a frame counter (also reused by Phase 2 tests);
  measure both WebRTC crates against the same recorded H.264; include the Tailscale identity spike
  (unelevated pipe, tagged/shared-in output) and the `openProxyFileDescriptor` spike so Phase 3
  design is grounded.
- Autonomous: everything except C6 (needs a password to unlock) and real-device latency numbers.

### Phase 1: Browse, Preview, Fetch
- Likely trouble: Office COM in a worker (STA threading, hidden instance cleanup, hangs); shell
  thumbnail handlers crashing (hence the worker); listing 50k-entry folders; ReadDirectoryChangesW
  on cloud folders flooding events; firewall Block rules from earlier dev builds; emulator
  reaching the host only via `10.0.2.2` (the host must bind loopback, which it does) while the
  tailnet adapter path cannot be tested from the emulator.
- Improvements added: `recent`, `search`, `stat?meta`, cloud-only flag, ETag-based list caching,
  events channel, relay classification, firewall self-check, kill switch pausing the listener,
  per-device concurrency limits, honest error codes.
- Autonomous: host builds + hostile corpus + integration tests on loopback; client builds + unit
  tests; emulator end-to-end if system images are installed (preflight). Real-device test and
  Tailscale identity binding are human-gated (plug in the tablet).

### Phase 2: Stream
- Likely trouble: foreground acquisition failures; already-running single-instance apps; menus
  invisible on pre-24H2 hosts; stream freezing on static content without keepalive; input landing
  on the wrong window when the local user switches apps; close-on-end with unsaved changes; relay
  bandwidth; bitrate adaptation; Android decoder variance.
- Improvements added: catalogue with launch templates and window-match strategies, fit-to-phone,
  idle-user prompt, pause-on-focus-loss, local-user-activity pause, session resume, audio track,
  clipboard, host state events, trusted-device framing, OS capture border kept as indicator.
- Autonomous: engine + policy + discovery against the test window; emulator can show the stream
  functionally. Latency, real apps (PowerPoint, Blender), and anything needing the lock screen are
  human-verified.

### Phase 3: Breadth
- iOS: 4.2.7 risk for Stream; File Provider is whole-file; plan Files + Preview + Fetch first.
- macOS host: ScreenCaptureKit needs screen-recording permission and periodic re-authorisation
  prompts (Sequoia) annoy users; Linux: portal picker means the user must choose the window on the
  host screen, which conflicts with remote initiation (document as attended-only).
- Write access: conflict handling, trash, upload via share sheet, SAF write modes; keep
  last-writer-wins with host-side trash.
- Headless or pre-login Preview/Fetch needs a service (ADR-0007 option 2) and SYSTEM-level
  LocalAPI access; separate ADR when reached.

### Phase 4: Later
- Unattended Stream: evaluate a virtual display (indirect display driver) versus RDP RemoteApp
  (RAIL) via FreeRDP on Windows Pro; RAIL gives server-side rendering, input, audio and clipboard
  in a separate session but is unsupported outside Server SKUs and logs out the console view.
  Needs its own threat review.
- Wake-on-LAN via another Gander host on the same LAN; guest shares with expiry; desktop-to-desktop.

### Release engineering (new, was one paragraph)
- Code signing (Azure Trusted Signing or an OV cert) is a purchase; unsigned builds hit SmartScreen.
- Installer: cargo-wix (MSI) with autostart task and firewall rules; winget manifest later; MSIX
  only if the Store is wanted (changes WGC border consent behaviour, so decide deliberately).
- SBOM (`cargo auditable`, CycloneDX), `cargo deny` for licences/advisories, Dependabot/Renovate,
  reproducible build notes, SECURITY.md with a disclosure address, opt-in update check against
  GitHub releases, Play data-safety form ("no data collected"), F-Droid feasible only with Maven
  Central WebRTC AARs.

## 6. Feature exploration: are we thinking correctly?

**Keep (confirmed by research):** file-first home screen; three explicit modes with a remembered
default and a cost hint; host-side thumbnails only; touch-native Stream with local pinch-zoom and
two labelled modes; QR pairing with approval on host; OS capture border as the indicator; no relay
of our own; honest unavailability messages.

**Added to the plan:**
- Recent files across shares (the actual "I was just working on it" case).
- Search within a share now; Windows Search index later.
- Metadata panel (dimensions, duration, pages, EXIF subset).
- Cloud-only awareness (OneDrive placeholders) with a fetch warning.
- Live listing refresh over the events channel.
- Video and audio playback by Range (no transcoding) as part of Fetch.
- Office page rendering via Office COM / LibreOffice ladder; RAW embedded previews.
- Stream: audio, clipboard text, fit-to-phone, session resume, idle-user prompt, local-user pause,
  "fetch the saved file" after a stream, catalogue of apps with first-use approval.
- Relay awareness and honest README wording about Tailscale's DERP.
- Host-side device list with last seen, identity, revoke; audit log viewer; connectivity self-check.
- Client: offline pinning, cache cap, biometric lock, diagnostics export.

**Considered and deferred (recorded so they are not re-litigated):**
- Folder download as zip: Phase 3, only on explicit request (no silent zipping).
- Deep-zoom tiles for huge images and pages: Phase 3.
- Watch-only Stream (no input): cheap and useful for "show me what is on screen"; Phase 2.2.
- Pen/stylus pressure via pointer injection: Phase 3 tablets.
- Per-share expiry / away mode / guest shares: Phase 4.
- Host-initiated "send to phone": Taildrop covers Tailscale users; revisit with write shares.
- Wake-on-LAN: out of scope until another always-on Gander host exists.
- Cross-host search from the client: trivial fan-out once per-host search exists; Phase 3.

**Open product questions, with the defaults the run will take:**
| Question | Default |
| --- | --- |
| Fetch handoff: built-in viewers or system Open-with? | Both: built-in viewers for images/PDF pages/text/video (they are the Preview viewers), system resolver for everything else |
| Write access in v1? | No. Protocol reserves `write` capability; Phase 3 |
| Stream while locked? | No (Phase 4 after threat review) |
| Multi-user sharing? | The share model already allows another person's paired device; no account concept; "guest shares with expiry" in Phase 4 |
| One stream at a time? | Yes |
| Minimum Windows for host? | Windows 11 for Stream (22000+, popups need 24H2); Windows 10 2004+ best-effort for Browse/Preview/Fetch |

## 7. Decision defaults for the implementation run

See `plans/2026-11-01-master-plan.md`, section "Decision defaults". Summary: Rust host, Kotlin
client, OS-VPN adapters, Apache-2.0, pinned TLS + WebRTC, handler allowlist, user-session tray app,
read-only v1, attended Stream only, Windows 11 minimum for Stream.

## 8. Environment facts for the run (measured 2026-11-01)

- Windows 11 build 26200, a laptop, Intel iGPU + NVIDIA RTX 5060 Laptop, 31 GB RAM, two monitors.
- Present: Rust 1.96 MSVC, VS 2022 Build Tools, JDK 17, Node 24, Python 3.14, git, gh, ffmpeg 9,
  Android SDK (platform 36, build-tools 35/36, NDK 27.1, cmdline-tools), WebView2, Office
  (PowerPoint), Tailscale 1.104.4 logged in, two Android devices on the tailnet.
- Missing: pwsh 7 (only Windows PowerShell 5.1), Android emulator and system images, Android
  Studio, Gradle cache, cmake/ninja/clang, LibreOffice.
- Default handlers: `.pdf` Edge, `.pptx` PowerPoint, `.txt` Notepad (packaged), `.py` none.
- Tailscale adapter category Private; `Tailscale-In` rules present; sleep never on AC; display off
  after 60 min on AC; no inactivity lock policy.
- NDK 27 predates r28's default 16 KB alignment; AGP 8.5.1+ handles packaging alignment, but the
  CI ELF check must run.
