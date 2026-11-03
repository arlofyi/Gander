# Gander installer (WP-1A)

A per-user MSI for the Gander host, a sign-in autostart task, and Windows Firewall allow rules
limited to your private network. **Status: the MSI is built with WiX 5 by `cargo xtask
installer-build` and passes `wix msi validate`; its files were listed with an administrative
extract (which installs nothing).** Installing it, uninstalling it and the live autostart and
firewall tests are human-gated: nothing in this folder has been run against a real firewall or Task
Scheduler, and the MSI has not been installed on any machine yet.

## What is in this folder

| File | What it is |
| --- | --- |
| `main.wxs` | The WiX 5 product definition (the v4 schema): per-user install, features, custom actions, the icon. `wix build` builds it, driven by `cargo xtask installer-build`. |
| `autostart-task.xml` | Template of the Task Scheduler logon task. The host embeds it (`host/gander-host/src/ui/autostart.rs`) and fills in the user and the path. |
| `firewall-rules.ps1` | Creates (`-Action Install`) or removes (`-Action Remove`) the firewall rules; `-Action Plan` prints what Install would do and changes nothing. Re-launches itself elevated. |
| `firewall-common.ps1` | The shared logic: CIDR and port validation, `config.toml` reading, the rule plan. |
| `verify-rules.ps1` | Read-only check of the live rules against the plan. `-SelfTest` tests the pure logic without looking at the firewall. |
| `parse-check.ps1` | Parses scripts without running them (used by `cargo xtask installer-check`). |

