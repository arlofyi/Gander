# Orchestrator kickoff brief (read this first, then start)

- Date: 2026-11-01
- For: the Conductor session (Claude Opus 5.5) that spawns and steers worker sessions in Conduit
- Companion: `2026-11-01-master-plan.md` (the work packages, gates, fallbacks, report template)

## 0. Thirty-second status

Gander is planned to implementation depth and has no code. The owner has read the plan review
and the master plan and has approved the decision defaults and the Night 1 scope (section 1).
The repo is a local git repository on `main` with the planning corpus committed; there is no
remote and you must not create one. Your job tonight is to turn the master plan into verified,
committed code through workers, and to leave a morning report.

## 1. Owner approvals in force (given 2026-11-01 evening, recorded in `docs/context.md`)

| Topic | Owner's answer | What it means for you |
| --- | --- | --- |
| Decision defaults D1-D14 | Accept all | Flip ADR-0001 (host and client lines), 0002, 0005, 0006, 0007 to `Accepted` in the first docs commit; ADR-0003 stays Proposed with the Apache-2.0 default (nothing is published) |
| Installs | Android emulator + API 36 system image, PowerShell 7 | WP-00 may run `sdkmanager` for `emulator`, `platform-tools`, `system-images;android-36;google_apis;x86_64` and create the `gander` AVD, and `winget install Microsoft.PowerShell`. Nothing else may be installed. LibreOffice was NOT approved |
| Night 1 scope | Everything, including a Phase 2 attempt | Gates G0-G4 first, in order; only after G4 passes may WP-50 onward start. A half-finished Phase 2 package is acceptable if it is on its own branch and the report says exactly where it stopped |
| Physical device | Emulator only for now; a phone later | Device-gated items are deferred to the morning list, never attempted. Do not `adb connect` to anything on the tailnet |

Still human-gated (master plan section 6): remote creation and pushes, licence confirmation,
unlocking the machine (spike item C6), signing, store accounts, the real-app Stream checklist,
any change to `security.md` non-negotiables.

## 2. Startup sequence (first 15 minutes)

1. `fleet_capabilities()` and `fleet_list()` to learn which agents exist and the spawn cap.
2. Read `CLAUDE.md`, `docs/context.md`, master plan sections 0-3 and 5-9. Do not read every spec
   yourself; workers read the spec for their package.
3. Spawn one worker for **WP-00 + WP-01** (preflight, scaffold, CI, licence file, `cargo xtask`).
   Everything else depends on it. Give it the approvals table above verbatim so it knows the
   emulator and pwsh installs are allowed. Expect 30-45 minutes including the first Gradle and
   crates downloads.
4. While it runs, prepare the mission briefs for wave 1 (section 4) from the master plan text.
5. When WP-01 reports, have the integrator (section 5) create the `night-1` branch from the
   worker's branch, then start wave 1.

## 3. Mission brief template for every worker

Use `fleet_spawn` with `objective`, `outputShape` and `boundaries` filled from the master plan
entry. Keep briefs byte-stable where possible so caches hit.

```
task: Implement <WP-id> <title> for Gander per docs/plans/2026-11-01-master-plan.md.
objective: <the WP's Goal, verbatim>. Read first: CLAUDE.md, docs/specs/<spec for this WP>.md,
  docs/specs/2026-11-01-protocol-v1.md (if it touches the API), docs/security.md non-negotiables.
outputShape: Code and tests on branch feat/<wp-id>-<slug>, committed with Conventional Commits
  (scope host|client|spike|docs). Then call fleet_result once with: status, the exact Verify
  command(s) run and their tail output, files created, anything the spec got wrong (do NOT edit
  specs), and open questions. If Verify cannot pass after three attempts, apply the WP's Fallback,
  mark status "deferred" and say why.
boundaries: Do not push, do not create remotes, do not install software (only WP-00 may, and
  only the approved items), do not edit docs/specs or docs/decisions, do not bind 0.0.0.0 or ::
  anywhere including tests, do not copy code from GPL or unlicensed projects (Sunshine, Baton),
  do not put secrets or tokens in fixtures, do not change power or lock settings except where
  the stream spike spec allows and then restore them. Start by merging the integration branch
  `night-1` into your branch if your package depends on earlier packages.
```

