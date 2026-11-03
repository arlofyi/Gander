# 0006. Stream mode launch policy: allowlist by handler executable, owner-approved, trusted-device permission

- Status: Accepted on 2026-11-01 (owner approved defaults D1-D14)
- Date: 2026-11-01
- Spec: `docs/specs/2026-11-01-stream-launch-policy.md`

## Context

`security.md` says Stream "opens a file with an allowlisted desktop application" and lists a
blocklist of extensions. Two findings make that insufficient:

1. File associations can point at interpreters (`.py` -> python, `.js` -> wscript, `.jar` -> java)
   and at browsers (on the development machine `.pdf` opens in Microsoft Edge, which would stream
   the owner's whole browser window with all its tabs). The security property must be about the
   **executable that receives the file**, not the extension.
2. Any streamed desktop app exposes the whole filesystem through its own Open/Save dialogs, and
   input injection on Windows is screen-coordinate based (research 2026-11-01: `SendInput` and
   pointer injection hit whatever is at that point; UIPI blocks elevated windows). Stream cannot
   be made share-scoped in v1.

## Options

1. Extension blocklist only (as drafted). Insufficient, see above.
2. **Resolve the handler executable without executing; refuse a never-list of interpreters and
   shells; allow only apps in an owner-approved catalogue (first use prompts on the host);
   browsers only through an isolated-window launch template; Stream is a per-share, per-device
   permission off by default and described in the UI as trusted-device access.**
3. Run streamed apps inside a sandbox (AppContainer, Windows Sandbox, restricted token). Heavy,
   breaks many apps, and still leaks through dialogs of apps that run at all. Possibly later for
   specific apps.
4. Only ship a fixed list of supported apps. Too narrow for the "stream Blender/CAD" promise and
   still needs the handler resolution.

## Decision

Option 2 as specified. Hard refusals by final extension remain as a first filter. The policy
engine ships in Phase 1 so listings can show `stream: allowed | refused | ask` before Stream itself
exists.

## Consequences

- Easier: the owner sees and approves every app that can ever be launched by a phone; the audit
  log records the decision path; browsers stop being a privacy leak.
- Harder: the catalogue needs launch templates and window-matching strategies per app family;
  first-use prompts need someone at the host (unattended -> 412); packaged (UWP) apps need the
  ApplicationFrameHost mapping.
- Must do: the UI copy when granting Stream on a share must say that the app's own dialogs can
  reach other folders; `security.md` gains a "Stream is trusted-device access" paragraph and the
  residual-risk statement.
