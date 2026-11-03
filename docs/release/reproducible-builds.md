# Supply chain and reproducible builds (WP-82)

What a release of Gander carries to prove what is inside it, how to rebuild it, and what does not
yet rebuild to the same bytes. Written 2026-11-02 from measurements on the Windows dev laptop
(rustc 1.96.1, MSVC 14.44.35207, Windows SDK 10.0.26100.0, WiX 5.0.2, .NET SDK 8.0.427, Gradle 9.6.0,
AGP 9.5.0, JDK 17). Where something was not measured it says so.

## 1. What ships with each artefact

| Artefact | Dependency list | SBOM | Licence texts |
| --- | --- | --- | --- |
| `gander-host.exe`, `gander-render.exe` (in the MSI) | Embedded in each exe: a `.dep-v0` section, `cargo auditable` format | `sbom\gander-host.cdx.json` installed beside them (CycloneDX 1.5) | `THIRD-PARTY-NOTICES.txt` |
| Android APK | Not embedded (Android has no equivalent) | `gander-android.cdx.json`, a CI artefact (CycloneDX 1.5) | not generated yet (WP-83/84) |

The SBOMs are meant to be attached to a release next to the MSI and the APK so a scanner (Grype,
OSV-Scanner, Dependency-Track) can check them against advisories without the source tree.

## 2. SBOMs

### Host: `cargo xtask sbom`

```
cargo xtask sbom [--out-dir <dir>]        # writes target/sbom/gander-host.cdx.json
cargo xtask sbom --check [<file>...]      # validates SBOM files (default target/sbom/*.cdx.json)
```

Plain Rust (`xtask/src/sbom.rs`), no tool to install. Inputs: `cargo metadata --locked
--filter-platform x86_64-pc-windows-msvc` (names, versions, licences, repository links) and
`Cargo.lock` (the SHA-256 of every `.crate` file, which Cargo itself verifies on download).

- Covers the dependency closure of `gander-host` and `gander-render`: 349 third-party crates and 4
  first-party ones (measured 2026-11-02, 416 KB). The product component is `gander-host`; the render
  worker is a component it depends on, because the host starts it beside itself.
- Crates linked into the binaries are `scope: required`. Crates needed only to build them
  (build-script dependencies, procedural macros and what those use; 49 of the 349) are
  `scope: excluded` with the property `gander:cargo-kind = build`. They are in the build, not in the
  product, but a compromise of one still matters, so they are listed rather than dropped.
- libopus is C inside the `opusic-sys` crate, so it is one component there (`pkg:cargo/opusic-sys@0.7.6`,
  BSD-3-Clause), not a separate C-library entry. The `cmake` crate that drives its build is a
  `scope: excluded` build component like the other build-script dependencies.
- Dev-dependencies (tests) are not included. Platform: Windows x64 only, like the installer.
- Every registry component has a `pkg:cargo/<name>@<version>` purl, a licence (an SPDX id the schema
  knows, otherwise an SPDX expression) and a SHA-256. `cargo xtask sbom` fails if one lacks them.
- Validated against the CycloneDX 1.5 JSON schema before it is written: the command refuses to write
  an invalid file. The schema files (`bom-1.5`, `spdx`, `jsf-0.82`) are vendored unmodified in
  `xtask/schemas/cyclonedx-1.5/` (from the `1.5` tag of the CycloneDX specification repository,
  Apache-2.0) and checked by a small validator of our own (`xtask/src/jsonschema.rs`, the keywords
  that schema uses, with unit tests that break the document in a dozen ways). No schema-validator
  crate was added: a supply-chain tool should not pull dozens of new crates.
- Deterministic: sorted, serial number derived from the content, timestamp from `SOURCE_DATE_EPOCH`
  if set, otherwise the time of the HEAD commit (the wall clock only outside a git checkout). Two
  runs on one commit give the same file. The `rustc` tool entry records the compiler that was found.
