# Findings: Files-app spike (WP-47)

- Status: Measured on one emulator image (below); loopback HTTP only, no real network, no physical device
- Date: 2026-11-01 (exploration that evening; the committed run executed 2026-11-02 06:17-06:25 local)
- Feeds: `2026-11-01-android-client-v1.md` section "Files-app integration" (Phase 3, WP-60)
- Code and data: `spikes/files-app/` (throwaway Gradle project, package `fyi.arlo.gander.spike.files`).
  Raw logs `spikes/files-app/results/logs/NN-<run>.txt` (`.txt`, not `.log`: the repo ignores `*.log`), tables `spikes/files-app/results/analysis.md`,
  device facts `spikes/files-app/results/device.txt`, screens `spikes/files-app/results/screens/`

## What was measured, and where

| Item | Value |
| --- | --- |
| Emulator | Android emulator 37.2.13.0 (build 16740729), AVD `gander` (pixel_6 profile, 1080x2400), WHPX, headless: `-no-window -no-audio -no-snapshot-save -no-snapshot-load -gpu swiftshader_indirect -port 5554`; cold boot to `sys.boot_completed` in about 25 s |
| System image | `system-images;android-36;google_apis;x86_64`, revision 7 ("Google APIs Intel x86_64 Atom System Image") |
| Build | `google/sdk_gphone64_x86_64/emu64xa:16/BE2A.250530.026.F3/13894323:userdebug/dev-keys`, Android 16, **API 36**, security patch 2025-07-05, ABI x86_64 |
| Kernel | 6.6.66-android15-8-gd0c43a640eab-ab13812146 |
| Files app | `com.google.android.documentsui`, system priv-app `DocumentsUIGoogle.apk`, versionName 16 |
| Video handler | `video/mp4` ACTION_VIEW resolves to exactly one activity on this image: Google Photos 7.8.0.697873850 (`com.google.android.apps.photos/.pager.HostPhotoPagerActivity`). No account, no internet on the emulator |
| Spike app | AGP 9.5.0, Kotlin 2.4.21, compileSdk/targetSdk 36, minSdk 26, no dependencies (versions read from `client/android/gradle/libs.versions.toml`) |
| Test media | generated at run time by ffmpeg 9.0.1, not committed: `sample-faststart.mp4` (moov at offset 40) and `sample-moovend.mp4` (moov at 30,775,289), both 30,811,338 bytes, 1280x720 H.264 Main 30 fps + AAC, 40 s, about 6.2 Mbit/s |

How the provider is built: `SpikeDocumentsProvider` (one root, a flat directory of `*.mp4`, all documents
flag 0 = read-only) starts an HTTP/1.1 server inside the app on **127.0.0.1 only** (ephemeral port; every
`HTTP_LISTEN` line in the logs says `addr=127.0.0.1`) that serves `GET|HEAD /f/<name>` with single-range
`Range:` support and keep-alive. Sizes come from `HEAD`. `openDocument` (mode `r` only) returns
`StorageManager.openProxyFileDescriptor(MODE_READ_ONLY, callback, handler)`; every `onRead(offset, size,
data)` does one HTTP Range request (`bytes=offset-offset+size-1`) with `HttpURLConnection` and logs
offset, requested size, bytes returned and the callback time. Optional per-run knobs (a `spike.conf` file):
a one-block read-ahead, an artificial server delay, and shared versus per-fd handler thread.

Clients driven: DocumentsUI (browse, tap-to-open, "Copy to... Downloads", "Get info"), Google Photos (reached
through the Files app tap), the platform `MediaPlayer` through a `VideoView` in the spike app (the engine
Photos uses: its `dumpsys media.player` shows NuPlayer), and a controlled probe in the spike app
(`ProbeActivity`: sequential and random reads with chosen sizes). All of it was driven by
`spikes/files-app/scripts/Invoke-FilesAppSpike.ps1` (adb, `uiautomator dump`, `input`).

## Answers

| Question (WP-47 Verify) | Result |
| --- | --- |
| Does the emulator Files app open the document? | Yes. DocumentsUI lists the root "Gander Spike" and both files with correct sizes ("30.81 MB", `screens/files-app-listing.png`). A tap sends ACTION_VIEW with a URI grant to the only `video/mp4` handler (Photos), which opened the document 3 times and played it from the proxy file descriptor in 3 of the 4 launches of the final run (`screens/files-open-moovend-try1.jpg`). The fourth launch, the first Photos launch after boot, stopped after prepare (below). "Copy to... Downloads" copied the whole file: 30,811,338 bytes in `/sdcard/Download`, equal to the source |
| Read chunk sizes recorded? | Yes, every `onRead` offset, size, return and time, per fd (table below). Maximum request is **131,072 bytes** (also the size of the `data` array handed to the callback); playback asks for much less |
| Threading and timeouts | Callbacks run on the handler thread passed to `openProxyFileDescriptor`, one at a time per fd (`max_in_flight` never above 1 over the 26 fds of the committed run; 8 of them read nothing). A shared handler thread lets one slow read stall every other fd and even `openDocument`. A read held for 20 s and for 75 s completed normally; no platform timeout was hit (details below) |

