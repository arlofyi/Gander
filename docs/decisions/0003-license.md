# 0003. License

- Status: Proposed, with **Apache-2.0 as the default for the implementation run** (the owner must
  confirm before anything is pushed to a public remote; the run writes `LICENSE` and SPDX headers
  as Apache-2.0 and stops before publishing)
- Date: 2026-11-01

## Options

1. **Apache-2.0** -- permissive, patent grant, easy adoption and contribution.
2. **MIT** -- permissive, shortest, no patent grant.
3. **AGPL-3.0** -- forces modified hosted versions to stay open; deters commercial forks; some
   contributors and companies avoid it.
4. **MPL-2.0** -- file-level copyleft middle ground.

## Dependency constraints found on 2026-11-01

- Sunshine is GPL-3.0: its code cannot be copied into an Apache-2.0 (or MIT) repo. Read for
  ideas only.
- Baton (closest prior art) shows no license at all; treat as all rights reserved.
- FFmpeg is LGPL only in its default build; `--enable-gpl` builds (libx264/x265) would make a
  shipped binary GPL. Gander avoids bundling FFmpeg: previews use OS codecs (WIC, Windows.Data.Pdf,
  Media Foundation) and only call an `ffmpeg.exe` already on the user's PATH as an optional extra.
- `webrtc-rs`, `str0m`, `windows-capture`, `moq-video`, `wasapi`, `egui`, `tray-icon` are MIT
  and/or Apache-2.0: compatible.
- The Android client will ship prebuilt libwebrtc binaries (BSD-3). F-Droid inclusion would require
  building libwebrtc from source; that is a release-engineering question, not a licence blocker.

## Leaning

Apache-2.0 for host and client, unless there is a plan to sell a hosted or pro version, in which
case AGPL-3.0 for the host keeps forks honest. Nothing in the architecture depends on this choice.

## Decision

Owner to decide before the repo goes public. Default for the run: Apache-2.0.
