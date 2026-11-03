# Spec: Stream mode launch policy, window discovery and session lifecycle

- Status: Draft (Phase 2; the policy engine and its tests are Phase 1 work so `list` can report
  `stream: allowed|refused|ask` from day one)
- Date: 2026-11-01
- Implements: `security.md` non-negotiable "Stream mode never executes arbitrary files"; ADR-0006
- Related: `2026-11-01-protocol-v1.md` (Stream endpoints), `2026-11-01-stream-spike.md` (measurements)

## The problem stated honestly

"Open this file with its default app" on Windows is `ShellExecute`, and `ShellExecute` is how
malware runs: `.py` opens with the Python interpreter, `.js` with Windows Script Host, `.jar` with
`java`, `.lnk` runs whatever it points at, and the default handler for `.pdf` on many machines is a
web browser (on the development machine it is Microsoft Edge). A blocklist of extensions is not a
policy; the policy must be about **which executable will receive the file**.

Two further facts shape this spec:

1. **Stream mode is app-level access, not share-level.** Once an app is streaming, its own
   File > Open / Save As dialogs expose the whole filesystem to the phone, read and write, through
   the app. Gander cannot sandbox arbitrary desktop apps in v1. Stream is therefore a
   **trusted-device permission**, off by default per share, and the UI says so when granting it.
2. **The window you get is not always the window you launched.** PowerPoint already running opens
   the file in the existing process. Browsers open a new tab in an existing window (streaming the
   whole browser is a privacy leak). Some apps show a splash window first. Packaged apps are hosted
   by `ApplicationFrameHost.exe`.

## Policy: who may open what

Decision inputs: file extension, the resolved **handler executable** for that extension, the
share's `stream` permission for the requesting device, the host's app catalogue, and the host
state (locked, attended, battery).

### Step 1: hard refusals by extension (never streamable, regardless of handler)

`.exe .com .scr .pif .bat .cmd .ps1 .psm1 .vbs .vbe .js .jse .wsf .wsh .msi .msp .msc .hta .cpl
.reg .inf .lnk .url .appx .appxbundle .msix .msixbundle .jar .py .pyw .pyc .rb .pl .sh .ahk
.iso .img .vhd .vhdx` and macro-enabled Office (`.docm .dotm .xlsm .xltm .xlam .pptm .potm .ppam .sldm`).
Match on the **final** extension after lower-casing; `report.pdf.exe` is `.exe`. A file with no
extension is refused.

### Step 2: resolve the handler without executing anything

- Windows: `AssocQueryStringW(ASSOCF_NOTRUNCATE, ASSOCSTR_EXECUTABLE, ".ext", "open")` and
  `ASSOCSTR_COMMAND` for the command template. Packaged apps resolve to an `AppUserModelID` via
  `ASSOCSTR_APPID` / the `Applications` registry; record the package family name.
- The handler path is canonicalised (`GetFinalPathNameByHandle` on an opened handle) and its
  digital signature read (publisher name, or "unsigned").

### Step 3: the app catalogue decides

The host keeps an **app catalogue** in `%APPDATA%\Gander\apps.toml`:

```toml
[[app]]
id = "powerpoint"
name = "Microsoft PowerPoint"
exe = "C:\\Program Files\\Microsoft Office\\root\\Office16\\POWERPNT.EXE"
publisher = "Microsoft Corporation"       # from the Authenticode signature at approval time
extensions = ["pptx", "ppsx", "potx", "odp"]
approved_by_owner = "2026-11-02T20:11:03Z"
launch = "default"                        # or a template: "{exe} --new-window {file}"
window_match = "title_contains_filename"  # or "new_window_of_process" | "pick"
close_on_end = "ask"                      # ask | never | if_launched_by_us
```

Decision for a request (extension `e`, resolved handler `h`):

1. `e` in step-1 list -> **refused** (`stream_refused`, "This kind of file cannot be streamed").
2. `h` is on the **never list** -> refused. Never list (by file name, case-insensitive):
   `cmd.exe powershell.exe pwsh.exe wscript.exe cscript.exe mshta.exe rundll32.exe regsvr32.exe
   msiexec.exe python*.exe pythonw*.exe java.exe javaw.exe node.exe ruby.exe perl.exe wsl.exe
   bash.exe explorer.exe control.exe conhost.exe wt.exe` and anything under `%SystemRoot%\System32`
   except an explicit allowlist (`notepad.exe`, `mspaint.exe`, `WindowsTerminal` is NOT allowed).