## Read pattern table

"Sequential" counts reads whose offset equals the end of the previous read on the same fd; "forward" and
"backward" are the other two cases. "Unique" is the union of the byte ranges the provider was asked for.
Every value is from `results/analysis.md` (final run) unless marked.

| Client and scenario | fds opened | onRead calls | Sizes asked | Order | Bytes asked vs file (30,811,338) |
| --- | --- | --- | --- | --- | --- |
| DocumentsUI browse (list root and children) | 0 | 0 | - | queries only: `queryRoots` x2, `queryDocument(root)`, `queryChildDocuments` x1 | 0 |
| DocumentsUI "Get info" on a document | 0 | 0 | - | queries only (`queryDocument` x3, `queryChildDocuments` x2) | 0 |
| DocumentsUI "Copy to... Downloads" | 1, with a `CancellationSignal` | 236 | 235 x 131,072, then 12,288 (returned 9,418 at EOF) | 236 sequential, 0 jumps | 30,811,338 = 100.0 %, 0 % re-read; 764 ms on loopback |
| Photos, tap from Files app, moov at start, played (try 2) | 3 (see below) | 1,234 on the playing fd | 4,096 x348, 8,192 x325, 20,480 x203, 16,384 x193, 12,288 x72, 24,576 x39, 28,672 x15, 131,072 x15; median 8 KiB | 94 sequential, 1,131 forward, 9 backward. Forward gaps: median 8 KiB, p90 20 KiB. Backward jumps of 1.6 to 2.2 MiB about every 2 s of playback (7 seen) | 16.16 MiB = 55.0 % of the file in about 21 s (about 0.75 MiB/s, the video bit rate) |
| Photos, moov at end, played (try 1; try 2 alike) | 3 | 1,280 (try 2: 1,253) | same mix; median 8 KiB | start: 5 reads 16K, 32K, 64K, 128K, 128K at offsets 0 to 376,832, then **jump forward 28.99 MiB** to the tail: 4,096 + 16,384 + 20,480 (returned 17,610 at EOF) at 30,773,248, then **jump back** to 409,600 and continue as above; 100 sequential, 1,170 forward, 10 backward | 17.17 MiB = 58.4 % of the file in about 23 s |
| Photos, first launch after boot, moov at start (stalled) | 3 | 33 | 16K, 32K, 64K, 128K, 128K, then 4 to 36 KiB | 9 sequential, 23 forward (gaps up to 36 KiB), 1 backward | 904 KiB = 3.0 %; then no more reads for the rest of the 16 s window. `dumpsys media.player`: NuPlayer state 4 (prepared), `renderer(null)`, UI spinner. The provider was idle (nothing in flight). See "Photos stalls" |
| Platform `MediaPlayer` (own app, `VideoView`), moov at start, seeks to 50 %, 90 %, 10 % | 1 | 1,043 in 17.1 s (61 per second) | 4,096 x321, 8,192 x317, 16,384 x144, 20,480 x121, 12,288 x42, 24,576 x38, 131,072 x17; median 8 KiB | 66 sequential, 967 forward, 10 backward; 10 jumps of 1 MiB or more | 13.79 MiB asked, 46.9 % of the file unique, 0 % re-read; 1,043 HTTP requests (one per onRead) |
| Same, moov at end | 1 | 993 | same mix | same start as Photos (head, tail, head); 914 forward, 11 backward | 13.48 MiB, 45.9 % unique |
| Controlled: sequential `read()`, client buffer 4 KiB, 64 KiB, 1 MiB, 8 MiB (one fd each) | 4 | 238, 236, 236, 236 | 235 or 234 x 131,072 for every buffer size; with the 4 KiB buffer the first reads ramp 16,384, 28,672, 32,768, 65,536 before 131,072 | sequential | 100 % each; a 7,523-call 4 KiB client costs the same 238 provider reads |
| Controlled: random `pread` 200 x 4 KiB | 1 | 198 | 8,192 x192, 4,096 x4, 12,288 x2 | 1 sequential, 96 forward, 101 backward | 1.54 MiB (a 4 KiB pread costs 8 KiB) |
| Controlled: random `pread` 100 x 64 KiB | 1 | 99 | 69,632 x80 (64 KiB + one page), rest 4 to 131 KiB | random | 6.34 MiB |
| Controlled: random `pread` 50 x 1 MiB | 1 | 200 | 131,072 x186 | 168 sequential (the chunks inside one pread) | 23.88 MiB asked of 50 MiB read by the client |

