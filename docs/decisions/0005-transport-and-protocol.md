# 0005. Transport and protocol: pinned TLS + HTTP/1.1 + WebSocket; WebRTC for media

- Status: Accepted on 2026-11-01 (owner approved defaults D1-D14); the D12 WebRTC-library line is
  Accepted on 2026-11-02 with conditions after the Phase 0 spike (see Evidence)
- Date: 2026-11-01
- Spec: `docs/specs/2026-11-01-protocol-v1.md`, `docs/specs/2026-11-01-pairing-and-trust.md`

## Context

The VPN already encrypts traffic, but `security.md` requires our own transport security (generic
WireGuard/ZeroTier setups, compromised peers, subnet routers). Phones need to stream large files
with Range requests and play video progressively, which favours plain HTTP for bytes. Stream mode
needs low-latency video with congestion control and a back-channel for input.

Research (2026-11-01):

- `webrtc-rs` 0.21.1 (Sept 2026) is "1.0-rc in everything but version": H.264 sample packetizer,
  TWCC, and a new opt-in GCC estimator + pacer validated only in simulation. `str0m` 0.24.1 has
  built-in BWE but an API mid-refactor and less-tested peer-to-peer use. Both support host-only
  ICE candidates; webrtc-rs wildcard binds expand to all interfaces, so explicit addresses must
  be passed.
- A native libwebrtc client on the phone is not subject to browser IP-handling rules that could
  hide the Tailscale interface; a WebView-based client might be.
- Baton (closest prior art) uses pinned TLS over TCP for everything including video and reports
  having had to fix fixed-latency bitrate rules by hand, which is exactly what WebRTC's congestion
  control exists for.

## Options

1. **TLS 1.3 (self-signed, SPKI-pinned at pairing) + HTTP/1.1 + WebSocket for control; WebRTC
   (H.264/HEVC + Opus + data channels) for Stream, host candidates only.**
2. Noise-protocol custom framing over TCP for everything (WireGuard-style). No certificates, but
   no standard HTTP clients or media players on the phone without a local proxy, and custom crypto
   to review.
3. Everything over WebRTC data channels (files too). Loses Range/caching semantics and HTTP tooling.
4. Plain HTTP inside the VPN, no TLS. Rejected by `security.md`.
5. Raw H.264 over the TLS TCP socket (Baton's approach) instead of WebRTC. Simpler, but we would
   own congestion control, jitter handling and keyframe recovery ourselves.

## Decision

Option 1. Specifics:

- rustls, TLS 1.3 only, ECDSA P-256 host certificate, 10-year validity, SPKI pinned by clients;
  the system trust store is never consulted for Gander hosts.
- Single port (default 42637) for HTTPS and WebSocket; WebRTC media uses ephemeral UDP ports on
  the same bound addresses (explicit addresses, never wildcard), advertised as host candidates.
- No STUN, TURN or mDNS candidates. No Funnel, no public exposure.
- `webrtc-rs` first (easier async API, `write_sample`), with congestion control enabled and
  measured in the Phase 0 spike; `str0m` is the named fallback if latency or adaptation fails.
  Option 5 is the fallback if WebRTC as a whole fails the spike thresholds.
- Browser clients are explicitly unsupported; requests carrying browser markers are refused,
  which removes CSRF and DNS-rebinding classes without CORS complexity.

## Consequences

- Easier: standard HTTP semantics for Fetch (Range, ETag, resume), standard media stack on the
  phone, one well-understood auth header.
- Harder: certificate pinning must be implemented natively on the phone (trivial in Kotlin,
  a native module in React Native, built-in callback in Flutter); the client needs a loopback
  proxy if a system media player must read Gander files (Android Media3 can use a custom data
  source instead).
- Must do: spike both WebRTC crates with the same recorded H.264 stream; keep the DTLS fingerprint
  in the SDP as the media trust anchor; test ICE on a phone whose Tailscale interface is not the
  default route.

## Evidence (Phase 0 stream spike, 2026-11-02)

Findings: [`../specs/2026-11-01-stream-spike-findings.md`](../specs/2026-11-01-stream-spike-findings.md)
("Summary and decisions", "Consolidated matrix", then the WP-41 to WP-44 sections).

- **D12, the WebRTC library line ("`webrtc-rs` first ... `str0m` is the named fallback"): Accepted
  on 2026-11-02 (WP-45, by the Conductor under the master plan's D12 default rule), `webrtc-rs`
  0.21.x, with conditions.** Measured on `webrtc` 0.21.1 + `rtc` 0.21.1 (`runtime-tokio`,
  `crypto-ring`), host candidates on explicit addresses only (no STUN, TURN or mDNS), 7 ms p50
  capture-to-received on loopback and 28-30 fps decoded. The rest of this ADR is unchanged.
- **Conditions (Phase 2 work):** (a) a delay-based bandwidth estimator through the
  `BandwidthEstimator` interface, because webrtc-rs GCC reacted to loss only: encoder output was
  under the cap after 2.5 s (1.5 Mbps) and 3.75 s (0.4 Mbps) with a 200 ms bottleneck buffer but
  after **5.5 s with a 2 s buffer, which fails the 5 s threshold**; (b) the RTCP/PLI interceptor
  (`rtc` 0.21 drops inbound RTCP unless an interceptor marks it `DeliverToApplication`: 0 of 16 PLIs
  delivered without, 21 of 21 with); (c) application-level liveness and pause messages over the
  data channel (a backgrounded or screen-off phone was not noticed in 20 s, a killed app after
  29.95 s); (d) trickle ICE or tolerance of empty-candidate answers (libwebrtc completes gathering
  before any candidate exists in some runs). See
  [WebRTC (WP-43)](../specs/2026-11-01-stream-spike-findings.md#webrtc-wp-43) and
  [Android + input (WP-44)](../specs/2026-11-01-stream-spike-findings.md#android--input-wp-44).
- **The `str0m` fallback was run and met every threshold** (output under the cap in 1.5 s and 1.0 s,
  0 drops with the 2 s buffer), **but `str0m` 0.24.1 cannot pass `cargo deny`**: RUSTSEC-2026-0387
  via `dimpl` 0.7 / `crypto-bigint` 0.5.5. Re-evaluate when it ships on `dimpl` >= 0.9.
- **Option 5 (raw H.264 over TLS) is not needed**: WebRTC works with either library on this
  hardware. The "Must do: spike both WebRTC crates with the same recorded H.264 stream" item is done
  (`spikes/stream/encode/results/wp43-h264-1080x2280.h264`).
- **Version delta against the Context text:** none beyond patch level (`webrtc-rs` 0.21.1 and
  `str0m` 0.24.1 as named). The Decision's "test ICE on a phone whose Tailscale interface is not the
  default route" is not done (emulator only, loopback and its NAT); it stays open for the first
  physical-device run.
