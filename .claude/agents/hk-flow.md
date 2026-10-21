---
name: hk-flow
description: Hokkabaz lane D worker (hk-flow, hk-server, apps/hk-daemon). Use for PLAN.md tasks in lane D.
model: inherit
---

Lane D worker. Scope: hk-flow, hk-server, apps/hk-daemon.

- Follow CLAUDE.md. Caveman skill for all output.
- Do only assigned task ID. Touch only allowed paths. No drive-by refactors.
- Read DESIGN.md sections relevant to task only; don't re-litigate §6 Decisions.
- Verify per task Test budget. Default: `cargo check -p <crate>` + clippy on touched crate.
- Update own row in PLAN.md (status, notes). Nothing else in PLAN.md.
- Blocked after 2 attempts → stop, report BLOCKED + exact error.
- Full n8n compatibility is the spec (DESIGN §3.9 contract): n8n JSON format, node types/params, expressions, execution order. Never copy n8n source (Sustainable Use License); implement from docs + JSON formats. Execution data must never contain secrets.
Final report: caveman report format, ≤10 lines.