Generated, not committed (`cargo xtask notices` writes them to `target/installer/`):
`THIRD-PARTY-NOTICES.txt` (licence texts of every shipped dependency, including egui's bundled fonts
under OFL-1.1 and Ubuntu-font-1.0 and the Boost-licensed `clipboard-win` and `error-code`) and
`License.rtf` (the repository `LICENSE` for the installer's licence page). The MSI itself is
written to `target/installer/Gander-<version>-x64.msi`. `cargo xtask sbom` writes the CycloneDX 1.5
software bill of materials of both executables to `target/sbom/gander-host.cdx.json`; the MSI installs
it as `sbom\gander-host.cdx.json` next to the programs (see
[the release notes on supply chain and reproducible builds](../docs/release/reproducible-builds.md)).

Generated and committed (`cargo xtask icons`, from the SVGs in `brand/`): the icon
`host/gander-host/assets/gander.ico` (16, 20, 24, 32, 40, 48, 64, 256 px). `host/gander-host/build.rs`
embeds it in `gander-host.exe` together with a version block (product, company, file description),
and the .wxs uses the same file as the Add/Remove Programs icon (`ARPPRODUCTICON`) and for the
network-setup shortcut. Compiling the exe resource needs `rc.exe` from the Windows SDK (the VS Build
Tools install it; `embed-resource` finds it through the registry).

All PowerShell here is Windows PowerShell 5.1 compatible (no ternary, `??`, `?.`, `&&`, `||`) and
ASCII only; `cargo xtask installer-check` parses it with the 5.1 parser and with `pwsh` and fails on
a non-ASCII character such as an em-dash.

## Design

Gander is a user-session tray application with no service (ADR-0007), so the installer is per-user
and never asks for elevation to install it.

| What | How | Needs administrator |
| --- | --- | --- |
| Files | `%LOCALAPPDATA%\Programs\Gander` (`gander-host.exe`, `gander-render.exe`, licences, the firewall scripts) and a Start menu folder with two entries: **Gander** (`gander-host.exe --open`, opens the settings window) and **Gander network setup (administrator)** | No |
| Autostart | A Task Scheduler task `\Gander\GanderHost`: one logon trigger for your account, `RunLevel LeastPrivilege`, interactive token, no time limit. Created by the MSI (`gander-host.exe --autostart on`) and by the Settings switch; removed by the uninstall and by switching it off. | No |
| Firewall rules | Two inbound Allow rules in the group `Gander`, created by `firewall-rules.ps1` | **Yes** (a separate, elevated step) |

### Starting Gander twice

The sign-in task starts the host, so by the time you click **Gander** in the Start menu it is
normally already running. The host keeps one copy per data directory (a named mutex in the session,
`gander_host::single_instance`): a second start prints "Gander is already running" and exits, and
when it was started with `--open` it first asks the running host to open (or focus) its window. So
the Start menu entry always lands on the settings window, whether or not the tray application was
running. Nothing is written to disk and nothing is sent over the network; hosts with a different
`--data-dir` (tests) do not see each other.

### Firewall rules: why they are a separate elevated step

Changing Windows Firewall rules needs administrator rights. A per-user MSI runs with your normal
rights, and a per-user package cannot silently run an action as the system. So the MSI does not
write rules itself and never elevates behind your back. Instead:

- At the end of a first install, the MSI runs `firewall-rules.ps1 -Action Install`. The script
  notices it is not elevated and re-launches itself with a UAC prompt (one prompt, a hidden
  window). **Decline it and the install still succeeds**; nothing is half-done.
- `msiexec /i Gander.msi NETWORKSETUP=0` skips the step entirely (no prompt), for unattended installs.
- The same step is always available later: Start menu, **Gander network setup (administrator)**, or

  ```powershell
  powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$env:LOCALAPPDATA\Programs\Gander\firewall-rules.ps1" -Action Install
  ```
  (from an already-elevated shell add `-NoElevate`).
- Uninstall runs `-Action Remove` the same way (another UAC prompt). If it is declined the files
  and the task are still removed and the rules stay; remove them later from any elevated PowerShell:

  ```powershell
  Get-NetFirewallRule -Group Gander | Remove-NetFirewallRule
  ```
- The elevated copy runs as whoever approves the prompt, so the script resolves your `config.toml`
  path and log path first and passes them down.
- Tailscale already installs its own `Tailscale-In` rule for the tailnet; Gander's rules cover the
  cases where it is not enough (the pairing spec, "Windows Firewall"): a Public adapter category, a
  dismissed first-listen prompt, or a different private network.

**Known limitation (worth a decision).** The elevated step runs a script from your user-writable
install folder. Code already running as you could change that script before you approve the UAC
prompt, and so gain administrator rights. For a single-owner machine where you are the administrator
this adds little (UAC is not a security boundary against code running as the same user), but for a
standard account whose administrator is someone else it is a real escalation path. Mitigations when
that matters: sign the scripts and enforce `AllSigned` by policy, or run the network setup once from
a copy of the scripts you placed in an administrator-only folder. The scripts do not check
themselves for tampering because a tampered script would just skip the check.

### The rules

| Rule | Protocol and port | Source (RemoteAddress) | Profiles | Program |
| --- | --- | --- | --- | --- |
| `Gander-In-TCP` | TCP, `listener.port` (default 42637) | tailnet + generic CIDRs | Private, Domain | `gander-host.exe` |
| `Gander-In-UDP` | UDP, `firewall.udp_ports` (default 50000-50099), for WebRTC host candidates | same | same | same |

- **RemoteAddress** is exactly `100.64.0.0/10`, `fd7a:115c:a1e0::/48` (Tailscale) plus every entry of
  `[listener.generic] cidrs` in `config.toml`. Never anything else.
- **The script refuses Any.** An empty list, `Any`, `*`, `LocalSubnet` and the other keywords, `/0`,
  the unspecified address, set host bits and anything broader than a private network (IPv4 shorter
  than /8, IPv6 shorter than /16) stop the install with exit 3 before any rule is touched.
- **Profiles are Private and Domain.** Public is added only when you set
  `[firewall] allow_public_profile = true` in `config.toml` (or pass `-IncludePublic`). The reason
  to: Tailscale's adapter is sometimes classified Public, and then a rule that skips Public never
  matches. The source scope is the same either way, so the rule still only admits tailnet and your
  configured networks.
- **Removal is by group.** `-Action Remove` deletes every rule in the group `Gander` and nothing else.
- **Idempotent.** Install deletes the group and recreates it from the plan (no duplicates; an
  edited rule is put back); if creating fails it removes whatever it created. Remove with nothing
  to remove succeeds. Exit codes: 0 done, 1 failed, 2 bad input or config, 3 refused, 5 not
  elevated (or the prompt was declined).

`config.toml` values the scripts read (the host validates the whole file when it starts):

```toml
[listener]
port = 42637                 # TCP rule

[listener.generic]
cidrs = ["10.147.17.0/24"]   # ZeroTier, WireGuard, ...: added to the RemoteAddress list

[firewall]
udp_ports = "50000-50099"    # WebRTC host candidates: 1024 or above, at most 1000 ports.
                             # The Stream engine (Phase 2) must bind inside this range.
allow_public_profile = false
```

After a change to `config.toml`, run the install step again.

### Check without risk

```powershell
# Prints what Install would create; changes nothing, needs no elevation
powershell.exe -NoProfile -File installer\firewall-rules.ps1 -Action Plan

# Compares the live rules with the plan; read-only
powershell.exe -NoProfile -File installer\verify-rules.ps1

# Tests the validation logic only, never looks at the firewall
powershell.exe -NoProfile -File installer\verify-rules.ps1 -SelfTest
```

## Autostart

The task definition is `autostart-task.xml`; the host fills in your SID, the path of the running
`gander-host.exe` and its folder, and registers it with `schtasks /Create /XML` as you (no
elevation). It has a 15 second logon delay, no execution time limit (the Task Scheduler default of
72 hours would kill the host), runs on battery, and refuses to start a second copy.

```powershell
gander-host.exe --autostart status      # prints "autostart: on" or "autostart: off" (reads only)
gander-host.exe --autostart on
gander-host.exe --autostart off
```

The Settings switch reports a task that was edited by hand (elevated run level, disabled, pointing
at a different copy of the program).

**Human-gated live test** (registers and deletes a throwaway task `Gander\GanderHostLiveTest`; it
never starts it and never touches the real `Gander\GanderHost`):

```powershell
cargo test -p gander-host --lib schtasks_live_roundtrip -- --ignored --nocapture
# if it is interrupted before the end:
schtasks /Delete /TN "Gander\GanderHostLiveTest" /F
```

It is the check for the one assumption that could not be measured here: that a standard,
unelevated user may register a logon-trigger task for their own account. If Windows refuses, the
Settings switch shows the error text and the Startup folder is the fallback to design.

## Release build, console and logs

The release build of `gander-host` (feature `ui`, no `test-hooks`) is a Windows GUI-subsystem
program: nothing opens a console window when Task Scheduler or the Start menu starts it. Its text
output (`println!`, `eprintln!`, panic messages, the final `Error:` line) goes:

1. to the existing pipe or file when started with redirected output (the UI smoke test and the
   harnesses read it this way, unchanged);
2. to the parent console when started from a terminal (note that `cmd` and PowerShell do not wait
   for a GUI program: use `start /wait` or `Start-Process -Wait` when the order matters);
3. otherwise to `%LOCALAPPDATA%\Gander\logs\host.log` (rotated to `host.log.1` at 2 MiB).

Debug builds, `--no-default-features` builds and `test-hooks` builds keep a console.
`cargo xtask installer-check --binary` builds the release host and tests all three cases.

## Build the MSI

The MSI is built with WiX 5 (the `wix` .NET tool, schema v4). One-time toolchain setup, from an
ordinary prompt:

```powershell
winget install --id Microsoft.DotNet.SDK.8 -e          # the .NET SDK; the installer asks for one UAC click
dotnet tool install --global wix --version 5.0.2       # WiX 5; the tool needs a .NET 6 or later runtime
wix extension add -g WixToolset.UI.wixext/5.0.2        # the licence page (WixUI_Minimal)
cargo xtask installer-build
```

Without the UAC click, the .NET SDK also installs per user (nothing machine-wide) with Microsoft's
own script: `dotnet-install.ps1 -Channel 8.0 -NoPath` from <https://dot.net/v1/dotnet-install.ps1>
installs to `%LOCALAPPDATA%\Microsoft\dotnet`. `cargo xtask` finds `dotnet.exe` there (and in
`DOTNET_ROOT`, on `PATH` and in `Program Files`) and passes it to `wix` as `DOTNET_ROOT`, and finds
`wix.exe` on `PATH`, in `%DOTNET_CLI_HOME%\.dotnet\tools`, `%USERPROFILE%\.dotnet\tools` or the real
user profile's `.dotnet\tools`. WiX 6 and 7 exist (7 has a different licence model); this source is
written for WiX 5 and `installer-check` refuses another major version.

**Toolchain recorded on the development laptop (2026-11-02).** The launcher that runs the Claude
profile overrides `HOME` and `USERPROFILE`, so the paths below are the real user's, not the
profile's: the .NET SDK 8.0.427 in `C:\Program Files\dotnet` (the winget package, machine-wide; its
UAC prompt waited about an hour for the owner, and a per-user copy from `dotnet-install.ps1` that was
installed meanwhile has been removed again), the `wix` 5.0.2 tool in
`%USERPROFILE%\.dotnet\tools\wix.exe` of the real profile (installed with
`DOTNET_CLI_HOME` set to that profile), and the global extensions `WixToolset.UI.wixext` and
`WixToolset.Util.wixext` 5.0.2 in `%USERPROFILE%\.wix\extensions` (WiX resolves that folder
from the real profile, not from `USERPROFILE`). The Util extension is installed but not used.

