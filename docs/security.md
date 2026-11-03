# Security and threat model

Updated 2026-11-01 after the plan review (`research/2026-11-01-plan-review.md`). The product is,
by definition, a way into someone's computer. Remote-access tools are among the most abused
software there is (tech-support scams, ransomware persistence, vendor breaches, and since 2025
device-linking and device-code phishing). The design goal is that a compromise of any one layer
does not expose the machine.

## Assets

1. Files on the host (confidentiality; integrity once write exists).
2. The ability to launch apps and send input on the host (Stream mode): the most dangerous asset,
   because a streamed app's own dialogs reach the whole filesystem.
3. Device tokens, pairing secrets, the host private key.
4. The owner's awareness: knowing when and by whom the machine is accessed.

## Adversaries

| Adversary | Example | Primary defence |
| --- | --- | --- |
| Internet attacker | port scans, exploit attempts | Never bound to a public interface; loopback + private-network addresses only; firewall rule scoped to tailnet ranges |
| Another device on the private network | a shared-in node, a tagged server, a compromised IoT device, a family member's phone | Pairing + per-device token + per-device shares; Tailscale identity recorded at pairing and re-checked; tagged peers refused by default |
| Man in the middle on the private network | compromised peer, rogue subnet router | TLS pinned to the host SPKI from the QR; SAS code for manual pairing; DTLS fingerprints over the pinned channel |
| Local web page on the host or phone | DNS rebinding, CSRF against localhost | Browser-marked requests refused, `Host` checked, bearer header required, GET never mutates |
| Lost/stolen phone | | Revoke from host; token in Keystore-backed storage excluded from backups; optional biometric lock |
| Scammer social-engineering the owner | "install this and read me the code"; "scan this QR"; "approve the prompt" | Pairing exists only while the owner opens the dialog; approval on host names the device; explicit warning text; one pending request; rate limits |
| Malicious file in a share | macro doc, script, `.lnk`, crafted PDF/PNG | Stream launch policy by handler executable; renderers in isolated workers with caps |
| Compromised or malicious client with a valid token | | Permissions per share and device; Stream off by default; audit log; kill switch; identity re-check |
| Path tricks | `..`, symlinks, junctions, ADS, 8.3 names, trailing dots, reserved names, case games | Per-segment validation; handle-relative opens; reparse-tag policy; short-name check; final-path check |
| Vendor-side compromise of us | stolen signing material (AnyDesk 2024) | Signed releases, SBOM, reproducible build notes, disclosure process; no auto-update without consent |

## Non-negotiables

1. **Bind narrowly.** Loopback + private-network addresses only. Never `0.0.0.0`/`::`, including
   WebRTC sockets (explicit addresses, no wildcard expansion). If the private network is down, the
   non-loopback listener is closed. Tests assert the plan never contains a wildcard.
2. **Two gates, always.** Private-network reachability AND a paired device token. Where the
   network offers identity (Tailscale WhoIs), it must match the identity recorded at pairing;
   tagged nodes (`tagged-devices`) are refused unless the owner opts in; LAN and loopback sources
   have no identity and rely on the token alone.
3. **Pairing is approved on the host**, only while the owner's "Add a device" dialog is open, with
   a short code shown on both screens. QR secrets are 256-bit, single-use, expire in 5 minutes.
   Five failed attempts per source per window. One pending request at a time. The dialog says:
   "If someone asked you to do this, click Deny."
4. **Deny by default.** No shares exist until the owner creates one. Read-only in v1. Stream is a
   separate per-share, per-device permission, off by default, and the UI that grants it says that
   the streamed app's own dialogs can reach other folders.
5. **Transport encryption is ours, not only the VPN's.** TLS 1.3 with a host certificate pinned at
   pairing; the system trust store is never consulted for Gander hosts; DTLS fingerprints travel
   only over that channel.
6. **Stream mode never executes files.** It resolves the handler executable for the file type
   without executing anything, refuses a never-list of interpreters and shells (`cmd`,
   `powershell`, `wscript`, `mshta`, `rundll32`, `python`, `java`, `node`, ...), hard-refuses
   executable/script/installer/shortcut/macro-enabled extensions by final extension (full list in
   the stream-launch spec), launches only apps in an owner-approved catalogue (first use prompts
   on the host), and opens browser-handled files in an isolated browser window and profile.
7. **Visibility on the host.** Tray icon changes state while any device is connected and again
   while streaming; a toast on session start naming the device and the file; the OS capture border
   stays on by default; a kill switch drops every session and optionally pauses the listener.
8. **Audit log** on the host: device, action, path, decision path, time. Local only, rotated,
   never containing secrets or file contents. Viewable in the tray UI.
9. **Secrets hygiene.** Tokens stored hashed (SHA-256 of 256-bit random values); plaintext shown
   once; host private key under a user-only ACL with DPAPI; types holding secrets redact
   themselves in debug output; QR payloads never logged; the phone excludes its token store from
   backups and uses `FLAG_SECURE` on pairing screens.