### What the table says

- **Chunk size is set by the client's access pattern, not by the FUSE limit.** 128 KiB (the limit) only
  appears for bulk sequential reads (copy, large `read()` buffers). Playback asks for 4 to 28 KiB (median
  8 KiB) about 60 times a second, with small forward gaps (8 to 20 KiB, the interleaved audio chunk in
  between) and a backward jump of 1.1 to 2.2 MiB roughly every 2 s (two demux readers, audio and video,
  sit about 2 s apart in the file). Random small reads are rounded up by about one page: 4 KiB becomes
  8 KiB, 64 KiB becomes 68 KiB.
- **The kernel caches per fd.** Re-read share is 0 % in every run: a range read once on an open fd never
  reaches the callback again (50 x 1 MiB random preads over a 29.4 MiB file asked the provider for 23.88
  MiB, 81 % unique coverage, no repeats). A new fd starts cold: each of the four sequential probes re-read
  the whole file.
- **Total bytes versus file size.** A full sequential consumer (copy) reads exactly the file size once.
  Players read about their played time x bit rate (55 to 58 % of the file after 21 to 23 s of a 40 s
  clip), plus what each seek triggers (a random Range request, then a ramp of 64 to 128 KiB reads, then the
  steady 4 to 28 KiB pattern). Nothing prefetched far ahead: Photos and MediaPlayer are paced by playback,
  not by the provider.
- **Seek behaviour** (own app, platform MediaPlayer, loopback, from `analysis.md` "Seek behaviour"):

  | Seek (from) | Where the reads land | Size of first reads | Seek completed after |
  | --- | --- | --- | --- |
  | to 20 s (4.5 s) | offset 15,171,584 (49.2 % of file), forward jump 10.46 MiB | 69,632, 65,536, 131,072, 131,072, then 4 to 28 KiB | 186 ms |
  | to 36 s (23.4 s) | the old position keeps being read for 66 more reads, then offset 27,238,400 (88.4 %), forward jump 6.00 MiB | small reads | 351 ms |
  | to 4 s (38.1 s) | offset 4,210,688 (13.7 %), backward jump 24.59 MiB | 24,576, 24,576, 20,480, 20,480 | 146 ms |

  The moov-at-end file lands at 15,134,720 / 27,201,536 / 4,046,848 (same shape). A seek is one random
  Range request followed by a short ramp of 64 to 128 KiB reads.
- **moov at the end costs almost nothing here**: three reads at EOF (38,090 bytes returned), one 29 MiB
  forward jump and one back. It matters only for how a read-ahead window reacts (below).
- **How many opens per tap**: Photos opened the document 3 times for one tap: two opens that read nothing
  (`GETSIZE` then release, open for 11 to 64 ms in the four launches) and one that plays. It called `queryDocument` 11 times in
  between. DocumentsUI passed a `CancellationSignal` on the copy open; Photos and the same-app resolver
  passed none. `onGetSize` ran exactly once per fd, before `openDocument` returned. `onWrite` and
  `onFsync` were never called.

## Read-ahead (spec: "1 MiB read-ahead")

The spike can put one 1 MiB block window in front of the HTTP request (a read inside the window is
answered from memory; a read outside refetches 1 MiB at the new offset).

| Run | onRead calls | HTTP requests | HTTP bytes | Bytes the client used |
| --- | --- | --- | --- | --- |
| Sequential 1 MiB client buffer, no read-ahead | 236 | 236 | 30,811,338 | 30,811,338 |
| Sequential 1 MiB client buffer, 1 MiB window | 236 | **30** | 30,811,338 (1.00x) | 30,811,338 |
| Platform MediaPlayer, seeks, no read-ahead | 1,043 | 1,043 | 14,454,784 | about the same |
| Platform MediaPlayer, seeks, 1 MiB window | 1,128 | **36** | **36,984,010** | 15,271,114 (**2.42x** over-fetch, 120 % of the file) |

A single window is right for bulk sequential reads and wrong for playback: the audio and video readers
alternate between two positions about 2 MiB apart and every seek discards the window, so it refetches 1
MiB blocks all the time. Without any read-ahead one HTTP request per `onRead` means about 60 sequential
round trips per second of playback; at tailnet latencies that cannot keep up (loopback was 1 to 5 ms per
read here, so this run cannot show the failure, only the request count).

