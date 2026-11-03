# Preflight report

- Generated (UTC): 2026-11-01T16:10:27Z
- Command: `cargo xtask preflight --report`
- Result: **PASS** (8 of 8 hard checks passed)
- Paths: written with `%USERPROFILE%` and `%LOCALAPPDATA%` of the real user profile;
  `<launcher profile>` is the folder the Claude Code launcher uses as `USERPROFILE`. The exact
  local paths are kept in `docs/local/machine.md` (gitignored, not committed).

## Checks (master plan section 1b)

| Check | Level | Expected | Result | Detail |
| --- | --- | --- | --- | --- |
| cargo | hard | cargo >= 1.90 | PASS | cargo 1.96.1 (356927216 2026-06-26) |
| msvc-build | hard | hello-world cargo build links and runs (MSVC) | PASS | built and ran hello-world, rustc host x86_64-pc-windows-msvc |
| java | hard | java 17 or 21 | PASS | openjdk version "17.0.21.0.2" 2026-10-22 LTS (C:/Program Files/Microsoft/jdk-17.0.21.21-hotspot/bin/java.exe) |
| android-sdk | hard | ANDROID_HOME with platforms/android-36, build-tools/36.*, cmdline-tools/latest | PASS | %LOCALAPPDATA%/Android/Sdk with platforms/android-36, build-tools 36.0.0, cmdline-tools/latest |
| tailscale | hard | `tailscale status` reports Running | PASS | BackendState Running, tailscale 1.104.4-t938213198-gd38c96d6a |
| git | hard | git user.name and user.email configured | PASS | user.name and user.email are set |
| disk | hard | free disk >= 20 GB | PASS | 60.6 GiB free |
| windows-build | hard | Windows build >= 22000 | PASS | Windows 10.0 build 26200 |
| msvc-cl | optional | cl.exe locatable through vswhere | PASS | C:/Program Files (x86)/Microsoft Visual Studio/2022/BuildTools/VC/Tools/MSVC/14.44.35207/bin/Hostx64/x64/cl.exe |
| emulator-avd | optional | `emulator -list-avds` lists `gander` | PASS | AVDs: gander (AVD home %USERPROFILE%/.android/avd) |
| emulator-accel | optional | `emulator -accel-check` reports usable | PASS | WHPX(10.0.26200) is installed and usable. |
| adb-devices | optional | `adb devices` lists a device | WARN | no devices attached (emulator only for Night 1) |
| office | optional | POWERPNT.EXE exists | PASS | C:/Program Files/Microsoft Office/root/Office16/POWERPNT.EXE |
| ffmpeg | optional | ffmpeg on PATH | PASS | ffmpeg version 9.0.1-full_build-www.gyan.dev (<launcher profile>/AppData/Local/Microsoft/WinGet/Packages/Gyan.FFmpeg_.../bin/ffmpeg.exe) |
| pwsh | optional | PowerShell 7+ installed | PASS | PowerShell 7.6.9 (%LOCALAPPDATA%/Microsoft/WindowsApps/pwsh.exe) |
| cargo-deny | optional | cargo-deny installed | PASS | cargo-deny 0.20.2 |

## Tool paths and versions

| Tool | Path | Version |
| --- | --- | --- |
| cargo | `<launcher profile>/.cargo/bin/cargo.exe` | cargo 1.96.1 (356927216 2026-06-26) |
| rustc | `<launcher profile>/.cargo/bin/rustc.exe` | rustc 1.96.1 (31fca3adb 2026-06-26) |
| java | `C:/Program Files/Microsoft/jdk-17.0.21.21-hotspot/bin/java.exe` | openjdk version "17.0.21.0.2" 2026-10-22 LTS |
| git | `C:/Program Files/Git/mingw64/bin/git.exe` | git version 2.53.0.windows.3 |
| tailscale | `C:/Program Files/Tailscale/tailscale.exe` | 1.104.4 |
| ffmpeg | `<launcher profile>/AppData/Local/Microsoft/WinGet/Packages/Gyan.FFmpeg_.../bin/ffmpeg.exe` | ffmpeg version 9.0.1-full_build-www.gyan.dev Copyright (c) 2000-2026 the FFmpeg developers |
| pwsh | `%LOCALAPPDATA%/Microsoft/WindowsApps/pwsh.exe` | PowerShell 7.6.9 |
| cargo-deny | `%USERPROFILE%/.cargo/bin/cargo-deny.exe` | cargo-deny 0.20.2 |
| sdkmanager | `%LOCALAPPDATA%/Android/Sdk/cmdline-tools/latest/bin/sdkmanager.bat` | 19.0 |
| avdmanager | `%LOCALAPPDATA%/Android/Sdk/cmdline-tools/latest/bin/avdmanager.bat` |  |
| emulator | `%LOCALAPPDATA%/Android/Sdk/emulator/emulator.exe` | Android emulator version 37.2.13.0 (build_id 16740729) (CL:N/A) |
| adb | `%LOCALAPPDATA%/Android/Sdk/platform-tools/adb.exe` | Android Debug Bridge version 1.0.41 |

