---
document_type: project-progress
project: "OpenCrabs Windows Desktop Control and ACP Bridge"
prd: "PRD.md"
status: draft
current_milestone: 0
total_milestones: 3
last_updated: "2026-10-03 09:20"
---

# OpenCrabs Windows Desktop Control and ACP Bridge Progress

Live state for this target only. The PRD owns milestone outcomes, Done When, and FR/NFR/AC coverage; reference them rather than copying the specification.

## Current State

- PRD version: 0.1.0 (draft, structural validation passed: 6 sections, 12 requirements, 18 acceptance criteria, 12 traceability rows, 1 diagram, 3 milestones, 0 blockers)
- Execution mode: native by default; the Windows lane runs on a hosted runner, never on the 4 GB remote host.
- Build authority: The Windows control paths are already merged upstream; the relay and protocol image work live on the fork and were built on request. No new build request is pending.
- Active milestone / next action: Milestone 2. Next useful action is to confirm the relay revision currently packaged in the release matches the source head, then capture a handshake receipt against that revision and a frame dump image on a fresh runner.
- Required unresolved input: A Windows desktop to run the clipboard and self-update paths by hand, since a runner receipt proves the code compiles and starts but not that a human-visible flow behaves. Owner decision pending on whether the parked Windows service branch is kept or dropped.

## Milestones And Evidence

| # | PRD milestone reference | Status | Current evidence / remaining gap |
|---|---|---|---|
| 1 | Milestone 1: Windows desktop control parity | ✅ Verified | The lock, scheduled-job interpreter, self-update and clipboard changes are merged upstream, and the fork's own Windows lane compiles the crate and executes the binary with a version receipt. Gap: no human run of the clipboard or self-update paths on a Windows desktop; the lock and job paths carry runner-level evidence only. |
| 2 | Milestone 2: Shippable Windows build and relay | 🔄 In Progress | The artifact workflow builds and uploads the Windows executable, and a relay handshake completed from a Windows editor on 2026-10-02 with byte counts recorded in both directions. Gaps: the packaged relay revision is not yet re-verified against the current source head, and no frame dump image has been captured on a fresh runner. |
| 3 | Milestone 3: Integrated audit and repair | ⬜ Not Started | No integrated audit exists. Every requirement still needs a source-state-bound VERIFIED or explicitly UNVERIFIED status. |

Statuses: ⬜ Not Started, 🔄 In Progress, ✅ Verified, ✅ Approved. Verified means required checks passed; Approved requires an actual owner verdict.

## Resume And Changes

Changed paths recorded so far (fork `adi805/opencrabs-windows`, main at commit `b6d0d48f`):

- `src/config/winlock.rs` and its use in `src/config/profile.rs` (kernel-held profile lock)
- `src/cli/service_windows.rs` (parked service branch, not on main)
- `src/tests/ui_snapshot_dump_test.rs` and `src/tests/browser_default_windows_test.rs`
- `.github/workflows/windows-artifact.yml` (build, execute, upload)
- `.github/workflows/ci.yml` (Windows build lane)
- `.github/workflows/pr-agent.yml` and `.github/workflows/pr-agent-manual.yml` (review workflows)
- Relay source on branch `windows/08-acp-images`: `src/bin/opencrabs_bridge.rs`, plus the protocol image flag and the catalog read path
- `docs/WINDOWS-SETUP.md` (operator setup guide, in the companion MonoCode fork)

Source-bound checks already performed:

- The Windows artifact workflow's smoke step was run on a hosted runner and printed `exit=0 version=opencrabs 0.5.4`.
- The upstream Windows build lane was proven to skip its build steps while still concluding success, which is why the fork's artifact job is written to fail on skipped steps.
- A relay handshake from a Windows editor returned the server's initialize response, and the receipt log showed a first byte in both directions.
- A path-filtered green job is not proof that a platform lane compiled or ran.

Preserved failures worth keeping:

- A published release is immutable, so replacing an asset without regenerating the digest leaves the manifest lying. The republish rule is: upload, regenerate the digest from the real files, upload the digest, then re-download and check.
- A pre-readiness write to standard output from a shell startup file poisoned the first protocol frame and made every editor attempt time out with no client-side error.

Follow the PRD's embedded builder contract; no new approval gate is created by this file or an ordinary milestone transition.
