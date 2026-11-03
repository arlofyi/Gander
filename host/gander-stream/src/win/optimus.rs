// SPDX-License-Identifier: Apache-2.0
//! Hybrid-GPU opt-in symbols, switchable at run time.
//!
//! An executable that EXPORTS `NvOptimusEnablement` with the value 1 is put on the NVIDIA GPU of
//! an Optimus laptop; without that the NVIDIA H.264 MFT does not activate (`0x8000FFFF`, WP-42).
//! `AmdPowerXpressRequestHighPerformance` is AMD's equivalent (not measured).
//!
//! Measured on the dev laptop (Intel iGPU plus RTX 5060, driver 595.79, build 26200; see
//! `docs/specs/2026-11-03-igpu-ui-findings.md`): the driver reads the exported VALUE each time it
//! decides, not once per process. With the value 0 DXGI lists the Intel adapter first and the
//! NVIDIA MFT fails to activate; set to 1 at run time, a fresh DXGI factory lists NVIDIA first and
//! the MFT activates; set back to 0, the order flips back. So the symbols start at 0 and the host
//! raises them only while it needs the discrete GPU: while the stream source sets up (or
//! re-creates) its NVIDIA or AMD encoder, and while a settings window's OpenGL context is created
//! when the owner chose `[ui] gpu = "discrete"` ([`GpuDecision`]). By default the settings UI then
//! lands on the integrated GPU. Once activated, the NVIDIA MFT keeps encoding with the value back
//! at 0 (measured, `cargo test -p gander-stream --test pipeline` with `GANDER_TEST_VENDOR=nvidia`).
//!
//! Defining the symbols here is not enough: the executable must export them, which
//! `host/gander-host/build.rs` (and this crate's `build.rs` for its tests) asks the linker for with
//! `/EXPORT:`; that also pulls this object into the link.
#![allow(non_upper_case_globals)]

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard};

/// NVIDIA Optimus: 1 = prefer the discrete GPU. Same layout as the documented `DWORD`.
#[unsafe(no_mangle)]
#[used]
pub static NvOptimusEnablement: AtomicU32 = AtomicU32::new(0);

/// AMD PowerXpress: 1 = prefer the discrete GPU.
#[unsafe(no_mangle)]
#[used]
pub static AmdPowerXpressRequestHighPerformance: AtomicU32 = AtomicU32::new(0);

/// Serialises GPU decisions: one holder at a time sets the exported values.
static DECIDING: Mutex<()> = Mutex::new(());

fn publish(on: bool) {
    let v = u32::from(on);
    NvOptimusEnablement.store(v, Ordering::SeqCst);
    AmdPowerXpressRequestHighPerformance.store(v, Ordering::SeqCst);
}

/// True while a [`GpuDecision`] for the discrete GPU is held (the exported values are 1).
pub fn discrete_requested() -> bool {
    NvOptimusEnablement.load(Ordering::SeqCst) != 0
}

/// A GPU decision in progress. While it lives, the exported values say "discrete" (1) or "no
/// preference" (0), and every other decision waits, so the Stream engine's encoder setup and a
/// settings window's OpenGL context creation never see each other's value. Hold it around the
/// moment the driver decides (DXGI factory creation, `IMFActivate::ActivateObject`, OpenGL context
/// creation) and drop it as soon as that is done; the values return to 0.
#[derive(Debug)]
#[must_use = "the decision holds only while the guard lives"]
pub struct GpuDecision {
    _lock: MutexGuard<'static, ()>,
}

impl GpuDecision {
    /// Waits for any other decision to finish, then sets the exported values.
    pub fn take(discrete: bool) -> Self {
        let lock = DECIDING.lock().unwrap_or_else(|e| e.into_inner());
        publish(discrete);
        Self { _lock: lock }
    }
}

impl Drop for GpuDecision {
    fn drop(&mut self) {
        publish(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decision_sets_and_restores_the_exported_value() {
        let d = GpuDecision::take(true);
        assert!(discrete_requested());
        drop(d);
        let d = GpuDecision::take(false);
        assert!(!discrete_requested());
        drop(d);
        assert!(!discrete_requested());
    }
}
