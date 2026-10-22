# Upstream tracking

Hokkabaz ports semantics (not code) from two MIT-licensed TypeScript projects.

| Upstream | Repo | Pinned | Role |
|---|---|---|---|
| DeepSeek Harness (`dsh`) | https://github.com/deepseek-ai/deepseek-harness | `687f282e` (2026-10-17) | Primary: core architecture, agent loop, session log, tools, DeepSeek adapter, schedule/webhook/workflow |
| OpenCode | https://github.com/anomalyco/opencode | `5fa426f` (2026-10-21) | Secondary: provider catalog (models.dev), agent modes, plugin hook surface, client/server event manifest |

## Reading list per Hokkabaz crate

| Crate | Read in dsh | Read in opencode |
|---|---|---|
| hk-core | `docs/architecture.md`, `docs/cordis-primer.md`, `docs/capability-seams.md`, `packages/boot` | – |
| hk-session | `docs/subsystems/session.md`, `docs/persistence-catalog.md`, `packages/session` | – |
| hk-agent | `docs/agent-lifecycle.md`, `packages/core/agent-loop`, `packages/compaction`, `packages/guard`, `packages/subagent`, `packages/goal`, `packages/todo`, `packages/plan` | agents (build/plan/general) |
| hk-tools | `docs/tool-execution-pipeline.md`, `docs/tool-catalog.md`, `packages/fs`, `packages/shell`, `packages/spill` | – |
| hk-sandbox | `packages/sandbox/*`, `native/system` (landlock launcher) | – |
| hk-llm | `packages/llm/*`, `docs/deepseek-llm-api-wire-extensions.md` | `packages/schema/src/models-dev.ts`, `packages/schema/src/config/provider.ts`, `packages/ai` |
| hk-mcp | `packages/mcp/*` | – |
| hk-acp | `packages/acp` | ACP mode |
| hk-flow | `packages/schedule`, `packages/workflow`, `packages/experimental/webhook*` | – |
| hk-server | `docs/api-gateway.md` | `packages/server`, `packages/schema/src/event-manifest.ts` |
| hk-plugin | `docs/cordis-primer.md` (effects/unload) | `packages/plugin` |

## Update procedure

1. Bump pinned SHA here.
2. Diff the reading-list paths since last SHA.
3. Open a task in PLAN.md per behavior worth porting.

## Concat (included, AGPL-3.0-or-later)

- Repo: https://github.com/jub0t/concat — submodule `third_party/concat`
- Pinned SHA: 2756209a92766a4e4726999a51648349a272c159 (v0.2.8)
- Bridge crate: `hk-concat` (P8.12). Update SHA here when bumping the submodule.