## Threading and timeouts

| Test | Result |
| --- | --- |
| Which thread runs callbacks | The handler passed to `openProxyFileDescriptor` (log thread `spike-proxy-fd` for the shared `HandlerThread`, `spike-proxy-fd-<n>` for a thread per fd). The two-argument overload `openProxyFileDescriptor(mode, callback)` is hidden API (does not compile against the SDK), so a handler is required |
| Concurrency inside one fd | None: `max_in_flight` never above 1 on any fd, including the player with two demux readers. Reads on one fd are served strictly one after another |
| Shared handler, one slow fd (t0 read held 5 s by the server, t1 starts 1 s later) | t1's `openDocument` blocked **4.07 s** (`OPEN` 06:21:54.010, `OPEN_OK` 06:21:58.081): `openProxyFileDescriptor` waits for `onGetSize` to run on the callback thread, which was busy. t1's 1 MiB read finished after **9,115 ms** |
| Handler per fd, same test | t1's open took 23 ms and its 1 MiB read **84 ms** total; t0 still took 10,038 ms (two reads of 5 s each: the kernel issued a 68 KiB read and then a 128 KiB read for two client `read()` calls) |
| Read held 20 s | Completed normally: `cb_ms` 20,004 and 20,006 for the two reads, no error |
| Read held 75 s (past a 60 s limit someone might expect) | Completed normally: `cb_ms` 75,003 for each of two reads, 150 s on one fd, no error, no kill, no timeout from the FUSE bridge or the framework. The client thread simply stayed blocked in `read()` |
| Not tested | A client reading on its main thread while the callback blocks (expected ANR after 5 s), cancellation (`CancellationSignal` was supplied by DocumentsUI on copy and never fired), HTTP errors surfacing as `ErrnoException(EIO)` (the code path exists, no run exercised it) |

## Photos stalls

Across the whole session Google Photos reached "prepared" and then never started playback on 6 of 10
launches (2 manual runs, 8 scripted launches over three driver runs; only the last driver run is
committed). In the committed run it was the first Photos launch after boot (`sample-faststart.mp4`,
try 1); the three later launches played. The stall is not provider I/O: NuPlayer state was 4 (prepared),
the proxy fd was idle, and the screen showed a spinner (a "Viewing full screen" tip appeared the very first
time). The emulator has no Google account and no internet, so a dependency of the Photos viewer is the
likely cause, but that was not established. Treat "Photos on a bare emulator" as a flaky client; the same
platform MediaPlayer driven from the spike app never stalled in any run.

## Implications for WP-60 (`GanderDocumentsProvider`)

1. **Do not return one fd with one HTTP request per `onRead`.** Playback is about 60 reads a second, median
   8 KiB. Put a block cache in front of the host: several blocks per fd (at least two live windows, one
   per demux reader, more for headroom), block size independent of the request size, sequential detection
   for prefetch, prefetch on a worker so the single callback thread is not the fetcher. Drop it on
   `onRelease`. A single 1 MiB window gave 2.42x over-fetch on playback.
2. **Use a handler thread per open fd, never one shared thread.** `openProxyFileDescriptor` blocks the
   `openDocument` binder call until `onGetSize` runs on that thread (4 s measured), so a shared thread
   turns one slow host into every document hanging, and ties up binder threads. Quit the thread in
   `onRelease`.
3. **Keep `openDocument`, `queryDocument` and `onGetSize` free of network.** One tap in Photos made 3
   opens (2 never read) and 11 `queryDocument` calls. Answer sizes and metadata from the listing cache;
   fetch bytes on the first `onRead` only (an eager read-ahead at open would be wasted twice per tap).
4. **Size block fetches for the first read after a jump.** After a seek, or for a moov-at-end file's tail,
   the first read is 4 to 20 KiB at an unrelated offset. Fetch the requested range (grow on sequential
   reads) rather than a full block at the new offset.
5. **Bulk readers are easy, players are the hard case.** Copy and large reads arrive as 128 KiB sequential
   reads (one request per 8 reads with a 1 MiB window, no over-fetch). `data.size` is 131,072; never
   assume `size` is a round number (4,096 to 28,672 is normal, 69,632 and 9,418 at EOF happen).
6. **Fail fast and honour cancellation.** The platform does not time a read out (75 s measured), so a
   hung host hangs the client thread. Give each host request its own timeout (the client's transfer
   client has 60 s idle; a callback wants less, tens of seconds) and throw `ErrnoException(EIO)`; check
   the `CancellationSignal` DocumentsUI passes on copy.