Routing (master plan section 9, plus Conduit's own rules): Rust core, security and capture
packages (WP-10, 11, 12, 13, 15, 19, 21, 27, 41-43, 50-52) go to a Claude `hard` tier worker at
high effort; screens and UI (WP-18, 22-26, 54) to `std`; scaffolding, docs, CI, installer
(WP-01, 1A, 1C, 20, 24, 28, 40, 47) to a `cheap` tier or a local OpenCode model; git-heavy
integration to Codex if available, else Claude `std`. Only Tier-1 agents (Claude, OpenCode,
Command Code) may be given tasks that need `fleet_note`/`fleet_inbox`. Gemini has no result path
on this build; do not use it. Escalate effort on objective signals only (failing verify, no
result), never on a worker's self-rated confidence.

## 4. Waves (dependency order; parallel within a wave, capped by the spawn limit)

| Wave | Packages | Notes |
| --- | --- | --- |
| 0 | WP-00 + WP-01 (one worker) | preflight report + scaffold; creates `cargo xtask`, CI, LICENSE, SECURITY.md |
| 1 | WP-10 core types/config/openapi · WP-20 client scaffold · WP-40 test window · WP-46 identity spike | all independent of each other |
| 2 | WP-11 path resolver · WP-12 listener/TLS/adapters · WP-15 render worker + thumbs · WP-19 stream policy · WP-21 client net layer · WP-41 capture spike | WP-11/12/15/19 need WP-10; WP-21 needs WP-20; WP-41 needs WP-40 |
| 3 | WP-13 pairing/tokens/audit · WP-14 Browse/Fetch API · WP-16 previews · WP-17 events · WP-22 pairing screens · WP-23 browser · WP-42 encode spike | WP-13 needs WP-12; WP-14 needs WP-11+13; WP-16 needs WP-15 |
| 4 | WP-18 tray UI · WP-1B e2e harness · WP-1A installer · WP-24 thumbs · WP-25 file sheet · WP-26 previews · WP-27 fetch engine · WP-43 WebRTC spike · WP-47 files-app spike (optional) | |
| 5 | WP-28 cache/settings · WP-29 emulator e2e · WP-44 Android spike + input · WP-45 findings + ADR flips · WP-1C docs sync | G1-G4 are checked here |
| 6 (only after G4) | WP-50 stream engine · WP-51 launch/discovery · then WP-52, 53, 54 as time allows | Phase 2 attempt per the owner's scope; stop cleanly at the first package that cannot finish |

Rules of thumb: never run two workers on the same crate at once; if the cap forces a choice,
prefer finishing a gate over starting a new wave; a worker that has not produced `fleet_result`
is "not yet reported", not failed, so check `fleet_list` before re-spawning.

## 5. Integration and branches

- `main` holds the planning corpus and is never merged into tonight (CLAUDE.md rule).
- One **integrator** worker owns the `night-1` branch: it merges each verified `feat/*` branch,
  re-runs `cargo test --workspace` and the client unit tests on the merged tree, and reports.
  Merge conflicts go back to the package's worker (via `fleet_send`) or to a fresh worker with
  both branch names.
- Dependent workers start by merging `night-1` into their branch (the brief says so). If
  `fleet_spawn` can base a worktree on a branch, base it on `night-1` instead.
- Docs-only fixes to specs (a worker found an error) are made by a docs worker on `night-1`
  directly, in a `docs(spec): ...` commit, and announced on the fleet channel.
- At the end, `night-1` is the deliverable. The morning report names it and the commit range.

## 6. Gates and stop conditions

Gates G0-G6 and their commands are in master plan section 5. The run stops at the first gate it
cannot pass after fallbacks, writes the report, and leaves the machine clean. Also stop if: the
spawn cap is exhausted and every worker is blocked; any worker reports a security non-negotiable
cannot be met; disk space falls under 5 GB; or a worker asks for an install not in the approved list.

## 7. Environment traps you will meet (measured 2026-11-01)

- Only Windows PowerShell 5.1 is present until WP-00 installs pwsh 7; the owner's Claude Code profile hooks
  block inline `pwsh -c`. Repo automation is `cargo xtask`, not shell scripts.
- The emulator reaches the host at `10.0.2.2`; the host must bind loopback (it does). The tailnet
  adapter path cannot be exercised from the emulator; that is a device-gated test.
- Display turns off after 60 minutes on AC; the session is not configured to lock on that, but
  capture tests should run early in the night. Any `powercfg` change must be restored.
- Hybrid GPU (Intel + RTX 5060): the capture adapter may differ from the preferred encoder;
  the spike measures both (matrix item C12).
- `.pdf` opens in Edge here; `.txt` in packaged Notepad; PowerPoint is installed (Office COM
  tests run). ffmpeg is on PATH (used only to validate spike output).
- `windows-capture` may own its D3D device; the fallback is a thin WGC wrapper (WP-41).
- The Tailscale tablet on this tailnet is usually DERP-relayed; irrelevant tonight (emulator only).

## 8. Morning report

Write `docs/reports/2026-11-02-overnight-report.md` from master plan section 8. Update
`docs/context.md` (state, next actions, human actions needed) and `CHANGELOG.md`. Confirm the
machine state is restored and no spike or emulator process is left running. The report is the
only thing the owner is guaranteed to read; make its first paragraph carry the whole picture.
