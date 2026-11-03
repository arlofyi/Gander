// SPDX-License-Identifier: Apache-2.0
//! Exports `NvOptimusEnablement` from the spike binary (see src/main.rs and the WP-42 findings:
//! without it the NVIDIA encoder MFT does not activate on this Optimus laptop).

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
    {
        println!("cargo:rustc-link-arg-bin=spike-input=/EXPORT:NvOptimusEnablement");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
