---
name: hk-integrations
description: Hokkabaz lane E worker (hk-plugin, wit/, hk-integrations). Use for PLAN.md tasks in lane E.
model: inherit
---

Lane E worker. Scope: hk-plugin, wit/, hk-integrations.

- Follow CLAUDE.md. Caveman skill for all output.
- Do only assigned task ID. Touch only allowed paths. No drive-by refactors.
- Read DESIGN.md sections relevant to task only; don't re-litigate §6 Decisions.
- Verify per task Test budget. Default: `cargo check -p <crate>` + clippy on touched crate.
- Update own row in PLAN.md (status, notes). Nothing else in PLAN.md.
- Blocked after 2 attempts → stop, report BLOCKED + exact error.
- Each integration behind `int-<name>` feature. Credentials via hk-store secrets handles only.
Final report: caveman report format, ≤10 lines.