- The MSI installs it as `sbom\gander-host.cdx.json`; `cargo xtask installer-build` generates it
  first, and `installer-check` generates and validates it on every run (the dry-run MSI includes it).

### Android: `./gradlew --no-daemon cyclonedxSbom`

From `client/android` (with `GRADLE_USER_HOME` set as in `CLAUDE.md`). Uses the CycloneDX Gradle
plugin `org.cyclonedx.bom` 3.5.1 (Apache-2.0, pinned in `gradle/libs.versions.toml`; built with
Gradle 9.8, runs on 9.6.0 and with the configuration cache on). Configured in the root
`build.gradle.kts`, so the app module's build file is untouched. It resolves
`releaseRuntimeClasspath`, the closure that ships in the release APK; test configurations and the
build's own plugins are excluded. Output: `target/sbom/gander-android.cdx.json` (CycloneDX 1.5, 228
components, 495 KB, 2026-11-02), validated in CI with `cargo xtask sbom --check`.

- 148 of the 228 components (the `aar` and `jar` artefacts) carry SHA-1 to SHA-512 hashes and a
  licence; the 80 `type=pom` entries are umbrella coordinates (Compose, AndroidX multiplatform) with
  a licence and no artefact of their own to hash.
- Not deterministic: the plugin writes a random serial number and the current time. The component
  list is deterministic for a given `libs.versions.toml`.
- The plugin prints "Configuration 'releaseRuntimeClasspath' was resolved during configuration
  time". That is the plugin's design (a Gradle performance note, not a failure).

## 3. Auditable binaries without `cargo auditable`

`cargo auditable` was not installed and installing tools is not allowed here, so the same artefact is
produced by hand. Each exe contains a section named `.dep-v0` holding its dependency list as JSON
compressed with zlib: `{"format":0,"packages":[{"name","version","source","kind"?,"dependencies"?,
"root"?}...]}` (the `auditable-serde` "format 0": `source` is `crates.io`, `registry`, `git`,
`local` or `other`; `kind` is `"build"` for a package only needed to build; `dependencies` are
indices into the sorted `packages`; `root` marks the binary's own package).

How it gets there (`host/build-support/auditable.rs`, included by both `build.rs` files and by
xtask, so the exe and the SBOM come from one walk of the dependency graph):

1. `build.rs` runs `cargo metadata --locked --offline --filter-platform <target>`, walks the normal and
   build dependencies from the binary's package (dev-dependencies excluded; procedural macros are
   `build`), and compresses the JSON.
2. It writes a 100-byte-overhead COFF object (one read-only data section `.dep-v0`, one symbol, no
   timestamp) to `OUT_DIR` and passes it to the linker with `cargo:rustc-link-arg-bin=...`.
3. MSVC `link.exe` keeps every non-COMDAT section of an object it is handed, and `/OPT:REF` does not
   drop it, so the section lands in the PE image (measured: 4,983 bytes compressed, 33 KB of JSON,
   348 packages in the host; 1,345 bytes and 91 packages in the render worker).

`cargo xtask installer-build` reads the section back out of both finished exes and fails unless the
root package is right, every dependency index is in range and every package is also in the SBOM
(`installer-check --binary` does the same for the release host).

Limits, stated plainly:

- Windows MSVC (x64, ARM64) only. Other targets get no section, by design (WP-63/64 will need an
  ELF and Mach-O writer, or the real `cargo auditable`).
- The real tool wraps `rustc` and sees the exact feature set of each compilation. A build script
  can only ask `cargo metadata`, which unifies features across the whole workspace, so the list is
  never smaller than what is linked and may name a crate that a `-p` build leaves out.
- It was checked with our own reader (xtask and a throwaway script), not with `rust-audit-info` or
  `cargo audit bin`, which are not installed. The format follows the published `auditable-serde`
  layout. Before the first public release, run `rust-audit-info gander-host.exe` and
  `cargo audit bin gander-host.exe` once (installing them is a human step) and fix any difference.
  If the real tool is installed on the release machine, `cargo auditable build` replaces this and the
  `build.rs` calls can go.