7. **Per-fd page cache, no cross-fd cache.** Re-reads within an fd never reach the callback, but a
   re-open is cold (the copy and Photos both start fresh). If a file was already fetched into the app's
   cache (Fetch mode), serve it with a real file descriptor instead of a proxy one.
8. **Host side:** the host answered small Range requests in 1 to 5 ms only after the server wrote header
   and body in one segment with `TCP_NODELAY`. Before that fix (an exploratory run, not committed) every
   small response took 42 to 50 ms (Nagle plus delayed ACK). Set `TCP_NODELAY` on the Rust host's
   sockets and write the head and a small body together.
9. **Test harness:** `adb shell content read` against the provider fails with `SecurityException`
   (`MANAGE_DOCUMENTS` is required; only DocumentsUI, same-app code, or an app holding a URI grant can
   open it). WP-60's tests need to run in-app (as `ProbeActivity` does) or drive DocumentsUI with
   `uiautomator` (`scripts/SpikeLib.ps1` has the helpers and the exact UI path for tap, copy and info).
10. **Document flags:** with `COLUMN_FLAGS` 0 the Files app offered only Share, Open with, Sort by, Select
    all, Copy to..., Get info (no delete, rename or move), which matches the "write modes refused" plan
    (seen while exploring the UI by hand; the scripted run only exercises Copy to and Get info).

## Where this differs from the spec

- `2026-11-01-android-client-v1.md` ("Files-app integration") says the FUSE layer asks in 64-128 KiB
  chunks and plans a 1 MiB read-ahead. Measured: 64-128 KiB holds only for bulk sequential reads;
  playback asks 4 to 28 KiB and random small reads 8 KiB. The 1 MiB read-ahead as one window is not enough
  for playback (2.42x over-fetch, see above); it needs several windows. The spec is also silent on the
  handler thread, which turned out to matter (4 s open stall with a shared one).
- The master plan WP-47 line says to log what "the Files app and a video player" request. The Files app
  itself never reads file bytes to open a document (it only queries) and hands the stream to the VIEW
  handler; of the actions tried it read bytes only for copy. Its thumbnail path (`openDocumentThumbnail`,
  `FLAG_SUPPORTS_THUMBNAIL`) was not enabled in the spike.
- `CLAUDE.md` Commands says to stop the Gradle daemon with `./gradlew --stop` after a build. With several
  workers sharing one `GRADLE_USER_HOME` that stops other workers' daemons; this spike always ran
  `--no-daemon`.

## Not observed (needs something this setup cannot provide)

- Real network latency (loopback HTTP, 1 to 5 ms per read; the over-fetch and request-count results hold,
  the "can it keep up" question needs a tailnet).
- Media3/ExoPlayer or other players (VLC, MX Player): Photos and the platform MediaPlayer only. ExoPlayer
  has its own buffering and may read differently from NuPlayer; repeat the table for the players that
  matter before sizing the block cache.
- A physical device, other Android versions, OEM Files apps (Samsung My Files and similar), Android 17.
- The thumbnail path, search, recents, write modes, `onFsync`, a file that changes while open, files
  larger than 4 GiB, TLS with the pinned OkHttp client (the spike used cleartext on 127.0.0.1 through a
  network security config scoped to that address).
- Why Photos stalls on some launches.
- A client main thread blocked by a slow read (ANR), and `CancellationSignal` delivery.

## Reproduce

From the repository root, with `GRADLE_USER_HOME`, `ANDROID_HOME` and `ANDROID_AVD_HOME` set as in
`CLAUDE.md` (machine notes), ffmpeg on `PATH`, and no emulator running:

```
pwsh -NoProfile -File spikes/files-app/scripts/Invoke-FilesAppSpike.ps1 -Boot
```

It generates the two test files into `spikes/files-app/build/media`, builds the APK
(`gradlew.bat --no-daemon assembleDebug`), boots the `gander` AVD headless, installs and stages the app,
runs 17 numbered runs, writes `results/`, uninstalls the app, removes anything it copied to
`/sdcard/Download`, and kills the emulator (`adb emu kill`, then it checks that no emulator or qemu process
remains). `-UseRunningEmulator` skips the boot and the kill. `Analyze-ReadLog.ps1 -LogPath <log>` turns any
saved log into the per-fd table. The committed screenshots were reduced after the run (the playing frame
converted to a 540 px JPEG with ffmpeg); a re-run writes full-size PNGs. The wait for the 75 s read in the
script was raised from 150 s to 220 s after the committed run (it had finished at 150.02 s, one second
after the wait gave up); no measurement depends on it.