`cargo xtask installer-build` does, in order:

1. `cargo xtask icons --check`: the committed icons are what `brand/*.svg` produce.
2. `cargo xtask notices`: `THIRD-PARTY-NOTICES.txt` and `License.rtf` in `target/installer/`, then
   `cargo xtask sbom`: `target/sbom/gander-host.cdx.json`, validated against the CycloneDX 1.5 schema
   (the build stops if it is not valid).
3. `cargo build --locked --release -p gander-host -p gander-render`. Each executable's `build.rs`
   embeds its dependency list in a `.dep-v0` section (the `cargo auditable` format, written by hand
   in `host/build-support/auditable.rs`); the xtask then reads the section back out of both exes and
   checks that its root is the right package and that every package in it is in the SBOM.
4. `wix build -arch x64 -ext WixToolset.UI.wixext -d Version=<workspace version> -bindpath ...
   installer/main.wxs`, with the bind paths `bin` (`target/release`), `repo`, `installer`,
   `notices`, `sbom` (`target/sbom`) and `assets` (`host/gander-host/assets`) that the .wxs names as
   `!(bindpath.x)`. The
   output is `target/installer/Gander-<version>-x64.msi` (about 7.7 MiB). The version is
   `[workspace.package] version` in `Cargo.toml` and must be a valid MSI version
   (`major.minor.build`).
