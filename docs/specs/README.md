# Specs

Design specs and spike findings, one file per topic: `YYYY-MM-DD-<topic>.md` or
`YYYY-MM-DD-<topic>-findings.md`. A spec says WHAT and WHY; the matching plan in `../plans/`
says HOW, step by step. Findings record what was MEASURED, on which OS and version.

## Findings index

- `2026-11-01-stream-spike-findings.md` -- stream spike (WP-41 capture, WP-42 encode, WP-43 WebRTC,
  WP-44 Android + input) and the WP-45 summary, consolidated matrix and streaming-stack decisions;
  spec: `2026-11-01-stream-spike.md`.
- `2026-11-01-identity-spike-findings.md` -- Tailscale LocalAPI identity (WP-46), feeds
  `2026-11-01-pairing-and-trust.md`.
- `2026-11-01-files-app-spike-findings.md` -- `DocumentsProvider` and proxy file descriptors
  (WP-47), feeds `2026-11-01-android-client-v1.md`.
