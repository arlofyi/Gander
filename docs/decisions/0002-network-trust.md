# 0002. Network trust model

- Status: Accepted on 2026-11-01 (owner approved defaults D1-D14)
- Date: 2026-11-01

## Context

The app requires a private network between devices but must not be tied to one vendor. Tailscale
offers peer identity (LocalAPI WhoIs); plain WireGuard and ZeroTier offer only an address.

Research on 2026-11-01 (`docs/research/2026-11-01-plan-review.md`, section "Tailscale") found:

- `WhoIs` and `Status` are the only LocalAPI calls Tailscale marks stable; `tailscale whois --json`
  exists since v1.56 and works for an unelevated user on Windows (verified on the dev machine).
- The Windows LocalAPI pipe admits the logged-in Tailscale user and LocalSystem only. A host
  running under any other account cannot ask WhoIs.
- WhoIs resolves tailnet node addresses only; LAN, loopback and subnet-routed sources are
  "not found". Tagged nodes have the pseudo-user `tagged-devices`. Shared-in nodes resolve to the
  external user with `Node.Sharer` set. Headscale returns plain usernames.
- Tailscale installs its own inbound firewall allow (`Tailscale-In`) for Private/Domain profiles,
  but a stale Block rule or a Public category defeats it silently.
- DERP-relayed paths measure 2-10 Mbit/s; direct paths are needed for Stream. `status --json`
  exposes `CurAddr`, `Relay`, `PeerRelay` to tell them apart.
- Official `tailscale-rs` (embedded node) is experimental and has no NAT traversal; `tsnet` would
  force a Go host.

## Options

1. **Rely on the OS-installed VPN client**, bind to its interface, add our own pairing. Network-agnostic.
2. **Embed Tailscale (`tsnet` / `tailscale-rs`)** so the host is its own tailnet node. Strong identity, Tailscale-only,
   another node to manage, Go or an experimental Rust library.
3. **Our own transport** (WireGuard keys, NAT traversal). Out of scope; that is the VPN's job.
4. **`tailscale serve` in front of a loopback-only host.** Gets identity headers and a trusted
   TLS cert for free, removes the firewall question for the control channel. Tailscale-only, and
   WebRTC media still needs UDP on the tailnet address, so the firewall and binding work remains.

## Decision

**Option 1 with pluggable trust adapters**, as proposed, with these specifics:

- `tailscale` adapter: auto-detected by adapter GUID/description and address ranges; WhoIs via
  LocalAPI named pipe, CLI fallback; identity recorded at pairing and re-checked per connection;
  tagged peers refused by default; relay classification surfaced to the client.
- `generic` adapter: owner-selected interface or CIDR list; identity from pairing only.
- Pairing + per-device token are mandatory under both adapters (`security.md` "two gates").
- The host runs as the interactive user (ADR-0007), which the LocalAPI access rule requires anyway.
- Option 4 is recorded as a possible later "serve mode" for users who want a browser-trusted
  certificate; not pursued for v1.

## Consequences

- Easier: one host binary works on Tailscale, Headscale, ZeroTier and WireGuard; no Go dependency.
- Harder: no identity on non-Tailscale networks (pairing carries all the weight); we own the
  firewall-rule and interface-change handling; we must test against several Tailscale versions
  because `status --json` is "subject to change".
- Must do: Phase 0 identity spike items (non-elevated pipe access, tagged and shared-in WhoIs
  output on a real tailnet, Headscale CapMap round trip) and the connectivity self-check in the
  tray UI.

## Evidence (Phase 0 identity spike, 2026-11-01)

Findings: [`../specs/2026-11-01-identity-spike-findings.md`](../specs/2026-11-01-identity-spike-findings.md)
(WP-46; Windows 11 build 26200, Tailscale 1.104.4, an unelevated process, one tailnet of one user
and three nodes). The decision stands; two statements in the Context above need these corrections,
and the trust adapter was built on them (WP-12).

- **The LocalAPI pipe's DACL is `BUILTIN\Users` and SYSTEM, not "the logged-in Tailscale user".**
  The descriptor read from the open handle is `O:BAD:P(A;;0x12019f;;;BU)(A;;0x12019f;;;SY)`: any
  local user can open the pipe, and tailscaled restricts the API per connection after that. Not
  testable with one account, so "another account cannot ask WhoIs" is still unmeasured.
- **The pipe needs identification-level QoS.** Opening it with `std::fs::File::open` or with
  `SECURITY_SQOS_PRESENT` alone (anonymous, what go-winio defaults to) gets HTTP 401 ("Unable to
  impersonate using a named pipe until data has been read from that pipe") in every round;
  `SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION` and `| SECURITY_IMPERSONATION` get 200 every
  time. The host uses identification, the least privilege that works (tokio's default too).
- **Works unelevated, over the pipe and the CLI:** `whois` resolved the host's own address and all
  three peers (including the offline one) both ways with the same JSON shape; the pipe took about
  1 ms per call and the CLI fallback 25-40 ms. Tagged and shared-in nodes and Headscale were not
  observable on this tailnet.
- **The relay rule needs `Online` and `Active`, not `Relay` alone.** Observed path classes were
  `derp` and `offline`; `direct`, `peer_relay` and `idle_unknown` are covered by unit tests on
  synthetic status only. Top-level `CapMap` is JSON `null`, and `Tags` and `Sharer` are omitted when
  empty, so the adapter parses tolerantly.
