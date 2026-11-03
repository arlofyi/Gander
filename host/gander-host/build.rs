// SPDX-License-Identifier: Apache-2.0
//! Build script of the host binary:
//!
//! 1. Exports the GPU-selection symbols (WP-50; defined in `gander_stream::win::optimus`). Without
//!    `NvOptimusEnablement` exported, the NVIDIA H.264 encoder MFT does not activate on an Optimus
//!    laptop (WP-42 finding, `0x8000FFFF`). AMD's `AmdPowerXpressRequestHighPerformance` is exported
//!    for the same reason (not measured). Their values start at 0 and are raised only while the
//!    discrete GPU is wanted (encoder setup, `[ui] gpu = "discrete"`), so the settings window
//!    draws on the integrated GPU by default (`docs/specs/2026-11-03-igpu-ui-findings.md`).
//! 2. Delay-loads the graphics, media and accessibility DLLs (host footprint, 2026-11-02): the
//!    tray-only host never calls into them, so they load on the first window (OpenGL, DWM,
//!    UI Automation) or the first stream (D3D11, DXGI, Media Foundation) instead of at start. All of
//!    them ship with every supported Windows; `delayimp.lib` comes with the MSVC toolset.
//! 3. Embeds the Gander icon and the version information into `gander-host.exe` (Windows, MSVC
//!    only). `assets/gander.ico` is generated from `brand/` by `cargo xtask icons` and committed;
//!    compiling the resource needs `rc.exe` from the Windows SDK, which the Visual Studio Build
//!    Tools install (found through the registry, so it need not be on `PATH`).
//! 4. Release builds link with `/Brepro`: the PE time stamp and debug-directory stamps become a
//!    hash of the image, so two builds of the same source can be byte-identical (with the path
//!    remapping of `cargo xtask installer-build`; `docs/release/reproducible-builds.md`).
//! 5. Embeds the dependency list (`cargo auditable` format, section `.dep-v0`; WP-82) so scanners
//!    can audit the exe without the source: `host/build-support/auditable.rs`, shared with
//!    `gander-render` and `cargo xtask sbom`; see `docs/release/reproducible-builds.md`.

// The dependency-list writer is shared with gander-render/build.rs and xtask.
#[path = "../build-support/auditable.rs"]
mod auditable;

/// DLLs the host loads on first use rather than at start.
const DELAY_LOAD: [&str; 6] = [
    "opengl32.dll",
    "d3d11.dll",
    "dxgi.dll",
    "mfplat.dll",
    "uiautomationcore.dll",
    "dwmapi.dll",
];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    auditable::embed("gander-host");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
    {
        println!("cargo:rustc-link-arg-bin=gander-host=/EXPORT:NvOptimusEnablement");
        println!(
            "cargo:rustc-link-arg-bin=gander-host=/EXPORT:AmdPowerXpressRequestHighPerformance"
        );
        for dll in DELAY_LOAD {
            println!("cargo:rustc-link-arg-bin=gander-host=/DELAYLOAD:{dll}");
        }
        println!("cargo:rustc-link-arg-bin=gander-host=delayimp.lib");
        if std::env::var("PROFILE").as_deref() == Ok("release") {
            println!("cargo:rustc-link-arg-bin=gander-host=/Brepro");
        }
        #[cfg(windows)]
        embed_resources();
    }
}

/// The resource script: the icon (resource id 1, the one Explorer shows) and the version block.
fn resource_script(icon: &str, version: &str) -> String {
    let mut parts = version
        .split(|c: char| !c.is_ascii_digit())
        .map(|p| p.parse::<u16>().unwrap_or(0));
    let numbers = [
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        0,
    ];
    let file_version = format!(
        "{},{},{},{}",
        numbers[0], numbers[1], numbers[2], numbers[3]
    );
    format!(
        r#"1 ICON "{icon}"

1 VERSIONINFO
FILEVERSION {file_version}
PRODUCTVERSION {file_version}
FILEFLAGSMASK 0x3fL
FILEFLAGS 0x0L
FILEOS 0x40004L
FILETYPE 0x1L
FILESUBTYPE 0x0L
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904b0"
        BEGIN
            VALUE "CompanyName", "Arlo"
            VALUE "FileDescription", "Gander host"
            VALUE "FileVersion", "{version}"
            VALUE "InternalName", "gander-host"
            VALUE "LegalCopyright", "Apache-2.0"
            VALUE "OriginalFilename", "gander-host.exe"
            VALUE "ProductName", "Gander"
            VALUE "ProductVersion", "{version}"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x409, 1200
    END
END
"#
    )
}

#[cfg(windows)]
fn embed_resources() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    let out_dir = std::env::var("OUT_DIR").expect("cargo sets OUT_DIR");
    let icon = format!(
        "{}/assets/gander.ico",
        manifest_dir.replace(std::path::MAIN_SEPARATOR, "/")
    );
    println!("cargo:rerun-if-changed=assets/gander.ico");
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_owned());
    let script = std::path::Path::new(&out_dir).join("gander-host.rc");
    std::fs::write(&script, resource_script(&icon, &version)).expect("write the resource script");
    // Only the host binary gets the resource (not the library's unit-test executables).
    embed_resource::compile_for(&script, ["gander-host"], embed_resource::NONE)
        .manifest_required()
        .expect("compile the icon and version resource (rc.exe from the Windows SDK is required)");
}
