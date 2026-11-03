// SPDX-License-Identifier: Apache-2.0
//! The host side again, with `str0m` (ADR-0005 D12's named fallback) instead of webrtc-rs: same
//! signalling, same webrtc-rs viewer (so this is also an interop check), same metrics and the
//! same source thread. str0m is sans-IO: one task owns the `Rtc`, a UDP socket bound to
//! 127.0.0.1 and the frame channel, and drives it with the drain/feed loop str0m documents.
//!
//! Bandwidth estimation is str0m's own (TWCC feedback, delay + loss based, with probing); its
//! `EgressBitrateEstimate` is published as the "GCC target" so the source thread and the report
//! treat both stacks alike. str0m does not expose the delay/loss halves.
//!
//! **Not compiled by default.** str0m 0.24.1 depends on `dimpl` 0.7 -> `ecdsa`/`p256` 0.13 ->
//! `crypto-bigint` 0.5.5, which has RUSTSEC-2026-0387 (non-constant-time arithmetic, fixed only
//! in 0.7.6), so `cargo deny check` fails while it is in the graph. The `str0m-*` results under
//! `../results/` were measured with this file compiled in. To build it again (locally, do not
//! commit while the advisory stands):
//!
//! 1. root `Cargo.toml` `[workspace.dependencies]`:
//!    `str0m = { version = "0.24.1", default-features = false, features = ["rust-crypto"] }`
//!    (the default `aws-lc-rs` needs CMake/NASM), and `str0m.workspace = true` in
//!    `spikes/stream/webrtc/Cargo.toml`;
//! 2. copy this file to `src/peer_str0m.rs` and add `mod peer_str0m;` to `src/main.rs`;
//! 3. in `src/main.rs`, make `--stack str0m` set a flag instead of exiting, and in `session` spawn
//!    `crate::peer_str0m::run_host(listener, frx, m.clone(), relay.clone(), a.gcc, ctx)` instead
//!    of `peer::run_host(..)` when it is set.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use str0m::bwe::{Bitrate, BweKind};
use str0m::change::SdpAnswer;
use str0m::format::Codec;
use str0m::media::{Direction, Frequency, MediaKind, MediaTime};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, RtcConfig};
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::mpsc;

use crate::metrics::{Metrics, now_100ns};
use crate::peer::{EncodedFrame, GccConfig, HostReport};
use crate::relay::{self, Relay};
use crate::signal::{self, Signal};

/// H.264 constrained baseline level 5.0 (`42e032`), packetization mode 1, PT 102.
const PROFILE_LEVEL_ID: u32 = 0x0042_e032;