## Installs performed by WP-00 (owner-approved list only)

| Item | Result | Where |
| --- | --- | --- |
| Android Emulator | installed, 37.2.13 (build 16740729) via `sdkmanager --install emulator` | `$ANDROID_HOME/emulator/emulator.exe` |
| `platform-tools` | already present at 37.0.1; the install request was a no-op | `$ANDROID_HOME/platform-tools/adb.exe` |
| `system-images;android-36;google_apis;x86_64` | installed, revision 7 | `$ANDROID_HOME/system-images/android-36/google_apis/x86_64` |
| AVD `gander` | created: `avdmanager create avd -n gander -k "system-images;android-36;google_apis;x86_64" -d pixel_6` (device profile added so the screen is phone-sized) | `%USERPROFILE%/.android/avd` (about 1 GB after first boot) |
| PowerShell 7 | installed with `winget install --id Microsoft.PowerShell` (MSIX, per user), 7.6.9 | `%LOCALAPPDATA%/Microsoft/WindowsApps/pwsh.exe`, already on `PATH` |
| cargo-deny | `cargo install cargo-deny --locked`, 0.20.2 (allowed by the WP-01 brief; crates.io download) | `%USERPROFILE%/.cargo/bin/cargo-deny.exe` (`$CARGO_HOME/bin`) |

Licences: no blanket `sdkmanager --licenses` was run. The install only touched packages covered by
the `android-sdk-license` already accepted on this machine; the `licenses/` folder is unchanged.
Not installed, by design: LibreOffice, Windows features (WHPX was already usable), anything else.

### Emulator check

- `emulator -accel-check`: `WHPX(10.0.26200) is installed and usable.` No human action needed.
- Quick boot test (headless, `-no-window -no-audio -no-snapshot -gpu swiftshader_indirect`,
  console port 5554 on loopback): the `gander` AVD reached `sys.boot_completed=1` in 61 s and
  reports Android 16, SDK 36, ABI x86_64. Killed with `adb emu kill`; zero emulator or qemu
  processes remained and `adb devices` listed nothing afterwards.
- No `adb connect` was issued. `adb devices` shows no device attached (emulator only for Night 1);
  the optional `adb-devices` check therefore reports WARN, which is expected.

## Notes for later workers

- **AVD home trap.** The owner's Claude Code launcher sets `HOME` and `USERPROFILE` to its profile folder, so
  Android tooling looks for AVDs in `<profile>/.android/avd` and will not find `gander`. The AVD
  lives in the real user profile (`%USERPROFILE%/.android/avd`). Set
  `ANDROID_AVD_HOME=%USERPROFILE%/.android/avd` before calling `emulator`/`avdmanager`
  (the preflight and the future `cargo xtask e2e-android` resolve it automatically).
- `emulator`, `adb`, `sdkmanager` and `avdmanager` are not on `PATH`; use the paths in the table
  above (the exact local paths are in `docs/local/machine.md`, not committed).
  `sdkmanager.bat`/`avdmanager.bat` need `JAVA_HOME` (set, JDK 17).
- `cargo deny` works without `PATH` changes because cargo also searches `$CARGO_HOME/bin`
  (`CARGO_HOME` is `%USERPROFILE%/.cargo`; the active `cargo.exe` itself lives under a
  different profile's `.cargo/bin`, which is only read through `PATH`).
- The `ffmpeg` on `PATH` is a WinGet package under another profile's AppData; it runs fine but
  its path is machine specific.
- A pre-existing `adb` server (started before this run) is running; the run did not start or
  kill it.
- `cl.exe` (MSVC 14.44.35207) is installed, but `cmake`, `ninja`, `clang` and `nasm` are not on
  `PATH` (Android SDK ships CMake 3.22.1 under `$ANDROID_HOME/cmake`, not on `PATH`). Crates that
  compile C with only `cl.exe` work (for example `ring`); crates that need NASM or CMake (for
  example the default `aws-lc-rs` backend of rustls) are likely to fail to build on this machine.
  WP-12 should pick the rustls crypto provider with that in mind and, if it needs another, record
  an install request instead of installing.

## Where the plan or spec differs from what was measured

- Master plan 1b asks for "cl.exe reachable (cargo build of a hello-world)". A Rust hello-world
  links with `link.exe` and never runs `cl.exe`, so the build alone does not prove `cl.exe` is
  usable. The preflight keeps the hello-world build as the hard check and adds an optional
  vswhere lookup of `cl.exe`.
- Master plan 1a installs `platform-tools` with `sdkmanager`; it was already present (37.0.1).
- Master plan 1a creates the AVD with no device profile and without mentioning that the AVD store
  depends on `HOME`/`USERPROFILE` (see the trap above).
- Master plan section 3 lists `.github/workflows/docs.yml`; WP-01 only asks for the host and
  client workflows, and `docs.yml` needs `cargo xtask docs-check` (WP-1C), so it is not created yet.
- `tailscale status` is parsed from `status --json --peers=false` (local daemon only) rather than
  the text table, which is brittle to parse. No peer is listed or contacted.