- If `cargo metadata` fails inside `build.rs`, the build continues with a `cargo:warning` and no
  section; the installer build then fails on the missing section instead of shipping without it.

## 4. Dependabot

`.github/dependabot.yml`: weekly (Monday), grouped, Conventional-Commit prefixes (`build(deps)`,
`ci(deps)`):

| Ecosystem | Directory | Grouping |
| --- | --- | --- |
| cargo | `/` (host, xtask and the Rust spikes share one lock file) | one group for minor and patch; majors one PR each |
| gradle | `/client/android` (wrapper, version catalogue, plugins) | AndroidX, Kotlin plus KSP, everything else minor and patch |
| gradle | `/spikes/files-app`, `/spikes/stream/android` | one group |
| github-actions | `/` (actions are pinned to commit SHAs with the tag in a comment; Dependabot bumps both) | one group |

Lines that are blocked on purpose are in `ignore` with the reason: eframe and egui 0.34+ (need Rust
1.92, above the MSRV of 1.90) and the AndroidX, Compose, Coil and OkHttp lines that need compileSdk
37. Security advisories open their own PRs outside the schedule. Dependabot does not bump the Rust
toolchain, cargo-deny (`CARGO_DENY_VERSION` in `host-windows.yml`) or the NDK package in
`client-android.yml`; those stay manual.

## 5. Reproducible builds

### Pinned inputs