/// The str0m host. Same contract as [`crate::peer::run_host`].
pub async fn run_host(
    listener: TcpListener,
    mut frames: mpsc::Receiver<EncodedFrame>,
    m: Arc<Metrics>,
    relay: Option<Arc<Relay>>,
    gcc: GccConfig,
    connected: tokio::sync::oneshot::Sender<HostReport>,
) -> Result<()> {
    let sock = UdpSocket::bind(("127.0.0.1", 0))
        .await
        .context("bind ICE")?;
    let local = sock.local_addr()?;

    let mut cfg = RtcConfig::new()
        .clear_codecs()
        .enable_bwe(Some(Bitrate::bps(gcc.initial as u64)));
    cfg.codec_config()
        .add_h264(102.into(), None, true, PROFILE_LEVEL_ID);
    let mut rtc = cfg.build(Instant::now());
    rtc.add_local_candidate(Candidate::host(local, "udp").context("host candidate")?);
    let mut api = rtc.sdp_api();
    let mid = api.add_media(
        MediaKind::Video,
        Direction::SendOnly,
        Some("gander".into()),
        Some("window".into()),
        None,
    );
    let _ = api.add_channel("input".into());
    let (offer, pending) = api.apply().context("empty SDP change")?;
    m.gcc_target.set(gcc.initial);
    // str0m does not expose the delay/loss halves.
    m.gcc_delay.set(f64::NAN);
    m.gcc_loss.set(f64::NAN);

    // Signalling (identical messages to the webrtc-rs host).
    let mut ch = signal::accept(&listener).await?;
    let t_accept = Instant::now();
    match ch.recv().await? {
        Signal::Hello { .. } => {}
        other => bail!("expected hello, got {other:?}"),
    }
    let offer_sdp = offer.to_sdp_string();
    let mut report = HostReport {
        offer_candidates: relay::candidates(&offer_sdp),
        offer_sdp: offer_sdp.clone(),
        ..Default::default()
    };
    let to_send = match &relay {
        Some(r) => r.munge_offer(&offer_sdp)?,
        None => offer_sdp,
    };
    ch.send(&Signal::Offer { sdp: to_send }).await?;
    let answer = match ch.recv().await? {
        Signal::Answer { sdp } => sdp,
        other => bail!("expected answer, got {other:?}"),
    };
    report.answer_candidates = relay::candidates(&answer);
    report.answer_sdp = answer.clone();
    let answer = match &relay {
        Some(r) => r.munge_answer(&answer)?,
        None => answer,
    };
    rtc.sdp_api()
        .accept_answer(pending, SdpAnswer::from_sdp_string(&answer)?)?;

    let mut buf = vec![0u8; 2048];
    let mut is_connected = false;
    let mut connected = Some(connected);
    let mut echoes: VecDeque<(str0m::channel::ChannelId, Vec<u8>, i64)> = VecDeque::new();
    let mut base_cap: Option<i64> = None;
    let mut last_rtp = 0u64;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        // Drain until the next timeout.
        let timeout = loop {
            match rtc.poll_output()? {
                Output::Timeout(t) => break t,
                Output::Transmit(t) => {
                    let _ = sock.send_to(&t.contents, t.destination).await;
                }
                Output::Event(e) => match e {
                    Event::Connected => {
                        is_connected = true;
                        report.connect_ms = t_accept.elapsed().as_secs_f64() * 1000.0;
                        if let Some(tx) = connected.take() {
                            let _ = tx.send(report.clone());
                        }
                    }
                    Event::IceConnectionStateChange(IceConnectionState::Disconnected) => {
                        return Ok(());
                    }
                    Event::KeyframeRequest(_) => {
                        m.plis_recv.fetch_add(1, Ordering::Relaxed);
                        m.force_key.store(true, Ordering::Relaxed);
                        m.rtcp_kinds
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .push("KeyframeRequest".into());
                    }
                    Event::ChannelData(cd) => echoes.push_back((cd.id, cd.data, now_100ns())),
                    Event::EgressBitrateEstimate(BweKind::Twcc { estimate, .. }) => {
                        m.gcc_target.set(estimate.as_f64());
                    }
                    _ => {}
                },
            }
        };
        if !is_connected && Instant::now() > deadline {
            bail!("str0m: not connected after 15 s");
        }
        if m.stop.load(Ordering::Relaxed) {
            break;
        }
        // One mutation per drain (str0m's single-mutation rule): pending echoes first.
        if let Some((id, data, host_rx)) = echoes.pop_front() {
            if let Ok(mut v) = serde_json::from_slice::<Value>(&data) {
                let kind = v.get("t").and_then(Value::as_str).unwrap_or("").to_owned();
                v["t"] = json!("echo");
                v["of"] = json!(kind);
                v["host_rx"] = json!(host_rx);
                if let Some(mut c) = rtc.channel(id)
                    && c.write(false, v.to_string().as_bytes()).is_ok()
                {
                    m.dc_echoed.fetch_add(1, Ordering::Relaxed);
                }
            }
            continue;
        }
        let wait = timeout.saturating_duration_since(Instant::now());
        tokio::select! {
            r = sock.recv_from(&mut buf) => {
                // Windows reports ICMP port-unreachable as a recv error on UDP: skip it.
                let Ok((n, source)) = r else { continue };
                let Ok(contents) = buf[..n].try_into() else { continue };
                rtc.handle_input(Input::Receive(
                    Instant::now(),
                    Receive { proto: Protocol::Udp, source, destination: local, contents },
                ))?;
            }
            f = frames.recv() => {
                let Some(f) = f else { break };
                if !is_connected {
                    continue;
                }
                rtc.bwe().set_desired_bitrate(Bitrate::bps(gcc.max as u64));
                let Some(w) = rtc.writer(mid) else { continue };
                let Some(pt) = w
                    .payload_params()
                    .find(|p| p.spec().codec == Codec::H264)
                    .map(|p| p.pt())
                else {
                    bail!("H.264 not negotiated");
                };
                // RTP time from the capture time: 90 kHz from 100 ns units.
                let base = *base_cap.get_or_insert(f.cap);
                let rtp = (((f.cap - base).max(0) as u64) * 9 / 1000).max(last_rtp);
                last_rtp = rtp;
                w.write(pt, Instant::now(), MediaTime::new(rtp, Frequency::NINETY_KHZ), f.data)?;
                let t = now_100ns();
                m.with_frame(f.id, |r| r.sent = Some(t));
            }
            () = tokio::time::sleep(wait) => {
                rtc.handle_input(Input::Timeout(Instant::now()))?;
            }
        }
    }
    let _ = ch.send(&Signal::Bye).await;
    rtc.disconnect();
    Ok(())
}
