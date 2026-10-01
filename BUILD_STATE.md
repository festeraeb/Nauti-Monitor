# Build State — Nauti-Monitor

Auto-generated gate baseline (2026-10-01) by `tools/reposstatus` survey.
Every gate below is individually checkable. A gate is marked CLOSED only
where the check was actually run; anything unverified is TODO on purpose —
an optimistic gate is worse than an open one.

## Gates

| Gate | What it checks | How to verify | State |
|---|---|---|---|
| **G1** | README describes purpose and how to run it | test -f /codebase/repos/Nauti-Monitor/README.md | ✅ CLOSED |
| **G2** | CI workflow builds on every push | ls .github/workflows/*.yml | TODO |
| **G3** | Working tree is clean (no uncommitted work in flight) | git status --porcelain | ✅ CLOSED |
| **G4** | Cargo workspace builds clean | `cargo build --workspace` | TODO |
| **G5** | Repo is reachable from the fleet agent runtime | cd /codebase/repos/Nauti-Monitor && git rev-parse --short HEAD | ✅ CLOSED |

## Notes

- Class: real · source files: 9 · tracked LOC: 1735
- Rebuild this file when a gate changes state; it is the source repostatus reads.