| Input | State | Where |
| --- | --- | --- |
| Rust dependencies | Pinned by the committed `Cargo.lock` (every crate has a checksum). `installer-build` and `installer-check --binary` now build with `--locked`, so a stale lock file fails the build. | `Cargo.lock` |
| Rust toolchain | **Not pinned in the repository.** `rust-version = "1.90"` is the minimum; CI installs `stable`. The release build of record used rustc 1.96.1 (`31fca3adb 2026-06-26`) with the `x86_64-pc-windows-msvc` target. See below. | `Cargo.toml`, `host-windows.yml` |
| MSVC and Windows SDK | Not pinned. Measured: MSVC 14.44.35207 (VS 2022 Build Tools), Windows SDK 10.0.26100.0 for `rc.exe`. A different linker version gives different bytes. | machine notes in `CLAUDE.md` |
| CMake and libopus | CMake is **not pinned**: `opusic-sys` needs 3.16 or newer to build the bundled libopus (measured with 4.4.4, installed machine-wide with `winget install --id Kitware.CMake -e` into `C:\Program Files\CMake\bin`; the `windows-latest` runner image brings its own). The libopus **source** is pinned: it is the C code inside the `opusic-sys` 0.7.6 crate (libopus 1.6.1 plus that crate's patch), covered by the `Cargo.lock` checksum. It is compiled by `cl.exe` through CMake (no NASM, clang or bindgen) and linked statically into the host. | `Cargo.toml`, `host-windows.yml` |
| WiX | 5.0.2 plus `WixToolset.UI.wixext` 5.0.2, on .NET SDK 8.0.427. Checked by `installer-check` (major version). | `installer/README.md` |
| Gradle | Distribution pinned by `distributionSha256Sum` (`bbaeb2fe...`), which matches the checksum Gradle publishes for 9.6.0. The committed `gradle-wrapper.jar` has SHA-256 `497c8c2a...`, which matches Gradle's published wrapper checksum (verified 2026-11-02, and re-verified by a CI step in `client-android.yml` on every run). | `gradle-wrapper.properties` |
| Android dependencies | Exact versions in `libs.versions.toml` (no dynamic versions). Gradle does **not** verify their checksums: there is no `gradle/verification-metadata.xml`. Adding one (`./gradlew --write-verification-metadata sha256 assembleDebug testDebugUnitTest lintDebug`) is the next step and would make the dependency downloads tamper-evident. | `client/android/gradle/` |
| Build tool versions in CI | Actions pinned to commit SHAs. cargo-deny pinned (`0.20.2`). | `.github/workflows/` |

**Why there is no `rust-toolchain.toml`.** The dev laptop has one toolchain installed, named
`stable`. A `rust-toolchain.toml` with `channel = "1.96.1"` names a different toolchain, which rustup
would have to download before any `cargo` command in the repository ran (and CI would need the same).
That is an install, which this work package may not do, and it would break every existing shell
until it finished. When the owner is ready to pin, add this file at the repository root and
install the toolchain once with `rustup toolchain install 1.96.1`:

```toml
[toolchain]
channel = "1.96.1"
components = ["rustfmt", "clippy"]
targets = ["x86_64-pc-windows-msvc"]
profile = "minimal"
```

and in `host-windows.yml` replace `rustup toolchain install stable ...` with
`rustup toolchain install 1.96.1 --profile minimal --component rustfmt --component clippy` (or just
run `rustup show`, which installs what the file names). Bump it deliberately, in its own commit.

### `SOURCE_DATE_EPOCH`

The convention for "the time of this build is the time of the source": set it to the commit time
before building anything of record.

```
$env:SOURCE_DATE_EPOCH = (git log -1 --format=%ct)      # PowerShell
export SOURCE_DATE_EPOCH=$(git log -1 --format=%ct)     # bash
```

What honours it today: `cargo xtask sbom` (its timestamp; without the variable it uses the HEAD
commit time anyway). What does not need it: `rustc` (it does not embed a clock) and MSVC
`link.exe` with `/Brepro` (no link time). What does not honour it: `wix build` (the MSI and its cabinet carry build-time stamps) and the
CycloneDX Gradle plugin (current time, random serial number).

### What is reproducible, and what is not (measured 2026-11-02 unless noted)

| Thing | Reproducible? | Detail |
| --- | --- | --- |
| Host SBOM, `.dep-v0` payload | Yes | Pure functions of `Cargo.lock`, the manifests and the commit time; unit-tested for equal bytes on equal input. |
| Rust code generation | Yes (measured) | `codegen-units = 1` and thin LTO are deterministic here, and no `build.rs` reads a clock (the resource script and the COFF object carry no timestamp). |
| `gander-*.exe` bytes | **Yes, across directories** (measured 2026-11-03) | `cargo xtask installer-build` in the worktree and again in a fresh `git clone` of the same commit at a different path (no spaces, different depth) gave byte-identical `gander-host.exe` (SHA-256 `257b220b...`, 20.9 MB) and `gander-render.exe` (`f3f9d05c...`, 2.5 MB); a `cargo clean --release` rebuild in the same directory did too. Two things made it so: release links of both binaries add `/Brepro` (their `build.rs`, release profile only), which turns the PE `TimeDateStamp`, the debug-directory stamps and the PDB GUID into hashes of the content (on 2026-11-02 those were the only differing bytes, 23 and 24), and the path remapping below. Same machine, toolchain and user both times; two machines not shown yet. |
| Absolute paths inside the exes | **None** (measured 2026-11-03) | Before: the release `gander-host.exe` named the user profile 960 times and the workspace 2 times, `gander-render.exe` the profile 121 times (panic locations of every registry crate under `<cargo home>\registry\src`, and the `include!`d generated bindings of `glutin_*_sys` under `target\release\build`; workspace crates are already relative). After: 0 and 0 in both, counted as 8-bit and UTF-16LE text, any case, either separator. `installer-build` and `installer-check --binary` fail if a count is not 0. |
| libopus (C, built through CMake) | Not measured | The static library is compiled with `cl.exe` during the Cargo build; its `__FILE__` strings are trimmed (see Path remapping), but whether two builds give the same object bytes has not been checked. The MSVC `/Brepro` question above applies to it as well. |
| MSI | **No, by design today** | Every `wix build` generates a new `ProductCode` and `PackageCode` and stamps the creation time (measured on two builds of the same exes: `{DB6AF322-...}` and `{A7091604-...}`; the bytes differ almost everywhere because the cabinet is compressed around those and the exe stamps). The `UpgradeCode` is fixed (pinned by `installer-check`). Compare MSIs by their extracted files (`msiexec /a`), not by their bytes. Making the `ProductCode` a function of the version is possible but is a product decision (it decides how upgrades are detected). Signing (WP-81) adds a timestamp by design. |
| Android APK | Not measured | AGP packages deterministically in principle; R8 and resource shrinking are the usual sources of drift. Compare two clean `assembleRelease` builds with `apkdiff` or by unzipping before claiming anything. |
| Android SBOM | Component list yes; file no | Random serial number and current timestamp from the plugin. |

### Path remapping (`xtask/src/repro.rs`)

The release builds of `cargo xtask installer-build` and `cargo xtask installer-check --binary`
pass, in `CARGO_ENCODED_RUSTFLAGS` (0x1F-separated, because a workspace path may contain spaces):

| Local folder (computed when the build runs) | Becomes |
| --- | --- |
| `RUSTUP_HOME` (else `~/.rustup`) | `/rustup` |
| `rustc --print sysroot` | `/rustc-sysroot` |
| `CARGO_HOME` (else `~/.cargo`) | `/cargo` |
| the workspace root (and so `target/`) | `/gander` |

rustc lets the last matching flag win, so the order is most general first. No tracked file names a
real folder. Consequences:

- C code compiled by build scripts (the bundled libopus, through `cc` and CMake) is not covered by
  rustc's remap, and libopus puts `__FILE__` in its assertion messages (260 registry paths in the
  host on 2026-11-03). The same builds therefore set `CFLAGS_x86_64_pc_windows_msvc` to one MSVC
  `/d1trimfile:<folder>\` per folder above, which strips it from `__FILE__` (the path is left
  relative). `cc` splits that variable on white space, so a folder whose path has a space is
  skipped; the workspace is, but no C file lives there. The path check after the build catches
  anything left.

- A panic message in a release build names `/cargo\registry\src\index.crates.io-...\<crate>\src\x.rs`
  instead of the builder's real folder; workspace crates were already relative (`host\...`).
- `RUSTFLAGS` or `CARGO_ENCODED_RUSTFLAGS` already set are kept (put in front); `RUSTFLAGS` itself
  is cleared for that one command, since cargo ignores it when the encoded form is set.
- The flags change cargo's fingerprint, so a plain `cargo build --release` and an `installer-build`
  rebuild each other's release crates. Use the xtask for anything of record. Dev builds are not
  affected (no remap, no `/Brepro`), and neither is `cargo xtask build-host --release` (a local
  convenience build).

### Rebuilding a release from source

1. Check out the tag; make sure the tree is clean.
2. Same toolchain as the release notes record (`rustc -vV`; the SBOM's `rustc` tool entry also has it),
   MSVC build tools and WiX as in the table above.
3. `SOURCE_DATE_EPOCH` as above. Build with `cargo xtask installer-build` (it builds with `--locked`).
4. Compare `target/sbom/gander-host.cdx.json` with the released one (must be identical); compare the
   exes' packages with `cargo xtask installer-check --binary` (the `.dep-v0` list is checked against the
   SBOM); compare the MSI by its extracted files until the items above are fixed.
5. Android: `cd client/android && ./gradlew --no-daemon assembleRelease cyclonedxSbom`; compare the
   component list of `gander-android.cdx.json` with the released one.

### Next steps (not done here)

- Two clean release builds on two machines (same toolchain) to prove the exes beyond one machine.
- `rust-toolchain.toml` and the CI toolchain pin (needs the owner to install the toolchain).
- `gradle/verification-metadata.xml` for the Android dependencies.
- Run `rust-audit-info` and `cargo audit bin` on the release exes once.
- Sign the SBOMs with the release (WP-81), or publish their SHA-256 next to them.
