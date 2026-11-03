# Findings: stream spike (WP-41 capture, WP-42 encode, WP-43 WebRTC, WP-44 Android + input, WP-45 decisions)

- Status: **Complete for what an unattended, emulator-only run on a locked laptop can measure
  (gate G4).** The streaming stack is decided (next section). Open and not hidden: C6 and C8 need the
  owner, glass-to-glass and cellular need a physical phone, and the real-application items
  (C16, C17, C18, C20 on Notepad/PowerPoint/Edge) need a human-attended run.
- Date: 2026-11-01 (WP-41), 2026-11-02 (WP-42 to WP-45)
- How to read this file: "Summary and decisions" and the "Consolidated matrix" come first and are
  the authoritative answer (WP-45). The four measurement sections after them (WP-41 capture, WP-42
  Encode, WP-43 WebRTC, WP-44 Android + input) are kept exactly as their packages wrote them,
  append-only; where one of them says "pending", this summary supersedes it.
- Related findings (other spikes, separate files): `2026-11-01-identity-spike-findings.md`
  (WP-46, Tailscale LocalAPI identity) and `2026-11-01-files-app-spike-findings.md` (WP-47,
  `DocumentsProvider` and proxy file descriptors).
- Spec: `2026-11-01-stream-spike.md` (matrix items C1..C22, "Recording findings")
- Code: `spikes/stream/capture/` (crate `spike-capture`; library `spike_capture` = the capture API
  for WP-42, binary `spike-capture` = the matrix runner)
- Reproduce: `cargo run -p spike-capture -- --matrix auto` (about 90 s; prints the machine facts
  and the table below; exit 0 when no item FAILs). `--matrix C1,C2`, `--secs N`, `--dump DIR`
  (PPM frames of the test window only, for eyeballing; never commit them), `--list-adapters`.

## Summary and decisions (WP-45)

Machine for every number in this file: Windows 11 Pro 25H2 build 26200.9711, Intel iGPU (8086:7D67)
plus NVIDIA RTX 5060 Laptop, two monitors (2560x1600 at 150 % on the iGPU, 1920x1080 at 100 % on the
dGPU), Tailscale 1.104, rustc 1.96.1. **No physical phone**: every Android number is from an API 36
emulator. The session was locked for the WP-41, WP-42 and WP-43 runs and for the first WP-44 input
matrix; the owner was back at the machine for the later WP-44 runs. Nothing here was measured over
Wi-Fi, cellular or the tailnet address (loopback and the emulator's NAT only).

### What the spike established

One Windows host can capture a single window with Windows Graphics Capture (WGC), hardware-encode it
with a Media Foundation H.264 MFT on either GPU, and send it over WebRTC with host-only ICE
candidates to an Android client built on libwebrtc, which renders it and sends touches back that
are injected into the window. This ran end to end on this machine (taps on a moving box changed its
colour on the host and in the decoded stream, 65 of 66 aimed taps hit). Transport is cheap (7 ms
capture to received on loopback), encode holds 30 fps on both GPUs, and a covering window does not
leak into the frames. What needs work is bounded: congestion adaptation in webrtc-rs, the Intel
encoder's rate control, dead-peer detection, trickle ICE, and, above all, lock-state handling
(next section). **Nothing measured forces the fallback design (raw H.264 over TLS).**

### Security-relevant findings (owner decision needed; `docs/security.md` is human-gated and was NOT edited)

1. **WGC keeps capturing a window's real content while the session is locked** (WP-41: the whole
   run was locked and delivered full-rate frames with exact pixels, including a window the lock
   screen was drawn over).
2. **`PostMessage` injects input while the session is locked** (WP-44: 5 of 5 clicks reached an
   unfocused, covered test window with the lock screen up) and is not stopped by a covering window
   either. `SendInput` was refused in that run only because the pre-click check saw the lock-screen
   window (`LockScreenBackstopFrame`) on top of every point; what `SendInput` itself returns while
   locked was **not measured**. So the covering-window check is not a lock check.
3. **`security.md`'s statement that the OS enforces "Stream only works on an unlocked session" is
   wrong.** Its "Unattended streaming" section says "WGC is not meant to capture the secure desktop
   and injection is silently dropped while locked". On build 26200 application windows stay
   capturable under the lock, posted mouse input is delivered, and `OpenInputDesktop` succeeds while
   locked (so it is not a lock detector either). The OS enforces nothing here; **the host enforces
   the rule, and must fail closed**: `gander_win::session::watch` (WP-17: `WTSRegisterSessionNotification`
   plus a 15 s re-query; `LockState::Unknown` counts as locked, and so does an RDP-disconnected or
   switched-away session) refuses a new Stream session (`412 host_locked`) and ends a running one,
   and capture and both injection paths are gated on it. Suggested replacement for that sentence,
   for the owner to accept or change: "Windows does not enforce this: Windows Graphics Capture keeps
   delivering a window's content, and posted window messages are still delivered, while the session
   is locked (measured on build 26200, stream spike findings). The host therefore enforces it
   itself: it tracks the session lock state, treats an unknown state as locked, refuses new Stream
   sessions and ends running ones when the session locks. v1 and v2: Stream only works on an
   unlocked session; Preview and Fetch work regardless."
4. Borderless capture is granted to an unpackaged desktop app **silently** (no consent prompt) and
   the yellow border is never in the frames, so Gander's own "being viewed" indicator (already a
   non-negotiable) is the only guarantee the local user has; it must not rely on the WGC border.
5. No change needed: a covering window never leaks into a window capture (C2, 0 of 159 frames);
   the stream carries only the shared window's own pixels. Popup menus and tooltips are excluded
   by default and a modal dialog is never included, which is a usability gap (the phone user cannot
   see a dialog that blocks the app), not a leak.

### Decisions (Conductor, under the master plan's default rules; each cites its evidence)

