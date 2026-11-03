// SPDX-License-Identifier: Apache-2.0
//! Repository automation for Gander, run as `cargo xtask <task>` (see `.cargo/config.toml`).
//!
//! Plain Rust on purpose: the same tasks run on Windows (developer machines, CI) and Linux
//! (Android CI) with no shell-script dependency.

mod docs;
mod e2e;
mod e2e_android;
mod e2e_stream;
mod elf;
mod host;
mod icons;
mod installer;
mod jsonschema;
mod mockups;
mod notices;
mod preflight;
mod proc;
mod repro;
mod sbom;
mod testall;
mod vectors;

use std::process::ExitCode;

const USAGE: &str = "\
cargo xtask <task>

Tasks:
  preflight [--report <path>]       check the toolchain (master plan 1b); exit 1 on a hard failure
  test-all [--skip-deny] [--skip-android]
                                    fmt, clippy, test, deny, vectors-check, installer-check, docs-check, Android unit tests
  build-host [--release]            build gander-host AND gander-render (previews need the worker
                                    beside the host; `cargo run -p gander-host` does not build it)
  run-host [--release] [-- <args>]  build both as above, then run the host with <args>
  vectors-check [dir]               parse every JSON file under protocol/test-vectors/
  check-elf-alignment <path>...     verify .so files in an APK, directory or file are 16 KB aligned
  e2e-host [--no-build]             start a loopback test host on a fixture share, pair, run the
                                    HTTP conformance suite; writes target/e2e/report.json
  e2e-android [--no-build] [--address <a>] [--adb-reverse]
                                    boot the `gander` AVD, run the instrumented suite against a
                                    loopback test host; JUnit XML in target/e2e/android/
  e2e-stream [--no-build] [--android] [--skip-engine]
                                    Stream mode end to end: loopback test host + gander-testwin, the
                                    loopback viewer, ffmpeg decode, tap, keepalive, pause, lock, close,
                                    kill switch, plus the in-process engine stage; --android also runs
                                    StreamViewerFixtureTest on the `gander` AVD; writes
                                    target/e2e/stream/report.json
  icons [--check]                   render brand/*.svg to the committed PNG and ICO assets (resvg);
                                    --check writes nothing and exits 1 if a committed file is stale
  mockups [--browser <path>] [--only <id>[,<id>]] [--scale <n>] [--jobs <n>] [--allow-fallback-fonts]
                                    render every screen of docs/design/mockups/index.html to PNG (light and
                                    dark) with headless Edge or another Chromium into
                                    docs/design/mockups/png/<group>/<id>-<theme>.png, and write the index
                                    docs/design/mockups/README.md from the page's screen registry
  installer-check [--binary]        validate the MSI source (WiX 5), the autostart task and the installer
                                    PowerShell scripts (5.1 and 7 parse, pure self-test), the notices;
                                    with the wix tool present also a dry `wix build`, `wix msi validate`
                                    and an administrative extract; --binary also builds and checks the
                                    release host (GUI subsystem, output to pipe and to host.log)
  installer-build                   check the icons, regenerate the notices, build the release binaries,
                                    `wix build` target/installer/Gander-<version>-x64.msi, validate it and
                                    list it with an administrative extract (needs the .NET SDK, wix 5.0.x
                                    and WixToolset.UI.wixext; exits 1 and says how to install them if absent)
  notices [--out-dir <dir>]         write THIRD-PARTY-NOTICES.txt and License.rtf (default target/installer/)
  sbom [--out-dir <dir>]            write the CycloneDX 1.5 SBOM of the host payload (default target/sbom/
                                    gander-host.cdx.json), validated against the CycloneDX schema
  sbom --check [<file>...]          validate SBOM files (default: target/sbom/*.cdx.json), for example the
                                    Android one from `./gradlew cyclonedxSbom`
  docs-check [--root <dir>]         check that every relative link, image and #anchor in the Markdown
                                    files resolves (case-sensitively); exit 1 and list the broken ones
  help                              show this message
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("preflight") => preflight::run(&args[1..]),
        Some("test-all") => testall::run(&args[1..]),
        Some("vectors-check") => vectors::run(&args[1..]),
        Some("build-host") => host::run_build(&args[1..]),
        Some("run-host") => host::run_host(&args[1..]),
        Some("check-elf-alignment") => elf::run(&args[1..]),
        Some("e2e-host") => e2e::run(&args[1..]),
        Some("e2e-android") => e2e_android::run(&args[1..]),
        Some("e2e-stream") => e2e_stream::run(&args[1..]),
        Some("icons") => icons::run(&args[1..]),
        Some("mockups") => mockups::run(&args[1..]),
        Some("installer-check") => installer::run_check(&args[1..]),
        Some("installer-build") => installer::run_build(&args[1..]),
        Some("notices") => notices::run(&args[1..]),
        Some("sbom") => sbom::run(&args[1..]),
        Some("docs-check") => docs::run(&args[1..]),
        Some("help" | "--help" | "-h") | None => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Some(other) => {
            eprintln!("unknown task `{other}`\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(2)
        }
    }
}