3. `h` matches a catalogue entry with `approved_by_owner` set and `e` in its `extensions` ->
   **allowed**.
4. `h` matches an approved catalogue entry but `e` is new for it -> **ask** (owner approves the
   extension once on the host; typical for Blender opening `.obj`).
5. `h` is unknown -> **ask**: the host shows "Pixel 9 wants to open `Deck.pptx` with
   **POWERPNT.EXE** (Microsoft Corporation). Allow this app for streaming? Allow once / Always /
   Deny". "Always" writes a catalogue entry. The device waits (`stream.state: approving`) up to
   60 s. No host user present -> `host_unattended` (412) after the timeout.
6. Handler is a **browser** (Edge, Chrome, Firefox, Brave, Opera, Vivaldi, Arc detected by exe
   name or publisher) -> allowed only via a browser-specific launch template that opens an
   isolated window: Chromium family `--new-window --app=file:///<path> --user-data-dir=<gander
   profile>`; Firefox `-new-window -P gander-stream file:///<path>`. The dedicated profile keeps
   the owner's tabs, sessions and extensions out of the stream. The phone is told
   "opens in Microsoft Edge (isolated window)".
7. `list` reports `stream` as the outcome of steps 1-6 without prompting; prompts happen only on
   `POST /stream`.

The catalogue ships with **suggested** entries (not pre-approved) for common apps: Office, Adobe
Acrobat/Reader, Blender, GIMP, Inkscape, Krita, VLC, mpv, Notepad, Notepad++, VS Code, Paint.NET,
Affinity suite, Autodesk Fusion, FreeCAD, KiCad, LibreOffice, Sumatra PDF, Foxit, IrfanView, XnView,
Audacity, DaVinci Resolve, Kdenlive, OBS is excluded. Suggested entries still require the owner's
first-use approval; what they add is the right `launch`/`window_match` strategy.

### Step 4: host-state preconditions

- Session locked (`WTSQuerySessionInformation`/`WTS_SESSION_LOCK` events, or the
  `SessionSwitch` notification) -> 412 `host_locked`: "studio-pc is locked. Unlock it to stream,
  or use Preview/Fetch." (v1 does not stream to a locked session; see security.md.)
- Someone is actively using the computer (last input under `idle_prompt_seconds`, default 120,
  via `GetLastInputInfo`) -> the host shows a 10-second toast "Pixel 9 wants to stream Deck.pptx.
  This will move the mouse. Allow / Not now"; no click within 10 s -> allowed (it is the owner's
  own device), configurable to default-deny.
- On battery and below 20 % -> allowed but `prefer.max_kbps` is capped at 2500 and the phone is told.
- Another stream session active -> 409 `stream_busy` (v1: one session per host).

## Launch and window discovery

1. Snapshot top-level windows: `EnumWindows` -> set S0 of (hwnd, pid, title, class, visible).
2. Launch: `CreateProcessW` with the catalogue `launch` template (never `ShellExecute` with the
   file as the verb target; the template is built by us from the handler's own command string
   with the file path as a single quoted argument). Record `pid`.
3. Poll every 100 ms up to 15 s (Office may take longer on cold start: 30 s for catalogue entries
   marked `slow_start = true`):
   - **new_window_of_process**: a visible top-level window whose pid == launched pid (or whose
     process tree descends from it; Office and browsers re-exec), excluding splash classes
     (`MsoSplash`, `Chrome_WidgetWin_0` with empty title).
   - **title_contains_filename**: any new visible top-level window (not in S0) whose title
     contains the file name without extension. This covers single-instance apps re-using an
     existing process.
   - **ApplicationFrameHost**: for packaged apps, find the `ApplicationFrameWindow` whose child
     `Windows.UI.Core.CoreWindow` belongs to the launched package (via `GetPackageFamilyName` of the
     core window's process).
4. Ambiguity (two candidates) or timeout -> `stream.state: pick` with the candidate titles; the
   phone shows a picker; the owner at the host also sees a toast. No choice within 30 s -> end
   session, leave the app open, report `app_unavailable`.
5. **Fit to phone** (default on): compute the target client-area size from the phone viewport and
   the window's monitor DPI so that 1 window pixel maps to 1 phone pixel at the phone's natural
   scale (`viewport.width / dpr * host_scale` rounded; minimum 640x480; respect the app's
   `WM_GETMINMAXINFO`). `SetWindowPos` the window to that size on the monitor where it appeared,
   `ShowWindow(SW_RESTORE)` if minimised (WGC cannot capture minimised windows), and keep it
   restored for the session. Record the original placement and restore it at session end.
6. Start capture of that HWND. Capture includes owned popups/menus when the OS supports it
   (`IncludeSecondaryWindows`); the spike records what older builds lose.
7. Bring the window to the foreground for input. If `SetForegroundWindow` is refused (foreground
   lock), use the documented `AttachThreadInput` fallback, then `AllowSetForegroundWindow` for
   our own pid at process start so later attempts succeed.

## Input mapping

- Pointer events arrive normalised (0..1) to the captured frame; the host maps them to screen
  coordinates via the window's current client rect (`DwmGetWindowAttribute(DWMWA_EXTENDED_FRAME_BOUNDS)`
  for the visible bounds, `ClientToScreen` for the client origin) re-read on every `WM_SIZE`/move.
