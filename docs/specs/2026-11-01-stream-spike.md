# Spec: Phase 0 stream spike (Windows host -> Android)

- Status: Ready to run (Phase 0, WP-40..WP-44)
- Date: 2026-11-01
- Deliverable: `docs/specs/2026-11-0X-stream-spike-findings.md` with measured numbers, plus a
  recommendation that flips ADR-0001's streaming stack line from Proposed to Accepted.

## Why a spike

Stream mode is the differentiator and the only part whose feasibility is not already proven by an
existing library. Everything below is a question the docs cannot answer; only running code on this
Windows build (10.0.26200, hybrid Intel + NVIDIA laptop, two monitors) can.

## What to build (throwaway, in `spikes/stream/`)

1. `gander-testwin`: a Win32 window (Rust, `windows` crate) that paints a frame counter, a moving
   box and the current time at 60 Hz, sets its title from argv, and on `Ctrl+P` opens a popup menu
   and a tooltip. Deterministic content for latency measurement (the phone photographs both screens;
   difference in displayed counter / frame time = glass-to-glass latency).
2. `spike-capture`: captures one HWND with Windows Graphics Capture to D3D11 textures; prints
   frame rate and whether frames arrive under each condition in the matrix below.
3. `spike-encode`: feeds the textures to a Media Foundation H.264 hardware encoder (and HEVC if
   available) in low-latency CBR mode; measures encode time per frame and bitrate at 1080x2280
   portrait and 1920x1080 landscape, on the Intel iGPU and on the NVIDIA dGPU (hybrid laptop:
   which adapter does WGC produce textures on, and does encoding on the other adapter force a
   cross-adapter copy?).
4. `spike-webrtc`: `webrtc-rs` (first) sending the encoded H.264 as a video track with host
   candidates only, plus a data channel echoing input; fallback candidate `str0m` if webrtc-rs
   shows latency or bandwidth-estimation problems. Signalling over a plain WebSocket for the spike.
5. `spike-android`: a minimal Kotlin app using the native WebRTC library that shows the track
   full screen, reports decode stats, sends taps/drags over the data channel as normalised
   coordinates, and shows a frame counter overlay.
6. `spike-input`: maps the normalised coordinates to the window and injects via `SendInput`;
   also tries `PostMessage` to the HWND for comparison; measures tap-to-visible-effect latency.

## Measurement matrix (record OS build, GPU, driver, Tailscale state, Wi-Fi vs cellular)

| # | Condition | Measure / observe |
| --- | --- | --- |
| C1 | Window visible, foreground, on primary monitor | fps, encode ms/frame, bitrate, glass-to-glass latency on Wi-Fi via tailnet |
| C2 | Window partially covered by another window | does the covering window's content appear in frames (must NOT) |
| C3 | Window on secondary monitor, different DPI scale | correct size and coordinates; input mapping accuracy at 125 % / 150 % |
| C4 | Window minimised | frames stop? black? (expected: stops). Does `ShowWindow(SW_RESTORE)` recover |
| C5 | Popup menu / tooltip / modal dialog open | visible in frames with and without `IncludeSecondaryWindows`; which build exposes the property |
| C6 | Session locked (Win+L) while streaming | frames stop? last frame repeats? input refused? any crash. Resume after unlock |
| C7 | Display turned off by power timeout (and by `SendMessage(HWND_BROADCAST, WM_SYSCOMMAND, SC_MONITORPOWER, 2)`) | do frames continue? does DWM keep compositing |
| C8 | Laptop lid closed, external monitor attached | continues? |
| C9 | Window moved/resized by the local user mid-stream | capture follows? aspect change handled? coordinate map updated |
| C10 | `IsBorderRequired = false` | consent prompt appears? border gone in host display? was the border ever in the frames? |
| C11 | Cursor | is the cursor in the frames with `IsCursorCaptureEnabled` on/off; should the phone draw its own |
| C12 | Hybrid GPU: capture on iGPU, encode on dGPU vs iGPU | cross-adapter copy cost; which is lower latency |
| C13 | Cellular (phone on 5G) via tailnet, direct vs DERP relayed (`tailscale status` shows relay) | latency, achieved bitrate, stalls; does webrtc-rs adapt bitrate |
| C14 | Phone screen off / app backgrounded | host notices (ICE/data channel) within how many seconds |
| C15 | Two windows of one process (PowerPoint re-using its instance) | which HWND appears, title matching works |
| C16 | Edge opening a PDF with and without `--new-window --app=` | tab in existing window vs isolated window |
| C17 | Packaged app (Photos) | HWND under ApplicationFrameHost capturable; input works |
| C18 | Protected content (a DRM video in Edge) | black frames expected; no crash |
| C19 | Input: `SendInput` while another window is foreground | where do the clicks land (must detect and pause) |
| C20 | Input: `PostMessage` to the HWND without focus | works for testwin, Notepad, PowerPoint, Edge? |
| C21 | Keep-awake: `SetThreadExecutionState(ES_SYSTEM_REQUIRED \| ES_DISPLAY_REQUIRED)` | machine stays awake during a session on DC power |
| C22 | Capture from a process started by Task Scheduler at logon vs from a console | WGC works in both (it must; a service would not) |

## Pass / fail thresholds (for the ADR decision, not for shipping)

- Glass-to-glass latency on Wi-Fi through the tailnet under 150 ms median, under 250 ms p95
  (Moonlight-class is 30-60 ms; remote-desktop-class is 100-200 ms; we need "usable for documents").
- Encode cost under 6 ms/frame at 1080x2280 on the iGPU.
- 30 fps sustained at 4-8 Mbps for document content; bitrate adapts within 5 s when the link is
  throttled (use `clumsy` or Windows QoS policy to throttle).
- C2 shows no leakage. C6 is documented truthfully whatever it does.
- Tap-to-effect under 120 ms median.

If `webrtc-rs` fails the latency or adaptation threshold, repeat with `str0m` before concluding
anything about WebRTC itself. If WebRTC as a whole fails, record the measured numbers and note
the fallback design: raw H.264 NAL stream over a TLS TCP/QUIC socket with `MediaCodec` on the
phone (Baton's approach), which forfeits WebRTC's congestion control and needs its own.

## Recording findings

The findings doc has one row per matrix item with: observed behaviour, numbers, screenshots path
under `docs/specs/assets/`, and the API/build facts confirmed. It ends with:

- the streaming stack decision (library versions pinned),
- the minimum Windows build to support and why,
- the list of behaviours Gander must handle in Phase 2 (from the matrix),
- anything that changes `security.md` (for example if C2 leaks).

## Operational notes for an autonomous run

- C6, C7 and C8 change the machine's state; run them last and restore the state afterwards
  (`powercfg` settings are read before and restored after). Locking the session (C6) needs the
  owner's password to unlock, so an unattended run must **skip C6** and mark it "needs human".
- The phone side can be exercised on an emulator for functionality, not for latency; latency
  numbers require a physical device and are marked "deferred: needs device" if none is attached.
- Never leave `gander-testwin` or spike processes running at the end; the spike binary has a
  `--self-test` flag that exits after 20 s.
