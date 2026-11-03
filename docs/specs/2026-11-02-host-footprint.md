# Host footprint: a lightweight tray process (2026-11-02)

The owner saw the release `gander-host.exe` at about 313 MB working set with only the tray
showing, and chose "keep one process, slim it" over a Windows service or a split agent. This
records what was measured, what changed and what is left. ADR-0007's process model stands; its
Evidence section has a line for this refinement.

## How it was measured

Windows 11 Pro build 26200, the dev laptop (24 logical CPUs, Intel iGPU plus NVIDIA dGPU,
Optimus). Release builds of `gander-host` and `gander-render` side by side, `main` at `fab890b`
("before") and `feat/slim-host` ("after"). Each run used temp `--config-dir`/`--data-dir`, port
42711 (the listener's own plan: loopback plus the Tailscale address, never a wildcard), one share
of 108 JPEG wallpapers (copies of `C:/Windows/Web`), and a seeded paired device (its token hash in
`devices.toml`, its grant in `config.toml`), so `curl.exe -k` (no pin) with the bearer token could
act as the phone over loopback.

Sampling was `Get-Process` once a second: working set (WS), private bytes, thread and handle
count, and CPU time over the window divided by wall time (percent of one logical CPU). States:

- **(a)** 20 s after start, nothing opened, sampled for 30 s.
- **(b)** the settings window opened through the tray icon's own callback (a posted
  `WM_USER_TRAYICON` left-click to the `tray_icon_app` window), sampled 10 s; then closed with
  `WM_CLOSE` and sampled 30 s from 5 s after.
- **(c)** the device listing the share 40 times; during (10 s) and after (15 s).
- **(d)** every image as a 256 px thumbnail plus 20 previews at 1600 px (128 requests, 6 in
  parallel, retries on 429/503); during (12 s) and from 10 s after (30 s). All 128 answered 200
  in both builds.

The scripts were throwaway PowerShell 7 files in the session scratchpad (`Get-Process`, `curl.exe`,
an `Add-Type` helper for the window messages); nothing was installed.

## Results

| State | Build | WS MB | Private MB | Threads | Handles | CPU % |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| (a) idle, no window | before | 100.1 | 95.8 | 74 | 665 | 3.32 |
| | after | **31.6** | **3.9** | **15** | **265** | **0.10** |
| (b) settings open | before | 189.7 | 197.9 | 102 | 863 | 2.45 |
| | after | 188.7 | 195.4 | 83 | 828 | 10.58 (window start) |
| (b) after close | before | 133.0 | 126.1 | 70 | 677 | 0.66 |
| | after | **12.3** | **71.6** | 21 | 472 | 0.15 |
| (c) browsing | before | 133.5 | 126.7 | 70 | 684 | 1.53 |
| | after | 15.7 | 72.5 | 20 | 475 | 0.77 |
| (c) after browsing | before | 133.5 | 126.7 | 69 | 682 | 0.70 |
| | after | 15.7 | 72.5 | 20 | 475 | 0.20 |
| (d) during burst | before | 154.5 | 160.7 | 103 | 870 | 12.2 |
| | after | 65.8 | 136.8 | 51 | 629 | 21.8 |
| (d) after burst | before | 138.1 | 133.5 | 68 | 696 | 0.87 |
| | after | **12.2** | 75.8 | 17 | 473 | 0.36 |

(c) and (d) follow (b) in the same process, so they carry what (b) left behind. Burst totals:
before 23.4 s wall and 1.58 s host CPU, after 21.9 s and 2.78 s; the difference is a toast
window that started during the burst (the "device connected" notice, below), whose GL context
and font atlas cost about 1 s of CPU to create. Idle runs were repeated four times; (a) varied
between 18 and 34 MB WS (DLL image pages, mostly shared) while private bytes stayed at 3.8 to
4.6 MB and CPU at 0 to 31 ms per 30 s.

Six open/close cycles in one process (12 s after each close): private bytes 62, 68, 67, 73, 73,
74 MB, threads 25 to 20, handles 496 to 489. No growth per cycle beyond heap noise.

Binary size: 19,267,072 bytes before, 19,275,776 after. The release profile already had thin LTO,
one codegen unit and stripped symbols; `opt-level = 3` is now stated explicitly. `panic` stays
`unwind`.

## Where the memory went (before)

