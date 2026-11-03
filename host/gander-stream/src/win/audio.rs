// SPDX-License-Identifier: Apache-2.0
//! WASAPI process loopback (WP-55): the sound of the streamed window's process tree only.
//!
//! `ActivateAudioInterfaceAsync(VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK)` with
//! `PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE` on the process that owns the window
//! (through the `wasapi` crate), shared mode, event driven, 48 kHz stereo float with the audio
//! engine's conversion. Windows 10 2004 (build 19041) or later; on anything older, or if the
//! activation fails for any reason, the session has no audio. There is deliberately no endpoint
//! (system-wide) loopback anywhere in this crate.
//!
//! The capture thread follows [`AudioControl`]: inactive means the WASAPI stream is stopped
//! (`IAudioClient::Stop`) and the client released, nothing is read or encoded, and the thread
//! sleeps on the control's condition variable; the next activation opens a new client (a
//! stopped process-loopback client does not deliver again after a restart).

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use wasapi::{AudioClient, Direction, SampleType, StreamMode, WaveFormat};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;

use crate::audio::{
    AudioControl, AudioInfo, AudioPacket, AudioSource, AudioUnavailable, CHANNELS, OpusFramer,
    SAMPLE_RATE, SilencePad, deliver,
};
use crate::source::WindowTarget;

/// The production [`AudioSource`]: process loopback on the window's owning process tree.
#[derive(Debug, Default, Clone)]
pub struct ProcessLoopback;

/// The process that owns `hwnd` (0 when the window is gone).
pub fn window_process(hwnd: isize) -> u32 {
    let mut pid = 0u32;
    // SAFETY: plain query on a window handle; an invalid handle returns 0 and leaves pid at 0.
    unsafe {
        GetWindowThreadProcessId(HWND(hwnd as *mut core::ffi::c_void), Some(&mut pid));
    }
    pid
}

impl AudioSource for ProcessLoopback {
    fn start(
        &self,
        window: &WindowTarget,
        ctl: Arc<AudioControl>,
        tx: mpsc::Sender<AudioPacket>,
    ) -> Result<AudioInfo, AudioUnavailable> {
        let pid = window_process(window.hwnd);
        if pid == 0 {
            return Err(AudioUnavailable("the window has no process".into()));
        }
        if pid == std::process::id() {
            return Err(AudioUnavailable(
                "refusing to capture the host itself".into(),
            ));
        }
        Self::start_pid(pid, ctl, tx)
    }
}

impl ProcessLoopback {
    /// Starts capturing the process tree rooted at `pid` (the tests use it directly).
    pub fn start_pid(
        pid: u32,
        ctl: Arc<AudioControl>,
        tx: mpsc::Sender<AudioPacket>,
    ) -> Result<AudioInfo, AudioUnavailable> {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("gander-audio".into())
            .spawn(move || capture_thread(pid, ctl, tx, ready_tx))
            .map_err(|e| AudioUnavailable(format!("thread: {e}")))?;
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(AudioInfo {
                process_id: pid,
                description: format!("WASAPI process loopback, process tree of {pid}"),
            }),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(AudioUnavailable(
                "process loopback did not activate within 5 s".into(),
            )),
        }
    }
}

struct Capture {
    client: AudioClient,
    capture: wasapi::AudioCaptureClient,
    event: wasapi::Handle,
    block_align: usize,
}

fn open(pid: u32) -> Result<Capture, AudioUnavailable> {
    let fail = |what: &str, e: wasapi::WasapiError| AudioUnavailable(format!("{what}: {e}"));
    let format = WaveFormat::new(
        32,
        32,
        &SampleType::Float,
        SAMPLE_RATE as usize,
        CHANNELS,
        None,
    );
    let mut client = AudioClient::new_application_loopback_client(pid, true)
        .map_err(|e| fail("process loopback activation", e))?;
    client
        .initialize_client(
            &format,
            &Direction::Capture,
            &StreamMode::EventsShared {
                autoconvert: true,
                buffer_duration_hns: 200_000,
            },
        )
        .map_err(|e| fail("initialize", e))?;
    let event = client
        .set_get_eventhandle()
        .map_err(|e| fail("event handle", e))?;
    let capture = client
        .get_audiocaptureclient()
        .map_err(|e| fail("capture client", e))?;
    Ok(Capture {
        client,
        capture,
        event,
        block_align: format.get_blockalign() as usize,
    })
}