5. `wix msi validate -sice ICE91`: the Windows Installer ICE rules. ICE91 is silenced because it
   only matters for a per-machine install of a package that writes to a user folder, and this
   package is per-user only.
6. An administrative extract: `msiexec /a <msi> /qn TARGETDIR=<temporary folder>` copies the files
   out and writes nothing else (no registry, shortcuts, custom actions, tasks or rules), then the
   xtask lists them and checks every file the .wxs names is there and not empty. It needs no
   elevation. To look yourself: `msiexec /a Gander-<version>-x64.msi /qn TARGETDIR=C:\temp\gander-extract`.

`main.wxs` is per-user (`Scope="perUser"`), x64 only, checks for Windows 10 1809 or later (build 17763, read from `CurrentBuildNumber` in the
registry, because Windows Installer reports Windows 8.1 to every package), upgrades
in place (`UpgradeCode` `D86137D4-...`, pinned by `installer-check`; the component GUIDs that
hold files are fixed on purpose and must never change, WiX refuses automatic GUIDs for a component
with a file and a registry key path), and shows the licence page (`WixUI_Minimal`).

Properties: `NETWORKSETUP=0` skips the firewall step, `AUTOSTART=0` skips the sign-in task.
Upgrades keep your autostart and firewall choices (the setup actions run on first install only).

### Signing (human step)

The build is **unsigned**. Windows will show "Unknown publisher" and SmartScreen may warn until it
is signed and has reputation. Signing needs a code-signing certificate, which is the owner's to
obtain and keep (an OV or EV certificate, or Azure Trusted Signing); never commit the key. Sign in
this order, with a timestamp so signatures outlive the certificate:

