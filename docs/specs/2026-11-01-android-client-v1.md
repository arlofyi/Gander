# Spec: Android client v1 (Browse, Preview, Fetch; Stream viewer shell)

- Status: Draft for implementation (Phase 1, WP-20..WP-29; Stream viewer WP-50..WP-54)
- Date: 2026-11-01
- Related: `2026-11-01-protocol-v1.md`, `2026-11-01-pairing-and-trust.md`, ADR-0001, ADR-0005

## Product shape

Home screen = **your computers** (hosts), each with online state and last seen. A host opens to its
**shares**; a share opens to a **browser** (list or grid, sort, filter, search); a file opens a
**file sheet** that shows what the file is, how big it is, where it is (cloud-only, cached locally)
and the three actions **Preview / Fetch / Stream**, with the default for this type pre-selected and
a one-line cost hint ("Preview: your PC renders pages", "Fetch: 1.8 GB over cellular",
"Stream: opens PowerPoint on studio-pc").

Honesty rules (vision principle 7): every unavailable action shows *why* it is unavailable in the
sheet itself ("Stream: studio-pc is locked", "Preview: no renderer for .blend on studio-pc, try
Stream"), using the protocol's capability and error codes, never a generic failure toast.

## Stack (ADR-0001)

Kotlin, Jetpack Compose, minSdk 26, targetSdk/compileSdk 36, AGP 8.5.1+, NDK r28+ (16 KB pages),
OkHttp 5.x, Media3, Coil, `org.jitsi:webrtc` 155.x, CameraX + ZXing core, DataStore + Keystore,
Room, coroutines/Flow, Compose Navigation. Package id `fyi.arlo.gander` (ADR-0004: not `com.gander.*`).
Manual DI through a single `AppGraph` object.

## Networking layer (`core/net`)

- One `OkHttpClient` per host, built with:
  - `sslSocketFactory(factory, GanderTrustManager(pinnedSpkiSha256))`: the trust manager accepts a
    chain only if the leaf certificate's SPKI SHA-256 equals the pin stored at pairing; it ignores
    the system trust store entirely. `hostnameVerifier` returns true only for this pinned
    connection (the host cert names a host id, not an IP).
  - Timeouts: connect 5 s, read 30 s (file streams use a separate client with no read timeout but
    a 60 s idle watchdog), WebSocket ping 20 s.
  - `Authorization: Bearer` interceptor; `X-Gander-Protocol: 1`; a `User-Agent` of
    `Gander-Android/<version>`.
  - Dispatcher limited to 8 concurrent requests per host (matches the host's concurrency limit).
- A `HostEndpoint` tries the host's addresses in order (IPv4 tailnet, IPv6 tailnet, MagicDNS
  name) and remembers the one that worked; it re-reads `/me.host.addresses` on every connect.
- No `DownloadManager`, no WebView, no system trust. Media3 reads files through an OkHttp
  `DataSource` on the pinned client, so video plays directly from the host with Range requests.
- Pairing scan: CameraX preview + ZXing decode of the `gander://pair` URI; manual entry screen as
  fallback. Pairing logic follows the pairing spec exactly, including the SAS display and the
  "if someone told you to do this, stop" text.
- `TRANSPORT_VPN` presence is shown as a hint on the home screen ("Private network: connected");
  which VPN it is cannot be determined (Tailscale exposes no API).
- Android 17 (`targetSdk` 37) local-network permission: declare `ACCESS_LOCAL_NETWORK` when the
  target is raised and request it only when the user types a LAN address; tailnet addresses over
  VPN are expected to be exempt but this is verified in the Phase 3 target bump.

## Data and cache (`core/data`)

- Room tables: `hosts` (id, name, addresses, pin, last_seen, capabilities JSON), `shares`,
  `entries` (per host+share+path listing snapshot with etag and fetched_at; used for offline
  display and instant back-navigation), `cached_files` (local path, size, etag, pinned, last_used),
  `thumb_cache` is Coil's disk cache keyed by host+share+etag+size.
- Token store: Keystore-wrapped AES key encrypting a DataStore blob; excluded from backup via
  `dataExtractionRules` (`cloud-backup` and `device-transfer` exclude), see pairing spec.
- Cache cap default 2 GiB, LRU; pinned files are exempt; the Storage screen shows usage per host.
- Offline: listings and cached files render with a "cached <relative time>" chip; actions that
  need the host are disabled with the reason.

## Screens

| Screen | Content | Notes |
| --- | --- | --- |
| Hosts | cards per host: name, OS icon, online/offline, relay warning, "paired as", kebab: rename, forget | `GET /v1/host` ping every 20 s while visible; pull to refresh |
| Add host | scan QR / enter address; progress; SAS screen; success | pairing spec flows A and B |
| Shares | list of shares with permission chips (preview/fetch/stream) | `shares.updated` refreshes |
| Browser | list/grid toggle, sort, kinds filter, search field, breadcrumb, multi-select (Fetch many), "Recent" tab at share root | paged by cursor; thumbnails via Coil with visible-first priority and cancellation on scroll |
| File sheet | name, size, dates, flags, metadata (`stat?meta=1`), Preview/Fetch/Stream buttons with hints and default | default per extension stored in DataStore; network-aware rule (metered -> Preview/Stream first) |
| Preview: pages | horizontal pager of page images with pinch zoom, page counter, "Fetch this file" shortcut | prefetch n+1; pages cached by etag+page+width |
| Preview: image | zoomable image at phone width, "View original (Fetch)" | |
| Preview: text | monospace viewer, encoding shown, load more | highlighting via a small Kotlin tokenizer for common languages; not a requirement |
| Preview: video | Media3 player with the poster first, streaming via Range | HEVC/AV1 depend on the device |
| Fetch | progress in the sheet and a notification; resume; verify option; done -> Open with / Share / Keep offline | foreground service type `dataSync`; survives `onTimeout` by persisting state and resuming on next launch |
| Stream (Phase 2) | video surface, mode toggle (Direct touch / Trackpad), keyboard bar, local text box, zoom, session state banner, end dialog | see Stream viewer |
| Settings | cellular rule, verify downloads, cache cap, app lock (biometric), default actions per type, audit of this device's hosts | |
| Host details | identity shown at pairing, token rotate, forget | |

## Browser behaviour details

- Sort: directories first; by name (natural order), modified, size, kind. Persisted per share.
- Search: debounced 300 ms, host-side `search` endpoint with `partial` indicator; results list
  shows paths.
- Thumbnails: request size 256 on phones, 512 on tablets; Coil fetcher maps `503 generating` to a
  retry after `retry_after_ms` up to 3 times, then shows the type glyph.
- Entry flags render as chips: cloud-only (cloud icon; fetch warns), link (greyed, not openable),
  hidden (only when the share shows hidden).
- Multi-select -> Fetch queue; a folder is never fetched by selecting it. The one way to take a
  folder is the explicit "Download folder as zip" in the open folder's overflow menu (WP-68,
  offered only when `capabilities.fetch.zip` is present and the share allows `fetch`): the app
  asks the host for `GET /v1/zip?dry_run=1`, shows "N items, X MB" (plus a warning for cloud-only
  files and a note for skipped links), a folder over the host's limits is refused in the host's own
  words (`413 too_large`), and only Download queues it. The zip is a fetch job like any other
  (cellular rule, notification, Downloads list, Save to device) with three differences: it reads
  `/v1/zip`, it is NOT resumable (no `Range`; a broken connection starts over from zero) and it is
  not verified (no hash of an archive built as it is sent). The job key is `zip:<folder path>`; a
  share path can never contain `:`, so it cannot collide with a file.
