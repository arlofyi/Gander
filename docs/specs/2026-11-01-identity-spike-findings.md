# Findings: identity spike (WP-46)

- Status: Measured; one machine, one tailnet (see "Not observed")
- Date: 2026-11-01
- Feeds: `2026-11-01-pairing-and-trust.md` (tailscale trust adapter, WP-12), ADR-0002, ADR-0007
- Code and data: `spikes/identity/` (crate `spike-identity`), redacted run in
  `spikes/identity/results/windows-tailscale-1.104.4-redacted.json`

## What was measured, and where

| Item | Value |
| --- | --- |
| OS | Windows 11 Pro, build 26200 |
| Tailscale | 1.104.4 (`tailscale` CLI and tailscaled), backend `Running`, Windows adapter category Private |
| Toolchain | rustc/cargo 1.96.1; the pipe and HTTP use std only (no async runtime), `windows-sys` 0.61 for the token and security-descriptor queries, `serde_json` for parsing |
| Process | unelevated: token elevation `false`, elevation type `limited` (an administrator account running its filtered token), integrity `medium` |
| Tailnet | one user (`<user-1>`), three nodes: self (`<self>`, Windows) and two Android phones/tablets (`<peer-A>` online, `<peer-B>` offline). No tagged nodes, no shared-in nodes |

Method: the binary talks only to the local tailscaled, over the LocalAPI named pipe
(`\\.\pipe\ProtectedPrefix\Administrators\Tailscale\tailscaled`, HTTP/1.1 with
`Host: local-tailscaled.sock`) and through the `tailscale` CLI. It never pings or connects to a
peer and changes no setting. `whois` and `status` are local netmap lookups.

Privacy: this file and the committed JSON contain placeholders only (`<user-N>`, `<peer-X>`,
`100.x.y.z`, `nodekey:<redacted>`, `<derp-region-N>`). `--redact` routes every identity-bearing
value through one redactor, scrubs error text, and refuses to emit output that still contains a
registered value or an address-shaped pattern; `--out` refuses to run without `--redact`. The
committed file was also checked independently against the raw status and whois output (no hit
other than public Tailscale capability URLs).

## Answers

| Question | Result |
| --- | --- |
| Does the pipe accept an unelevated process? | Yes, at the right impersonation level (next section). Status and whois both return 200. |
| `whois` for the host's own address? | Works, over the pipe and the CLI. Returns the same shape as a peer plus `Node.CapMap` (self only). |
| `whois` for each peer in `status --json`? | 3 of 3 nodes resolved over the pipe and 3 of 3 over the CLI, including the offline peer. |
| `Node.Tags`, `Sharer`, `CapMap`, `UserProfile.LoginName` present? | See "Fields". Tags and Sharer were **absent** for every node (untagged, not shared); LoginName always present; top-level CapMap is JSON `null`. |
| Path classification | Peer path classes observed: `derp` (online, active, no `CurAddr`) and `offline`. `direct`, `peer_relay` and `idle_unknown` were not observable here (unit-tested on synthetic status only). |
| CLI fallback | Works unelevated for `status --json` and `whois --json`; identical JSON shape to the pipe (3 of 3 whois, status shape equal). Tens of milliseconds per call against about a millisecond over the pipe. |

## Pipe access

The pipe's security descriptor (read from the open handle, user SIDs masked) is
`O:BAD:P(A;;0x12019f;;;BU)(A;;0x12019f;;;SY)`: owner Administrators, protected DACL,
`FILE_GENERIC_READ | FILE_GENERIC_WRITE` for **BUILTIN\Users** and SYSTEM. The pipe is therefore openable by any local user; whatever
restricts the API to the logged-in Tailscale user happens inside tailscaled, per connection.

The client's security quality of service decides whether tailscaled accepts the connection. Four
rounds, each trying every level, alternating direction so an ordering effect would show:

| Open flags | Result (4 rounds each) |
| --- | --- |
| none (what `std::fs::File::open` does) | HTTP 401 every time |
| `SECURITY_SQOS_PRESENT` (anonymous, what go-winio defaults to) | HTTP 401 every time |
| `SECURITY_SQOS_PRESENT \| SECURITY_IDENTIFICATION` | HTTP 200 every time |
| `SECURITY_SQOS_PRESENT \| SECURITY_IMPERSONATION` | HTTP 200 every time |

The 401 body is `authentication failed: Unable to impersonate using a named pipe until data has
been read from that pipe.` Use **identification**: it is the least privilege that works, and it is
also tokio's `ClientOptions` default.

Calls are cheap: pipe `status` about 0.6-1.1 ms, pipe `whois` median 0.3-2.3 ms (20 calls per run,
four runs; the machine was not idle), one connection per request with `Connection: close`.
`ERROR_PIPE_BUSY` (231) did not occur: 0 retries in the final run's 62 pipe connections (earlier
runs did not count, and none failed). The client retries on it anyway. A synchronous pipe read has no
timeout, so the spike runs each call on a worker thread with a 10 s limit; the host needs
overlapped I/O or tokio for the same reason.