- **The always-on eframe session.** The 1x1 root viewport kept a WGL context on the NVIDIA ICD
  (`nvoglv64.dll`, 44.7 MB image) plus egui, fonts, AccessKit (`uiautomationcore.dll`) for the
  whole process life, and repainted once a second: about 1 s of CPU every 30 s (3.3 % of a core)
  with nothing on screen. This was most of (a).
- **Tokio worker per logical CPU**: 24 workers on this laptop. The bulk of the 74 threads.
- **Eagerly loaded DLLs**: `d3d11`, `dxgi`, `mfplat` (Stream engine), `opengl32`, `dwmapi`,
  `uiautomationcore` were load-time imports, so they mapped at start even though no stream or
  window existed. The Stream engine itself was already lazy: no D3D device, Media Foundation
  startup or `timeBeginPeriod` happens before a stream; the engine object only holds config.
- **Not significant**: the redb caches (the whole host is 3.9 MB private once the UI is gone), the
  render queue threads (a handful, parked on a condvar), the 30 s listener replan, the 15 s
  session reconcile and the 5 s host-state poll (together under the 15.6 ms timer resolution per
  30 s window: measured 0 ms in the best idle run).
- **A window, once opened**, makes the GL drivers load (NVIDIA because the exe exports
  `NvOptimusEnablement` for the NVENC MFT, and the Intel D3D driver for presenting); they stay
  mapped after their context is gone.

## What changed

1. **The UI runs in sessions** (`host/gander-host/src/ui/app.rs`, `ui/idle.rs`, `ui/mod.rs`).
   The tray icon lives on the main thread on a plain Win32 message loop
   (`MsgWaitForMultipleObjectsEx` with a timeout, then `PeekMessage`/`DispatchMessage`), which
   polls the host once a second for the tray state and the toast triggers. When a window or a toast
   must show, `eframe::run_native` runs with `run_and_return`; it returns once nothing is left to
   show, dropping the GL context, egui and AccessKit. eframe keeps its winit event loop in a
   thread local and reuses it for the next session (that is its documented way of opening a
   window again; winit itself still cannot make a second event loop). A session that cannot start
   (no GL) is reported and the tray keeps running. After each session the working set is trimmed
   (`SetProcessWorkingSetSize(-1, -1)`), which is why WS falls to about 12 MB.
   No in-process restart limitation was found, so the `--settings` helper process was not needed.
2. **Two tokio workers** (`RUNTIME_WORKERS` in `main.rs`) instead of one per logical CPU.
3. **Delay-loaded DLLs** (`host/gander-host/build.rs`): `opengl32`, `d3d11`, `dxgi`, `mfplat`,
   `uiautomationcore`, `dwmapi` load on the first window or stream. Modules at idle: 87 -> 47.
4. `--ui-smoke` now closes everything, checks that the session ended, opens the Gander window
   again in a second session and quits from there (`2 window sessions` in its output).

Targets: idle WS under 40 MB (31.6, best 18.0), idle private under 30 MB (3.9), idle CPU about
0 (0.1 %, 0 to 31 ms per 30 s). Met.

## What is left (open)

- **After the first window, about 70 MB private stays committed** (the NVIDIA and Intel GL/D3D
  driver state; `SHIM_MCCOMPAT` made no difference). It is not resident after the trim (WS
  12 MB), so Task Manager's default memory column shows it small, but the commit charge does not
  return until the host restarts. Getting it back needs the settings UI in a short-lived process
  (`gander-host --settings` over an owner-only named pipe); not done, because in-process
  sessions met the targets and that is a much larger change (the whole `HostController` over IPC).
- **Toasts start a session too.** A "device connected" notice or a pairing request opens a GL
  window for a few seconds (about 1 s CPU to start, and the residue above). A native Windows
  notification for the plain notices would keep GL off unless the owner opens a window; that is a
  UI decision for the UI overhaul, not made here.
- **The settings window runs on the dGPU** because of the `NvOptimusEnablement` export that NVENC
  needs; waking the discrete GPU for a settings window costs power on battery. **Resolved
  2026-11-03** (`2026-11-03-igpu-ui-findings.md`): the exported value is now 0 except while an
  encoder is set up, and `[ui] gpu` (default `integrated`) decides the window's GPU; with the
  default the settings window loads the Intel GL driver only (63 MB private instead of 191 MB
  while open).
- `--no-ui` with the GUI-subsystem release build exited at once when started hidden without a
  console (it waits for `Ctrl+C`), so no headless release baseline was taken. Noticed while
  measuring; cause not investigated, nothing changed.
