# Roadmap

Order retires the biggest risk first. Stream mode is the differentiator and the hardest part, so
it is spiked before the easy parts are polished. The executable breakdown with verification
commands is `plans/2026-11-01-master-plan.md`; this page is the narrative.

Legend: **[auto]** an autonomous session can complete and verify it on the development machine;
**[device]** needs a physical phone or tablet; **[human]** needs a decision, login, purchase or
someone at the keyboard.

## Phase 0: spikes (throwaway code; findings are the deliverable)

1. **Stream spike (Windows -> Android):** deterministic test window; WGC capture; Media Foundation
   hardware H.264; `webrtc` (then `str0m` if needed) with host-only candidates; minimal Kotlin
   viewer; `SendInput`. Measure the 22-item matrix in `specs/2026-11-01-stream-spike.md`: latency,
   encode cost, bitrate adaptation, occlusion leakage, popups, minimised and static windows, DPI,
   hybrid GPU, display off. [auto] except lock-screen behaviour [human] and latency numbers [device].
2. **Identity spike:** unelevated LocalAPI WhoIs; tagged, shared-in and LAN sources; relay
   classification; CLI fallback. [auto]
3. **Files-app spike:** `DocumentsProvider` + `openProxyFileDescriptor` read sizes on the emulator. [auto]

Exit: findings docs in `specs/`, streaming line of ADR-0001 and D12 of ADR-0005 settled.

## Phase 1: Browse, Preview, Fetch (MVP)

- Host: workspace, config, path resolver with the hostile corpus, narrow listener with pinned TLS
  and trust adapters, pairing + tokens + audit, Browse/Fetch API (list, stat, search, recent, file
  with Range, hash), render worker with thumbnails and previews (images, RAW, PDF, text, Office
  via COM/LibreOffice, video posters), events channel with live refresh and host state, stream
  policy engine (outcomes only), tray UI with kill switch and firewall self-check, MSI installer
  with autostart and firewall rules. [auto], installer install/uninstall test [human].
- Client: Kotlin/Compose app: pairing (QR + manual + SAS), hosts, shares, browser with search and
  Recent, thumbnails, file sheet with honest reasons and default-action rules, preview viewers,
  fetch engine with resume/verify/open-with, cache and offline, settings and app lock. [auto];
  emulator end-to-end [auto if the AVD is installed]; Tailscale identity binding and real-device
  behaviour (metered detection under VPN, decoder) [device].

Exit: daily-driveable for "find and open a file from my PC on my phone"; gates G1-G3 green.

## Phase 1.5: hardening before Stream

- Fuzzing budget on the resolver and the protocol parsers; dependency audit; first external
  read-through of `security.md` against the code. [auto] + review [human]
- Real-world soak: a week of daily use by the owner; issues triaged into Phase 2. [human]

## Phase 2: Stream (Windows host, unlocked session)

- Engine: capture -> encode -> WebRTC with keepalive, keyframe on request, playout-delay hint,
  bitrate control with battery cap. App catalogue with launch templates and window matching;
  fit-to-phone; input with foreground rules and pauses; lifecycle with resume; idle-user prompt;
  host indicators; audio (per-process loopback); clipboard text. [auto on the test window]
- Client: stream viewer with local pinch-zoom, Direct touch and Trackpad modes, keyboard bar,
  local text box, state banner, relay warning. [auto on emulator functionally; feel is device]
- Real apps checklist: PowerPoint (re-used process), Edge PDF (isolated window), Blender,
  Notepad, Photos (packaged). [human]

Exit: gate G5; the "stream a .pptx from PowerPoint" demo works end to end on a phone.

## Phase 3: Breadth

- Files-app integration on Android (SAF provider, read-only, on-demand). [auto + device]
- Write access: upload via share sheet, rename, delete to host trash, SAF write modes. [auto]
- iOS / iPadOS client: SwiftUI; Files + Preview + Fetch first; Stream behind an App Review risk
  flag (guideline 4.2.7). [human for review]
- macOS host (ScreenCaptureKit) and Linux host (portal + PipeWire, attended only). [auto on
  those machines; none available here]
- Preview/Fetch service for headless or pre-login hosts (new ADR). Windows Search index for
  search. Deep-zoom tiles. Folder zip on explicit request. Watch-only Stream. Pen pressure.

## Phase 4: Later / maybe

- Unattended Stream (virtual display, or RDP RemoteApp/RAIL on Windows Pro) only after a
  dedicated threat review.
- Guest shares with expiry; away mode; desktop-to-desktop; Wake-on-LAN through another host.

## Release hygiene (runs alongside Phases 1-2)

- Public repo once the licence is confirmed [human]. CI on every push: host (fmt, clippy, tests,
  cargo-deny), client (unit, lint, 16 KB ELF check), docs link check. [auto]
- Signed builds (Azure Trusted Signing or OV cert; Play upload key) [human]; SBOM and
  `cargo auditable`; reproducible build notes; SECURITY.md with disclosure address; opt-in update
  check; Play data-safety form; F-Droid metadata (WebRTC from Maven Central only); winget manifest.
- External security review before the first public release [human]; closed beta with a
  diagnostics-export workflow.
