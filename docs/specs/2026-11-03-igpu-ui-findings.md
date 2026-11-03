# Findings: settings UI on the integrated GPU, Stream encode on NVIDIA (2026-11-03)

Owner decision: the settings window should draw on the integrated GPU; Stream encoding keeps the
NVIDIA H.264 MFT. Heavier future windows may need the discrete GPU, so the choice is a setting,
not hard-wired. This records what was measured, what was chosen and what is left.

Machine: the dev laptop, Windows 11 Pro build 26200, Intel iGPU (8086:7D67) plus NVIDIA GeForce
RTX 5060 Laptop GPU (driver 595.79), two monitors (the internal panel on the Intel adapter, the
external one on NVIDIA), session unlocked. Nothing on the machine was changed: no driver setting,
no Windows graphics preference, no NVIDIA profile.

## The problem

`gander-host.exe` exported `NvOptimusEnablement = 1` (and AMD's
`AmdPowerXpressRequestHighPerformance = 1`) because WP-42 found the NVIDIA H.264 MFT does not
activate without it (`0x8000FFFF`). The export applies to the whole process: the eframe/glow
settings window's OpenGL context went to the NVIDIA ICD (`nvoglv64.dll`), so opening settings
woke the dGPU (host footprint spec, "What is left").

## What was measured

### 1. Without the export, a device on the NVIDIA adapter does not help (option "LUID + device")

Production already creates the D3D11 device on the encoder's adapter (by LUID) before
`IMFActivate::ActivateObject`, and enumerates the MFT with `MFT_ENUM_ADAPTER_LUID`. A probe with
no export, the device created on the NVIDIA adapter first, then `Encoder::new`:
`ActivateObject: Catastrophic failure (0x8000FFFF)`. So dropping the export and relying on the
explicit adapter is not enough; WP-42's finding stands.

### 2. The driver reads the exported VALUE at run time, not once per process

The symbol stays exported but its value was made writable (an `AtomicU32`, same layout as the
documented `DWORD`). Reproduce: `cargo run -p gander-stream --example gpu_probe`.

| Exported value at the moment of the call | DXGI adapter order (fresh factory) | NVIDIA H.264 MFT | Intel Quick Sync MFT |
| --- | --- | --- | --- |
| 0 | Intel first (1 output), NVIDIA (1 output) | fails `0x8000FFFF` | activates |
| 1 (set at run time, after a factory was created with 0) | NVIDIA first (2 outputs), Intel (0 outputs) | activates | activates |
| back to 0 | Intel first again | fails `0x8000FFFF` | activates |

Other orders tried with a throwaway probe: value 1 from the start then 0 before activation fails;
0 then 1 before activation works. The decision is taken when the driver is asked (factory
creation, MFT activation), every time.

### 3. Once activated, the NVIDIA MFT keeps encoding with the value back at 0

The Stream source now raises the value only around its encoder setup (`setup()`, and `recreate()`
for a non-Intel encoder) and drops it before the first frame. Results with the value at 0 while
encoding:

