# CLAUDE.md — Hokkabaz agent rules

Hokkabaz = Rust-native, multi-provider AI agent harness + n8n-style automation, Flutter UI (desktop + Android APK). Ports concepts from `deepseek-ai/deepseek-harness` (primary) and `anomalyco/opencode` (secondary). Read `DESIGN.md` for architecture, `PLAN.md` for tasks. UI language: English.

## Talk style (mandatory)

Caveman mode. Applies to every agent and subagent, all replies to user and orchestrator.

- Short. Drop articles, filler, pleasantries, recaps. Fragments ok.
- Report = what changed + what blocks. No narration of steps.
- No "I will now…", no "Great question", no summaries of summaries.
- Code, paths, commands, errors: exact and full. Terse prose only.
- Subagent final report ≤ 10 lines: `DONE|BLOCKED`, files touched, open issues.
- User speaks Turkish → reply Turkish, same terse rules. Repo content stays English.
- Skill: `.claude/skills/caveman/SKILL.md`.

## Token budget rules

- Read only files needed. Use Grep/Glob before Read. Read ranges, not whole large files.
- No re-reading files just edited. No re-deriving decisions in `DESIGN.md` §Decisions.
- Don't paste large outputs. Pipe through `tail -n 30` / `head`.
- Don't spawn subagents for tasks doable in < 5 tool calls.

## Test policy (lean)

User wants minimal test spend.

- Default verification: `cargo check -p <crate>` + `cargo clippy -p <crate> -- -D warnings`. Not whole workspace.
- Run tests only for crate touched: `cargo test -p <crate> <filter>`. Never full `cargo test --workspace` locally unless task says so.
- Write tests only for: session log replay, turn/step state machine, tool pipeline guards, flow engine scheduling, n8n expression shim, provider stream parsers (fixture-based), sandbox policy. Skip tests for UI glue, FFI wrappers, trivial adapters.
- n8n compat: fixture corpus (`crates/hk-flow/tests/n8n/`: workflow + trigger payload → expected runData). Main test investment; run only cases for nodes touched.
- Provider tests use recorded fixtures (`tests/fixtures/*.jsonl`). No live API calls in tests.
- Flutter: `flutter analyze` only. Widget tests only for flow canvas model logic.
- CI runs the full matrix; agents don't duplicate it.

## Repo map (target)

```
crates/
  hk-core/          plugin host, services, events, effects, config
  hk-session/       append-only event log, projections, message derivation
  hk-llm/           provider trait, stream vocab, adapters (deepseek first), models.dev catalog
  hk-agent/         agent loop (turn/step), modes, subagents, compaction
  hk-tools/         tool registry, guarded pipeline, builtin tools
  hk-sandbox/       policy, approvals, landlock/seatbelt/none
  hk-mcp/           MCP client (rmcp)
  hk-plugin/        WASM plugin runtime (wasmtime component model, WIT)
  hk-flow/          n8n-compatible workflow engine: n8n JSON, nodes, JS expressions, executions
  hk-integrations/  built-in OSS integration nodes (feature-gated)
  hk-store/         SQLite (sqlx), secrets, migrations
  hk-server/        axum: webhooks, REST + WS API, instance auth
  hk-acp/           ACP client: drive external agents (dsh, opencode) — desktop only
  hk-ffi/           flutter_rust_bridge API surface
apps/
  hk-cli/           `hk` CLI + TUI (ratatui)
  hk-daemon/        `hokkabazd` headless instance
  flutter/          Flutter app (desktop + android)
wit/                plugin WIT interfaces
docs/               UPSTREAM.md, ADRs, guides
```

## Conventions

- Rust stable, edition 2024. `tokio` runtime. `thiserror` in libs, `anyhow` only in apps.
- Errors never panic across FFI. All `hk-ffi` fns return `Result<T, HkError>`.
- Async traits: native `async fn` in traits; `dyn` via `async-trait` only where needed.
- Serialization: `serde`, JSON on the wire, snake_case fields. Event names `domain/action`.
- Logging: `tracing`. No `println!` in libs.
- Feature flags gate heavy deps: `local-llm`, `wasm-plugins`, `acp`, `server`, per-integration `int-*`.
- Android target builds must not pull: landlock, acp, stdio MCP spawn. Guard with `cfg(target_os = "android")`.
- Secrets never in logs, session log, or flow execution data. Use `hk-store::secrets` handles.
- Model-visible means logged (from dsh): every model request reconstructable from session log.
- Comments: why, not what. Match surrounding density.

## Upstream porting rules

- Don't vendor TypeScript. Port semantics into Rust. Cite upstream path in a `// port: dsh packages/core/agent-loop` comment at module top only.
- Pinned upstream SHAs + concept map: `docs/UPSTREAM.md`. Update when porting new behavior.
- Both upstreams MIT. Keep `THIRD_PARTY_NOTICES.md` current.

## Git

- Branch per task: `hk/<task-id>-<slug>` unless session assigns branch.
- Commit msg: `<area>: <imperative summary>` e.g. `hk-llm: add deepseek stream parser`.
- One task = one PR-sized change. No drive-by refactors.

## Orchestration

- Orchestrator owns `PLAN.md` status. Workers update only their task row (`status`, `notes`).
- Worker agents: `.claude/agents/*.md`. Pick by lane (A–F in `PLAN.md`).
- Worker gets: task id, DoD, files allowed. Worker returns caveman report.
- Blocked > 2 attempts → stop, report `BLOCKED` with exact error. No guessing loops.
