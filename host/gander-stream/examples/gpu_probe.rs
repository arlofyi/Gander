// SPDX-License-Identifier: Apache-2.0
//! Hybrid-GPU probe (`docs/specs/2026-11-03-igpu-ui-findings.md`): shows that the exported
//! `NvOptimusEnablement` value is read at run time. For "no preference" (0) and "discrete" (1) it
//! prints the DXGI adapter order and whether each hardware H.264 encoder MFT activates on a
//! device of its own adapter. Read only: changes no setting, opens no window.
//!
//! ```text
//! cargo run -p gander-stream --example gpu_probe
//! ```
//!
//! The example executable exports the symbols like the host does (`build.rs`).

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use gander_stream::win::optimus::GpuDecision;
    use gander_stream::win::{d3d, encoder, mft};

    mft::startup()?;
    for discrete in [false, true, false] {
        let _decision = GpuDecision::take(discrete);
        println!("NvOptimusEnablement = {}", u8::from(discrete));
        let adapters = d3d::adapters()?;
        for a in &adapters {
            println!(
                "  adapter {} {} (vendor {:04X}, LUID {:X}, {} output(s))",
                a.info.index,
                a.info.name,
                a.info.vendor_id,
                a.info.luid,
                a.info.outputs.len()
            );
        }
        for a in &adapters {
            for e in mft::hardware_encoders(mft::Codec::H264, Some(a.info.luid))? {
                let device = d3d::create_device(Some(&a.adapter))?;
                let cfg = encoder::Config::new(mft::Codec::H264, 640, 1280, 30, 2_000_000);
                match encoder::Encoder::new(&e.activate, &device, &cfg) {
                    Ok(enc) => println!("  {}: activates ({})", e.name, enc.profile),
                    Err(err) => println!("  {}: fails ({err:#})", e.name),
                }
            }
        }
    }
    mft::shutdown();
    Ok(())
}

#[cfg(not(windows))]
fn main() {}
