# Spec: pairing, tokens, transport trust and network identity

- Status: Draft for implementation (Phase 1, WP-12 / WP-13)
- Date: 2026-11-01
- Implements: `security.md` non-negotiables 1, 2, 3, 5, 9; ADR-0002; ADR-0005

## Threats this spec must defeat

1. A device on the private network that is NOT paired reads files or launches apps.
2. A man in the middle on the private network (a compromised peer, a malicious subnet router)
   intercepts pairing and obtains a token or impersonates the host.
3. A scammer walks the owner through pairing an attacker device ("read me the code").
4. A stolen or leaked token is replayed from a different device or network.
5. A malicious local web page on the phone or the host talks to the Gander listener.
6. Secrets leak through logs, backups or crash dumps.

## Host identity

- At first run the host generates an ECDSA P-256 key pair and a self-signed certificate
  (10-year validity). The **host id** is the base64url SHA-256 of the certificate's
  SubjectPublicKeyInfo. The private key is stored in `%LOCALAPPDATA%\Gander\host.key` with an ACL
  limited to the owning user (macOS/Linux: 0600). Windows DPAPI protection of the key file is used
  when available (`CryptProtectData`, current-user scope).
- Clients pin the host id. The system trust store is never consulted for Gander hosts.
- Key rotation is deliberately unsupported in v1: a rotated key means re-pairing. (A future
  `host.rekey` flow may sign the new SPKI with the old key.)

## Listener and network gate

- The listener binds **loopback** plus the addresses selected by the active **trust adapter**.
  It never binds a wildcard address. Each bound socket is logged at startup and whenever it changes.
- **tailscale adapter** (auto-selected when the Tailscale adapter is present: Wintun adapter GUID
  `{37217669-42da-4657-a55b-0d995d328250}`, description "Tailscale Tunnel", with addresses in
  100.64.0.0/10 or fd7a:115c:a1e0::/48): binds those addresses; on each new connection calls
  WhoIs on the peer address and caches the answer for 60 s.
  - Primary: LocalAPI over the named pipe `\\.\pipe\ProtectedPrefix\Administrators\Tailscale\tailscaled`,
    `GET /localapi/v0/whois?addr=<ip>&proto=tcp` with `Host: local-tailscaled.sock` and no
    `Origin`/`Referer` headers (the `tailscale-localapi` crate wraps this). The pipe admits the
    logged-in Tailscale user and LocalSystem only, which is one reason the host runs as the
    interactive user (ADR-0007).
  - Fallback and debugging aid: `tailscale whois --json <ip>` (CLI, v1.56+), same JSON shape
    (`Node`, `UserProfile`, `CapMap`). Confirmed working unelevated on the development machine.
  - Semantics: WhoIs resolves tailnet node addresses only. Loopback, LAN and subnet-routed
    sources return not-found and are treated as "no network identity". **Tagged nodes** carry
    `Node.Tags` and a pseudo-user login `tagged-devices`; they are refused at pairing by default
    (option `allow_tagged_peers`). **Shared-in nodes** resolve to the external user and carry
    `Node.Sharer`; the approval dialog shows "shared from another tailnet" so the owner notices.
    Headscale returns a plain username instead of an email; the login is treated as an opaque string.
  - The adapter also reads `tailscale status --json` (or LocalAPI `/status`) to classify the
    peer's path: direct (`CurAddr` set), peer relay (`PeerRelay` set) or DERP relayed (`Relay`
    set, `CurAddr` empty). The result is reported in `/me.network.relayed` so the client can warn
    before a Stream: measured DERP throughput is a few Mbit/s.
  - **Shields Up** (Tailscale "Allow incoming connections" off) blocks every inbound connection
    over the tailnet; the tray UI detects `tailscale status` reporting it and explains.
- **generic adapter** (ZeroTier, Headscale without LocalAPI access, plain WireGuard): the owner
  picks the interface in the tray UI, or enters the private CIDRs. Binds that interface's addresses.
  No identity is available; pairing and tokens are the only gate.
- The host subscribes to interface/address change notifications (Windows:
  `NotifyUnicastIpAddressChange`; also re-plan every 30 s as a safety net). When a bound address
  disappears the socket is closed within 5 s; when the private network comes back the listener is
  re-created. Loopback stays up throughout.
- Windows Firewall: tailscaled itself installs `Tailscale-In` (allow inbound to each tailnet
  address, profiles Private and Domain, no program restriction), so a listener on the tailnet IP
  usually works without a rule of its own. It silently fails when the adapter's category is
  Public, when the user once dismissed Windows' first-listen prompt for a Gander binary (that
  creates Block rules, which override allows), or when Shields Up is on. The installer therefore
  adds an explicit allow rule for the Gander executable (`New-NetFirewallRule -Program <exe>
  -Protocol TCP -LocalPort <port> -RemoteAddress 100.64.0.0/10,fd7a:115c:a1e0::/48 -Profile Any`,
  plus a UDP rule for WebRTC media on the same remote ranges), and the tray UI runs a connectivity
  self-check that lists Block rules naming Gander and offers to remove them with elevation.

## Pairing

Pairing only exists while the owner has the **"Add a device" window open on the host**. At every
other time `POST /pair` returns 404 `pairing_closed`. There is no inbound "accept a session from a
code" flow anywhere in Gander; the only way to get a token is for the owner to open that window
and approve the device on the host screen.

### Flow A: QR (primary)

