# Architecture

Status: decided by default on 2026-11-01 (ADR-0001, 0002, 0005, 0006, 0007); the streaming stack
was settled by the Phase 0 spike on 2026-11-02: `webrtc` (webrtc-rs) 0.21.x with conditions, a
Media Foundation hardware encoder, an own thin WGC wrapper (see
`specs/2026-11-01-stream-spike-findings.md`). Details live in `docs/specs/`; this file is the map.

## Components

```
 Phone / tablet (Kotlin, Compose)                   Computer (host, Rust, user session)
 +-------------------------------+   private net   +--------------------------------------------+
 | Hosts -> Shares -> Browser    | <=============> | gander-host (tray app, egui)               |
 |  File sheet: Preview/Fetch/   | (Tailscale,     |  Listener: loopback + private-net addresses |
 |  Stream with reasons          |  Headscale,     |   TLS 1.3 self-signed, SPKI pinned         |
 |  Preview viewers              |  ZeroTier,      |  Trust adapter: tailscale (WhoIs) | generic |
 |  Fetch engine + cache         |  WireGuard)     |  Pairing + device tokens + audit log       |
 |  Stream viewer (WebRTC)       |                 |  Policy: shares, permissions, stream policy |
 |  DocumentsProvider (Phase 3)  |  control: HTTPS |  Path resolver (handle-relative)           |
 +-------------------------------+  + WebSocket    |  Browse/Fetch API, events, thumbs/previews |
                                     bytes: HTTP   |  Stream engine: launch, WGC, MF encode,    |
                                     Range         |   WebRTC, input (Phase 2)                  |
                                     media: WebRTC +---------------------+----------------------+
                                     (host-only ICE)                    | per job
                                                                 +------v------+
                                                                 | gander-render| (worker, job object,
                                                                 | WIC/PDF/Office| handle in, image out)
                                                                 +-------------+
```

## Host agent (Rust)

- **Process model (ADR-0007):** one tray application per logged-in user, autostarted at logon. It
  keeps serving Preview and Fetch while the session is locked; Stream refuses with `host_locked`.
  No Windows service in v1: WGC and input need the interactive session and Tailscale's LocalAPI
  only answers the logged-in user. Renderers run in a separate worker process per job.
- **Listener (ADR-0005, pairing spec):** binds loopback plus the addresses chosen by the trust
  adapter, never a wildcard, exclusive; re-plans on interface/address change notifications and a
  30 s poll; closes the private-network sockets within 5 s of the address disappearing. Port
  42637 by default. TLS 1.3 only, ECDSA P-256 self-signed, SPKI = host id, pinned by clients.
- **Trust adapters (ADR-0002):** `tailscale` auto-detects the Wintun adapter and tailnet ranges,
  calls WhoIs via the LocalAPI named pipe (CLI fallback), records identity at pairing and re-checks
  per connection, refuses tagged peers by default, classifies the peer path (direct, peer relay,
  DERP). `generic` uses an owner-selected interface or CIDRs. Pairing and tokens are mandatory
  under both.
- **Pairing (pairing spec):** exists only while the owner's "Add a device" dialog is open; QR with
  host id + addresses + one-time secret, or manual address entry with a SAS code compared on both
  screens; approval always on the host; 32-byte tokens stored hashed; immediate revocation.
- **Shares and policy:** Share = (root, name, per-device permissions list/preview/fetch/stream,
  show-hidden, hidden patterns, show-path). Read-only in v1. Stream permission is per share and
  per device, off by default, and framed as trusted-device access (ADR-0006).
- **Path resolution (path spec):** per-segment validation, handle-relative opens, reparse-tag
  policy (cloud placeholders allowed, links denied), short-name check, final-path belt-and-braces.
  Every other module receives handles, never paths. Hostile corpus in `protocol/test-vectors/`.
- **API (protocol spec):** `/v1/host`, `/me`, pairing, `/shares`, `/list`, `/stat`, `/search`,
  `/recent`, `/thumb`, `/preview*`, `/file` (Range), `/hash`, `/events` (WebSocket), `/stream`.
  Machine-readable error codes; capabilities advertised; browser-marked requests refused.
- **Thumbnails and previews (renderer spec):** WIC, RAW embedded previews, `Windows.Data.Pdf`,
  shell thumbnail handlers, Office embedded thumbnails, Office COM or LibreOffice page rendering,
  video posters; worker isolation; content-keyed cache with an LRU cap; visible-first queue.
- **Events:** share change notifications, host state (locked, attended, on battery), device
  revocation, stream state and signalling.