## Default-action rules (user-editable)

```
if host.capabilities.stream.enabled and ext in stream_only_types -> Stream
else if ext in {pdf, images, text, video, audio} -> Preview
else if phone has a handler for the MIME type and (not metered or size < 25 MiB) -> Fetch
else if preview available -> Preview
else -> Fetch (with size warning on metered)
```
`stream_only_types` ships with: blend, psd, ai, indd, dwg, dxf, skp, step, stp, f3d, kicad_pcb,
kicad_sch, aep, prproj, drp, als, flp, logicx, cpr, sln, ipynb, accdb, mpp, vsdx, max, ma, mb,
zpr, sbs, sbsar, uasset, unity, mat, slx, and any extension the host reports `preview: none`
for and the phone has no handler for.

## Fetch engine

- Single downloader per host with 2 parallel files, each a single sequential Range stream
  (parallel ranges of one file are not worth the complexity on a VPN).
- Persisted job state (Room): bytes done, etag; resume with `Range` + `If-Range`; `412
  file_changed` restarts from zero after telling the user.
- Verification: after completion, `GET hash?algo=blake3` (or sha256) when enabled; mismatch ->
  delete and offer retry.
- Open with: write into app-private storage, expose via `FileProvider`, `ACTION_VIEW` with the
  MIME type and `FLAG_GRANT_READ_URI_PERMISSION`, no chooser forced (the system resolver is the
  "Open with"). Share uses `ACTION_SEND` + `createChooser`.
