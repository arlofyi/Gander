// SPDX-License-Identifier: Apache-2.0
//! Receiver-side decode: complete access units are piped into `ffmpeg` (on PATH) as raw H.264;
//! ffmpeg prints one `framecrc` line per decoded picture, which marks the decode time.
//!
//! Every access unit is followed by an access unit delimiter so ffmpeg's H.264 parser can close
//! the picture as soon as it arrives instead of waiting for the next one.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::au::AUD;
use crate::metrics::{Metrics, now_100ns};

/// A running ffmpeg decoder.
#[derive(Debug)]
pub struct Decoder {
    tx: Option<Sender<(u64, Vec<u8>)>>,
    child: Child,
    threads: Vec<JoinHandle<()>>,
    /// Frames ffmpeg reported decoded.
    pub decoded: Arc<Mutex<u64>>,
}

impl Decoder {
    /// Starts `ffmpeg` reading H.264 from stdin.
    pub fn start(m: Arc<Metrics>) -> Result<Decoder> {
        let mut child = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                // Measured (WP-43): `-fflags nobuffer` makes ffmpeg 9 hold ~46 frames (1.55 s)
                // and lose them at EOF; without it, and with an input frame rate and a tiny
                // probe, the pipe adds one frame (~35 ms at 30 fps: the parser closes a
                // picture when the next start code arrives).
                "-flags",
                "low_delay",
                "-probesize",
                "32",
                "-analyzeduration",
                "0",
                "-framerate",
                "30",
                "-threads",
                "1",
                "-f",
                "h264",
                "-i",
                "pipe:0",
                "-fps_mode",
                "passthrough",
                "-flush_packets",
                "1",
                "-f",
                "framecrc",
                "pipe:1",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .context("start ffmpeg (is it on PATH?)")?;
        let mut stdin = child.stdin.take().context("ffmpeg stdin")?;
        let stdout = child.stdout.take().context("ffmpeg stdout")?;
        let fifo: Arc<Mutex<VecDeque<u64>>> = Arc::default();
        let decoded: Arc<Mutex<u64>> = Arc::default();
        let (tx, rx) = channel::<(u64, Vec<u8>)>();
        let w = {
            let fifo = fifo.clone();
            std::thread::Builder::new()
                .name("ffmpeg-in".into())
                .spawn(move || {
                    for (id, au) in rx {
                        fifo.lock().unwrap_or_else(|e| e.into_inner()).push_back(id);
                        if stdin.write_all(&au).is_err()
                            || stdin.write_all(&AUD).is_err()
                            || stdin.flush().is_err()
                        {
                            return;
                        }
                    }
                })?
        };
        let r = {
            let decoded = decoded.clone();
            std::thread::Builder::new()
                .name("ffmpeg-out".into())
                .spawn(move || {
                    for line in BufReader::new(stdout).lines() {
                        let Ok(line) = line else { return };
                        if line.starts_with('#') || line.trim().is_empty() {
                            continue;
                        }
                        let t = now_100ns();
                        let id = fifo.lock().unwrap_or_else(|e| e.into_inner()).pop_front();
                        if let Some(id) = id {
                            m.with_frame(id, |f| f.dec = Some(t));
                        }
                        *decoded.lock().unwrap_or_else(|e| e.into_inner()) += 1;
                    }
                })?
        };
        Ok(Decoder {
            tx: Some(tx),
            child,
            threads: vec![w, r],
            decoded,
        })
    }

    /// A sender for (frame id, Annex-B access unit).
    pub fn feeder(&self) -> Sender<(u64, Vec<u8>)> {
        self.tx.clone().expect("decoder running")
    }

    /// Closes stdin, waits for ffmpeg to flush (killing it after `timeout`).
    pub fn finish(mut self, timeout: Duration) -> u64 {
        drop(self.tx.take());
        let deadline = Instant::now() + timeout;
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                _ => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break;
                }
            }
        }
        // The reader ends with ffmpeg's stdout; the writer ends when every feeder is dropped.
        // Join only what has finished so a stray feeder cannot hang the spike.
        std::thread::sleep(Duration::from_millis(50));
        for t in self.threads.drain(..) {
            if t.is_finished() {
                let _ = t.join();
            }
        }
        *self.decoded.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