- **Tray UI (egui):** shares, devices, add-device dialog, activity log, network status with a
  firewall self-check, settings, live indicator, kill switch. Windows draw with OpenGL (glow) on
  the integrated GPU of a hybrid laptop by default (`[ui] gpu = "integrated" | "discrete" |
  "auto"`); the host exports the Optimus/PowerXpress symbols with the value 0 and raises it only
  while the Stream encoder is set up or a `discrete` window is created
  (`specs/2026-11-03-igpu-ui-findings.md`).
- **Installer:** MSI (WiX 5, `cargo xtask installer-build`), autostart task, firewall rules scoped to tailnet CIDRs; signing
  is a release step.

## The three modes

### Preview (compute: host, small)
Images resized; PDF pages rendered on the host; Office pages via COM/LibreOffice when available,
else embedded thumbnail only; text with encoding detection (highlighting on the phone); video and
audio poster/cover plus playback by Range. Every renderer is a separate process under caps so a
hostile file cannot take the host down.

### Fetch (compute: phone)
HTTP Range with ETag, resume, optional BLAKE3/SHA-256 verification; cellular warning by size;
open with the system resolver or built-in viewers; LRU cache with pinning for offline. Phase 3:
Files-app integration via a `DocumentsProvider` backed by on-demand proxy file descriptors.

### Stream (compute: host; Phase 2)
1. Phone asks to stream file F with its viewport.
2. Policy (ADR-0006): final extension not hard-refused; the handler executable resolved without
   executing; never-list; owner-approved app catalogue (first use prompts on the host); browsers
   only via an isolated window template; host not locked; one session at a time; idle-user prompt
   if someone is at the keyboard.
3. Launch with the catalogue template, discover the window (new-process, title-match,
   ApplicationFrameHost mapping, or pick), fit the window to the phone, restore if minimised.
4. Capture that HWND with Windows Graphics Capture (own wrapper over the `windows` crate, not
   `windows-capture`; popups included on Windows 11 24H2+), hardware H.264 via Media Foundation on
   the capture device (one device on the encoder's adapter, chosen by LUID), WebRTC (webrtc-rs
   0.21.x, plus a delay-based bandwidth estimator and a PLI interceptor) with host-only candidates,
   repeat-last-frame keepalive, keyframe on request, bitrate from the congestion controller.
5. Input over data channels: normalised coordinates mapped to the live window rect and injected
   with `SendInput` only while the window is foreground and uncovered; paused on focus loss or
   local activity; never while the session is locked (the host enforces it, see Known limits).
6. Session resume for 120 s after disconnect; end restores placement and applies the close policy.
   Sound (WP-55, `[stream] audio`, on by default): WASAPI process loopback of the window's process
   tree only (never the system mix; no fallback when it cannot start), Opus (libopus through
   `opusic-c`) on an audio track in the same BUNDLE, captured only while streaming and not muted
   (`{"t":"mute"}` on `control`); the being-viewed texts say "and sound" while it flows.
7. Clipboard text (WP-56, opt-in `[stream] clipboard`, off by default): `clipboard.get` and
   `clipboard` on the `control` channel, text only and at most 64 KiB, served by the input worker
   behind the same gate as a click (`gander_core::clipboard`, `gander_win::clipboard`), audited by
   direction and size, with a notification on the computer.

## Client app (Kotlin + Compose; ADR-0001)

Native because every hard part is native: SPKI pinning of a self-signed cert needs a custom trust
manager (library pinners pin on top of system trust), downloads need our own engine, the
Files-app provider runs without any JS/Dart engine, and WebRTC decode is libwebrtc either way.
Screens and behaviour are specified in `specs/2026-11-01-android-client-v1.md`. iOS later in
SwiftUI against the same protocol and test vectors.

## Protocol

`specs/2026-11-01-protocol-v1.md`. Versioned path (`/v1/`), capabilities in `/me`, additive
changes only within a version, unknown fields ignored by both sides.

## Known limits (stated, not hidden)

- Nothing is reachable after a reboot until the owner logs in (no service in v1).
- Stream needs an unlocked, attended-or-idle session. Windows does not enforce this (WGC captures
  and posted messages are delivered while locked, measured on build 26200): the host tracks the
  lock state itself and fails closed.
- Menus and tooltips need Windows 11 24H2 (documented; measured only on build 26200). Modal dialogs
  are never in a window capture and are streamed as their own window.
- Stream gives the phone the streamed app's own file dialogs; it is a trusted-device feature.
- Relayed Tailscale paths (DERP) are a few Mbit/s; the client warns before streaming.
- Protected media (DRM) captures as black.

## Decided elsewhere

Write access (Phase 3), unattended streaming (Phase 4), multi-user sharing (model allows paired
guest devices; expiry in Phase 4), headless hosts (Phase 3 service ADR when reached).