- `GANDER_TEST_VENDOR=nvidia cargo test -p gander-stream --test pipeline -- --ignored
  --test-threads=1`: `real_static_window_keepalive_cadence` (source `NVIDIA GeForce RTX 5060
  Laptop GPU / NVIDIA H.264 Encoder MFT` at 792x1610, 106 frames in 3 s animated, 35.3 fps, max
  gap 36 ms; static: keepalive every 455 ms), `real_keyframes_on_pli_and_on_reconnect` (PLI to IDR
  in 51 ms, three times) and `real_lock_guard_and_window_close` pass. The two adaptation tests fail
  with NVIDIA exactly as on `main` (by design: "NVIDIA's CBR undershoots far below a 1.5 Mbit/s
  cap ... which measures nothing"; they default to Intel and pass there, 2 of 2).
- Pre-existing, not caused by this change: with `GANDER_TEST_VENDOR=nvidia`,
  `real_lock_guard_and_window_close` often ends the test process with `STATUS_ACCESS_VIOLATION`
  after the lock step passes (`lock -> Ended after ~100 us`), when the second session starts or the
  window closes. With the `main` versions of `optimus.rs` and `pipeline.rs` it crashed 3 of 3 runs;
  with this change it passed once and crashed twice. Not investigated here; not run on Intel.
- `cargo xtask e2e-stream` (session unlocked): the host stage streams from the real (test-hooks)
  `gander-host.exe`. Polled during the run, that process loaded `nvwgf2umx.dll`,
  `nvEncMFTH264x.dll` and `nvEncodeAPI64.dll` and `nvidia-smi` listed it (`C+G`) while streaming:
  the NVIDIA encoder is in use. All 14 host steps passed on the second run; on the first, `host.tap`
  failed with `paused: focus_lost` (another window took the foreground on the attended desktop;
  input gating, unrelated to the GPU) and the other 20 steps passed. The engine stage runs inside
  `gander-e2e.exe`, which does not export the symbols, so it uses the Intel encoder (as before).

### 4. The settings window lands on the integrated GPU

A release host with temp `--config-dir`/`--data-dir`, port 42719 (loopback plus the tailnet
address, never a wildcard), started with `--open`, sampled 8 s after start with the settings
window showing. GPU driver DLLs in the process (`Get-Process -Module`), `nvidia-smi` process list,
memory:

| Build | `[ui] gpu` | GL / D3D driver DLLs loaded | In `nvidia-smi`? | Working set | Private | Threads |
| --- | --- | --- | --- | ---: | ---: | ---: |
| `main` (export = 1 always) | n/a | `nvoglv64` (NVIDIA OpenGL), `nvwgf2umx`, `nvldumdx`, Intel D3D (`igd10iumd64`) | yes, `C+G` | 176.7-187.5 MB | 190.7-193.9 MB | 83 |
| this change | default (integrated) | `igxelpgicd64` (Intel OpenGL ICD) and Intel helpers; no NVIDIA DLL | no | 104.7-116.2 MB | 63.6-63.9 MB | 24 |
| this change | `integrated` | same as default | no | 104.7 MB | 63.1 MB | 24 |
| this change | `discrete` | `nvoglv64`, `nvwgf2umx`, `nvldumdx`, Intel D3D | yes, `C+G` | 176.0 MB | 192.5 MB | 83 |

So the integrated path costs about 130 MB less private memory and 59 fewer threads (the NVIDIA GL
driver's own threads) while the window is open, and the dGPU is not woken. `discrete` reproduces
the old behaviour. Binary size: 20,881,408 bytes before and 20,901,888 after (+20 KB). After the
window closes the residue measured in the footprint spec (about 70 MB private) was not re-measured
here; the NVIDIA part of it no longer loads with the default.

## Options considered

1. **eframe on wgpu with `PowerPreference::LowPower` (DX12/Vulkan on the iGPU).** Not needed:
   option 3 gives the iGPU with the existing glow renderer and no new dependency tree (wgpu, naga
   and a shader compiler would add several MB and a large `cargo deny` surface). Kept as the path
   if a future window needs explicit adapter selection.
2. **Drop the export, select the NVIDIA adapter by LUID.** Measured above (1): the MFT does not
   activate. Rejected.
3. **Keep the export, make its value switchable (chosen).** The values start at 0;
   `gander_stream::win::optimus::GpuDecision::take(discrete)` sets them and holds a process-wide
   lock until dropped, so the encoder setup and a window's OpenGL context creation are serialised
   and never see each other's value. The Stream source holds `take(true)` around encoder setup;
   the UI holds `take(wants_discrete)` from just before `eframe::run_native` until eframe's app
   creator runs (the GL context exists then). A UI started during an encoder setup waits for it
   (at most a few hundred ms), and the reverse.
4. A separate settings process without the export (`gander-host --settings`) or a separate encode
   process with it: much larger changes (IPC for the controller or for frames). Not needed.

## The setting

`config.toml`:

```toml
[ui]
gpu = "integrated"   # integrated (default) | discrete | auto
```

- `integrated`: the host asks for no discrete GPU when it creates a window; on this laptop the
  window draws on the Intel GPU.
- `discrete`: the host asks for the discrete GPU (NVIDIA Optimus or AMD PowerXpress) while it
  creates the window's GL context.
- `auto`: discrete on mains power, integrated on battery, decided when a window session starts.
- Read when the host starts. Stream encoding is not affected by it.

## Limits and open points

- Measured on one Optimus laptop and one driver (595.79). That the driver reads the value at run
  time is observed behaviour, not a documented contract. A driver that read it once at process
  start would see 0, so the NVIDIA MFT would not activate; the Stream source then falls back to
  the next adapter's encoder (Intel Quick Sync here), so Stream keeps working at higher latency.
  `cargo run -p gander-stream --example gpu_probe` and the NVIDIA pipeline tests are the
  regression check after a driver update.
- AMD PowerXpress is handled the same way and is not measured (no AMD hardware).
- The Windows per-app graphics preference (Settings > Display > Graphics) is a user setting and
  was not touched; if the owner sets "High performance" for `gander-host.exe`, Windows may put the
  window on the dGPU regardless of `[ui] gpu`.
- Which monitor the window opened on was not recorded. Whether a window moved to the external
  monitor (scanned out by NVIDIA here) keeps its Intel GL context, and what the copy across
  adapters costs, was not measured.