- Cellular rule: before any fetch on a metered network above the threshold (default 50 MiB), a
  dialog states the size and offers Preview or Stream instead.

## Stream viewer (Phase 2)

- `SurfaceViewRenderer` from libwebrtc; aspect-fit with pinch-zoom and pan handled **locally**,
  persistent until reset (Moonlight's most requested gap).
- Modes (Windows App convention, labelled on screen, remembered per host):
  - **Direct touch**: tap = left click at the point; long-press = right click; two-finger drag =
    scroll; double-tap-hold-drag = drag.
  - **Trackpad**: one-finger move = pointer; tap = click; two-finger tap = right click; two-finger
    drag = scroll.
- Keyboard bar: sticky Ctrl/Alt/Shift, Esc, Tab, arrows, Fn row toggle, Ctrl+Z/Y, Ctrl+S, a
  "type" button opening a local text box that sends `text` messages (IME-friendly), and an
  on-screen keyboard button instead of a hidden multi-finger gesture.
- State banner from `stream.state`: launching, approving on host, paused (reason), reconnecting.
- Background: on `onStop` send `stream.pause`; on return `stream.resume`; the host keeps the
  session 120 s.
- End: "Leave PowerPoint open on studio-pc / Close it" per policy; "Fetch the saved file" shortcut
  when the host reports the file changed during the session.
- Relay warning: when `/me.network.relayed` is true, the sheet warns "connection is relayed
  through Tailscale, expect low quality" before starting.

## Files-app integration (Phase 3, designed now)

- `GanderDocumentsProvider` with roots per host, documents per share path; `openDocument` in read
  mode returns `StorageManager.openProxyFileDescriptor` backed by Range reads with 1 MiB
  read-ahead (the FUSE layer asks in 64-128 KiB chunks); write modes refused until write shares
  exist. Runs in the app process with the same pinned OkHttp client and token store.

## Security on the device

- Token and pins only in the encrypted store; `android:allowBackup` true but with extraction
  rules excluding the store; screenshots allowed (user's own content) except the pairing screen
  (`FLAG_SECURE` while the QR secret or SAS is visible).
- Optional biometric app lock (BiometricPrompt) gating the UI after 1 minute in background.
- No analytics, no crash reporting SDK; a local "Export diagnostics" produces a redacted log zip.

## Build, test, CI

- `./gradlew assembleDebug testDebugUnitTest lintDebug` must pass; release builds use a CI
  keystore (human-gated for Play).
- Unit tests: trust manager (accepts pinned, rejects other self-signed and system-trusted certs),
  pairing state machine, SAS derivation against the shared test vectors, default-action rules,
  cursor paging, downloader resume logic (MockWebServer with Range).
- Instrumented tests (emulator, Phase 1 gate): full flow against the real Rust host started on
  the Windows machine, reached from the emulator via `10.0.2.2`: pair (flow B against the host's
  pairing dialog driven by a test hook), list, thumb, preview page, fetch with resume, revoke.
- 16 KB check in CI: `check-elf-alignment` over every `.so` in the APK.

## Open items

- Tablet layouts (two-pane browser) are desirable but not required for v1.
- Localisation: strings in resources from day one; English only in v1.
- Accessibility: content descriptions on all icon buttons; TalkBack pass before release.
