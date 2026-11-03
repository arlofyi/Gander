// SPDX-License-Identifier: Apache-2.0
//! Exports the GPU-selection symbols (`NvOptimusEnablement`, `AmdPowerXpressRequestHighPerformance`,
//! defined in `src/win/optimus.rs`) from this crate's integration-test and example executables, so
//! the loopback tests and `examples/gpu_probe.rs` can activate the NVIDIA H.264 MFT on an Optimus
//! laptop like the host binary does (WP-42 finding: without the export the MFT fails with
//! 0x8000FFFF). The values start at 0; `optimus::GpuDecision` raises them while an encoder is set
//! up. The host binary gets the same export from `host/gander-host/build.rs`.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
    {
        for kind in ["tests", "examples"] {
            println!("cargo:rustc-link-arg-{kind}=/EXPORT:NvOptimusEnablement");
            println!("cargo:rustc-link-arg-{kind}=/EXPORT:AmdPowerXpressRequestHighPerformance");
        }
    }
    println!("cargo:rerun-if-changed=build.rs");
}