- Injection is `SendInput` with `MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK` coordinates on
  the virtual-desktop rectangle (multi-monitor safe). Keyboard via `SendInput` scancodes for
  `key` messages and `KEYEVENTF_UNICODE` for `text`.
- Input is injected only while the captured window is the foreground window; otherwise the host
  pauses injection, sends `stream.state: paused { reason: "focus_lost" }` and offers "Bring to
  front" on the phone (which calls the foreground procedure again). This prevents the phone from
  typing into whatever the local user switched to.
- The host process must be Per-Monitor-V2 DPI aware; coordinates are physical pixels throughout.
- Local user activity (any real input in the last 500 ms, detected via a low-level hook or
  `GetLastInputInfo` delta versus our own injection timestamps) pauses injection for 2 s with a
  phone-side "Someone is using the computer" notice; the stream keeps running.

## Session lifecycle

```
requested -> approving? -> launching -> window_found -> streaming <-> paused -> ended
```
- `paused` reasons: `focus_lost`, `local_user_active`, `phone_background` (client asked),
  `network` (ICE disconnected; the host keeps the session alive for 120 s for reconnection, then
  ends it).
- Reconnect: the client sends `POST /stream` with `resume: "<session>"` within the grace period;
  the host re-offers over a fresh peer connection without relaunching the app.
- `ended` reasons: `client_closed`, `host_kill_switch`, `window_closed` (the user closed the app on
  the host), `revoked`, `timeout`, `error`.
- On end: stop capture, restore window placement, then apply `close_on_end`: `never` (default
  for apps that were already running), `if_launched_by_us` (close only if we started the process
  and it has no other top-level windows; use `WM_CLOSE`, never `TerminateProcess`; if a "save
  changes?" dialog appears the app stays open and the phone is told "left open: unsaved changes"),
  `ask` (phone prompt at end).
- A host **kill switch** ends every session immediately and, if the owner chooses, pauses the
  listener for 10 minutes.

## Host visibility

- Tray icon state: idle / connected (device browsing) / **streaming** (distinct colour).
- Toast on stream start: "Pixel 9 is viewing Deck.pptx in PowerPoint" with "Stop".
- The WGC capture border stays **on** by default; it is the OS's own indicator of which window is
  shared. Turning it off is an owner setting and requires the OS consent prompt.
- The audit log records: session id, device, file, handler exe, decision path (allowed by catalogue
  / approved once / etc.), start, end, reason.

## What this spec does not do (recorded so nobody assumes it)

- It does not sandbox the launched app or limit what its dialogs can reach.
- It does not stream when the session is locked or no one is logged in.
- It does not support two devices streaming at once, or two windows in one session.
- It does not capture audio in Phase 2.0. Phase 2.1 (WP-55) captures the streamed window's own
  process tree by WASAPI process loopback (Windows 10 2004 or later), never the system mix; see
  `gander_stream::audio` and the protocol spec, "Sound".

## Tests

- Policy unit tests with a fake association table: every step-1 extension refused; every
  never-list handler refused even for a benign extension; approved catalogue entry allowed; new
  extension on approved app -> ask; browser -> isolated template; `A.PDF.EXE` refused;
  `deck.PPTX` allowed; file with no extension refused.
- Window discovery tests against a bundled test app (`gander-testwin`, a Win32 window that sets
  its title from argv and can spawn a popup on command): new process, re-used process via a named
  pipe, splash-then-main, two candidates -> pick.
- Fit-to-phone math tests (DPI 100/150/200 %, portrait and landscape viewports, min size clamp).
- Lifecycle state-machine tests including reconnection within and after the grace period.
