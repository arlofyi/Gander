# 0007. Host process model: user-session tray app, isolated render workers, no service in v1

- Status: Accepted on 2026-11-01 (owner approved defaults D1-D14)
- Date: 2026-11-01

## Context

Three constraints, all confirmed on 2026-11-01:

- Windows Graphics Capture and input injection work only inside the interactive user session;
  Sunshine's maintainers confirm WGC does not work from a service (session 0), an issue open since
  2024.
- Tailscale's Windows LocalAPI admits only the logged-in Tailscale user and LocalSystem; a host
  running under any other account cannot ask WhoIs.
- Preview renderers (image decoders, PDF, third-party shell thumbnail handlers, Office COM) parse
  untrusted bytes and crash or hang routinely; they must not run inside the process that holds
  the listener and the tokens.

A service would make Preview and Fetch available after a reboot before anyone logs in; a tray app
only runs after login. Modern Standby laptops are "fully running" whether the screen is on or off,
so a logged-in, locked laptop still serves Preview and Fetch.

## Options

1. **One tray application in the user session** (autostart at login) hosting the listener, policy,
   pairing UI and stream engine, plus a **separate worker executable** for every render job
   (job object limits, below-normal priority, handle passed in, no path). No Windows service in v1.
2. A Windows service (LocalSystem) for the listener plus a per-session helper for capture/input,
   talking over a local pipe. Covers "serve before login"; doubles the IPC surface and runs the
   network-facing code as SYSTEM.
3. A service running as the user's account via a scheduled task. Loses LocalAPI access unless the
   Tailscale user is the same, and still cannot capture.

## Decision

Option 1 for v1 and v2. The tray app is registered as a per-user autostart (Task Scheduler at
logon, so it can also request "run with highest privileges = no" explicitly), binds the listener
after the Tailscale adapter appears, and keeps running while the session is locked (Preview and
Fetch continue; Stream refuses with `host_locked`).

UI stack for the tray and settings windows: `tray-icon` + `egui`/`eframe` (pure Rust, no WebView2,
no JavaScript supply chain), per the 2026-11-01 research recommendation. Tauri 2.x is the named
fallback if the settings UI outgrows egui.

Renderer isolation: `gander-render.exe`, one process per job, in a job object (memory 512 MiB,
CPU 20 s, no child processes, kill on close), reading the input through an inherited handle and
writing the image to stdout; Office COM jobs run in the same worker at medium integrity because
COM activation requires it, still inside the job object.

## Consequences

- Easier: one install path, no elevation after install (except the firewall rule), identity and
  capture both just work, the kill switch is a tray click.
- Harder: nothing is reachable after a reboot until the owner logs in; fast user switching means
  a second user's instance cannot bind the same port (fail loudly with `SO_EXCLUSIVEADDRUSE`).
  These are documented limits, and "Preview/Fetch service for headless PCs" is a Phase 3 option.
- Must do: autostart registration in the installer; session lock/unlock tracking via
  `WTSRegisterSessionNotification`; `SetThreadExecutionState` during sessions; worker protocol
  and tests in `2026-11-01-preview-renderers.md`.

## Evidence (Phase 1 host, 2026-11-02)

Measured on one machine, Windows 11 build 26200. None of this changes the decision; it records
where the implementation had to add something the text above did not foresee.

- **The job object's CPU limit does not stop the worker (WP-15, WP-16).**
  `JOB_OBJECT_LIMIT_PROCESS_TIME` (20 s of user time) was set and did not stop a worker that ran
  ten times over it. The host therefore polls the worker's CPU time every 25 ms and terminates the
  job itself (exit code 3), beside the wall-clock cap; the job limit stays set as a backstop.
  Memory cap, one active process, below-normal priority, UI restrictions and kill-on-close are
  unchanged. Code: `host/gander-win/src/render/worker.rs`.
- **Office COM is not inside the job object (WP-16).** An Office instance started through COM is
  launched by the DCOM service, so it is not a child of the worker and the worker's job does not
  contain it. "Still inside the job object" above holds for the worker, not for Office. Cleanup is
  the worker's watchdog (it announces the instance's pid and sweeps), a host-side sweep of Office
  processes carrying the automation switch (`/automation`, `-Embedding`) that started during the
  job (`gander_win::preview::procs::sweep_automation`), and never touching an Office the owner is
  running (PowerPoint is refused when any instance exists; Word and Excel let go of an instance
  they attached to). A watchdog that fires inside `CoCreateInstance` keeps sweeping for 2 s so
  an instance DCOM finishes launching late is caught, and the worker's job and watchdog race for
  the exit through one claim.
- **The egui root viewport must live as long as the process (WP-18).** winit 0.30 cannot create a
  second event loop in a process, and hiding the root viewport on Windows stops `update()` for good
  (eframe 0.33.3, winit 0.30.13, build 26200: not even `request_repaint_after` or a repaint from
  another thread wakes it), which would freeze the tray and every toast. The root is a transparent,
  borderless, click-through 1x1 window parked off screen with no taskbar button; every real window
  is an immediate child viewport that exists only while open. The UI-stack decision (`tray-icon` +
  `egui`/`eframe`, Tauri as the named fallback) stands. Code: `host/gander-host/src/ui/app.rs`.
- **The UI exists only while a window shows (host footprint).** The previous point's process-long
  root viewport kept a GL context and a once-a-second repaint alive with nothing on screen (idle
  100 MB working set, 96 MB private, 74 threads, 3.3 % of a core). The tray now runs on a plain
  Win32 message loop and each window or toast runs one `eframe::run_native` session with
  `run_and_return`; eframe reuses its thread-local winit event loop, so sessions repeat in one
  process and the root viewport lives only as long as its session. With two tokio workers and the
  graphics/media DLLs delay-loaded, the idle host measured 32 MB working set, 3.9 MB private,
  15 threads, 0.1 % CPU. Still one process and one installed app; no `--settings` helper process
  was needed. About 70 MB of GL driver state stays committed after the first window (not
  resident). Details: `docs/specs/2026-11-02-host-footprint.md`. Code: `host/gander-host/src/ui/`.