1. Owner clicks "Add a device". Host creates a pairing window: `secret` (32 random bytes),
   expiry 5 minutes, max 5 attempts. Shows a QR encoding
   `gander://pair?v=1&id=<host_id>&a=<addr1>,<addr2>&p=<port>&n=<host name>&s=<secret b64url>&m=<magicdns>`.
   The QR is never logged and the secret is never displayed as text.
2. Phone scans, connects to the first reachable address, verifies the certificate SPKI equals `id`
   (hard failure otherwise: "This is not the computer the code came from"), then
   `POST /v1/pair` with `{ secret, device, nonce }`.
3. Host verifies `secret` (constant-time), creates a pending pairing, shows on the host screen:
   "**Pixel 9** (Android 16, Gander 0.1) wants to pair. Code **482 913**. Allow / Deny". The phone
   shows the same code and "Waiting for approval on studio-pc".
4. Owner clicks Allow. Host creates the device record and token; `GET /v1/pair/{id}` returns
   the token once; the host marks the pairing used. Deny -> 403; timeout (2 minutes) -> 410.

The SAS code is `SAS = decimal(first 20 bits of SHA-256("gander-sas-v1" || host_spki || device_nonce || host_nonce))`
zero-padded to 6 digits, grouped 3+3. In flow A it is informational (the QR already carried the
pin); in flow B it is the integrity check.

### Flow B: manual (no camera, or a tablet pairing a second host)

1. Owner opens "Add a device" on the host; the window also displays the host's addresses, port and
   MagicDNS name.
2. On the phone the user types the address. Phone fetches `GET /v1/host`, obtains `host_id`
   (trust on first use for THIS step only), then `POST /v1/pair` with `{ device, nonce }` and
   no secret.
3. Host shows the device and the SAS code; the phone shows the SAS code it computed from the
   certificate IT saw. The owner compares them and clicks Allow only if they match. A MITM holding
   a different certificate produces a different code on the phone.
4. The UI text on the host says explicitly: "Only approve if the code on your phone matches. If
   someone is telling you a code over the phone, click Deny."

### Rate limits and hygiene

- 5 failed `POST /pair` per source address per pairing window; then 429 for the rest of the
  window. Opening the window again resets it; the host UI shows the failure count.
- The pairing window closes automatically on success, on Deny, after 5 minutes, or when the owner
  closes the dialog.
- Tailscale adapter: at approval time the host records `WhoIs(peer) = { login, node_id, node_name }`
  on the device record and displays the login name in the approval dialog ("alice@example.com's
  pixel-9"). If WhoIs fails the dialog says "identity unknown (not a Tailscale peer)".

## Tokens

- 32 random bytes from the OS CSPRNG, base64url without padding. Sent as `Authorization: Bearer`.
- Stored on the host as SHA-256(token) in `%APPDATA%\Gander\devices.toml` (owner-only ACL). The
  plaintext exists only in the one `GET /pair/{id}` response.
- Device record: `id` (ULID), `name`, `platform`, `model`, `app_version`, `token_hash`,
  `created_at`, `last_seen_at`, `last_address`, `network_identity` (optional), `revoked_at`.
- Verification is a constant-time compare on the hash. Lookup is by hash (no device id in the
  request), so a token that is not in the table costs one hash.
- **Revocation** is immediate: the record is marked revoked (kept 30 days for the audit trail) and
  every open connection and stream session for that device is closed. The device sees 401
  `token_revoked` and a `device.revoked` event if its socket was open.
- **Rotation**: `POST /v1/token/rotate` returns a new token and invalidates the old one. The client
  rotates after a restore-from-backup is detected or on user request; not on a schedule.
- **Network binding** (tailscale adapter): when the device record has `network_identity.login`,
  every request's WhoIs login must match or the request is refused with 403
  `network_identity_mismatch` and logged. Node id mismatch is logged as a warning only (phones get
  new node ids when Tailscale is reinstalled). Option `strict_node_binding` makes it a refusal.
- **Loopback requests** (local tools, tests) still need a token. Nothing on loopback is
  token-free except `GET /v1/host`.

## Client-side storage

- Android: token, host id, addresses, host name in an encrypted store backed by the Android
  Keystore (EncryptedSharedPreferences or DataStore + Tink). Excluded from auto backup and device
  transfer (`dataExtractionRules`). Optional biometric app lock gates the UI, not the token.
- iOS (later): Keychain, `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`.
- The client never logs the token, the QR payload or the secret.

## Host-side visibility and audit

- Every pairing attempt, approval, denial, revocation, identity mismatch and listener change is an
  audit event: `{ ts, event, device_id, source_addr, outcome, detail }`, JSON lines in
  `%LOCALAPPDATA%\Gander\logs\audit.jsonl`, rotated at 10 MiB x 5. Never contains tokens,
  secrets or file contents. Paths appear in access events (the owner's own data, local only).
- The tray shows a persistent list "Paired devices" with last seen, address and identity, each
  with Revoke.

## Tests

- Unit: SAS derivation vectors; token hash + constant-time compare; pairing state machine
  (open/closed/expired/attempts); listener plan never contains `0.0.0.0` or `::` for any adapter
  configuration (property test over random interface tables).
- Integration: full flow A and flow B against a loopback host with a fake trust adapter; MITM
  simulation (different cert) must yield different SAS codes; revocation closes an open WebSocket
  within 1 s; identity mismatch refusal with a fake WhoIs.
- Negative: `POST /pair` while closed -> 404; 6th attempt -> 429; browser-marked request -> 403.
