// SPDX-License-Identifier: Apache-2.0
//! Build script of the render worker: embeds the dependency list of `gander-render.exe` (the
//! `cargo auditable` format, section `.dep-v0`; WP-82) so scanners can audit the exe without the
//! source. The code is shared with `gander-host` and `cargo xtask sbom`:
//! `host/build-support/auditable.rs`; see `docs/release/reproducible-builds.md`. Release builds
//! with MSVC also link with `/Brepro` (no time stamp in the PE header), for reproducible builds.

#[path = "../build-support/auditable.rs"]
mod auditable;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    auditable::embed("gander-render");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
        && std::env::var("PROFILE").as_deref() == Ok("release")
    {
        println!("cargo:rustc-link-arg-bin=gander-render=/Brepro");
    }
}