fn capture_thread(
    pid: u32,
    ctl: Arc<AudioControl>,
    tx: mpsc::Sender<AudioPacket>,
    ready: std::sync::mpsc::Sender<Result<(), AudioUnavailable>>,
) {
    let _ = wasapi::initialize_mta();
    let setup = open(pid).and_then(|c| OpusFramer::new().map(|f| (c, f)));
    let (first, mut framer) = match setup {
        Ok(x) => {
            let _ = ready.send(Ok(()));
            x
        }
        Err(e) => {
            let _ = ready.send(Err(e));
            wasapi::deinitialize();
            return;
        }
    };
    // The client that proved process loopback works; used for the first activation. Every
    // deactivation releases the client and the next activation opens a new one: measured
    // 2026-11-03 (Windows 11 26200), a process-loopback client stopped with `Stop` + `Reset`
    // delivers nothing after `Start` again. Released also means nothing holds the session's
    // audio while paused or muted.
    let mut spare = Some(first);
    let mut live: Option<Capture> = None;
    let mut pad = SilencePad::new(Instant::now());
    let mut bytes = vec![0u8; 0];
    let mut samples: Vec<f32> = Vec::new();
    loop {
        if ctl.is_stopped() || tx.is_closed() {
            break;
        }
        if !ctl.is_active() {
            if let Some(cap) = live.take() {
                let _ = cap.client.stop_stream();
                framer.reset();
            }
            ctl.wait(Duration::from_millis(500));
            continue;
        }
        if live.is_none() {
            let cap = match spare.take().map_or_else(|| open(pid), Ok) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("audio capture did not reopen: {e}");
                    ctl.wait(Duration::from_millis(1000));
                    continue;
                }
            };
            if let Err(e) = cap.client.start_stream() {
                tracing::warn!("audio capture did not start: {e}");
                ctl.wait(Duration::from_millis(500));
                continue;
            }
            ctl.stats.starts.fetch_add(1, Ordering::Relaxed);
            pad = SilencePad::new(Instant::now());
            live = Some(cap);
        }
        let Some(cap) = live.as_ref() else {
            continue;
        };
        let _ = cap.event.wait_for_event(20);
        // Drain every packet the engine has.
        loop {
            let frames = match cap.capture.get_next_packet_size() {
                Ok(Some(n)) if n > 0 => n as usize,
                Ok(_) => break,
                Err(e) => {
                    tracing::debug!("audio packet size: {e}");
                    break;
                }
            };
            bytes.resize(frames * cap.block_align, 0);
            let (got, info) = match cap.capture.read_from_device(&mut bytes) {
                Ok(r) => r,
                Err(e) => {
                    tracing::debug!("audio read: {e}");
                    break;
                }
            };
            let got = got as usize;
            if got == 0 {
                break;
            }
            samples.clear();
            if info.flags.silent {
                samples.resize(got * CHANNELS, 0.0);
            } else {
                samples.extend(
                    bytes[..got * cap.block_align]
                        .chunks_exact(4)
                        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
                );
            }
            ctl.stats
                .frames_captured
                .fetch_add(got as u64, Ordering::Relaxed);
            pad.delivered(got as u64);
            let packets = framer.push(&samples);
            if !deliver(&ctl, &tx, packets) {
                break;
            }
        }
        let silence = pad.pad(Instant::now());
        if silence > 0 {
            ctl.stats
                .frames_padded
                .fetch_add(silence, Ordering::Relaxed);
            let packets = framer.push_silence(silence as usize);
            if !deliver(&ctl, &tx, packets) {
                break;
            }
        }
    }
    if let Some(cap) = live.take() {
        let _ = cap.client.stop_stream();
    }
    drop(spare);
    wasapi::deinitialize();
}