10. **No relay, no telemetry by default.** Gander runs no servers. The private network itself may
    relay (Tailscale DERP, end-to-end encrypted); the client shows when a path is relayed. Update
    checks and diagnostics export are explicit, opt-in actions.
11. **Untrusted bytes are parsed out of process.** Thumbnails, previews and metadata run in a
    worker under a job object (memory, CPU time, no children), receiving a file handle rather
    than a path, killed on timeout. The host process links no image or PDF decoder.
12. **Input is injected only into the streamed window.** Injection happens only while that window
    is the foreground window on the host; otherwise it pauses and tells the phone why. Local user
    activity pauses injection.
13. **Clipboard text is opt-in, one direction at a time, and gated like a click.** Off by default
    (`[stream] clipboard`); text only, 64 KiB of UTF-8, never truncated. The clipboard is read or
    written only while the stream passes the input gate (session unlocked by the same fail-closed
    lock probe, phone looking, window in front, nobody using the computer's own input), and the
    setting is re-read on every request, so switching it off applies at once. The phone only
    receives text it asked for. Every request is audited by direction and size (never content),
    the computer shows a notification when text is read or written, and nothing about the content
    is logged.
14. **Stream sound comes only from the streamed application.** It is captured from that
    application's own process tree (WASAPI process loopback); the system mix and other applications
    are never captured, and there is no fallback when process loopback is unavailable. Sound
    follows the stream's approval and visibility (the being-viewed indicators say "with sound"), is
    captured only while the stream is live and unmuted, and `[stream] audio = false` disables it.

## Residual risks, stated

- **Stream is app-level access.** Once an app is streaming, its Open/Save dialogs expose the
  filesystem to the phone. v1 does not sandbox desktop apps. This is why Stream is a trusted-device
  permission and why the catalogue and audit log exist.
- **A valid token is powerful.** It can read every file in every share granted to that device and,
  if Stream is granted, open files in approved apps. Revocation is immediate but requires the
  owner to notice; the device list and toasts exist for that.
- **Relayed traffic transits Tailscale's servers** (encrypted end to end by WireGuard and again by
  our TLS). Users who object can use peer relays or direct paths; we surface which one is in use.
- **The clipboard can hold secrets.** With `clipboard` on, a paired device with Stream access can
  read whatever text is on the computer's clipboard (a password just copied) while a stream is
  active, and can replace it (a later paste on the computer pastes the phone's text). That is why
  it is off by default, split by direction, gated like input, audited and announced with a
  notification. Other clipboard formats (images, files, rich text) are never touched.
- **Stream sound covers the whole process tree.** Other windows of the same application process
  (for example a second document open in the same PowerPoint) are heard too.
- **Windows does not hide a locked session from capture or posted input.** Measured on build 26200:
  Windows Graphics Capture keeps delivering a window's content and posted window messages are
  still delivered while the session is locked. The host enforces the unlocked-only rule itself
  (see below) and fails closed.
- **Protected content** (DRM) captures black; this is a limitation, not a leak.
- **UIPI**: elevated windows cannot receive injected input from the unelevated host; they also
  cannot be launched by it. Documented, not worked around.

## Unattended streaming (open decision, Phase 4)

Streaming while nobody is at the computer (locked session) is the most useful and the most risky
feature. It requires either a virtual display plus an unlocked session, or a separate session
(RDP RemoteApp/RAIL). Windows does not enforce this: Windows Graphics Capture keeps delivering a
window's content, and posted window messages are still delivered, while the session is locked
(measured on build 26200, stream spike findings). The host therefore enforces it itself: it tracks
the session lock state, treats an unknown state as locked, refuses new Stream sessions and ends
running ones when the session locks. v1 and v2: **Stream only works on an unlocked session**;
Preview and Fetch work regardless. Revisit with a dedicated spec and threat review.

## Testing obligations

- Unit tests for path resolution with the hostile corpus (`protocol/test-vectors/paths.json`):
  traversal, symlink, junction, ADS, 8.3, trailing dots/spaces, reserved names, unicode variants,
  case variants, cloud placeholders allowed; a fuzz target proving the final-path check never fires.
- Tests that the listener plan never includes a wildcard address, for any interface table.
- Tests that the Stream policy cannot be bypassed by double extensions or case (`a.PDF.exe`), that
  every never-list handler is refused regardless of extension, and that browsers route to the
  isolated template.
- Pairing tests: MITM yields a different SAS; closed window 404; rate limit; revocation closes
  sockets; identity mismatch refused.
- Renderer tests: malformed PDF and decompression-bomb PNG are contained by the worker caps.
- Client tests: the trust manager rejects system-trusted and other self-signed certificates.
- External review before the first public release; SECURITY.md with a disclosure address.
