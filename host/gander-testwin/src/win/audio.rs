// SPDX-License-Identifier: Apache-2.0
//! "Play a tone on command" (WP-55): the pipe command `tone [HZ [MS]]` plays a quiet sine
//! through this process's own audio session, so a process-loopback capture of the test window's
//! process has something of known frequency to detect.
//!
//! The tone is generated as an in-memory 48 kHz 16-bit mono WAV and played with `PlaySoundW`
//! (`SND_MEMORY | SND_SYNC`) on a helper thread, so the pipe answers `ok` at once. It is quiet on
//! purpose: [`TONE_DBFS`] (-30 dBFS) with 5 ms fades, at most 1 s (the protocol bound), because
//! the speakers of the machine that runs the tests may be on.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Media::Audio::{PlaySoundW, SND_MEMORY, SND_NODEFAULT, SND_SYNC};
use windows::core::PCWSTR;

/// Level of the tone, dBFS (peak).
pub const TONE_DBFS: f64 = -30.0;
/// Sample rate of the generated WAV.
pub const TONE_RATE: u32 = 48_000;

/// Why a tone was not played.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToneError {
    /// A tone is still playing.
    Busy,
    /// The helper thread could not start.
    Thread,
}

impl fmt::Display for ToneError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ToneError::Busy => f.write_str("a tone is already playing"),
            ToneError::Thread => f.write_str("could not start the tone thread"),
        }
    }
}

impl std::error::Error for ToneError {}

static PLAYING: AtomicBool = AtomicBool::new(false);

/// A mono 16-bit PCM WAV of a `hz` sine, `ms` long, at [`TONE_DBFS`], with 5 ms fades.
pub fn tone_wav(hz: u32, ms: u32) -> Vec<u8> {
    let n = (u64::from(TONE_RATE) * u64::from(ms) / 1000) as usize;
    let amp = 10f64.powf(TONE_DBFS / 20.0) * f64::from(i16::MAX);
    let fade = (TONE_RATE as usize / 200).min(n / 2).max(1);
    let data_len = (n * 2) as u32;
    let mut w = Vec::with_capacity(44 + n * 2);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes()); // PCM
    w.extend_from_slice(&1u16.to_le_bytes()); // mono
    w.extend_from_slice(&TONE_RATE.to_le_bytes());
    w.extend_from_slice(&(TONE_RATE * 2).to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..n {
        let env = (i.min(n - 1 - i) as f64 / fade as f64).min(1.0);
        let t = i as f64 / f64::from(TONE_RATE);
        let v = (amp * env * (2.0 * std::f64::consts::PI * f64::from(hz) * t).sin()).round();
        w.extend_from_slice(&(v as i16).to_le_bytes());
    }
    w
}

/// Plays a sine tone of `hz` hertz for `ms` milliseconds (at most 1 s by the protocol) without
/// blocking the caller.
pub fn play_tone(hz: u32, ms: u32) -> Result<(), ToneError> {
    if PLAYING.swap(true, Ordering::AcqRel) {
        return Err(ToneError::Busy);
    }
    let wav = tone_wav(hz, ms.min(1000));
    std::thread::Builder::new()
        .name("tone".into())
        .spawn(move || {
            // SAFETY: `wav` is a complete RIFF/WAVE image that outlives the synchronous call.
            unsafe {
                let _ = PlaySoundW(
                    PCWSTR(wav.as_ptr().cast()),
                    None,
                    SND_MEMORY | SND_SYNC | SND_NODEFAULT,
                );
            }
            PLAYING.store(false, Ordering::Release);
        })
        .map(|_| ())
        .map_err(|_| {
            PLAYING.store(false, Ordering::Release);
            ToneError::Thread
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_is_quiet_and_well_formed() {
        let w = tone_wav(1000, 200);
        assert_eq!(&w[..4], b"RIFF");
        assert_eq!(&w[8..16], b"WAVEfmt ");
        assert_eq!(w.len(), 44 + 9600 * 2);
        let peak = w[44..]
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]).unsigned_abs())
            .max()
            .unwrap();
        // -30 dBFS is 1036 of 32767.
        assert!((1000..=1040).contains(&peak), "{peak}");
        // Fades: the first and last samples are silent.
        assert_eq!(i16::from_le_bytes([w[44], w[45]]), 0);
    }
}
