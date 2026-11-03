// SPDX-License-Identifier: Apache-2.0
//! Exports `NvOptimusEnablement` from the spike binary (see src/main.rs).

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
    {
        println!("cargo:rustc-link-arg-bin=spike-encode=/EXPORT:NvOptimusEnablement");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