```powershell
cargo build --release -p gander-host -p gander-render
signtool sign /fd SHA256 /tr http://timestamp.digicert.com /td SHA256 /a target\release\gander-host.exe target\release\gander-render.exe
cargo xtask installer-build                          # cargo sees nothing to rebuild, so it packs the signed binaries
signtool sign /fd SHA256 /tr http://timestamp.digicert.com /td SHA256 /a target\installer\Gander-<version>-x64.msi
signtool verify /pa /v target\installer\Gander-<version>-x64.msi
```

(Replace the timestamp URL and certificate selection with your provider's.) Also sign the `.ps1`
files with `Set-AuthenticodeSignature` before packing if you want `AllSigned` machines to run them;
the MSI starts them with `-ExecutionPolicy Bypass`, which a Group Policy `AllSigned` or
`RemoteSigned` setting overrides.

## Human-gated install test

In Windows Sandbox (no Tailscale in there, so only the install path is exercised) or on this
machine. Nothing below has been run; the MSI was built, validated and extracted, never installed.

1. `cargo xtask installer-build`, then install the MSI from `target\installer\`. Expect: no UAC
   prompt for the install itself, files under `%LOCALAPPDATA%\Programs\Gander`, a Start menu folder
   with **Gander** and **Gander network setup (administrator)**, the lens icon in Settings, Apps,
   one UAC prompt for the network setup (accept it).
2. `gander-host.exe --autostart status` says `on`; `schtasks /Query /TN "Gander\GanderHost" /XML`
   shows `LeastPrivilege` and your SID.
3. `powershell.exe -NoProfile -File "$env:LOCALAPPDATA\Programs\Gander\verify-rules.ps1"` passes
   every check.
4. Click **Gander** in the Start menu: the settings window opens (a second tray icon must not
   appear; the running host shows its window). Sign out and in: the tray icon appears;
   `%LOCALAPPDATA%\Gander\logs\host.log` has the start line.
5. Uninstall (Settings, Apps). Accept the UAC prompt. Then: the task is gone
   (`schtasks /Query /TN "Gander\GanderHost"` fails), `Get-NetFirewallRule -Group Gander` finds
   nothing, `%LOCALAPPDATA%\Programs\Gander` is gone. `%APPDATA%\Gander` (configuration, paired
   devices) and `%LOCALAPPDATA%\Gander` (host key, logs, caches) are kept on purpose; delete them for
   a full wipe.
6. Repeat the install with `NETWORKSETUP=0` and confirm there is no prompt and no rule.

## What `cargo xtask installer-check` checks

`main.wxs` is well-formed WiX 5 source with the expected feature, component, file, custom action
and icon ids, unique ids, resolving references, per-user only (no HKLM, no machine folders, no custom
action that runs without impersonation or can fail the install), a fixed GUID on every component
with a file, `Custom` conditions as attributes, only the bind paths the build passes, every
committed source file present, the UpgradeCode unchanged, and a Start menu entry that runs
`gander-host.exe --open`; `autostart-task.xml` is a least-privilege logon task with its
placeholders; every `*.ps1` is ASCII, free of PowerShell 7-only names, parses under Windows
PowerShell 5.1 and `pwsh`, and `verify-rules.ps1 -SelfTest` passes under both (CIDR and port
validation, `config.toml` reading, the plan, refusing Any); `gander-host` embeds its icon and has no
cargo-wix metadata left and both build scripts embed the dependency list; `cargo xtask notices` runs
and ships the font and Boost texts; `cargo xtask sbom` writes a schema-valid CycloneDX 1.5 SBOM. With the
WiX 5 tool present it also runs a dry `wix build` of the real .wxs against stand-in executables
(seconds, no release build needed), `wix msi validate` and the administrative extract; without it
the line reads `[DEFER] MSI build`. `--binary` adds the release-host checks (including that the exe's
`.dep-v0` list matches the SBOM). None of it touches the
firewall, Task Scheduler or the registry, and none of it installs anything.