These were taken by the Conductor under D12 ("`webrtc` 0.21 first, `str0m` fallback,
raw-H.264-over-TLS last resort") and the spike's own recording rules, not by the owner; the owner
may override in `docs/context.md`. The ADR status lines are flipped to match.

**1. D12 / ADR-0005, WebRTC library: ACCEPTED, `webrtc` (webrtc-rs) 0.21.x, with conditions.**
Measured on 0.21.1 with `rtc` 0.21.1 (`runtime-tokio`, `crypto-ring`; no CMake, NASM or clang).

Required Phase 2 work (conditions of the acceptance):

- (a) **A delay-based bandwidth estimator** behind webrtc-rs's `BandwidthEstimator` interface
  (`on_reports(now, &[PacketReport])` -> target), because its GCC reacted to loss only: the delay
  half sat at the 8 Mbps ceiling in every run quoted (one earlier probe saw it fire late, 2.5 s
  after the cap). Encoder output was under the cap after **5.5 s with
  a 2 s bottleneck buffer (fails the 5 s rule)** but after 2.5 s with a 200 ms buffer (3.75 s at
  0.4 Mbps).
- (b) **The RTCP/PLI interceptor** from WP-43: `rtc` 0.21 drops every inbound RTCP packet unless an
  interceptor marks it `Attribute::DeliverToApplication`; 0 of 16 PLIs reached the application
  without it, 21 of 21 with it (each forced an IDR).
- (c) **Application-level liveness and pause messages over the data channel**: a phone that
  stopped looking (backgrounded or screen off) was never noticed by the host in 20 s, and a killed
  app only after 29.95 s (WP-44).
- (d) **Trickle ICE, or tolerance of empty-candidate answers**: libwebrtc fired
  `IceGatheringState.COMPLETE` before any candidate existed in 2 of the emulator runs, so the answer
  carried none; the call worked only through peer-reflexive learning on the host (WP-44).

The `str0m` 0.24.1 fallback met every threshold (output under the cap in 1.5 s and 1.0 s, 0 packet
drops with the 2 s buffer, 28.2-28.6 fps decoded, 31-49 ms capture to received) but is **blocked
by `cargo deny`**: RUSTSEC-2026-0387 via `dimpl` 0.7 -> `ecdsa`/`p256` 0.13 -> `crypto-bigint`
0.5.5 (non-constant-time arithmetic on the DTLS certificate key). `dimpl` 0.9.0 already moved to
`ecdsa` 0.17 / `p256` 0.14, so **re-evaluate when str0m ships on `dimpl` >= 0.9**; Gander must not
ship or ignore a known crypto advisory. The raw-H.264-over-TLS fallback is **not needed**: WebRTC
works here with either library. The str0m host code stays uncompiled in
`spikes/stream/webrtc/str0m/peer_str0m.rs`.

Evidence: [WP-43 Bitrate adaptation](#bitrate-adaptation-the-5-s-threshold) (table and the three
excerpts), [WP-43 Recommendation (D12)](#recommendation-d12), [WP-44 Android receiver and C14](#android--input-wp-44),
`spikes/stream/webrtc/results/*-summary.txt`. Delta against the ADR text: none beyond patch level
(ADR-0005 names `webrtc-rs` 0.21.1 and `str0m` 0.24.1; ADR-0001 says `webrtc` 0.21).

**2. Capture: an own thin WGC wrapper over the `windows` crate. ACCEPTED.** `windows-capture`
2.0.1 cannot share a device: it always creates its own on the default adapter with `BGRA_SUPPORT`
only (no `VIDEO_SUPPORT`, no multithread protection), cannot be told another adapter, and lends a
frame only inside its callback from a one-buffer pool, while a Media Foundation hardware MFT needs a
device registered with `IMFDXGIDeviceManager`. The wrapper (about 700 lines, `spike_capture::wgc`
and `d3d`) takes the caller's device on any adapter, is free-threaded, hands frames over zero-copy
and measured equal or better (55.8-56.4 fps against 54.0). Evidence: WP-41 "Device ownership" and
the C1 rows; ADR-0001's "or an own thin WGC wrapper ... decided in the spike" branch is the one
taken, so `windows-capture` is not a production dependency.

**3. Encode: Media Foundation hardware MFT (H.264). ACCEPTED**, with these conditions:

- The D3D device lives on the **encoder's adapter, chosen by LUID** (never by index), and capture
  runs into it: there is no zero-copy path between adapters (`OpenSharedResource1` fails
  E_INVALIDARG both ways) and the CPU copy costs 1.3-3.1 ms plus 1-3 ms of extra convert time.
  dGPU encode latency is 2.4 ms p50 against 8.7 ms on the iGPU.
- The executable exports **`NvOptimusEnablement`**: without it the NVIDIA H.264 MFT does not
  activate on this Optimus laptop (`0x8000FFFF`). Side effect: DXGI then lists the NVIDIA adapter
  first and reports both monitors on it (select adapters by LUID, not order).
- **`timeBeginPeriod(1)`** for the streaming process: the Intel MFT in low-latency mode takes
  15.5 ms per frame and falls to 21 fps with the default 15.6 ms timer, 8.7 ms and 30 fps with it.
- **On Intel, re-create the encoder to change bitrate** (93-162 ms p50, one IDR each): runtime
  `MeanBitRate` returns S_OK and is ignored (output 2430 kbps at a 1181 kbps set point, 6.7 fps,
  1 s latency). NVIDIA honours the runtime change within 1-3 s.
- **H.264 level 5.0 (`profile-level-id=42e032`)** for 1080x2280 (9724 macroblocks is over level
  4.x), offered with `level-asymmetry-allowed=1`; the phone receiver advertises `42e01f` and the
  call negotiated.
- HEVC is available on both adapters at about the same cost (Intel) or 0.5-1.5 ms more (NVIDIA)
  but its phone-side decode was not measured; H.264 is the Phase 2 baseline. The `openh264`
  software fallback was not needed.

Evidence: [WP-42 Encode](#encode-wp-42) ("Two findings that change the host design", the ms/frame
table, the C12 table, "Encoder API").

**4. Input: `SendInput` primary when the target is uncovered and foreground; never inject while
locked. ACCEPTED.**

- `SendInput` takes the screen point, so it needs the pre-click check (`WindowFromPoint` +
  `GA_ROOT` must be the shared window): with window B covering A, a forced `SendInput` landed on B
  (+5 clicks, A +0) and the production policy refused all 5. It costs 4.9 ms p50 per click and
  moves the real cursor.
- `PostMessage` works even when the window is **locked or covered** (5 of 5 hits in both) and costs
  0.02-0.04 ms without moving the cursor, but only for `gander-testwin`, which handles
  `WM_LBUTTONDOWN` itself; Notepad, PowerPoint and Edge were not run. It is therefore not the
  primary path and not a safety net; as a per-application secondary path it needs the missing
  evidence first.
- **The host must gate on `session::watch` (WP-17), fail closed**, for capture and for both
  injection paths: `OpenInputDesktop` is **not** a lock check on build 26200 (it succeeded while
  locked); `WTSSessionInfoEx` `SessionFlags` is.
- Mapping: normalised coordinates through the window frame, 0 px error at 150 % and 100 % (25 points
  each, first and last client pixel included); aim at the picture, not the view (the picture is
  letterboxed).

Evidence: [WP-44 Input injection](#android--input-wp-44) (C19/C20/C3 table and "What the locked
run established exactly").

**Versions pinned by the spike** (Cargo.toml / Gradle at `night-1`): `windows` 0.62.2,
`windows-future` 0.3.2, `webrtc` 0.21.1 + `rtc` 0.21.1 (`runtime-tokio`, `crypto-ring`),
`tokio-tungstenite` 0.29 (spike signalling only), `org.jitsi:webrtc` 155.0.1 on the phone (no
software H.264 decoder: MediaCodec only), OkHttp 5.4.0 (spike signalling). Comparison only, not
dependencies: `windows-capture` 2.0.1, `str0m` 0.24.1 (`rust-crypto`). ffmpeg 9.0.1 verified every
encoder dump. `cargo deny check` is clean on the committed tree (webrtc-rs only).

**Minimum Windows build for Stream: Windows 11 (build 22000 or later) is the supported floor (the
default in force); 26200 is the only build measured.** Reason for 11: borderless capture
(`IsBorderRequired`) and `IncludeSecondaryWindows` (popup menus and tooltips) are Windows 11
features according to the documentation WP-41 recorded; every WGC property the spike uses was
present on 26200. Caveat: that 22000 floor is documented, not measured, and `docs/architecture.md`
says menus and tooltips need 24H2 (build 26100); nothing below 26200 was run. The host must
feature-detect each property (`ApiInformation::IsPropertyPresent`) at run time and degrade (a
"menus may be invisible" notice) instead of refusing. A second machine or VM on a build between
22000 and 26099 would turn the documented floor into a measured one.

### Consolidated matrix (WP-45)

One row per item of the spike spec's measurement matrix (**22 items, C1..C22**). Status legend:
**pass** = measured, behaviour understood and handled by a Phase 2 item (for observe-only items
with no threshold, "answered"); **partial** = part measured, the rest named; **fail** = measured
and failed (none); **skipped** = not run, reason given; **deferred-device** = needs a physical phone;
**deferred-human** = needs the owner at the machine. Row numbers are from the same machine and
sessions as the sections below; details and caveats are in the cited sections.

| # | Condition | Status | Measured | Source | Not run, or open, and why |
| --- | --- | --- | --- | --- | --- |
| C1 | Window visible, foreground, primary monitor: fps, encode ms/frame, bitrate, glass-to-glass | partial | Capture 55.8 fps (own wrapper; 54.0 `windows-capture`). Encode 1080x2280 H.264, 30 fps held: iGPU 8.70 ms p50 / 9.90 p95, 8.92 ms/frame sustained, 5.03 Mbps; dGPU 2.44 / 4.86, 3.23, 0.97 Mbps. Transport capture->received 7.2 ms p50 / 10.4 p95 (loopback, webrtc-rs). Emulator tap -> changed pixels 118-142 ms warm | WP-41 Matrix; WP-42 ms/frame table; WP-43 latency table; WP-44 emulator table | **Glass-to-glass on Wi-Fi: deferred-device** (no phone). The window was topmost but not foreground (session locked) |
| C2 | Window partially or wholly covered by another window | pass | 0 occluder-coloured pixels in 159 frames; patches exact 159/159, counter decoded 159/159. No leakage | WP-41 Matrix | - |
| C3 | Window on the secondary monitor, different DPI; input mapping accuracy | pass | dpi 144 -> 96 -> 144, frame 964x767 -> 641x510 -> 965x767 (DWM bounds each time), pool recreated twice; 45.6 fps on the 60 Hz dGPU output, 56.8 back on the primary. Mapping error 0 px at 150 % and 100 % (25 points each); 65 of 66 aimed taps on the emulator | WP-41 Matrix; WP-44 input table | 125 % not run: no such monitor and display scale is a machine setting. The lower rate on the secondary output was not isolated |
| C4 | Window minimised; does `SW_RESTORE` recover | pass | Frames stop completely (0 in 2 s; no black frames, no size change). After restore the first frame came 43-221 ms later, then 49-57 fps; no session re-creation | WP-41 Matrix | - |
| C5 | Popup menu, tooltip, modal dialog, with and without `IncludeSecondaryWindows` | pass | Unset or `false`: menu 0/85, tooltip 0/85, dialog 0/85. `true`: menu and tooltip in every frame (84/84), **modal `MessageBoxW` never** (0/85). Property present and settable on build 26200 | WP-41 Matrix | Which earlier build exposes the property: documented as 22000, not measured (see "Minimum Windows build") |
| C6 | Session locked (Win+L) while streaming; resume after unlock | deferred-human | Lock and unlock transition not run (needs the owner's password). Observed anyway: the whole WP-41 run was locked and WGC delivered full-rate exact frames; `PostMessage` 5/5 hits while locked; `SendInput` refused because the lock-screen window covers every point (its own return value under lock not measured) | WP-41 Matrix, "Anything that changes security.md"; WP-44 input table | Needs the owner at the machine. Remaining questions: frames on lock (stop, repeat, black), crash, resume after unlock, `SendInput` return under lock |
| C7 | Display turned off by power timeout and by `SC_MONITORPOWER` | skipped | No measurement. Indirect only: WGC captures a window's own surface, not the screen (C2 under the lock screen) | WP-41 Matrix (pending row) | Changes the machine's display state; the owner was at the machine and the plan keeps the laptop awake. Run in a human-attended pass with `powercfg` read before and restored after |
| C8 | Lid closed, external monitor attached | deferred-human | No measurement | WP-41 Matrix (pending row) | Needs a person to close the lid and an attended machine state; same restore rule as C7 |
| C9 | Window moved or resized mid-stream | pass | Move: same size, 54.7 fps, counter 55/55. Resize to 1227x931 outer: first frame at the new size (1209x922) **23 ms** after `SetWindowPos`, pool recreated once, 170/170 new-size frames exact | WP-41 Matrix | - |
| C10 | `IsBorderRequired = false` | partial | `RequestAccessAsync(Borderless)` returned Allowed at once, **no consent prompt** (unpackaged process); the setter succeeded for true and false; the yellow border is never in the frames (0 yellow pixels in the 3 px edge ring, either setting) | WP-41 Matrix | Whether the on-screen border disappears needs a human eye (not read: would sample pixels outside the test window): deferred-human |
| C11 | Cursor in the frames with `IsCursorCaptureEnabled` on/off | pass | On: cursor drawn into the frames (up to 280 non-magenta pixels in Patch A), even with the lock screen on top; off: 0. Decision: capture with the cursor off, the phone draws its own | WP-41 Matrix | - |
| C12 | Hybrid GPU: capture on iGPU, encode on dGPU vs iGPU | pass | No zero-copy sharing (`OpenSharedResource1` E_INVALIDARG both ways). CPU copy 1.3-1.8 ms p50 (691x1487) / 3.1 ms (1902x1071), plus 1-3 ms extra convert. dGPU encode 2.2-2.4 ms vs iGPU 8-9 ms; crossed rows add copy + convert to the destination encoder's cost. WGC delivers on either adapter at the same rate. Decision: one device on the encoder's adapter, never cross | WP-42 C12 table; WP-41 C1 rows | Whether the iGPU speeds up when awake and loaded: open (locked, idle machine) |
| C13 | Cellular (5G) via tailnet, direct vs DERP: latency, bitrate, stalls, does webrtc-rs adapt | deferred-device | **Substitute only**, in-process relay on loopback (token bucket + drop-tail, no jitter or random loss): webrtc-rs output under the cap in 2.50 s (1.5 Mbps, 200 ms buffer), 3.75 s (0.4 Mbps, 200 ms), **5.50 s (1.5 Mbps, 2 s buffer: fails 5 s)**; str0m 1.50 s and 1.00 s. webrtc-rs GCC adapts on loss only | WP-43 Bitrate adaptation | Real cellular, DERP and direct paths, achieved bitrate and stalls need a phone on 5G (the run must not `adb connect` to tailnet devices) |
| C14 | Phone screen off or app backgrounded: does the host notice, and in how many seconds | partial | Emulator: Home key or screen off for 20 s: **the host noticed nothing** (`frames_sent` kept rising by about 150 per 5 s); the phone received no packets for about 15 s after about 5 s, ICE `DISCONNECTED` 10.6 s after `onStop`, recovery within about 0.6 s on return without renegotiation. App force-stopped: host `Failed`/`Closed` after **29.95 s**, encoding 30 fps into the void meanwhile | WP-44 C14 table | Emulator only; OS network restrictions and Doze on a real phone and the effect of a foreground service are not isolated |
| C15 | Two windows of one process (PowerPoint re-using its instance): which HWND, title matching | partial | Test-window analogue: two concurrent sessions on one device, 49.3 fps at 964x767 and 49.9 fps at 767x611, each matching its own DWM bounds, counters 148/148 and 150/150; `EnumWindows` + exact title found each HWND | WP-41 Matrix | PowerPoint not driven (unattended run captures test windows only): skipped by boundary |
| C16 | Edge opening a PDF with and without `--new-window --app=` | skipped | No measurement | WP-41 Matrix | Boundary: the unattended run captures test windows only. Needs a human-attended run or WP-50 |
| C17 | Packaged app (Photos) under `ApplicationFrameHost`: capturable, input works | skipped | No measurement | WP-41 Matrix | Same boundary; needs a human-attended run |
| C18 | Protected content (a DRM video in Edge): black frames expected, no crash | skipped | No measurement (architecture.md states "captures as black"; documented, not measured on this build) | none | No package ran it: needs a human-attended Edge session with a DRM-protected video, outside the test-window boundary |
| C19 | `SendInput` while another window is foreground: where do the clicks land | pass | Unlocked, B covers A: forced `SendInput` -> B +5 clicks, A +0 (**clicks land on the covering window**); the pre-click check refused 5/5. Locked: the lock-screen window covers every point, refused 5/5. `SendInput` 4.9 ms p50 per click, moves the real cursor | WP-44 input table | `SendInput` return value while locked: not measured (open) |
| C20 | `PostMessage` to the HWND without focus: testwin, Notepad, PowerPoint, Edge | partial | `gander-testwin`: 5/5 hits unfocused, locked, covered (5/5 on A, 0 on B) and on the 100 % monitor; 0.02-0.04 ms per call; no cursor movement | WP-44 input table | **Notepad, PowerPoint, Edge not run** (boundary: injection into test windows only). Posted messages skip `WM_NCHITTEST`, `WM_MOUSEACTIVATE`, capture and raw input, so the test-window pass predicts little for real apps |
| C21 | Keep-awake: `SetThreadExecutionState` with `ES_SYSTEM_REQUIRED` and `ES_DISPLAY_REQUIRED` on DC power | skipped | No measurement | none | Needs the laptop on battery with a sleep timeout allowed to elapse, against the owner's keep-awake setting; no package ran it. Human-attended pass |
| C22 | Capture from a Task Scheduler process at logon vs from a console | partial | Console half: every run started from a console (background, not foreground, locked and unlocked) and WGC worked. Expected to hold for a logon task in the same interactive session; a service (session 0) would not | WP-41 method and Matrix | Task Scheduler half not run: needs registering a task and a logon cycle (machine state); belongs with the autostart task (WP-1A) |

Status counts (assert these when editing): 22 rows; pass 8 (C2, C3, C4, C5, C9, C11, C12, C19);
partial 6 (C1, C10, C14, C15, C20, C22); deferred-device 1 (C13); deferred-human 2 (C6, C8);
skipped 5 (C7, C16, C17, C18, C21); fail 0.

### Thresholds (spike spec, "Pass / fail thresholds")

| Threshold | Measured | Verdict | Where it is ill-defined |
| --- | --- | --- | --- |
| Glass-to-glass on Wi-Fi via the tailnet under 150 ms median, 250 ms p95 | Not measured. Pieces: capture->received 7.2 ms p50 / 10.4 p95 (loopback), encode 8.7 ms (iGPU) or 2.4 ms (dGPU), emulator phone-side jitter buffer 42-51 ms and decode 13-28 ms (not phone numbers) | **Deferred (device)** | "Through the tailnet" mixes the network path (direct, DERP, Wi-Fi, 5G) with the pipeline; record the path class with each number |
| Encode cost under 6 ms/frame at 1080x2280 on the iGPU | 8.70 ms p50 latency (p95 9.90) and 8.92 ms/frame sustained in low-latency mode; 5.21 ms/frame sustained but 33.8 ms latency with low-latency off; dGPU 2.44 ms and 3.23 ms/frame | **FAIL as written on the iGPU; PASS on the dGPU** | Latency (a frame in to the same frame out) and throughput (ms/frame sustained) differ by up to 6.5x on Intel depending on low-latency mode, and the spec does not say which. 30 fps was held using 26 % of the frame budget, so this is not a feasibility limit; the machine was idle and locked (iGPU clocks possibly low). Suggested wording: "p95 encode latency under 33 ms with 30 fps held" (measured 9.9 ms) |
| 30 fps sustained at 4-8 Mbps for document content | 28-30 fps decoded, 578 of 579 captured frames decoded (unimpaired link); 5.03 Mbps (Intel, 6 Mbps CBR target), 0.97 Mbps (NVIDIA) | **PASS on the test window; document content not measured** | The test window is mostly flat colour and NVIDIA's CBR undershoots on simple content. Real documents belong with WP-50 |
| Bitrate adapts within 5 s when the link is throttled | webrtc-rs 2.50 s (1.5 Mbps, 200 ms buffer), 3.75 s (0.4 Mbps, 200 ms), **5.50 s (1.5 Mbps, 2 s buffer)**; str0m 1.50 s and 1.00 s. Intel with runtime `MeanBitRate`: never (fixed by re-creating the encoder) | **webrtc-rs PASS at 200 ms, FAIL at 2 s; str0m PASS** | Depends on the bottleneck buffer depth (one library, both verdicts) and on what "adapts" means (estimate halved after 1.0-5.25 s, encoder output under the cap after 2.5-5.5 s, queue drained). Phase 2 should say "queueing delay under 300 ms within N s" instead. Substitute shaper: no jitter or random loss, so it measures reaction to a clean bottleneck only |
| C2 shows no leakage | 0 occluder pixels in 159 frames | **PASS** | - |
| C6 documented truthfully whatever it does | Lock transition not run; what was observed under lock is recorded (capture continues, `PostMessage` delivered, `SendInput` refused by the covering lock window) | **Partial (deferred-human)** | - |
| Tap-to-effect under 120 ms median | Emulator, warm: 118 ms (`post20`, 20 taps) and 142 ms (`final12`, 12 taps); steady-state taps alone 82-142 ms (median 104); cold or just-booted runs 300-800 ms. Host side per tap: 0.5-0.7 ms to injection start | **Undecided (deferred-device)**: the emulator's best case brackets the line | Endpoints are not defined. Measured as phone touch-up to the first decoded frame with the new colour on the phone's own clock (excludes display scan-out and `adb shell input tap`'s 60-150 ms start-up lag). A camera-based touch-down to scan-out number needs a real phone |
| Rule: if webrtc-rs fails latency or adaptation, repeat with str0m before concluding about WebRTC | webrtc-rs failed adaptation at a 2 s buffer; str0m repeated and passed both runs | **Done: WebRTC as a whole did not fail** | str0m is blocked by `cargo deny` (decision 1), so the outcome is webrtc-rs plus estimator work, not a library switch |

### Behaviours Gander must handle in Phase 2 (consolidated from the four sections)

Lifecycle and lock state

1. Track the session lock state with `gander_win::session::watch`, fail closed; refuse and end
   Stream while locked, for capture and both injection paths (security findings 1 to 3).
2. Minimised window: no frames at all. Detect (no frames for N ms plus `IsIconic`), tell the phone,
   offer restore; frames return 43-221 ms after `SW_RESTORE` with no session re-creation.
3. Popup menus and tooltips need `IncludeSecondaryWindows = true`; modal dialogs are not included
   even then: enumerate the target process's owned top-level windows and stream them as their own
   capture (or switch the stream to the dialog).
4. Resize and DPI change recreate the frame pool (first new-size frame about 23 ms); frames carry
   their content size (odd sizes occur: pad or crop to even for the encoder) and the phone's
   coordinate map must follow.
5. The phone must tell the host when it stops watching (`pause`/`resume` and a liveness ping on the
   data channel with a short timeout) and the host must stop encoding then; transport notices a dead
   peer only after about 30 s.
6. Capture does not need the window to be foreground or visible; the host's own "being viewed"
   indicator must not rely on the WGC border.

Capture and encode

7. Capture with the cursor off; the phone draws its own pointer.
8. One D3D device on the encoder's adapter chosen by LUID; export `NvOptimusEnablement` (and
   `AmdPowerXpressRequestHighPerformance` for AMD, not measured); `timeBeginPeriod(1)` while streaming.
9. Drive the encoder rate from the estimate with headroom (set point 0.85 x estimate); re-create the
   Intel MFT to change its rate (IDR, 100-160 ms), NVIDIA changes at runtime; answer PLI/FIR with a
   forced IDR; keep fewer than ring-1 samples in flight.
10. H.264 level 5.0 `42e032` with `level-asymmetry-allowed=1`; if the phone has no MediaCodec
    H.264 decoder the offer must fall back (HEVC, VP8) or the client must refuse Stream mode
    (libwebrtc 155 has no software H.264 decoder).

Transport and signalling

11. Delay-based estimator for webrtc-rs plus the PLI interceptor (decision 1 a, b).
12. NACK off or a short history; after any loss wait for an IDR (the viewer drops damaged access
    units and asks at most every 300 ms).
13. Keep input ahead of video: on a congested link the data-channel echo went to 0.5-1.6 s. Cut video
    faster, send input on its own unordered channel with `maxRetransmits: 0`, pause video while the
    estimate collapses.
14. Trickle ICE or tolerate an empty-candidate answer; bind ICE to explicit addresses (loopback plus
    the tailnet address; the tailnet address itself was not exercised) and bind outgoing TCP to the
    chosen address too.
15. The signalling shapes WP-43 defined (hello / offer / answer / candidate / bye, the `input` data
    channel, the frame-id SEI) ride the authenticated TLS WebSocket, never a separate port.

Input

16. `SendInput` only after the pre-click check (the shared window is the top-level window under the
    mapped point); otherwise pause input and tell the phone ("covered by another window"). A small
    race remains between check and call; the real cursor jumps.
17. Aim at the picture, not the view (letterbox bars are not part of the window).

### Not covered, and who can close it

- **Owner at the machine:** C6 (lock and unlock transition, plus `SendInput` return under lock), C8
  (lid closed), C7 and C21 (display and power state, with `powercfg` restore), the on-screen border
  half of C10, C22's Task Scheduler half.
- **Physical phone on the tailnet:** glass-to-glass (C1), C13 on 5G and DERP, C14 on a real phone,
  decode and jitter-buffer time on hardware, libwebrtc's PLI reaching the host's interceptor, the
  tailnet address as an ICE bind address.
- **Human-attended run of real applications:** C15 PowerPoint, C16 Edge, C17 Photos, C18 DRM, C20
  Notepad, PowerPoint and Edge, document-content bitrate, a 125 % display scale, a second Windows
  build (between 22000 and 26099) for the minimum-build question.
- **Upstream or later:** str0m on `dimpl` >= 0.9; a minimal webrtc-rs reproduction of the silent
  delay detector before writing our own estimator.

## Machine and method

| Item | Value |
| --- | --- |
| OS | Windows 11 Pro 25H2, build 26200.9711 (the registry `ProductName` still says "Windows 10 Pro", a known quirk; use `CurrentBuild`) |
| GPU 0 | Intel(R) Graphics (8086:7D67), LUID 0x16248, driver 32.0.101.8724, scans out `\\.\DISPLAY1` (internal panel) |
| GPU 1 | NVIDIA GeForce RTX 5060 Laptop GPU (10DE:2D59), 7899 MiB, driver 32.0.15.9579, scans out `\\.\DISPLAY5` (external) |
| Monitor 1 | `\\.\DISPLAY1`, primary, 2560x1600, 144 dpi (150 %), 240 Hz, on the Intel iGPU |
| Monitor 2 | `\\.\DISPLAY5`, 1920x1080 at (2560, 426), 96 dpi (100 %), 60 Hz, on the NVIDIA dGPU |
| Default D3D11 device | `D3D11CreateDevice(NULL adapter)` lands on adapter 0, the Intel iGPU |
| WGC properties present | `IsCursorCaptureEnabled`, `IsBorderRequired`, `IncludeSecondaryWindows`, `MinUpdateInterval`, `DirtyRegionMode` (all, via `ApiInformation::IsPropertyPresent`) |
| Session | **locked** for the whole unattended run (WTS `SessionFlags = WTS_SESSIONSTATE_LOCK`; the window at any probe point was `LockScreenBackstopFrame`) |
| Toolchain | rustc 1.96.1, `windows` 0.62.2, `windows-capture` 2.0.1 (comparison only), `windows-future` 0.3.2 |
| Tailscale, Wi-Fi/cellular | not relevant to capture; recorded by WP-43/WP-44 |

Method: every item launches a fresh `gander-testwin` (WP-40), places it at (60, 60) on the primary
monitor's work area, makes it topmost without activation (focus-stealing prevention keeps a window
started from a background console behind the foreground window; `SetForegroundWindow` was refused,
and with the session locked nothing is foreground anyway), captures it with the own WGC wrapper on
a device created with `BGRA_SUPPORT | VIDEO_SUPPORT` and multithread protection, reads every frame
back through a staging texture, and checks the test window's known content: Patch A (magenta) and
Patch B (green) exact at 0 tolerance, and the 24-bit counter strip decoded with
`gander_testwin::render::decode_counter`. The spike process is per-monitor-v2 DPI aware. Default
session options: `IsCursorCaptureEnabled = false`, 3 pool buffers, no `MinUpdateInterval`.
Measurement window 3 s per item (1-2 s for phases).

Columns: "Arrival" is handler time minus the frame's `SystemRelativeTime` (both QPC, 100 ns units);
"Interval" is the gap between consecutive frames' `SystemRelativeTime`; "missed src" counts 60 Hz
counter values that never appeared in a frame (see open question 2).

## Matrix

One row per item WP-41 ran or skipped; items owned by later packages are listed as pending so
WP-45 can fill them. Numbers are from the final `--matrix auto` run (2026-11-01, session locked).

| # | Condition | Result | Observed behaviour and numbers |
| --- | --- | --- | --- |
| C1 | Visible, primary monitor (Intel-scanned, 240 Hz, 150 %), own wrapper, default device | PASS | 55.8 fps (168 frames / 3 s), interval p50 16.7 ms / p95 29.2 / max 45.8; arrival -4.3 ms p50 / -2.8 p95; texture 964x767 = DWM extended frame bounds; patches exact 168/168, counter decoded 168/168; 14 missed src, 3 repeats; 0 frames dropped for a full channel. Window was topmost but not foreground (refused while locked) |
| C1 | same, device on adapter 0 (Intel) | PASS | 56.4 fps, interval 16.7/29.2/45.8, arrival -4.5/-2.7, all frames exact; texture's `GetDevice()` adapter = Intel |
| C1 | same, device on adapter 1 (NVIDIA) | PASS | 56.3 fps, interval 16.7/29.2/33.3, arrival -3.0/-1.1, all frames exact; texture's `GetDevice()` adapter = NVIDIA. WGC delivers into a pool on a device of the other adapter than the one scanning out the window's monitor, without any visible cost in rate |
| C1 | `windows-capture` 2.0.1, its own device | INFO | 54.0 fps, interval 16.7/33.3/45.8, arrival -4.7/-2.5. Its device: Intel (default adapter), **no** `VIDEO_SUPPORT`, **not** multithread protected; frames lent only inside `on_frame_arrived` with a 1-buffer pool |
| C2 | Window's whole client area covered by another window (this process's topmost occluder, RGB(0,170,170)) | PASS | Occluder above the test window in z-order (verified by `GetWindow(GW_HWNDPREV)` walk); 0 occluder-colour pixels in 159 frames; Patch A and B exact in 159/159; counter 159/159. **No leakage.** The lock screen covered both windows on screen, so the frames also show WGC does not read the screen at all: it captures the window's own surface |
| C3 | Moved to the secondary monitor (NVIDIA-scanned, 60 Hz, 100 %) mid-capture, then back | PASS | Window dpi 144 -> 96 -> 144; frame size 964x767 -> 641x510 -> 965x767 (DWM bounds match on each); pool recreated twice; on the secondary 45.6 fps, patches exact 137/137, counter 137/137; back on primary 56.8 fps. The lower rate on the 60 Hz dGPU-scanned output (interval p95 33.3 ms, max 45.8) was not isolated: a 60 Hz window on a 60 Hz output leaves no slack, so any late paint or late composition costs a whole frame. Input mapping accuracy at 125/150 %: WP-44 |
| C4 | Minimised, then `ShowWindow(SW_RESTORE)` | INFO | **Frames stop completely** while minimised (0 frames in 2 s; no black frames, no size change). After restore: first frame 43-221 ms later (two runs), then 49-57 fps with the counter decoded (98/99). No re-creation of the session needed |
| C5 | Popup menu + tooltip, then modal `MessageBoxW`, `IncludeSecondaryWindows` unset | INFO | Menu not in frames (0/80), tooltip not in frames (0/80), dialog not in frames (0/85): the default is "exclude" |
| C5 | same, `IncludeSecondaryWindows = false` | INFO | Same as unset: menu 0/85, tooltip 0/85, dialog 0/85 |
| C5 | same, `IncludeSecondaryWindows = true` | INFO | **Menu and tooltip in every frame** (84/84 each, fully drawn at their screen positions, verified on a dumped frame). **The modal message box is never included** (0/85) even when the window is moved underneath it. Property present on 26200 (and per docs since Windows 11 build 22000); setter succeeded |
| C6 | Session locked (Win+L) while streaming | SKIP (needs human) | Lock/unlock transition and unlock need the owner's password. **Observed anyway:** the entire run happened with the session locked and WGC delivered correct, full-rate frames of the test window throughout; input under lock not tested (WP-44) |
| C7 | Display off by power timeout / `SC_MONITORPOWER` | pending (WP-45, machine-state item) | not run by WP-41 (would change display state) |
| C8 | Lid closed, external monitor | pending (needs human) | |
| C9 | Moved, then resized, mid-capture | PASS | Move: frames continue at the same size (54.7 fps, counter 55/55). Resize to 1227x931 outer: first frame at the new size (1209x922 = DWM bounds) **23 ms** after `SetWindowPos`; pool recreated once (the old-size frame is dropped); 170/170 new-size frames with counter and patches exact (layout recomputed from the window's new client size and dpi) |
| C10 | `IsBorderRequired = false` | INFO | `GraphicsCaptureAccess::RequestAccessAsync(Borderless)` returned **Allowed (4) at once, no consent prompt** for this unpackaged process; setter succeeded for true and false. The yellow border is **never in the frames** (0 yellow pixels in the 3 px edge ring, either setting). Whether the on-screen border disappears needs a human eye (not read: would sample desktop pixels outside the test window) |
| C11 | Cursor over the window | INFO | Cursor parked on Patch A's centre with `SetCursorPos` (restored afterwards). `IsCursorCaptureEnabled = true`: cursor drawn into the frames (up to 280 non-magenta pixels in Patch A); `false`: 0. Drawn even though the lock screen was on top (the window under the point was not the test window). Recommendation: capture with the cursor **off** and let the phone draw its own pointer (no stale cursor in a window the user is not pointing at, cheaper, input feedback stays local) |
| C12 | Hybrid GPU: capture/encode adapter | INFO (capture side) | The texture lives on whatever device the caller gives the frame pool, on either adapter, at the same rate (C1 rows). The internal panel is scanned out by the Intel iGPU and the external by the NVIDIA dGPU; the default device is Intel. Encode cost and cross-adapter copy: WP-42 |
| C13 | Cellular via tailnet | pending (WP-43/44) | |
| C14 | Phone screen off / app backgrounded | pending (WP-44) | |
| C15 | Two windows of one process, captured by HWND at the same time | PASS | Test-window analogue (`open-second`); PowerPoint not driven because the unattended run may capture test windows only. `EnumWindows` + exact title finds each HWND exactly; two concurrent sessions on one device: main 49.3 fps 964x767, second 49.9 fps 767x611, each matching its own DWM bounds, counters decoded 148/148 and 150/150. (Interval max 221 ms is the single-threaded readback loop of the harness polling two sessions, not WGC) |
| C16 | Edge opening a PDF, with/without `--new-window --app=` | SKIP | Boundary: unattended run captures test windows only. Needs a human-attended run or WP-50 |
| C17 | Packaged app (Photos) under ApplicationFrameHost | SKIP | Same boundary. Needs a human-attended run |
| C18..C22 | DRM, input, keep-awake, Task Scheduler | pending (WP-44/45) | |

## Device ownership: can the encoder share the capture device? (the WP-41 key question)

Answer: **not cleanly with `windows-capture` 2.0.1; yes with the own wrapper, which is the path
forward.**

- `windows-capture` 2.0.1 (MIT, `windows` 0.62, so no duplicate bindings): the supported entry
  points `GraphicsCaptureApiHandler::start` / `start_free_threaded` always call its
  `d3d11::create_d3d_device()`, which uses the NULL adapter (here: Intel), flags
  `BGRA_SUPPORT` only, no `VIDEO_SUPPORT`, and no multithread protection (measured: video=false,
  mt=false). You can **obtain** the device (`Context::device`, `Frame::device()`), but not choose
  the adapter or flags; a Media Foundation hardware MFT needs a device registered with
  `IMFDXGIDeviceManager`, which requires multithread protection (settable after the fact) and in
  practice `VIDEO_SUPPORT` (not settable after creation). Frames are lent by `&mut Frame` only for
  the duration of the callback, from a pool of **one** buffer, so an asynchronous encoder must copy
  every frame. The lower-level `GraphicsCaptureApi::new(d3d_device, ...)` is public and does take
  a device, but it needs the library's dispatcher-queue thread model and posts `WM_QUIT` to a
  thread id; not a stable surface to build on.
- Own wrapper (`spike_capture::wgc` and `d3d`, about 700 lines over the `windows` crate): the caller creates
  the device (any adapter, `BGRA | VIDEO_SUPPORT`, multithread protected,
  `spike_capture::d3d::create_device`), the frame pool is free-threaded (no dispatcher queue or
  message loop), frames are handed over zero-copy and keep their pool buffer until dropped, the
  pool is recreated on resize, and all session options are reported as applied or refused.
  Measured equal to or slightly better than `windows-capture` on the same window (C1 rows).

### Capture API for WP-42

```rust
use spike_capture::{d3d, wgc};

let adapters = d3d::adapters()?;                       // Vec<Adapter { adapter: IDXGIAdapter1, info }>
let device: ID3D11Device = d3d::create_device(Some(&adapters[i].adapter))?; // or None = default
// register `device` with IMFDXGIDeviceManager::ResetDevice for the encoder
let cap = wgc::Capture::start(hwnd, &device, &wgc::CaptureOptions::default())?;
// CaptureOptions { cursor: Some(false), border_required: None,
//                  include_secondary_windows: None, min_update_interval: None, buffers: 3 }
while let Some(frame) = cap.recv_timeout(Duration::from_millis(100)) {
    let tex: &ID3D11Texture2D = frame.texture();   // BGRA8, on `device`
    let (w, h) = frame.size();                      // content size; texture may be larger once after a resize
    let t = frame.system_relative_time_100ns();     // QPC 100 ns: use as the MF sample time
    // encode; drop(frame) returns the buffer to the pool
}
cap.stop();
```

Also: `wgc::WindowGeometry::of(hwnd)` (DWM frame bounds, client origin and size, dpi;
`client_offset()` is where the client area starts inside a frame: (2, 45) at 150 %, confirmed
by locating Patch A in the pixels), `wgc::request_borderless`, `wgc::session_property_present`,
`readback::Readback` (CPU copy for verification), `d3d::device_luid`.

Notes for the encoder: the texture is `B8G8R8A8_UNORM` (MF H.264 MFTs want NV12, so a
`ID3D11VideoProcessor` BGRA->NV12 step on the same device is expected); sizes are odd numbers
(964x767), so the encoder input needs padding or cropping to even dimensions; `MinUpdateInterval`
is available to cap the rate.

## Behaviours Gander must handle in Phase 2 (from these rows)

1. **Minimised window: no frames at all.** The host must detect it (no frames for > N ms plus
   `IsIconic`), show "window minimised" on the phone, and offer restore; restoring brings frames
   back in 50-220 ms without restarting the session.
2. **Popup menus and tooltips need `IncludeSecondaryWindows = true`** (default excludes them, so a
   right-click menu would be invisible to the phone user). **Modal dialogs (message boxes, Save
   As) are not included even then**: the host must enumerate owned top-level windows of the
   target process and stream them as their own capture (or switch the stream to the dialog).
3. **Resize and DPI changes** recreate the frame pool; the first frame at the new size arrives in
   about 23 ms; frames carry their content size, which the encoder and the phone's coordinate map
   must follow (odd sizes occur).
4. **Moving to the other monitor/adapter** keeps the session working; the rate dropped to about
   46 fps on the 60 Hz external output (cause not isolated, see C3).
5. Capture **does not need the window to be foreground or visible on screen** (C1, C2 under a lock
   screen), which is what makes covered windows safe and also what makes the next point necessary.
6. Draw the cursor on the phone; capture with `IsCursorCaptureEnabled = false`.
7. Borderless capture is granted silently to an unpackaged desktop app on this build; the border
   is not in the frames either way. Gander's own "being viewed" indicator (security.md) must not
   rely on the WGC border.

## Anything that changes `security.md`

- **WGC keeps capturing a window's real content while the session is locked** (measured here for
  the whole run: full rate, exact pixels). `security.md` says "WGC is not meant to capture the
  secure desktop" and "v1 and v2: Stream only works on an unlocked session". The first statement is
  about the secure desktop itself, not about application windows behind it: those remain
  capturable. So the unlocked-only rule is **not** enforced by the OS; the host must enforce it
  (watch `WTS_SESSION_LOCK` via `WTSRegisterSessionNotification`, refuse new Stream sessions and
  stop running ones while locked). WP-45/owner to update `security.md` (WP-41 does not edit it).
- C2 shows no leakage of covering windows into a window capture (a Stream session only ever shows
  the shared window's own pixels).
- Borderless capture needs no consent on this build, so a capture can be invisible to the local
  user unless Gander shows its own indicator (already a non-negotiable).

## Pending (filled by later packages)

- Streaming stack decision with pinned library versions (WP-45, after WP-42/43).
- Minimum Windows build: WGC property presence measured here on 26200; the floor (likely 22000 for
  `IncludeSecondaryWindows` and borderless, 19041 for `IsCursorCaptureEnabled`) is decided by
  WP-45.
- Screenshots under `docs/specs/assets/`: none committed by WP-41 (the run's frames are of the test
  window and can be regenerated with `--dump`).

## Open questions

1. `SystemRelativeTime` is 1-5 ms **ahead** of the moment the `FrameArrived` handler runs (negative
   "arrival" column), more on the 240 Hz panel than on the dGPU path. It looks like the target
   present (vblank) time rather than a capture-complete time. Fine as a media timestamp; do not use
   it to measure capture latency. Glass-to-glass latency stays with WP-44.
2. Missed source frames (11-14 of about 180 counter values per 3 s, 3-8 %) with zero frames
   dropped by the consumer: either the test window's own thread skipped them (it skips rather than
   repeats when late) or DWM composed two window updates into one present. Distinguishing needs the
   window's `frame?` history; irrelevant at the 30 fps target but worth knowing for WP-42 pacing.
3. C3/C5/C11 ran with the session locked; a human-attended rerun with an unlocked desktop should
   confirm C5's dialog result and C10's on-screen border (expected to be identical, since WGC does
   not read the screen).

## Encode (WP-42)

- Date: 2026-11-02, same machine and drivers as above, session **locked** for the whole run.
- Code: `spikes/stream/encode/` (crate `spike-encode`; library `spike_encode` = the encoder API for
  WP-43, binary `spike-encode` = the measurement runner). Depends on `spike-capture` (WP-41).
- Reproduce: `cargo run --release -p spike-encode` (about 2 minutes; prints adapters, encoder
  MFTs, the C12 shared-handle probe and the table below; exit 0 when every run decodes with
  ffmpeg). `--list` (enumeration only), `--wp43` (the WP-43 dump), `--secs N`, `--fps N`,
  `--mbps N`, `--gop N`, `--burst N`, `--sizes WxH,..`, `--adapters I,..`, `--codecs h264,hevc`,
  `--no-cross`, `--no-timer`, `--no-low-latency`, `--out DIR`.

### Pipeline as measured

WGC frame (BGRA8, on device D) -> `ID3D11VideoProcessor` on D: BGRA -> NV12, scaled to the encode
size, full-range RGB in, BT.709 limited-range out (`ID3D11VideoContext1` colour spaces), into a
ring of 8 NV12 textures -> `MFCreateDXGISurfaceBuffer` sample -> asynchronous hardware MFT bound to
D through `IMFDXGIDeviceManager` -> Annex-B output. Settings: `MF_LOW_LATENCY` and
`CODECAPI_AVLowLatencyMode` on, `CODECAPI_AVEncCommonRateControlMode` CBR, `MeanBitRate` 6 Mbps,
`AVEncMPVGOPSize` 60 (2 s at 30 fps), `AVEncMPVDefaultBPictureCount` 0, output type set before
input type, H.264 profile tried in the order constrained baseline, baseline, main, high (the first
was accepted by both vendors), HEVC Main. A forced IDR (`CODECAPI_AVEncVideoForceKeyFrame`) is
requested at input frame 45 of every run and checked by parsing the NAL units of the matching
output sample. Pattern only from `moq-video`/Sunshine as named by the plan; no code or structure
was copied, so their licences did not come into play.

Paced phase: 6 s, frames from the 60 Hz test window, every second one taken (30 fps), sample
time = `SystemRelativeTime` minus the first frame's. Then a burst phase re-feeds the last 8
converted frames 90 times as fast as the MFT asks (`METransformNeedInput`), which measures
throughput independent of the source rate; burst output is not written to the dumps.

Columns: "Encode latency" = `ProcessInput` call to the `METransformHaveOutput` event of the same
sample (QPC, event-thread timestamps on receipt); this is the per-frame cost a low-latency stream
sees. "Burst ms/frame" = (last burst output - first burst input) / 90, i.e. sustained throughput.
"Convert" = video processor blit submitted to the GPU event query signalling it finished (the CPU
waits; measured, not estimated). "Cross copy" = staging copy + `Map` on the capture device +
`UpdateSubresource` on the encode device. "Mbps" = bytes of the paced phase * 8 / its stream
duration. "Source" = the WGC frame size before scaling.

**How the two sizes were produced (asked by the plan):** the test window's pipe has **no resize
command**, so the spike resizes the window itself with `SetWindowPos` to the target aspect and the
video processor scales to the exact encode size. 1920x1080 fits the primary monitor's work area
(2560x1528), giving a 1902x1071 frame (DWM bounds), scaled about 1 % up. 1080x2280 does not fit
(work area height 1528), so the window is 709x1496 outer (691x1487 frame) and the video processor
scales it up 1.56x. A real 2280-pixel-tall window is impossible on this machine anyway.

### Hardware encoder MFTs per adapter

`MFTEnum2(MFT_CATEGORY_VIDEO_ENCODER, HARDWARE | SORTANDFILTER, NV12 -> codec)`; the per-adapter
filter is `MFT_ENUM_ADAPTER_LUID` set as an 8-byte LUID blob, and it works (each adapter gets only
its own vendor's MFT). The activation objects themselves carry **no** `MFT_ENUM_ADAPTER_LUID`.

| Adapter | Codec | MFT friendly name | Vendor | CLSID | `MFT_ENUM_HARDWARE_URL_Attribute` |
| --- | --- | --- | --- | --- | --- |
| Intel iGPU (LUID 0x16248) | H.264 | Intel® Quick Sync Video H.264 Encoder MFT (listed **twice**, same CLSID) | VEN_8086 | 4BE8D3C0-0515-4A37-AD55-E4BAE19AF471 | AA243E5D-2F73-48c7-97F7-F6FA17651651 |
| Intel iGPU | HEVC | Intel® Hardware H265 Encoder MFT (listed twice, same CLSID) | VEN_8086 | BC10864D-2B34-408F-912A-102B1B867B6C | AA243E5D-2F73-48c7-97F7-F6FA17651651 |
| NVIDIA dGPU (LUID 0x18773) | H.264 | NVIDIA H.264 Encoder MFT | VEN_10DE | 60F44560-5A20-4857-BFEF-D29773CB8040 | NVIDIA H.264 Encoder MFT |
| NVIDIA dGPU | HEVC | NVIDIA HEVC Encoder MFT | VEN_10DE | 362A74CD-E177-4916-A569-C3D4D99CE786 | NVIDIA HEVC Encoder MFT |

### Two findings that change the host design

1. **The NVIDIA encoder MFT does not activate in a default process on this Optimus laptop.**
   `IMFActivate::ActivateObject` returns `0x8000FFFF` (E_UNEXPECTED) before any device is
   involved; a second activation of the same object then returns `0xC00D36B2`
   (MF_E_INVALIDREQUEST). Exporting `NvOptimusEnablement = 1` from the executable (the documented
   NVIDIA Optimus opt-in; `spikes/stream/encode/build.rs` adds `/EXPORT:NvOptimusEnablement`)
   makes it activate and work. Side effect, measured: DXGI then enumerates the **NVIDIA adapter
   first** and reports **both** monitors (`DISPLAY1`, `DISPLAY5`) as its outputs, while the Intel
   adapter reports none; a NULL-adapter D3D11 device lands on NVIDIA. WGC capture and the Intel
   encoder keep working (all rows below were measured with the export). The per-app Windows
   graphics preference (Settings > Display > Graphics) would probably do the same but was not
   tried (it is a machine setting). Phase 2: the host must either export the symbol (plus
   `AmdPowerXpressRequestHighPerformance` for AMD) or not offer the NVIDIA encoder, and must
   select adapters by LUID/vendor, never by index.
2. **The Intel MFT in low-latency mode is gated by the timer resolution.** With the default
   15.6 ms timer, the Intel H.264 encoder at 1080x2280 took 15.5 ms p50 per frame (one timer
   tick) and sustained only 31 ms/frame, so the 30 fps stream fell to 21 fps. With
   `timeBeginPeriod(1)` (per process since Windows 10 2004, so no machine-state change; the spike
   does it by default, `--no-timer` to compare) the same encoder takes 8.7 ms. NVIDIA is not
   affected (2.2 ms either way). The host must raise the timer resolution while streaming.

| Variant (H.264, 1080x2280, 3 s paced + 60 burst) | Encoder | fps | Latency p50 / p95 / max ms | Burst ms/frame |
| --- | --- | --- | --- | --- |
| default timer (`--no-timer`) | Intel | 21.1 | 15.51 / 21.27 / 50.26 | 31.04 |
| default timer (`--no-timer`) | NVIDIA | 30.3 | 2.19 / 2.58 / 3.15 | 3.34 |
| 1 ms timer, low latency **off** (`--no-low-latency`) | Intel | 30.5 | 33.80 / 35.61 / 50.75 | 5.21 |
| 1 ms timer, low latency off | NVIDIA | 30.5 | 2.04 / 2.74 / 6.67 | 2.44 |

Without low-latency mode the Intel MFT pipelines several frames: throughput rises to 5.2 ms/frame
but each frame waits about 34 ms. Low-latency mode keeps one frame in flight.

### ms/frame and bitrate (final run, `cargo run --release -p spike-encode`)

30 fps target, 6 Mbps CBR, GOP 60, 6 s paced + 90 burst frames, 1 ms timer, low-latency mode.
"C1" rows: capture device = encode device. "C12" rows: capture on one adapter, CPU copy, encode on
the other. Every dump decoded with `ffmpeg -i <dump> -f null -` (exit 0, no messages); profile and
level from `ffprobe`. 0 frames were skipped because the encoder was busy; the first output sample
of every run carried SPS + PPS (+ VPS for HEVC) + IDR with Annex-B start codes.

| Item | Capture -> encode | Codec | Size | Source | fps | Encode latency p50 / p95 / max ms | Burst ms/frame | Convert p50 ms | Cross copy p50 / p95 ms | Mbps | IDRs (forced ok) | Profile / level (ffprobe) |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| C1 | dGPU -> dGPU | H.264 | 1080x2280 | 691x1487 | 30.2 | 2.44 / 4.86 / 6.01 | 3.23 | 0.29 | - | 0.97 | 5 (yes) | Constrained Baseline / 5.0 |
| C1 | dGPU -> dGPU | HEVC | 1080x2280 | 691x1487 | 30.0 | 3.47 / 5.53 / 8.83 | 5.81 | 0.28 | - | 0.82 | 5 (yes) | Main / 5.0 |
| C1 | iGPU -> iGPU | H.264 | 1080x2280 | 691x1487 | 30.1 | 8.70 / 9.90 / 14.83 | 8.92 | 0.54 | - | 5.03 | 5 (yes) | Constrained Baseline / 5.0 |
| C1 | iGPU -> iGPU | HEVC | 1080x2280 | 691x1487 | 28.9 | 9.13 / 10.04 / 15.76 | 9.03 | 0.54 | - | 4.78 | 5 (yes) | Main / 5.0 |
| C12 | dGPU -> iGPU | H.264 | 1080x2280 | 691x1487 | 29.7 | 9.25 / 10.34 / 13.66 | 8.58 | 1.15 | 1.79 / 2.93 | 4.93 | 5 (yes) | Constrained Baseline / 5.0 |
| C12 | iGPU -> dGPU | H.264 | 1080x2280 | 691x1487 | 29.8 | 2.75 / 5.41 / 10.35 | 4.98 | 1.48 | 1.33 / 2.04 | 0.98 | 5 (yes) | Constrained Baseline / 5.0 |
| C1 | dGPU -> dGPU | H.264 | 1920x1080 | 1902x1071 | 29.5 | 2.18 / 8.96 / 19.10 | 3.93 | 0.38 | - | 0.74 | 5 (yes) | Constrained Baseline / 4.0 |
| C1 | dGPU -> dGPU | HEVC | 1920x1080 | 1902x1071 | 30.0 | 3.14 / 4.29 / 8.66 | 5.54 | 0.36 | - | 0.64 | 5 (yes) | Main / 4.0 |
| C1 | iGPU -> iGPU | H.264 | 1920x1080 | 1902x1071 | 29.8 | 7.84 / 9.60 / 12.01 | 8.09 | 0.61 | - | 4.96 | 5 (yes) | Constrained Baseline / 4.0 |
| C1 | iGPU -> iGPU | HEVC | 1920x1080 | 1902x1071 | 28.6 | 7.87 / 9.07 / 12.52 | 9.61 | 0.60 | - | 4.73 | 5 (yes) | Main / 4.0 |
| C12 | dGPU -> iGPU | H.264 | 1920x1080 | 1902x1071 | 30.2 | 8.25 / 9.57 / 17.72 | 7.89 | 1.46 | 3.06 / 3.36 | 5.04 | 5 (yes) | Constrained Baseline / 4.0 |
| C12 | iGPU -> dGPU | H.264 | 1920x1080 | 1902x1071 | 29.5 | 2.10 / 9.35 / 17.86 | 4.56 | 3.77 | 3.07 / 4.65 | 0.74 | 5 (yes) | Constrained Baseline / 4.0 |

Encoder names: dGPU = NVIDIA H.264 / HEVC Encoder MFT, iGPU = Intel® Quick Sync Video H.264 /
Intel® Hardware H265 Encoder MFT. Settings accepted: all of them on Intel; on NVIDIA all except
`AVEncMPVDefaultBPictureCount` (`0x80070057`), harmless here (constrained baseline has no
B-frames, and every HEVC output sample came back with its input's sample time, in order, so no
reordering was seen). Five IDRs in 270 frames (frame 0, the forced one at 45, then every 60)
means the GOP counter restarts at a forced IDR on both vendors.

Reading the numbers:

- **Threshold "encode cost under 6 ms/frame at 1080x2280 on the iGPU": not met in low-latency
  mode** (8.7 ms p50 per frame, 8.9 ms/frame sustained). Met by the dGPU (2.4 ms) and, as
  throughput only, by the iGPU without low-latency mode (5.2 ms/frame, but 34 ms per frame).
  Caveats: the session was locked and the machine otherwise idle, so the iGPU may have been at low
  clocks; the latency includes the MFT's own wait (it sleeps at timer granularity, finding 2).
  At 30 fps, 8.7 ms is 26 % of the frame budget and the stream held 30 fps, so the iGPU is usable;
  the 6 ms line is not a feasibility limit. HEVC costs about the same as H.264 on Intel and
  0.5-1.5 ms more on NVIDIA.
- **Bitrate:** CBR at 6 Mbps gave 4.7-5.0 Mbps on Intel and 0.6-1.0 Mbps on NVIDIA for the same
  content. Neither pads with filler: NVIDIA's "CBR" does not spend bits a simple picture does not
  need. Document-content bitrates and rate adaptation are for WP-43 (the test window is mostly
  flat colour).
- **Convert (BGRA -> NV12 + scale) costs 0.3-0.8 ms** on the GPU of the same device; not a factor.

### C12: hybrid GPU, capture on one adapter, encode on the other

| Question | Answer (measured) |
| --- | --- |
| Zero-copy sharing between the adapters? | **No.** A BGRA texture created with `SHARED_NTHANDLE \| SHARED_KEYEDMUTEX` gives an NT handle, but `ID3D11Device1::OpenSharedResource1` on the other adapter fails `0x80070057` (E_INVALIDARG), both directions |
| CPU copy cost (staging + Map + UpdateSubresource) | 691x1487 source: 1.3-1.8 ms p50, 2.0-2.9 p95. 1902x1071 source: 3.1 ms p50, 3.4-4.7 p95 |
| Extra cost on the encode side | the video processor waits for the upload: convert rises from 0.3-0.6 ms to 1.2-3.8 ms |
| Which is lower latency | capturing on the dGPU device and encoding there: 2.2-2.4 ms p50 per frame vs 8-9 ms on the iGPU. The cross rows add copy + convert on top of the destination encoder's own cost |
| Recommendation | **Never cross adapters.** WGC delivers into a pool on either adapter's device at full rate (WP-41 C1 rows), so the host creates ONE device on the adapter whose encoder it uses and captures into it; no copy exists in that design. Prefer the NVIDIA encoder when it activates (lower latency, needs the Optimus export), else Intel with the 1 ms timer |

### Annex-B dump for WP-43

- Stable path (committed, 2.0 MB): `spikes/stream/encode/results/wp43-h264-1080x2280.h264`
- Regenerate: `cargo run --release -p spike-encode -- --wp43` (Intel iGPU, H.264 constrained
  baseline level 5.0, 1080x2280, 30 fps, 4 Mbps CBR, GOP 60, 5 s = about 152 frames, a forced IDR
  at frame 45). It writes `target/spike-encode/wp43.h264` and refreshes the committed copy when it
  is under 5 MB (it fails if ffmpeg cannot decode it). Content: the test window (frame counter,
  clock, moving box, magenta/green patches; the caption shows the test window's pid, nothing
  else).
- The full run's dumps (one per row, 0.5-3.8 MB each) are under `target/spike-encode/`
  (`<igpu|dgpu>-<codec>-<WxH>.<codec>`, `cross-<from>-to-<to>-h264-<WxH>.h264`); not committed.
- Each output sample is one access unit; parameter sets come in band before every IDR.
- **For WP-43's SDP:** 1080x2280 needs H.264 **level 5.0** (the encoder chose it; 9724
  macroblocks per frame is over level 4.x's 8192). A `profile-level-id` of `42e01f` (constrained
  baseline 3.1, the usual WebRTC default) understates the stream; offer `42e032` or rely on the
  receiver's level-asymmetry handling.

### Encoder API (library `spike_encode`)

```rust
use spike_encode::{convert::Converter, encoder::{Config, Encoder, Event}, mft::{self, Codec}};
mft::startup()?;                                            // COM MTA + MFStartup
let encoders = mft::hardware_encoders(Codec::H264, Some(adapter_luid))?; // EncoderInfo { name, vendor, url, clsid, luid, activate }
let mut conv = Converter::new(&device, 1080, 2280, 30, 8)?;  // BGRA -> NV12 ring on the same device
let cfg = Config::new(Codec::H264, 1080, 2280, 30, 4_000_000); // gop = 2 s, low_latency = true
let mut enc = Encoder::new(&encoders[0].activate, &device, &cfg)?;
// loop on enc.next_event(timeout): NeedInput(t) | HaveOutput(t) | DrainComplete(t) | Error(_) | Other(_)
//   NeedInput:  let c = conv.convert(frame.texture(), w, h)?; enc.feed(&c.texture, time_100ns, period_100ns)?;
//   HaveOutput: if let Some(p) = enc.take_output()? { /* p.data: Annex-B AU, p.time, p.keyframe */ }
//   enc.force_keyframe()?  -> the next picture is an IDR (answer to a PLI/FIR)
// end: enc.drain()?, wait for DrainComplete, drop(enc)
```

`spike_encode::annexb` splits Annex-B into NAL units and classifies them (IDR/SPS/PPS/VPS);
`spike_encode::xadapter` holds the C12 probe and copier. A process using it should call
`timeBeginPeriod(1)` (finding 2) and export `NvOptimusEnablement` (finding 1); see
`spikes/stream/encode/src/main.rs` and `build.rs`. Keep fewer than ring - 1 samples in flight
(low-latency mode holds one).

### Fallback

Not needed: hardware MFTs work on both adapters for H.264 and HEVC. The `openh264` software
encoder was not added.

### Matrix rows added by WP-42

| # | Condition | Result | Observed behaviour and numbers |
| --- | --- | --- | --- |
| C1 | Encode ms/frame and bitrate, window on the primary monitor | INFO (threshold met on the dGPU, not on the iGPU in low-latency mode) | 1080x2280 H.264: dGPU 2.4 ms p50 latency, 3.2 ms/frame sustained, 1.0 Mbps; iGPU 8.7 ms, 8.9 ms/frame, 5.0 Mbps (6 Mbps CBR target, 30 fps held). Full table above. Glass-to-glass: WP-44 |
| C12 | Hybrid GPU: capture on iGPU, encode on dGPU vs iGPU | PASS (decision made) | No cross-adapter sharing (E_INVALIDARG); CPU copy 1.3-3.1 ms p50 plus 1-3 ms extra convert; dGPU encode 3-4x lower latency than iGPU. Recommendation: one device on the encoder's adapter, capture into it |

### Open questions (WP-42)

1. Is the Intel 8.7 ms a clock-state artefact of the locked, idle machine? A rerun with the desktop
   in use (or under GPU load) would show whether the iGPU meets 6 ms when awake.
2. NVIDIA activation without the Optimus export: does the per-app graphics preference also fix
   it, and what does the export cost in battery when the host is idle? The host only needs the
   dGPU while streaming (the export applies to the whole process lifetime).
3. Intel lists each hardware MFT twice with the same CLSID; harmless (the spike takes the first)
   but unexplained.
4. CBR on NVIDIA undershoots on simple content; rate behaviour on real documents and under
   WebRTC's bandwidth estimate is WP-43's to measure.

## WebRTC (WP-43)

- Date: 2026-11-02, same machine as above, session **locked** for every run (capture works, as
  WP-41/42 found). Windows 11 Pro 26200, Intel iGPU + NVIDIA RTX 5060 Laptop, ffmpeg 9.0.1.
- Code: `spikes/stream/webrtc/` (crate `spike-webrtc`, binary only). Depends on `spike-capture`
  (WGC) and `spike-encode` (MF hardware H.264; WP-43 added `Encoder::set_bitrate` and
  `Encoder::set_bitrate_event`).
- Reproduce: `cargo run --release -p spike-webrtc` (50 s: 20 s free, 1.5 Mbps cap at 20-35 s,
  free again; about 75 s with setup and shutdown; exit 0 = frames decoded and every cap met within
  5 s). Options: `--steps T:KBPS,..` (0 = no cap), `--queue-ms N` (relay
  buffer), `--no-relay`, `--vendor intel|nvidia`, `--nack`, `--secs N`, `--size WxH`,
  `--gcc-initial-kbps`/`--gcc-min-kbps`/`--gcc-max-kbps`, `--headroom F`, `--results DIR`,
  `--label S`. Diagnostics: `SPIKE_LOG=debug` (library log records), `SPIKE_RATE_API=codecapi|
  event|both|recreate`, `SPIKE_NO_CC=1`.
- The str0m host (`--stack str0m`) is kept in `spikes/stream/webrtc/str0m/peer_str0m.rs` and is
  **not compiled by default**: str0m 0.24.1 depends on `dimpl` 0.7 -> `ecdsa`/`p256` 0.13 ->
  `crypto-bigint` 0.5.5, which carries RUSTSEC-2026-0387 (non-constant-time arithmetic; fixed
  in 0.7.6), so `cargo deny check` fails while str0m is in the graph. The `str0m-*` results were
  measured with it compiled in; the file lists the three steps to do that again locally.
- Results (committed): `spikes/stream/webrtc/results/<label>-{summary.txt,timeline.csv,offer.sdp,
  answer.sdp}` for every run quoted below (ICE passwords redacted).

### What runs (one process, everything on 127.0.0.1)

```text
gander-testwin --WGC--> D3D11 (Intel) --VP--> NV12 --MF H.264 MFT--> Annex-B AU + frame-id SEI
  --> host peer (webrtc-rs 0.21.1, or str0m 0.24.1) --UDP--> relay B [token bucket + drop-tail]
  relay A --UDP--> viewer peer (webrtc-rs 0.21.1) --depacketise--> clean AU --> ffmpeg (framecrc)
viewer --"input" data channel {"t":"tap",...}--> host --echo--> viewer (RTT)
viewer --TWCC feedback, PLI--> relay (unimpaired) --> host: estimate -> encoder rate, PLI -> IDR
signalling: plain WebSocket, host listens 127.0.0.1:<ephemeral>, viewer connects from 127.0.0.1
```

- **Crates and licences:** `webrtc` 0.21.1 is the current release (crates.io, checked
  2026-11-02), MIT/Apache-2.0; it is now an async layer over the sans-IO `rtc` 0.21.1 crates.
  Built with `default-features = false, features = ["runtime-tokio", "crypto-ring"]` (and `rtc`
  with `crypto-ring`): no CMake, NASM or clang, no aws-lc-rs in the build. `str0m` 0.24.1 (MIT OR
  Apache-2.0) builds with `default-features = false, features = ["rust-crypto"]` (its default is
  aws-lc-rs), but see the advisory above. `cargo deny check` on the committed tree (webrtc-rs
  only): advisories, bans, licences, sources ok.
- **ICE:** webrtc-rs `PeerConnectionBuilder::with_udp_addrs(["127.0.0.1:0"])` binds exactly that
  address (only wildcards are expanded); no ICE servers, `MulticastDnsMode::Disabled`,
  `NetworkType::Udp4`. Gathered, every run: one host candidate `127.0.0.1 <port> typ host`
  (components 1 and 2 on the same port; rtcp-mux). str0m: `Candidate::host(127.0.0.1:<port>)` on
  a socket the spike binds. `Get-NetUDPEndpoint` / `Get-NetTCPConnection` for the process during
  a run: 4 UDP sockets, all `127.0.0.1` (host, viewer, relay A, relay B); the TCP listener and both
  ends of the signalling connection on `127.0.0.1`. (The client end is bound explicitly: an
  unbound `connect` shows up as a `0.0.0.0:<port> Bound` row, not a listener but it looks like
  one.) **The tailnet address was not exercised** (loopback only, by decision; binding it is one
  more entry in `with_udp_addrs`). No STUN/TURN/mDNS exists in the code path.
- **Congestion control:** webrtc-rs: `configure_congestion_control(Registry, Gcc::new(3 Mbps,
  300 kbps, 8 Mbps), CongestionFeedback::Twcc, ..)` installs the send history, TWCC sender, pacer
  and TWCC receiver; the estimator is wrapped to publish its target (and delay/loss halves) to
  the source thread. str0m: `enable_bwe(3 Mbps)`, `set_desired_bitrate(8 Mbps)`, estimate from
  `Event::EgressBitrateEstimate`. Encoder set point = estimate x 0.85 (the webrtc-rs pacer sends
  at exactly the target, so an encoder at the target builds a sender queue).
- **Impairment (substitute):** `clumsy` and Windows QoS policy are not installed and machine
  policy must not change, so an in-process UDP relay sits between the peers (the host's
  signalling code rewrites each side's SDP candidate to a relay socket, so both see consistent
  addresses and ICE uses the relay as the path). Media direction: token bucket at the cap plus a
  drop-tail buffer of `--queue-ms` (200 ms = a shallow router buffer; 2000 ms = bufferbloat).
  Feedback direction unimpaired. A clean shaper on loopback, not Wi-Fi or cellular: no jitter,
  no random loss.
- **Receiver:** webrtc-rs `TrackRemote::poll` -> `H264Packet` depacketiser -> access unit on the
  marker bit; an AU with a sequence gap is dropped and the viewer waits for an IDR (PLI at most
  every 300 ms). Clean AUs go to `ffmpeg -f h264 -i pipe:0 -f framecrc pipe:1` with an AUD
  appended; one framecrc line = one decoded picture. Latency is matched per frame by a
  `user_data_unregistered` SEI (UUID `GanderWP43Frame!`, text `id=<n>;cap=<QPC 100 ns>`) inserted
  before the first slice; every timestamp is on the same QPC clock.

### Frames decoded and latency (steady state, no cap)

`capture` = WGC `SystemRelativeTime` (1-5 ms ahead of frame arrival, WP-41 open question 1),
`encoded` = MFT `HaveOutput`, `sent` = `write_sample`/`write` returned (packets handed to the
pacer), `received` = marker packet of a clean AU at the viewer, `decoded` = ffmpeg's framecrc
line. p50 / p95 ms over frames from 3 s to the first cap; DC = data channel echo RTT.

| Run (label) | Host stack, encoder | Frames fed / decoded | capture->encoded | sent->received | capture->received | capture->decoded | DC RTT p50 / p95 (whole run) |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `intel-direct` (no relay, 20 s) | webrtc-rs, Intel | 579 / 578 | 5.1 / 7.5 | 0.3 / 0.5 | 7.2 / 10.4 | 74.3 / 89.7 | 0.3 / 0.4 |
| `intel-throttle-1500` | webrtc-rs, Intel | 1437 / 1229 | 4.9 / 7.3 | 0.4 / 0.6 | 7.1 / 10.1 | 73.6 / 96.8 | 0.4 / 462 |
| `nvidia-throttle-400` | webrtc-rs, NVIDIA | 1478 / 1304 | 5.1 / 9.3 | 0.4 / 0.7 | 6.9 / 12.0 | 74.9 / 91.6 | 0.6 / 807 |
| `str0m-throttle-1500` | str0m, Intel | 1443 / 1408 | 4.8 / 7.3 | 14.0 / 62.7 | 20.3 / 74.4 | 88.0 / 159.4 | 0.4 / 91 |
| `str0m-deepqueue-1500` | str0m, Intel | 1430 / 1429 | 7.2 / 12.3 | 14.7 / 56.3 | 23.4 / 68.7 | 93.7 / 154.2 | 0.5 / 28 |

- **A loopback receiver decodes the frames:** every run decoded every clean access unit it fed
  (ffmpeg lines = AUs fed); unimpaired, 578 of 579 captured frames (the last was inside the
  encoder at stop); 28-30 fps decoded in steady state. Frames fed minus decoded in the capped
  runs are AUs damaged by drops plus those skipped until the next IDR.
- `capture->received` is the transport number: **7 ms p50 / 10-12 ms p95 with webrtc-rs**
  (encode 5 ms, packetise and pace 2 ms, loopback 0.4 ms). `received->decoded` is **66-68 ms
  p50** and almost all of it is the ffmpeg CLI harness: its H.264 parser closes a picture only
  when the next start code arrives and the pipe/demux holds a second frame (measured separately:
  35 ms in a paced pipe test, 67 ms here; decoding itself is 1.4 ms/frame at 1080x2280,
  single-threaded). A `MediaCodec` fed one AU per `queueInputBuffer` has no such wait, so
  glass-to-glass belongs to WP-44. Also measured: ffmpeg 9 with `-fflags nobuffer` holds about
  46 frames (1.55 s) and drops them at EOF; the spike does not use it.
- str0m adds about 13-15 ms p50 (56-63 ms p95) between `write` and arrival on a free link (its
  pacer and probing; not tuned here).
- Data channel echo (tap JSON every 100 ms; ordered, reliable): 0.3-0.6 ms p50 on a free link.
  Under a cap it shares the congested queue: **p95 0.46-0.81 s with webrtc-rs** (behind video in
  the relay buffer), 28-91 ms with str0m (whose BWE keeps the queue short).

### Bitrate adaptation (the 5 s threshold)

Sampled every 250 ms (`results/<label>-timeline.csv`): estimate (GCC target or str0m's), its
delay and loss halves (webrtc-rs only), encoder set point, encoded output and received payload
over the last second, relay drops, decoded fps, capture->received p50.

| Run | Bottleneck | Before the cap | First set-point cut | Estimate <= 50 % | Encoded output <= cap | Settled under the cap (mean, last 5 s) | Recovery after the cap | Result |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `intel-throttle-1500` (webrtc-rs) | 1.5 Mbps, 200 ms buffer | GCC 8000, set 6493, out ~6.5 Mbps | 0.68 s | 1.00 s | **2.50 s** | GCC 2622 (swings 1.6-4.3 Mbps), out 1377, 17.9 fps, 55 ms; 725 drops in total | 6.5 s to 90 % of 8 Mbps | PASS |
| `nvidia-throttle-400` (webrtc-rs) | 0.4 Mbps, 200 ms buffer | GCC 8000, out ~1 Mbps | 0.79 s | 1.25 s | **3.75 s** | GCC 395, out 312, 19.8 fps, 128 ms | 12.5 s | PASS |
| `intel-deepqueue-1500` (webrtc-rs) | 1.5 Mbps, 2 s buffer | GCC 8000, out ~6.5 Mbps | 4.85 s | 5.25 s | **5.50 s** | GCC 1561, out 568, 21.7 fps, 32 ms; queue reached 1.97 s first | 5.75 s | **FAIL** |
| `str0m-throttle-1500` (str0m) | 1.5 Mbps, 200 ms buffer | 3941, set 3350, out ~1.8 Mbps | 0.94 s | 1.25 s | **1.50 s** | 1030, out 818, **28.6 fps, 31 ms**; 79 drops | not within 15 s (+5 % per ~2 s) | PASS |
| `str0m-deepqueue-1500` (str0m) | 1.5 Mbps, 2 s buffer | 2904, set 2210, out ~2.4 Mbps | 0.55 s | 0.75 s | **1.00 s** | 1075, out 849, **28.2 fps, 49 ms; 0 drops** | not within 15 s | PASS |
| `intel-codecapi` (webrtc-rs; Intel runtime `MeanBitRate`; cap at 10-20 s) | 1.5 Mbps, 200 ms buffer | GCC 6338, set 5387 | 1.38 s | 2.50 s | **never** | set 1181 but out **2430**, 6.7 fps, **1002 ms** | - | FAIL |

`intel-throttle-1500` (webrtc-rs, 200 ms buffer), excerpt, kbps / ms:

| t s | GCC | delay half | loss half | set point | encoded | received | relay drops | decoded fps | capture->recv p50 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 20.0 (cap on) | 8000 | 8000 | 8000 | 6493 | 6492 | 6503 | 0 | 30 | 7 |
| 20.5 | 7165 | 8000 | 7165 | 6493 | 6599 | 3930 | 262 | 16 | 7 |
| 21.0 | 3434 | 8000 | 3434 | 3626 | 4883 | 2303 | 360 | 1 | 160 |
| 21.5 | 2430 | 8000 | 2430 | 2378 | 2458 | 1375 | 388 | 0 | - |
| 22.5 | 1987 | 8000 | 1987 | 1812 | 1380 | 1365 | 395 | 7 | 172 |
| 23.5 | 2190 | 8000 | 2190 | 1812 | 1087 | 1288 | 395 | 33 | 48 |
| 26.0 | 4130 | 8000 | 4130 | 2376 | 1259 | 1206 | 395 | 27 | 79 |
| 35.25 (cap off) | 2004 | 8000 | 2004 | 1703 | 1356 | 1470 | 725 | 7 | 7 |
| 38.25 | 3599 | 8000 | 3599 | 2174 | 1240 | 1169 | 725 | 30 | 7 |
| 41.75 | 8000 | 8000 | 8000 | 6677 | 2667 | 2346 | 725 | 29 | 7 |

`intel-deepqueue-1500` (webrtc-rs, 2 s buffer), the failing case:

| t s | GCC | delay half | loss half | set point | encoded | received | relay drops | decoded fps | capture->recv p50 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 20.0 (cap on) | 8000 | 8000 | 8000 | 6548 | 6547 | 6557 | 0 | 30 | 13 |
| 21.0 | 8000 | 8000 | 8000 | 6548 | 6329 | 1370 | 230 | 6 | 414 |
| 22.0 | 8000 | 8000 | 8000 | 6548 | 6329 | 1416 | 756 | 6 | 1125 |
| 23.0 | 8000 | 8000 | 8000 | 6548 | 5238 | 1404 | 1167 | 4 | 1845 |
| 24.5 | 7186 | 8000 | 7186 | 6548 | 6329 | 1380 | 1968 | 0 | - |
| 25.0 | 4939 | 8000 | 4939 | 5281 | 3606 | 1378 | 1987 | 0 | - |
| 26.0 | 1142 | 8000 | 1142 | 1912 | 816 | 1362 | 1987 | 0 | - |

`str0m-deepqueue-1500` (str0m, 2 s buffer):

| t s | estimate | set point | encoded | received | relay drops | decoded fps | capture->recv p50 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 20.0 (cap on) | 2904 | 2210 | 2419 | 2353 | 0 | 30 | 23 |
| 20.5 | 2178 | 2210 | 2063 | 2028 | 0 | 27 | 51 |
| 21.0 | 1252 | 1566 | 1101 | 1527 | 0 | 21 | 82 |
| 21.5 | 1252 | 1151 | 1021 | 982 | 0 | 22 | 77 |
| 22.0 | 1252 | 1151 | 875 | 864 | 0 | 30 | 29 |
| 25.0 | 1252 | 1151 | 878 | 876 | 0 | 31 | 31 |

Reading the numbers:

- **webrtc-rs 0.21's GCC adapted through its loss half only.** In every webrtc-rs run quoted
  here the delay half stayed at the 8 Mbps ceiling while the relay queue grew to 200 ms (and, in
  the 2 s run, to 1.97 s); the cut came when the buffer overflowed and TWCC reported losses.
  With a shallow buffer that is quick (first cut 0.7 s, output under the cap 2.5 s) but the loss
  half then overshoots and re-congests every few seconds (GCC swings 1.6-4.3 Mbps over a
  1.5 Mbps link; 17.9 fps decoded with periodic IDR recoveries). With a deep buffer nothing
  reacts until the buffer is full: **5.5 s to adapt (fails the 5 s threshold) after 2 s of added
  latency.** One earlier 30 s probe with the same 2 s buffer did see the delay half fire
  (8000 -> 1512 kbps 2.5 s after the cap, with about 1 s queued), so the detector works but is
  far less sensitive than libwebrtc's, which reacts to tens of ms. The ADR's caveat ("GCC
  estimator + pacer validated only in simulation") is confirmed.
- **str0m's estimator reacts to delay:** first cut 0.55-0.94 s, output under the cap in 1.0-1.5 s,
  **0 drops** with the 2 s buffer and 79 with the 200 ms one (webrtc-rs: 725 and 1987), settled at
  28.2-28.6 fps and 31-49 ms capture->received. Caveats: its estimate sat at 2.9-3.9 Mbps before
  the cap (webrtc-rs had climbed to 8 Mbps), so it entered the cap with 2-2.6x the link rate
  rather than 4x; it ramps slowly (about +5 % per 2 s, never reached the 8 Mbps desired rate in
  50 s, partly because the test window does not need it and str0m probes only with padding); and
  its pacer adds 13-15 ms p50 on a free link.
- **Encoder following:** NVIDIA honours runtime `CODECAPI_AVEncCommonMeanBitRate` (output followed
  the set point down to ~300 kbps within 1-3 s). **The Intel Quick Sync H.264 MFT returns S_OK
  for runtime `MeanBitRate` and keeps its configured rate** (`intel-codecapi`: set point 1181
  kbps, output 2430 kbps; the stream collapses to 6.7 fps and 1 s latency behind the pacer), and
  `IMFTransform::ProcessEvent(MEEncodingParameters)` returns `0x80004001` (E_NOTIMPL).
  Re-creating the encoder at the new rate works: 93-162 ms p50 per re-creation (max 194 ms; once
  644 ms while congested), one IDR each, 1-9 in-flight frames lost per run. The spike re-creates
  on cuts over 15 % (100 ms apart) and rises over 25 % (1.5 s apart). Intel CBR then holds the
  rate (6.5 Mbps out at a 6.5 Mbps set point).
- **Keyframe requests (webrtc-rs): rtc 0.21 drops every inbound RTCP packet at the interceptor
  chain's terminus unless an interceptor marks it `Attribute::DeliverToApplication`.** Without
  that a PLI never reaches `TrackLocal::poll` (measured: 0 of 16 delivered). The spike adds a
  small interceptor at slot 14 000 that marks PLI/FIR; with it 21 of 21 arrived and each forced
  an IDR. Also `TrackLocal::poll` returns `None` until the track is bound (some time after
  `Connected`), so a reader must retry rather than stop at the first `None`. str0m surfaces
  PLI/FIR as `Event::KeyframeRequest` without extra work.
- **NACK** (`configure_nack`) is off by default in the spike: with it on, NACKs and
  retransmissions compete with media for the capped link and the viewer (which repairs nothing)
  sees only late duplicates. Phase 2: NACK off for this stream, or a short history, and rely on
  PLI + IDR.

### Recommendation (D12)

**On the measurements, str0m per D12's rule; in practice webrtc-rs 0.21.1 until str0m clears
RUSTSEC-2026-0387.** The owner decides in WP-45 (ADR flip); the facts:

- webrtc-rs passed the 5 s threshold with a shallow buffer (2.5 s, 3.75 s) and failed it with a
  deep one (5.5 s after 2 s of queueing). D12 says repeat with str0m when adaptation fails;
  str0m passed both (1.5 s and 1.0 s) without building a queue or losing packets in the deep
  case, and kept input RTT p95 under 0.1 s while capped.
- **str0m 0.24.1 cannot pass `cargo deny` today**: its DTLS (`dimpl` 0.7) pulls `crypto-bigint`
  0.5.5 (RUSTSEC-2026-0387, timing side channels in arithmetic used by ECDSA signing, i.e. the
  DTLS certificate key). `dimpl` 0.9.0 already moved to `ecdsa` 0.17 / `p256` 0.14, so a str0m
  release on dimpl 0.9 should clear it; Gander must not ship (or ignore) a known crypto advisory.
- str0m interoperated with the webrtc-rs viewer unchanged (same signalling, same SDP shape,
  H.264 PT 102 level 5.0), builds with pure-Rust crypto, gives PLI as an event, and its sans-IO
  loop is about 200 lines here. Costs to manage in Phase 2: slow ramp-up (tune desired bitrate
  and probing, or start the estimate higher on a known-fast tailnet path), 13-15 ms of pacer delay
  on a free link (worth a look), a peer-to-peer use its authors call less tested, and an API
  still moving (0.24, pin it).
- webrtc-rs remains attractive (frame-level `write_sample`, data channels, stats, 7 ms
  capture->received, faster ramp-up) and its estimator is a plug-in (`BandwidthEstimator`:
  `on_reports(now, &[PacketReport])` -> target), so swapping in a delay-sensitive estimator would
  close the gap; that is new estimator work, which D12 prefers to avoid, but it is bounded (a
  delay-gradient trigger on the same TWCC reports, or a port of str0m's delay controller behind
  the trait). Choosing webrtc-rs now means budgeting that estimator plus the PLI interceptor;
  choosing str0m means waiting for (or contributing) its dimpl 0.9 release.
- The raw H.264-over-TLS fallback (option 5) is not needed: WebRTC works here with either library.
- Whatever the library, the host must: re-create the Intel encoder to change its rate (or prefer
  NVIDIA when it activates); keep the encoder set point under the estimate (0.85 x here); answer
  PLI/FIR with an IDR; keep input ahead of video.

Pinned for the spike: `webrtc` 0.21.1 + `rtc` 0.21.1 (`runtime-tokio`, `crypto-ring`) in the
build; `str0m` 0.24.1 (`rust-crypto`) for the measured comparison only; `tokio-tungstenite` 0.29
for signalling.

### API and signalling shapes for WP-44 (Android receiver + input)

- **Signalling** (`spikes/stream/webrtc/src/signal.rs`): one JSON object per WebSocket text frame,
  tagged by `type`. The viewer speaks first; the host offers.

  | Direction | Message |
  | --- | --- |
  | viewer -> host | `{"type":"hello","client":"<name>","version":1}` |
  | host -> viewer | `{"type":"offer","sdp":"v=0..."}` (complete, non-trickle: candidates and `a=end-of-candidates` are in the SDP) |
  | viewer -> host | `{"type":"answer","sdp":"v=0..."}` (complete: gather before sending) |
  | either | `{"type":"candidate","candidate":"candidate:...","sdpMid":"0","sdpMLineIndex":0}` (trickle; defined, unused: host-only gathering is instant) |
  | either | `{"type":"bye"}` |

  The spike's WebSocket is plain and unauthenticated on 127.0.0.1. In Gander it rides the
  authenticated TLS WebSocket (paired device token), never a separate port.
- **Offer** (`results/intel-throttle-1500-offer.sdp`, webrtc-rs): BUNDLE of `m=video` (mid 0)
  and `m=application` (mid 1, `webrtc-datachannel`, sctp-port 5000); DTLS `setup:actpass`; video
  `a=rtpmap:102 H264/90000`, `a=fmtp:102 level-asymmetry-allowed=1;packetization-mode=1;
  profile-level-id=42e032`, `a=rtcp-fb:102 nack pli`, `ccm fir`, `transport-cc`, `a=extmap:1
  http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01`; one SSRC; host
  candidates only. webrtc-rs `add_track` offers `a=sendrecv` (Phase 2: a `sendonly`
  transceiver); the phone answers `recvonly`. str0m (`results/str0m-throttle-1500-offer.sdp`)
  offers `sendonly` with mids like `1MR`/`fej`, plus `abs-send-time`, `goog-remb`, `nack` and an
  RTX `ssrc-group:FID` without an RTX payload type; one candidate line carrying its ufrag.
- **The answer must** accept PT 102 at level 5.0 (or rely on `level-asymmetry-allowed=1`), keep
  `transport-cc` and the TWCC extmap (without them the host's estimator holds its start rate),
  send PLI on loss (libwebrtc does by default; the host forces an IDR), and gather host
  candidates on the tailnet interface (a native client is not subject to browser IP-handling
  rules).
- **Frame-id SEI** (optional for the phone; decoders ignore it): `user_data_unregistered` before
  the first slice, UUID `GanderWP43Frame!` (ASCII), payload `id=<n>;cap=<host QPC 100 ns>`. WP-44
  can log `id` per rendered frame to pair host and phone timelines.
- **Data channel `input`**: created by the host (in-band DCEP, ordered, reliable). Viewer ->
  host: `{"t":"tap","seq":<n>,"x":<0..1>,"y":<0..1>,"ts":<viewer clock>}`, coordinates
  normalised to the streamed picture, origin top-left. Host -> viewer: the same object with
  `"t":"echo"`, `"of":"tap"` and `"host_rx":<host QPC 100 ns>`; RTT = now - `ts` on the viewer.
  WP-44 adds `drag`/`key` kinds and the `SendInput` mapping; the echo stays as the latency probe.
- Picture: 1080x2280 H.264 constrained baseline level 5.0, IDR every 60 frames and on PLI,
  parameter sets in band before every IDR, 30 fps, about 1-7 Mbps on the test window
  (content-limited).

### Matrix rows added by WP-43

| # | Condition | Result | Observed behaviour and numbers |
| --- | --- | --- | --- |
| C1 | WebRTC send of the encoded stream, loopback | PASS (transport); glass-to-glass deferred to WP-44 | webrtc-rs 0.21.1, host candidates on 127.0.0.1 only: capture->received 7 ms p50 / 10-12 ms p95 (str0m 20-23 / 69-74); 28-30 fps decoded; the ffmpeg decode harness adds ~67 ms and is not representative |
| C13 | Rate adaptation when the link is throttled (substitute: in-process relay on loopback, not cellular) | webrtc-rs PASS with a 200 ms buffer (2.5 / 3.75 s), FAIL with a 2 s buffer (5.5 s); str0m PASS both (1.5 / 1.0 s) | webrtc-rs GCC reacted on loss only; str0m on delay. The Intel encoder needs re-creation to change rate. Real 5G / DERP runs remain for WP-44/45 |

### Anything the spec got wrong or left open

- The spec's throttling tools (`clumsy`, Windows QoS policy) are not installed and machine
  policy must not change; an in-process relay was used. It has no jitter or random loss, so it
  measures each estimator's reaction to a clean bottleneck, not radio behaviour.
- "Bitrate adapts within 5 s" depends on the bottleneck's buffer depth: the same library passes
  at 200 ms and fails at 2 s. Phase 2's criterion should name the buffer, or the queueing delay
  allowed before the cut (for example under 300 ms).
- The plan assumed the encoder's rate can follow the estimate; WP-42's API had no setter, and on
  Intel a runtime change has no effect (above).
- The usual `profile-level-id` (42e01f) understates 1080x2280; 42e032 negotiated without trouble
  with both libraries.
- Plan: "host candidates only on explicit addresses (loopback + tailnet)". Loopback only was
  exercised (by decision); the tailnet address is one more bind address, for WP-44 with a phone.

### Behaviours Gander must handle in Phase 2 (added by WP-43)

- Drive the encoder from the estimate with headroom; re-create the Intel MFT to change its rate
  (IDR, ~100-160 ms); prefer NVIDIA's runtime change when it activates.
- With webrtc-rs: install a PLI/FIR -> application interceptor (inbound RTCP is dropped by
  default) and supplement GCC with a delay-sensitive trigger. With str0m: tune the ramp-up.
- Keep input ahead of video: with webrtc-rs a congested link pushed the data channel echo to
  0.5-1.6 s. Options: cut video faster (above), send input on its own unordered channel with
  `maxRetransmits: 0`, pause video while the estimate collapses.
- After any loss wait for an IDR (the viewer here drops damaged AUs and asks once per 300 ms);
  NACK off or short-history.
- Bind ICE explicitly and bind outgoing TCP to the chosen address too (a bare `connect` reports a
  `0.0.0.0` row on Windows).

### Open questions (WP-43)

1. Why does webrtc-rs's delay-based detector stay silent with 0.2-2 s of queueing in most runs
   (trendline thresholds, arrival grouping under a perfectly smooth shaper, TWCC timestamp
   resolution)? Worth a minimal upstream reproduction before writing our own estimator.
2. str0m's slow ramp-up and 13-15 ms pacer delay: tunable (desired bitrate, padding, pacer) or
   inherent? A fairer comparison would start both stacks from the same pre-cap rate.
3. Behaviour on real Wi-Fi/5G through the tailnet (jitter, random loss, DERP relay): WP-44/45
   with a phone; the relay here cannot reproduce it.
4. Glass-to-glass with `MediaCodec` (no ffmpeg parser delay): WP-44.
5. When does str0m ship on `dimpl` 0.9 (clearing RUSTSEC-2026-0387)? Until then it cannot be
   a dependency under the current `cargo deny` policy.
6. Does Intel's MFT honour a runtime rate change by another route (newer driver, another
   `ICodecAPI` property such as the buffer size, or an `IMFSinkWriter`-hosted encoder)?
   Re-creation works but costs an IDR per change.

## Android + input (WP-44)

- Date: 2026-11-02, same machine as above (Windows 11 Pro 26200, Intel iGPU + NVIDIA RTX 5060 Laptop, two
  monitors: 2560x1600 at 150 % primary, 1920x1080 at 100 % secondary). The input matrix ran twice: with the
  session **locked** (first run) and **unlocked** (the owner was back at the machine for the later runs, which is why the emulator runs
  below were all unlocked).
- Code: `spikes/stream/android/` (throwaway Gradle project `gander-stream-spike`, package
  `fyi.arlo.gander.spike.stream`, wrapper and versions from `client/android`, `org.jitsi:webrtc` 155.0.1,
  OkHttp 5.4.0 for the signalling WebSocket; Kotlin: `MainActivity`, `StreamSession`, `FrameProbe`, `Evt`) and
  `spikes/stream/input/` (crate `spike-input`: `matrix`, `serve`; reuses `spike-webrtc`, which is now a library
  plus its binary, `spike-encode` and `spike-capture`). Driver script:
  `spikes/stream/android/scripts/Invoke-StreamSpike.ps1`.
- Reproduce, input matrix (no phone, about 30 s): `cargo run --release -p spike-input -- matrix --results
  spikes/stream/input/results` (writes `input-matrix-<locked|unlocked>.txt`). Reproduce, emulator:
  `gradlew --no-daemon assembleDebug` in `spikes/stream/android` (with `GRADLE_USER_HOME` set as in CLAUDE.md),
  `cargo build --release -p spike-input`, then `pwsh -NoProfile -File
  spikes/stream/android/scripts/Invoke-StreamSpike.ps1 -Label <name> -Taps 12` (boots the `gander` AVD headless
  when no other emulator runs, installs the app, starts the host, launches the app, drives taps with `adb shell
  input tap`, kills everything).

### Input injection: C19 (`SendInput`), C20 (`PostMessage`), and the tap mapping (C3, input part)

Every click went to a `gander-testwin` window started by the run. `SendInput` is only called when the top-level
window under the point (`WindowFromPoint` + `GetAncestor(GA_ROOT)`) is the target (the production policy: a
covered window pauses input) or, for the one "where do the clicks land" row, another test window.

Mapping used by both injectors. The phone's `(x, y)` is normalised to the streamed picture, which is the WGC
frame of the window (the DWM visible frame bounds, title bar and borders included) scaled to the encode size.
Frame pixel = `floor(x * frame_w)`, `floor(y * frame_h)`; screen point = frame origin + frame pixel; client point
= screen point - client origin (`WindowGeometry`; the process is per-monitor DPI aware v2). `SendInput` gets the
screen point as an absolute move over the virtual desktop (`((2p + 1) * 65536) / (2 * len)`, unit-tested to land
on the right pixel for every pixel of widths up to 7680); `PostMessage` gets `WM_MOUSEMOVE`, `WM_LBUTTONDOWN`
(`MK_LBUTTON`) and `WM_LBUTTONUP` with the client point in `lParam` (physical pixels).

| # | Condition | Session locked (first run) | Session unlocked (later run) |
| --- | --- | --- | --- |
| C20 | `PostMessage` to the HWND at the moving box's centre, 5 rounds, nothing covering the window | **5/5 hits** (box colour advanced, `hits` +5, `clicks` +5); the lock screen was the foreground window, so the target had no focus | **5/5 hits** (the target was the foreground window here) |
| C20 | same while a second test window B covers the box | **5/5 hits on A, 0 clicks on B** | **5/5 hits on A, 0 clicks on B** (B was foreground) |
| C20 | same after moving the window to the 100 % monitor | 5/5 hits | 5/5 hits |
| C19 | `SendInput` (absolute move, left down, left up) at the box centre, nothing covering the window | **not sent**: the pre-click check found `LockScreenBackstopFrame` (pid 13028) as the top-level window under every point, test window included, so the click would have hit the lock screen. Refused 5/5 rounds | **5/5 hits** (`SendInput` accepted 3/3 events; the real cursor moved onto the box and was put back afterwards) |
| C19 | `SendInput` at A's box centre while B covers it, production policy | refused (lock screen on top of both) | **refused 5/5** ("covered by GanderTestWindow pid ...", nothing sent): A +0 clicks, B +0 |
| C19 | same, guard relaxed to any test window (where do the clicks land) | refused (lock screen) | **clicks land on the covering window**: B +5 clicks, A +0 (0 hits: the click did not land on B's own box) |
| C19 | `SendInput` with no-op moves (relative (0,0), absolute onto the cursor's own pixel), no click | not run (the session unlocked before the row existed) | accepted 1/1 and 1/1, cursor unchanged |
| C3 | Mapping at 150 % (dpi 144, client 687x1440): 25 points incl. the first and last client pixel | normalise -> map round trip **0 px** worst; map vs `ScreenToClient` 0 px; posted point vs the window's `last_click` 0 px | same, 0 px |
| C3 | Mapping at 100 % (dpi 96, window moved to the second monitor): 25 points | 0 px, 0 px, 0 px | same, 0 px |

What the locked run established exactly:

- `OpenInputDesktop` **succeeded** while locked (`input_desktop=Ok`), so that call is not a lock detector on
  this build; `WTSSessionInfoEx` `SessionFlags` (`session_locked()`) is (`WTS_SESSIONSTATE_LOCK`).
- On Windows 11 26200 the lock screen is a set of top-level windows on the default desktop: the foreground
  window was `Windows.UI.Core.CoreWindow` (the lock app) and the window under every probed point, above
  topmost test windows, was `LockScreenBackstopFrame`. So the same pre-click check that pauses input for a
  covering application also pauses it for the lock screen, without a separate session query. What a
  `SendInput` call itself returns while locked is **not measured** (the only way was a click that would have
  landed on the lock screen, outside the allowed targets, and the no-op probe row was added after the session
  unlocked). Treat "`SendInput` does not reach a window while locked" as established through the covering lock
  window, and its return value as open.
- **`PostMessage` is unaffected by the lock**: 5 of 5 clicks reached an unfocused, covered window while the
  session was locked, and the lock does not stop WGC capture either (WP-41). So a locked session can still be
  captured **and** driven through posted messages: the host must refuse both while locked
  (`WTSRegisterSessionNotification`; `security.md` already says Stream needs an unlocked session).
- `gander-testwin` reacts to `WM_LBUTTONDOWN` directly (its README says so); that is why `PostMessage` works
  for it. **C20 for Notepad, PowerPoint and Edge was not run**: the unattended-run boundary allows injection
  into test windows only. Posted mouse messages skip everything real input goes through (`WM_NCHITTEST`,
  `WM_MOUSEACTIVATE`, mouse capture, `GetKeyState`, raw input and pointer messages), so a pass on the test window
  predicts little for Office or Chromium; that stays open for a human-attended run.

### Android receiver on the emulator (frames, taps, colour change)

Setup: AVD `gander` (Pixel 6 profile, API 36 `google_apis` x86_64, 1080x2400, WHPX, headless: `-no-window
-no-audio -no-snapshot-save -no-snapshot-load -gpu swiftshader_indirect`), build
`google/sdk_gphone64_x86_64/emu64xa:16/BE2A.250530.026.F3`. Host: `spike-input serve` streaming a
`gander-testwin` window at 1080x2280, 30 fps, Intel H.264 (the WP-43 stack: webrtc-rs 0.21.1, GCC, 127.0.0.1
sockets only), PostMessage injection unless stated. The phone logs one JSON line per event (`logcat` tag
`GanderSpike`; every run's full log is in `spikes/stream/android/results/<label>-logcat.txt`) and the host's
driver pairs them with its own timeline. **Verify result: PASS on the emulator**: the track renders (overlay
frame counter, 28-30 fps), and a tap on the moving box changes its colour in the host window and in the
decoded stream on the phone. The tap is a real touch (`adb shell input tap`) aimed at the box, sent as
normalised coordinates, mapped through the window frame and injected; 65 of 66 aimed taps over 7 runs hit
the box (the one miss was a prediction miss in the `adb reverse` run). The aim uses the host's own `state?` box
position, the phone's reported picture rectangle (view 1080x2400, picture 1080x2280 at y = 60, letterboxed) and
a measured `adb shell input tap` start-up lag (60-150 ms) to predict where the box will be.

- **Phone app** (`MainActivity`, `StreamSession`, `FrameProbe`): full-screen `SurfaceViewRenderer`, a text
  overlay (state, frames that reached the renderer, fps, decoded and dropped frames, decode ms per frame,
  jitter buffer ms, decoder name, ICE and data channel state, echo RTT, taps, last tap -> pixels time and the
  box colour the phone sees), WebSocket signalling with OkHttp, answers recvonly, sends
  `{"t":"tap","seq","x","y","ts"}` on the host's `input` channel on the touch-up of a tap, reads
  `{"t":"echo",...}` for the RTT. `FrameProbe` is a `VideoSink` that counts frames and, when the decoder hands
  over CPU I420 buffers, classifies the box colour by hue on a coarse grid, which gives **tap -> changed
  pixels in the decoded stream on the phone's own clock** (touch-up to the first frame with the new colour;
  excludes display scan-out). The screenshot `results/final12-screen.png` shows the overlay over the streamed
  window (counter, clock, box, caption `hits 5 / clicks 5`).
- **Decoder.** `org.jitsi:webrtc` 155.0.1 has no software H.264 decoder (`SoftwareVideoDecoderFactory` is
  VP8, VP9 and AV1); H.264 comes from MediaCodec only. The spike builds
  `HardwareVideoDecoderFactory(null, { true })` (CPU buffers, any MediaCodec H.264 decoder) and, with
  `-TextureDecode`, the same with the EGL context (GPU textures, no pixel readback). The emulator lists
  `c2.goldfish.h264.decoder` (hardware-accelerated flag set), `c2.android.avc.decoder`,
  `OMX.google.h264.decoder`; every run used `c2.goldfish.h264.decoder`. The phone's receiver capabilities
  list H.264 only as `profile-level-id=42e01f` (+ `packetization-mode=1`, `level-asymmetry-allowed=1`); the host
  offers `42e032` (level 5.0, WP-42) and the call negotiated, the answer carrying `42e01f`. 1080x2280 decoded
  at 13-31 ms per frame in the runs below (174 ms in the discarded fresh-boot run described under them).
- **Negotiation** (`results/final12-offer.sdp` is the host's offer as generated, loopback address; the phone
  received it with 10.0.2.2; `final12-answer.sdp`): the phone answers `a=recvonly` for video PT 102 with
  `a=rtcp-fb:102 transport-cc`, `ccm fir`, `nack pli` and the transport-wide-cc extmap kept, `a=setup:active`,
  and an `m=application` for the data channel; BUNDLE 0 1. The data channel `input` opened (created by the
  host, ordered, reliable) in every run.

| Run (label) | Condition | Taps hit | Phone: tap -> changed pixels p50 / p95 / max ms | Decode ms/frame, jitter buffer ms | Frames | Echo RTT p50 / max ms |
| --- | --- | --- | --- | --- | --- | --- |
| `emu1` | emulator booted about 2 minutes earlier, first session | 8/8 | 309 / 498 / 498 (falls 498 -> 164 over the run) | 31.5, 143 | 30.1 fps, 18 dropped | 7.5 / 41 |
| `post20` | same emulator, second session, 20 taps | 20/20 | **118 / 183 / 229** (taps 9-20: 82-142, median 104) | 13.4, 51 | 30 fps (stats), 1 dropped | 4.0 / 18.9 |
| `alt8` | alternate `SendInput` (odd taps) and `PostMessage` | 8/8 (4 + 4) | 226 / 293 / 293 | 19.8, 70 | 28 fps, 1 dropped | 4.3 / 11.9 |
| `rev6` | signalling over `adb reverse` | 5/6 | 97 / 104 / 104 | - | - | 2.8 / 23.3 |
| `strip6` | answer candidates stripped before the host applies the answer | 6/6 | 265 / 308 / 308 | - | - | 12.8 / 15.6 |
| `tex6` | GPU texture decode (no pixel readback, so no phone-side effect time) | 6/6 on the host | n/a | 16.7, 102 | 27 fps, 35 dropped | 3.3 / 20.1 |
| `final12` | fresh boot, 90 s settle, 12 taps (the committed Verify run) | **12/12** | **142 / 221 / 221** (taps 8-12: 100-122) | 17.7, 48 | 28.4 fps, **0 dropped**, 0 lost packets | 2.9 / 13.2 |

An earlier run started right after a fresh boot (not kept; the settle delay was added after it) gave 11/12 hits,
777 / 1473 ms and a decoder at 174 ms per frame, 17 fps, 333 dropped frames: the emulator's decoder and jitter
buffer dominate and move by a factor of ten with the emulator's warm-up, so **none of these are latency numbers
for a phone**. What they do show: the pieces that are not the decoder cost little.

- Host side, per tap (all runs): data channel message -> injection start 0.5-0.7 ms p50 (21 ms for the first tap of a session); the `PostMessage` call
  0.02-0.04 ms, the `SendInput` call 4.9 ms p50 (7-8 ms in the first calls); the box colour changes in the
  window within 0.1 ms of the injection (the test window repaints at once; polled on the pipe, 2.4-3.0 ms in the
  slowest cases). `adb shell input tap` itself takes 60-150 ms to start, which is not part of any number above.
- Echo RTT on the data channel through the emulator's NAT: p50 2.8-12.8 ms per run, single round trips 1.4-133 ms.
- Behind the 82-120 ms best case on the phone are, in order: the host's next captured frame (the source takes every
  second 60 Hz frame, up to 33 ms), Intel encode 9 ms, packetise and pace 2 ms (WP-43), then on the phone the jitter
  buffer (42-51 ms in the settled runs) and decode (13-28 ms). The jitter buffer is the biggest single item the
  receiver controls; libwebrtc has a playout-delay hint that was not tried.
- Threshold "tap-to-effect under 120 ms median": **not decided by the emulator**. Warm runs: 118 ms (`post20`),
  142 ms (`final12`); steady-state taps alone 82-142 ms (median 104 in `post20`); cold or just-booted runs 300-800 ms. A real phone (hardware
  decoder, real radio) is needed. Glass-to-glass (spec C1) and phone-side photographs: **deferred, device-gated**
  (no real device; the emulator has no display scan-out or camera path worth measuring).

### How the emulator reached the host (the UDP question)

Everything on the host side stays on 127.0.0.1 (`Get-NetUDPEndpoint` / `Get-NetTCPConnection` for the host
process: one UDP socket `127.0.0.1:<ephemeral>`, one TCP listener `127.0.0.1:47440`; no wildcard, LAN or tailnet
address). What was measured:

1. **UDP: nothing to configure.** The emulator's NAT (QEMU slirp) aliases the host's loopback as 10.0.2.2 for
   UDP as well as TCP. The host rewrites only the **IP** of its offered candidate to 10.0.2.2
   (`relay::rewrite_candidate_ip`, the port is the bound one; `HostOptions::advertise_ip`), the phone sends its
   STUN checks and then DTLS and SRTP to 10.0.2.2:`P`, and the host receives them on 127.0.0.1:`P` from a
   127.0.0.1 source port that the emulator's NAT picked. The selected pair on the phone (stats event
   `candidate_pair`): **local `prflx 127.0.0.1:P+1`, remote `host 10.0.2.2:P`**, ICE RTT 1-23 ms. Connected
   1.73-1.85 s after the WebSocket accept (the loopback viewer of WP-43 took 0.2 s).
2. **The phone's own candidates are irrelevant here.** The answer carried `10.0.2.15` (the guest) and a `fec0::`
   address, which the host cannot reach; the pair that works is peer-reflexive on the host's side, learned from
   the phone's first STUN request. The run with the answer's candidates **stripped** (`strip6`) behaved the same.
3. **TCP signalling:** direct `ws://10.0.2.2:47440/` works; `adb reverse tcp:47440 tcp:47440` with
   `ws://127.0.0.1:47440/` also works (`rev6`; the phone's `candidate_pair` events in `rev6-logcat.txt` show the same ICE pair). The first connect after a
   cold boot failed once with `ConnectException` (the guest network was not up yet although
   `sys.boot_completed` was 1), so the app retries the WebSocket (up to 15 times, 1.5 s apart).
4. **`adb emu redir` is not needed and runs the other way.** `adb emu redir add udp:40000:40000` makes the
   **emulator process** listen on the host's `127.0.0.1:40000` and forward into the guest's port 40000 (host to
   guest, `netstat` showed the qemu pid as owner); it was added and removed again. It would only matter for a host
   that dials into the guest, which ICE does not need here.

Two things that are only true for the emulator and must not be carried over: the phone's candidates do not reach the
host (so the host depends on peer-reflexive learning), and the host sees the phone at 127.0.0.1.

### Matrix rows added by WP-44

| # | Condition | Result | Observed behaviour and numbers |
| --- | --- | --- | --- |
| C1 | Glass-to-glass on Wi-Fi via the tailnet | **deferred (device-gated)** | No real device. Emulator numbers above are not latency numbers; the overlay (frame counter, box colour, tap -> pixels) and the test window's counter and clock are ready for the two-screen photograph |
| C3 | Input mapping at 125 % / 150 % | PASS at 150 % and 100 % (125 % not measurable: no such monitor, display scale is a machine setting) | 25 points per scaling including the first and last client pixel: normalise -> map round trip 0 px, map vs `ScreenToClient` 0 px, posted vs the window's `last_click` 0 px (dpi 144, client 687x1440; dpi 96, client 338x708). End to end on the emulator, 65/66 aimed taps hit a 64 px (96 px physical) box |
| C13 | Cellular via tailnet | **deferred (device-gated)** | needs a phone on 5G |
| C14 | Phone screen off / app backgrounded | INFO (emulator) | See below: the host notices nothing for at least 20 s; a killed app is noticed after 30 s |
| C19 | `SendInput` while another window is foreground | PASS: clicks land on the covering window, the pre-click check detects it | Unlocked: B covers A, forced click -> B +5 clicks, A +0; production policy refuses all 5. Locked: the lock screen covers everything, the check refuses. `SendInput` itself under lock: not measured (open question 1) |
| C20 | `PostMessage` to the HWND without focus | PASS for `gander-testwin` (locked, unlocked, covered, 2 monitors); **Notepad, PowerPoint, Edge not tested** (boundary) | 5/5 hits in every matrix condition including while the session was locked and while another window covered the target; end to end 65 of 66 aimed taps (PostMessage and SendInput) hit |

C14 on the emulator (`results/c14-*`; the app is the only thing on the phone, no taps driven; the host keeps
streaming 30 fps and prints `frames_sent` every 5 s, kept in `c14-<action>-host-output.txt`):

| Action (T = 12 s after connect) | Phone | Host |
| --- | --- | --- |
| Home key (app to the background) for 20 s, then back | `onPause`/`onStop` at once; frames keep arriving for another ~5 s, then **no packets for ~15 s** (received count flat although the app's own stats thread keeps ticking, so the process is alive); ICE `DISCONNECTED` 10.6 s after `onStop`; when the app returns, `CONNECTED` at the same instant and decoded frames continue within ~0.6 s with no renegotiation | **No notice**: `frames_sent` kept rising by ~150 per 5 s for the whole 20 s |
| Screen off (`KEYCODE_SLEEP`) for 20 s, then on | identical timeline to the Home key | identical: no notice |
| App force-stopped | process gone | the host's connection state went `Failed`/`Closed` **29.95 s** after the kill (webrtc-rs ICE failure timeout; no `bye`, no DTLS close alert); it encoded and sent 30 fps into the void for those 30 s |

So with the current protocol the host cannot tell a phone that stopped looking (backgrounded, screen off) from one that is
watching, and notices a dead one only after about 30 s. Cause of the packet stop while backgrounded (OS network
restriction, decoder pause, surface loss) is not isolated.

### Anything the spec got wrong or left open (WP-44)

- The brief's `adb emu redir add udp:<port>:<port>` "maps an emulator port to the host": it maps a host port into the
  guest and is not needed; 10.0.2.2 reaches a host socket bound to 127.0.0.1 over UDP unchanged (above).
- WP-43's signalling shape says the viewer gathers before answering (non-trickle). With libwebrtc that is not
  reliable: in two runs (`rev6`, `strip6`) `IceGatheringState.COMPLETE` fired right after `setLocalDescription` **before
  any candidate existed**, and the answer went out with no candidates (the candidates then arrived one tick later through
  `onIceCandidate`, which the app does not forward). The call worked only because the phone dials the host's candidate
  and the host learns the phone peer-reflexively. Phase 2 must either forward the phone's trickled candidates (the
  `candidate` message is already defined) or keep relying on peer-reflexive learning, and test it on the tailnet.
- "Do NOT unlock" was kept, but the session did not stay locked: it was unlocked for the later runs (the owner was
  at the machine; foreground windows seen: Windows Terminal, a Tauri window). The locked data are the first matrix run only.
- The spec's C20 wants Notepad, PowerPoint and Edge; the unattended-run boundary allows injection into test windows
  only. And `gander-testwin` handles `WM_LBUTTONDOWN` itself, so its pass is the weakest possible form of the claim.
- "Tap-to-effect under 120 ms median" does not say between which points. Measured here as touch on the phone to
  the first decoded frame with the new colour on the phone (the longest honest definition short of a camera).
- `OpenInputDesktop` succeeds while the session is locked on build 26200, so it cannot serve as the lock check
  `security.md` needs; use `WTSSessionInfoEx` (as WP-41's harness does) or the covering-window check.

### Behaviours Gander must handle in Phase 2 (added by WP-44)

- **Never inject while locked, through either path.** `PostMessage` is not stopped by the lock screen and not by a
  covering window, and WGC still captures: the lock must be enforced by the host (`WTSRegisterSessionNotification`,
  refuse new sessions, stop running ones).
- **Pre-click check for `SendInput`:** the top-level window under the mapped point (`WindowFromPoint` +
  `GA_ROOT`) must be the shared window, else pause input and tell the phone ("covered by another window"). A
  small race remains between the check and the call. `SendInput` also moves and clicks with the **real cursor** (the
  spike put it back; the user sees it jump) and takes 5-8 ms per click; `PostMessage` leaves the cursor alone, but only
  what the target reads from messages reacts (open for real applications). A hybrid (post when it works, `SendInput`
  when the window is visible and uncovered) needs the per-application evidence that C20 did not collect.
- **The phone must tell the host when it stops watching** (backgrounded, screen off) and the host must stop encoding
  then: no transport signal exists for 15-30 s. `{"t":"pause"}` / `{"t":"resume"}` on the `input` channel, and an
  application-level liveness ping with a short timeout, are the minimal fix.
- Handle the phone's gather quirk (above): trickle, or tolerate an empty-candidate answer.
- A phone cannot decode H.264 in software with this libwebrtc build: if no MediaCodec H.264 decoder exists the offer
  must fall back (HEVC, VP8) or the client must refuse stream mode.
- Aim input at the picture, not the view: the picture is letterboxed (60 px bars on a 1080x2400 screen); a tap on a bar
  is not a tap on the window.

### Open questions (WP-44)

1. What does `SendInput` return while the session is locked? The matrix row `C19 probe` exists (no-op moves, no
   click); run `spike-input matrix` while the session is locked to fill it, and compare with the lock screen
   covering check.
2. Real device: glass-to-glass by photographing both screens (counter and clock are on both), MediaCodec hardware decode
   time, and whether libwebrtc's jitter buffer (42-51 ms here) can be pinned lower for a screen stream.
3. PLI on loss was not exercised from libwebrtc (0 packets lost in every run, `pli_count` 0): does libwebrtc's PLI
   reach the host's keyframe interceptor and force an IDR? The WP-43 loopback test only covered a webrtc-rs viewer.
4. C20 on Notepad, PowerPoint, Edge, Photos (a human-attended run): do posted mouse messages reach them, and do they
   react without `WM_MOUSEACTIVATE`, capture and raw input?
5. Why do packets stop ~5 s after the app is backgrounded while its process keeps running, and does a foreground
   service (as a real client would need for a long stream) keep them flowing?
6. Does the first-session slowness (decode 31 ms, jitter buffer 143 ms, 18 dropped frames) come from the emulator's
   decoder warm-up, host contention (another worker's emulator ran before) or the app? Only a real device settles it.