## LocalAPI request semantics

All against the self address, pipe, identification QoS.

| Request | Result |
| --- | --- |
| `whois?addr=<ip>&proto=tcp` (the spec's form) | 200 |
| `addr=<ip>`, `addr=<ip>:0`, `addr=<ip>:<port>`, `addr=<ip>:<port>&proto=tcp` | 200 each |
| IPv6 `addr=<fd7a...>` and `addr=[<fd7a...>]:<port>&proto=tcp` | 200 each (percent-encode brackets and colons) |
| Loopback, a documentation address, a private LAN address | 404 `no match for IP:port` (the daemon does not echo the address) |
| `addr=not-an-address`, missing `addr` | 400 `invalid 'addr' parameter` / `missing 'addr' parameter` |
| `Host: local-tailscaled.sock` | 200 |
| No `Host` (HTTP/1.0) | 200 |
| `Host: localhost`, any other `Host`, any `Origin`, any `Referer` | 403 `invalid localapi request` |

CLI equivalents: `tailscale whois --json <ip>` and `<ip>:<port>` exit 0; loopback and unknown
addresses exit 1 with `peer not found`; a malformed address exits 1 with `400 Bad Request`.

## Fields

`status` (pipe and CLI identical in key sets and presence counts):

- Top level: `Version`, `TUN`, `BackendState`, `HaveNodeKey`, `AuthURL`, `TailscaleIPs`, `Self`,
  `Health`, `MagicDNSSuffix`, `CurrentTailnet`, `CertDomains`, `ExtraRecords`, `Peer`, `User`,
  `ClientVersion`. About 5.5 KB with two peers.
- Every peer entry has `PeerRelay` (empty string when unused), `CurAddr`, `Relay`, `Active`,
  `Online`, `OS`, `UserID`, `TailscaleIPs`, `PublicKey`. **`Tags` and `ShareeNode` are omitted when
  not applicable** (no key at all), so a missing key means "untagged / not shared-in".
- `Self` additionally has `CapMap` (object) and `Capabilities` (array); peers have neither.
- The top-level `User` map (`UserID` to `{ID, LoginName, DisplayName, ProfilePicURL}`) gave the
  same login as `whois` for all three nodes, so one `status` call is enough to map every tailnet
  address to a login and a path class.

`whois` (pipe and CLI identical in field names and JSON types for all three nodes):

| Field | Self | Peers (both) |
| --- | --- | --- |
| Top-level keys | `Node`, `UserProfile`, `CapMap` | same |
| `CapMap` (top level) | present, JSON **`null`** | present, JSON `null` |
| `Node.Tags` | absent | absent |
| `Node.Sharer` | absent | absent |
| `Node.CapMap` | object, 11 names (3 Tailscale-namespaced feature caps, 2 owner/admin role caps, 6 un-namespaced node attributes such as `ssh-behavior-v1`) | absent |
| `Node.User` equals `UserProfile.ID` | yes | yes |
| `UserProfile.LoginName` | present, string (an email address on this tailnet) | same login as self |
| `UserProfile.DisplayName`, `ProfilePicURL` | present, strings | same |
| Other `Node` keys | `ID`, `StableID`, `Name`, `Key`, `KeyExpiry`, `DiscoKey`, `Addresses`, `AllowedIPs`, `Endpoints`, `Hostinfo`, `Created`, `MachineAuthorized`, `ComputedName`, `ComputedNameWithHost`, `StableTailnetID` | `ID`, `StableID`, `Name`, `Key`, `KeyExpiry`, `DiscoKey`, `Addresses`, `AllowedIPs`, `Endpoints`, `HomeDERP`, `Hostinfo`, `Created`, `Cap`, `LastSeen`, `Online`, `ComputedName`, `ComputedNameWithHost` |

`whois` has no current-path information (`Endpoints` is a candidate list, `HomeDERP` the home
region), so path classification needs `status`.

## Path classification

Observed, from `status` alone:

| Peer | `Online` | `Active` | `CurAddr` | `PeerRelay` | `Relay` | Class |
| --- | --- | --- | --- | --- | --- | --- |
| `<peer-A>` | true | true | empty | empty | set (`<derp-region-1>`) | **derp** |
| `<peer-B>` | false | false | empty | empty | set (`<derp-region-1>`) | **offline** |

The offline peer still has `Relay` set: `Relay` is the peer's **home** DERP region, not proof that
traffic is relayed now. The spec's rule ("`Relay` set and `CurAddr` empty means DERP relayed")
would mislabel every offline and idle peer. Rule implemented and unit-tested in the spike:

1. `Online` false: `offline` (no path).
2. `CurAddr` non-empty: `direct` (scope `lan` or `wan` from the address class).
3. `PeerRelay` non-empty: `peer_relay`.
4. `Relay` non-empty and `Active`: `derp`.
5. `Relay` non-empty and not `Active`: `idle_unknown` (online, no recent traffic; current path
   unknown, so do not report "direct" and do not warn about relay).
6. Otherwise `no_path`.

Only `derp` and `offline` were seen live. Both phones are DERP-relayed or offline from this
machine, which matches the review's note that the owner's own tablet is relayed. A direct path
could not be provoked without sending traffic to a peer, which the spike must not do.

## CLI fallback

- `tailscale status --json` and `tailscale whois --json <ip>` work from the same unelevated
  process, exit 0, and return JSON with the same key sets (status) and the same field types
  (whois) as the pipe.
- Latency: CLI `status` about 30-40 ms, CLI `whois` median 23-41 ms (3 calls per run) against
  0.3-2.3 ms over the pipe. With the 60 s cache both are affordable; the CLI is a fallback and
  debugging aid, not the primary.
- Found at `tailscale` on `PATH` (Program Files fallback also implemented).

## `tailscale-localapi` crate (named in the pairing spec)

Checked in a scratch crate outside the repo: version 0.6.0 (MIT) works unelevated here. `status()`
and `whois(SocketAddr)` both succeed (tokio's default pipe QoS is `SECURITY_IDENTIFICATION`). But:

- the typed `PeerStatus` has no `PeerRelay` and no `CapMap`, so relay classification and CapMap
  need the raw JSON anyway;
- `PeerStatus` and `Node` have many required fields (strict deserialisation), while `status --json`
  is documented as subject to change;
- `whois` requires a `SocketAddr` (port) and has no `proto` argument;
- it pulls tokio, hyper and chrono.

## Implications for WP-12 (trust adapter)

1. Open the pipe with `SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION`. Map HTTP 401 to "LocalAPI
   refused this process", then try the CLI, then report "identity unavailable" in the tray.
2. Request form: `GET /localapi/v0/whois?addr=<ip>:<port>&proto=tcp`, percent-encoded, with
   `Host: local-tailscaled.sock` and no `Origin`/`Referer`. 404 means "no network identity"
   (loopback, LAN, not a tailnet node); 400 and 403 are client bugs.
3. Parse tolerantly: `serde_json::Value` or `#[serde(default)]` structs. `Node.Tags` and
   `Node.Sharer` are omitted when empty, top-level `CapMap` is `null`, `Node.CapMap` exists for self
   only. Treat the login as an opaque string (it is an email here; Headscale will differ).
4. Use whois for identity (the stable call) and `status` for path classification, using the rule
   above; do not use `Relay` alone. One cached `status` also maps every address to a login.
5. Self-whois works, so the host can show "this computer is signed in as `<user-1>`" and the
   approval dialog can tell "same login as this computer" (all three nodes here) from "another
   user" or "shared from another tailnet" without extra calls.
6. Build timeouts in from the start (synchronous pipe reads cannot time out) and retry on
   `ERROR_PIPE_BUSY`.
7. Adapter detection facts confirmed: interface GUID `{37217669-42da-4657-a55b-0d995d328250}`,
   description `Tailscale Tunnel`, network category Private, addresses a `/32` in 100.64.0.0/10 and
   a `/128` in fd7a:115c:a1e0::/48. The adapter also carries an `fe80::` link-local address, which
   the address filter must exclude.

## Where this differs from the spec and ADR text

- "The pipe admits the logged-in Tailscale user and LocalSystem only" (pairing spec, ADR-0002,
  ADR-0007): the pipe DACL admits BUILTIN\Users and SYSTEM. The restriction is enforced by
  tailscaled after the connection is made. Not testable with one account.
- The pairing spec does not mention the required client QoS; a default `File::open` or an
  anonymous client gets 401.
- The relay rule in the pairing spec and ADR-0002 needs `Online` and `Active` (see above).
- "Top-level `CapMap`" in the pairing spec reads as an object; it is `null` here.

## Not observed (needs something this machine cannot provide)

- **Tagged nodes**: `Node.Tags` populated and `LoginName` `tagged-devices`. Needs a tagged node.
- **Shared-in nodes**: `Node.Sharer` non-zero, `ShareeNode` in `status`. Needs a node shared from
  another tailnet.
- **Direct and peer-relay paths** live.
- **A non-administrator Windows account**, and the logged-in Tailscale user differing from the
  process's user: only an administrator account's filtered token was tested, so whether tailscaled
  refuses a different local user (and with which status) is unverified.
- **An elevated process** (not tried; no UAC prompt in an unattended run).
- **Headscale** and its CapMap round trip, **subnet-routed sources**, and other Tailscale versions.

WP-12 should be written against the Go field semantics and tested with fakes for these cases, and
the first run on a tailnet with tagged and shared nodes should rerun `cargo run -p spike-identity
-- --redact` and append the result here.

## Reproduce

```
cargo run -p spike-identity -- --redact                 # redacted summary on the console
cargo run -p spike-identity -- --redact --json          # redacted JSON on the console
cargo run -p spike-identity -- --redact --out <file>    # redacted JSON to disk
cargo run -p spike-identity                             # real values, console only (never commit)
cargo test -p spike-identity                            # codec, classification and redaction tests
```

Exit code 0 means the status JSON was parsed for self and at least one peer was classified into
`direct`, `peer_relay`, `derp` or `idle_unknown`.
