# Landscape research

_Researched 2026-11-01. Sources are listed at the bottom. Much of the evidence is forum
comments and vendor pages, not formal studies, so treat it as direction, not proof._

## 1. Categories that exist today

| Category | Examples | What it gets right | Where it misses for this use |
| --- | --- | --- | --- |
| Full remote desktop | AnyDesk, TeamViewer, Chrome Remote Desktop, Splashtop, RustDesk | Can open anything; mature | Whole machine for one file; poor phone UX; a big target for scams and attackers |
| Low-latency streaming | Parsec, Moonlight + Sunshine | Crisp, fast, hardware encode | Built for games/full desktop; app list launches apps, not files; no file browsing |
| Private-network file share | Tailscale Taildrive (WebDAV), Taildrop | Uses tailnet identity; no extra creds | Alpha; phones can only consume; needs a 3rd-party WebDAV app; no previews; Windows WebDAV 4 GB cap |
| Self-hosted cloud | Nextcloud, Seafile, Immich (photos), Jellyfin/Plex (media) | Real mobile apps, thumbnails, offline cache | Heavy to run; files must live inside the server; nothing for "can't open on phone" |
| Sync | Syncthing, Resilio | Files are local, works offline | Copies ahead of time; trust drama around forks; Resilio closed source |
| Ad hoc transfer | LocalSend, PairDrop | Very simple, loved | Push-only from the other side; LAN only; no browsing |

**The gap:** nothing is *file-first* AND lets you *choose the device that does the work*. Remote
desktop is compute-on-host but screen-first; file servers are file-first but compute-on-phone only.

## 2. What users like

- **Zero extra accounts when the network already identifies you.** Taildrive's selling point is
  that Tailscale identity decides permissions. Taildrop: "get the app, enroll it, it's an on/off switch."
- **Simplicity of LocalSend.** Free, open source, no internet needed, background auto-accept.
  82k GitHub stars vs PairDrop's 10k largely on this.
- **Low latency and crispness** (Parsec, Moonlight). People forgive a lot for a sharp, fast stream.
- **Offline thumbnails / cache** (Immich). Opening the app away from home and seeing *something*
  instead of a blank screen was called out explicitly.
- **Open source and self-hostable** (RustDesk, Sunshine, LocalSend) as a trust signal.

## 3. What users dislike

- **Phone remote desktop ergonomics.** "Text is just too small to effectively work with." Touch is
  fine for scrolling, "precision is terrible"; users fall back to trackpad mode or a Bluetooth keyboard.
  Text entry needs special handling (type locally, paste into the remote).
- **Setup friction.** Chrome Remote Desktop: long connection setup, unwanted browser permissions.
  Moonlight/Sunshine: installer + PIN pairing + (historically) NVIDIA-only.
- **Missing or flaky file transfer** inside remote-desktop tools (Splashtop complaints; failures from
  version mismatch and policy).
- **Zipping and bundling** (PairDrop zips multi-file sends; users left for LocalSend).
- **Heaviness** (Nextcloud "pretty big"; past bad experiences).
- **Closed source / licence** concerns (Resilio).
- **Audio lag, multi-monitor awkwardness** (Parsec).

## 4. What security people say

- The dominant real-world risk of remote-access tools is **social engineering**: tech-support scammers
  get victims to install AnyDesk/TeamViewer, then drain accounts (FBI warnings, TeamViewer's own KB).
  AnyDesk is favoured by scammers because it is free, fast to install and needs no account on the
  controlled side.
- Remote-access tools are routinely used by **ransomware groups for persistence** (Blackpoint).
- Vendors themselves get breached: AnyDesk in early 2024 lost source code and code-signing material.
- Recommended controls everywhere: 2FA, device allowlists, never accept a connection someone else
  initiated, download only from official sources.

**Implications for us:** no "accept incoming session from a code" flow at all (the scam vector);
pairing only from a device the owner already controls on their own private network; host-visible
indicator; scoped access instead of whole-desktop; no relay we could lose control of.

## 5. Technical building blocks found

- **Per-window capture is available on every target OS:** Windows Graphics Capture (WGC) can target
  a single `HWND` (WebRTC's native capturer uses it, with an option to include secondary windows such
  as popups and tooltips); macOS ScreenCaptureKit `SCContentFilter` for a window; Linux via the
  xdg-desktop-portal ScreenCast + PipeWire (user picks the window).
- **No open-source project combines per-window capture with "open this file, stream that app" to a
  phone.** Sunshine streams the desktop or launches app commands; there is no file-centric flow.
  Closest prior art for app-only streaming: Microsoft RemoteApp (RDP), Windows 365 / AVD app streaming.
- **Tailscale gives us identity for free when present:** the LocalAPI `WhoIs` lookup maps a peer IP to
  user + node + capability grants. `tsnet` embeds a node in a Go program. Gotchas: tagged devices have
  no user identity; userspace networking breaks WhoIs.
- **Taildrive** shows WebDAV over the tailnet is viable but also what to avoid: phones cannot share,
  http-only breaks some clients, 4 GB Windows WebDAV default cap.

## 6. Should / should not (synthesis)

Should:
- Be file-first, with thumbnails and previews before anything heavy.
- Default Stream mode to **touch-native**: pinch-zoom on the streamed window, tap = click, two-finger
  = right click / scroll, a local text box that sends keystrokes, a toolbar for common shortcuts.
- Adapt the default mode to the network (cellular: Preview/Stream; Wi-Fi: Fetch OK).
- Work offline for anything already opened (cache with a size cap).
- Use the network's identity when it has one, and still require explicit pairing.
- Be small and quick to install. One host binary with a tray UI.

Should not:
- Offer full-desktop control (scope creep, risk, and the thing remote-desktop tools already do).
- Accept connections initiated by a code read out over the phone.
- Run our own relay or require an account.
- Execute files. Stream mode only opens files with known desktop apps.
- Zip things the user did not ask to zip.

## Sources

- Tailscale Taildrive docs: https://tailscale.com/kb/1369/taildrive
- Tailscale identity / WhoIs: https://tailscale.com/docs/concepts/tailscale-identity , https://tailscale.com/blog/building-tsidp , https://forum.tailscale.com/t/utilising-whois-to-authenticate-users/805
- tsnet escape hatch write-up: https://4720d05c.transscendsurvival-org.pages.dev/blog/aperture-tagged-devices-and-the-tsnet-escape-hatch
- Taildrop overview: https://blog.lon.tv/2025/01/19/taildrop-simple-device-to-device-file-sharing-via-tailscale/
- Tailscale review: https://privacysavvy.com/reviews/vpn/tailscale
- Sync tool threads: https://lemmy.klein.ruhr/post/240179 , https://piefed.jeena.net/post/371412
- Moonlight/Parsec/RDP comparison: https://superrendersfarm.com/ja/article/moonlight-parsec-rdp-remote-desktop-gpu-rendering-2026
- Parsec reviews: https://www.techjockey.com/us/reviews/parsec
- RustDesk vs Parsec: https://toolradar.com/compare/rustdesk-vs-parsec
- Remote desktop forum thread: https://forum.quartertothree.com/t/remote-desktop-solutions-2022-2023/157456
- Sunshine usage docs: https://docs.lizardbyte.dev/projects/sunshine/v0.19.0/about/usage.html
- Sunshine + Moonlight + Tailscale as AnyDesk alternative: https://dev.to/thevenice/how-i-built-a-free-anydesk-alternative-using-sunshine-moonlight-tailscale-3lh8
- TeamViewer and scamming: https://www.teamviewer.com/en/global/support/knowledge-base/teamviewer-remote/security/teamviewer-and-scamming/
- RustDesk on AnyDesk safety (competitor source): https://rustdesk.com/blog/is-anydesk-safe
- Blackpoint on TeamViewer/AnyDesk in incidents: https://blackpointcyber.com/blog/blackpoint-tackles-teamviewer-and-anydesk-threats-in-government-and-industrial-incidents/
- Splashtop vs Chrome Remote Desktop: https://www.capterra.com/compare/162972-231371/Splashtop-Business-Access-vs-Chrome-Remote-Desktop , https://www.peerspot.com/products/splashtop-remote-support-reviews
- Phone remote desktop UX: https://www.macworld.com/article/1146710/pocketcloud.html , https://aux.engineering.ucsc.edu/publications/Su-Rajicon-MOBICOM02.pdf
- Immich offline cache: https://github.com/immich-app/immich/discussions/8107
- LocalSend vs PairDrop: https://openalternative.co/compare/localsend/vs/pairdrop , https://www.notebookcheck.net/This-open-source-tool-moves-data-between-my-devices-Android-Mac-Windows.1166528.0.html
- WGC window capture in WebRTC: https://webrtc.googlesource.com/src/+/412ea40eeb9971c6ca11e8f8789ad4d42e195710
- Pion: stream a Windows window: https://github.com/pion/webrtc/discussions/1637
- WebRTC remote desktop with WGC: https://github.com/DanielChrobak/WebRTC-Remote-Desktop

## Update 2026-11-01

The gaps listed below were researched in `2026-11-01-plan-review.md` (section 4). Two additional
prior-art projects were found, both from 2026 and both tiny: **Baton** (github.com/TheTobiTiger02/Baton,
C#/.NET WPF + Kotlin, no licence shown: WGC window streaming with Media Foundation H.264 over
pinned TLS, QR-pinned pairing, read-only shares with path hardening, fetch only, no preview,
cannot work while locked) and **FileApex** (github.com/byhowiecreations/FileApex, MIT: browse,
send and sync with embedded tsnet, QR or code pairing, no preview or streaming). Neither closes
the gap; Baton is worth reading for ideas only. Sunshine's maintainers confirmed in 2026 that it
cannot capture a single window and that WGC does not work from its service model.

## Gaps researched (resolved in the plan review)

- Microsoft RemoteApp / Windows App streaming UX on phones: gesture model adopted (direct touch
  vs pointer modes, two-finger tap = right click, two-finger drag = scroll, local pinch).
- iOS File Provider and Android SAF: Android works on demand via proxy file descriptors; iOS is
  whole-file on iPhone.
- WGC when locked or display off: not meant to capture the secure desktop; sleeping monitors
  yield no frames; static windows yield no frames.
- Apple 4.2.7: applies to "mirror of specific software"; LAN wording; enforcement on VPN clients unknown.
