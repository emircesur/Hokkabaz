# Hokkabaz — Implementation Plan

Orchestrator-owned. Workers update only their row's `status`/`notes`. Status: `todo | wip | done | blocked`.
Sizes: S ≤ ½ day, M ≤ 2 days, L ≤ 5 days (agent-time estimates, rough).
Test column: `none` = `cargo check`/`flutter analyze` only · `unit` = targeted unit tests in that crate · `fixture` = recorded-fixture tests.

---

## 0. Orchestration model

```
                ┌──────────── Orchestrator (main session) ────────────┐
                │ owns PLAN.md, picks next ready tasks, assigns lanes, │
                │ merges reports, resolves cross-lane contracts         │
                └──┬──────┬──────┬──────┬──────┬──────┬────────────────┘
                   A      B      C      D      E      F
                 core  providers  UI   flow/  plugins/ mobile/
                                       server integr. release
```

| Lane | Agent file | Scope |
|---|---|---|
| A Core | `.claude/agents/hk-core.md` | hk-core, hk-session, hk-agent, hk-tools, hk-sandbox, hk-store |
| B Providers | `.claude/agents/hk-providers.md` | hk-llm adapters, catalog, router, local LLM, hk-mcp, hk-acp |
| C UI | `.claude/agents/hk-ui.md` | apps/flutter, hk-ffi, hk-cli TUI |
| D Flow/Server | `.claude/agents/hk-flow.md` | hk-flow, hk-server, hk-daemon |
| E Plugins/Integrations | `.claude/agents/hk-integrations.md` | hk-plugin, wit/, hk-integrations |
| F Mobile/Release | `.claude/agents/hk-release.md` | Android build, CI, packaging, docs site |

Rules:
- Run at most 3 workers in parallel. Prefer tasks whose deps are `done`.
- **Contracts first**: tasks marked 📜 define a trait/schema other lanes consume. They merge before dependents start.
- Worker prompt template:
  ```
  Task <ID>: <title>. DoD: <dod>. Allowed paths: <paths>. Test: <budget>.
  Follow CLAUDE.md. Caveman report ≤10 lines.
  ```
- After each task: orchestrator updates status, checks next ready set. No re-review of done tasks unless a dependent breaks.

---

## Phase 0 — Bootstrap  (milestone **M0: repo compiles**)

| ID | Lane | Task | Deps | Size | Test | DoD | Status |
|---|---|---|---|---|---|---|---|
| P0.1 | F | Cargo workspace skeleton with all crates (empty lib + `todo!()`-free stubs), edition 2024, shared lints, `rust-toolchain.toml` | – | S | none | `cargo check --workspace` green | todo |
| P0.2 | F | CI: `fmt`, `clippy`, `check` on Linux; tests only on changed crates (path filters); Android build job stub | P0.1 | S | none | CI green on main | todo |
| P0.3 | F | `docs/UPSTREAM.md` (pinned SHAs, concept map from DESIGN §2), `THIRD_PARTY_NOTICES.md`, `LICENSE-MIT`, `LICENSE-APACHE` | – | S | none | files exist | wip — UPSTREAM.md done; licenses wait D10 |
| P0.4 | C | Flutter app skeleton (`apps/flutter`), go_router + Riverpod, theme tokens from DESIGN §4.7, empty screens for 7 tabs | – | M | none | `flutter analyze` clean, runs on Linux desktop | todo |
| P0.5 | C | flutter_rust_bridge v2 wiring: `hk-ffi` exposes `core_version()` + a demo stream | P0.1, P0.4 | S | none | desktop app shows version from Rust | todo |

## Phase 1 — Core engine + DeepSeek chat in CLI  (milestone **M1: `hk chat` talks to DeepSeek with tools**)

| ID | Lane | Task | Deps | Size | Test | DoD | Status |
|---|---|---|---|---|---|---|---|
| P1.1 📜 | A | `hk-core`: Host, Ctx, service registry, typed EventBus with interceptors, Effect guards, plugin load/unload | P0.1 | L | unit | load/unload unwinds effects; interceptor reject/rewrite works | todo |
| P1.2 📜 | A | `hk-core` config layers (defaults→profile→user→project→env), TOML, id-patch semantics, profiles `cli/desktop/mobile/daemon` | P1.1 | M | unit | layered config resolves; dump-config prints tree | todo |
| P1.3 📜 | A | `hk-session`: event types, JSONL writer (fsync on turn end), reader, SQLite index, `derive_messages`, fork | P1.1 | L | unit | replay of fixture log yields exact messages; fork at turn works | todo |
| P1.4 📜 | B | `hk-llm`: `Provider` trait, `ChatRequest`, `StreamEvent`, `ModelInfo`, cancellation, usage/cost types | P0.1 | M | none | trait compiles, docs on each type | todo |
| P1.5 | B | DeepSeek adapter: chat + reasoner, SSE parsing, `reasoning_content`, tool-call deltas, cache-hit usage, errors/retry | P1.4 | M | fixture | fixtures for text, reasoning, tool calls, error pass | todo |
| P1.6 | B | OpenAI-compatible adapter (base_url + headers config) | P1.4 | M | fixture | works vs fixture; reused by later providers | todo |
| P1.7 📜 | A | `hk-tools`: registry, JSON-Schema arg validation, pipeline events `tools/pre-execute|execute|post-execute`, output truncation/spill | P1.1 | M | unit | guard reject + spill paths tested | todo |
| P1.8 | A | Builtin tools: read, write, edit, glob, grep, bash (portable-pty), web_fetch, todo | P1.7 | L | unit (edit only) | each tool callable via registry | todo |
| P1.9 | A | `hk-agent` loop: turn/step per DESIGN §3.5, inbox, inject, cancellation, missing tool results on failure, max steps | P1.3, P1.4, P1.7 | L | unit | state machine tests w/ fake provider | todo |
| P1.10 | A | Modes `build` / `plan` + approval events (`approval/request|response`) | P1.9 | M | unit | plan mode blocks write, asks for bash | todo |
| P1.11 | C | `hk-cli`: `hk chat` (line mode) + `hk sessions ls/show`, key from env/keyring | P1.5, P1.10 | M | none | manual: multi-step tool turn with DeepSeek | todo |
| P1.12 | A | `hk-store`: SQLite migrations, secrets API (keyring desktop), settings table | P1.1 | M | unit (migrations) | keys stored/retrieved, never logged | todo |

## Phase 2 — Providers breadth, local AI, MCP, sandbox  (milestone **M2: any model, local fallback, MCP tools**)

| ID | Lane | Task | Deps | Size | Test | DoD | Status |
|---|---|---|---|---|---|---|---|
| P2.1 | B | models.dev catalog fetch + cache + capability flags; custom endpoints | P1.4, P1.12 | M | fixture | `hk models ls` shows catalog + local | todo |
| P2.2 | B | Ollama native adapter (list, pull progress, chat, tools) | P1.4 | M | fixture | local qwen/llama chat with tools | todo |
| P2.3 | B | Anthropic adapter (thinking, caching) | P1.4 | M | fixture | fixtures pass | todo |
| P2.4 | B | Gemini adapter | P1.4 | M | fixture | fixtures pass | todo |
| P2.5 | B | Router: per-agent model, fallback chain, retry/backoff, token meter + cost projection | P2.1 | M | unit | fallback on 5xx/timeout tested | todo |
| P2.6 | B | `hk-mcp` via rmcp: stdio + streamable HTTP, tools into registry, resources/prompts listing | P1.7 | M | none | filesystem MCP server usable in chat | todo |
| P2.7 | A | `hk-sandbox`: policy model, landlock backend, seatbelt backend, none+allowlist; bash wraps argv | P1.8 | L | unit (policy) | write outside root denied on Linux | todo |
| P2.8 | A | Subagents (`task` tool), parallel limit, linked child sessions | P1.9 | M | unit | parent receives child result | todo |
| P2.9 | A | Compaction + guards (repeat-tool, timeouts, cost cap) | P1.9 | M | unit | compaction keeps log replayable | todo |
| P2.10 | C | `hk` TUI (ratatui): sessions pane, stream view, tool cards, approval prompt | P1.11 | L | none | usable full TUI | todo |
| P2.11 | B | `hk-acp`: ACP client; run `dsh --profile acp` / `opencode acp` as external engine (desktop) | P1.4 | M | none | external engine chat via `hk chat --engine dsh` | todo |

## Phase 3 — Desktop app: Chat  (milestone **M3: desktop Flutter chat at TUI parity**)

| ID | Lane | Task | Deps | Size | Test | DoD | Status |
|---|---|---|---|---|---|---|---|
| P3.1 📜 | C | `hk-ffi` API v1: core init/shutdown, chat commands, session event stream, models, settings; `CoreHandle` trait (Local impl) | P1.11 | M | none | Dart bindings generated, no panics cross FFI | todo |
| P3.2 | C | Chat screen: sessions list, conversation stream, reasoning collapse, tool cards, diff viewer | P3.1 | L | none | DESIGN §4.3 layout works | todo |
| P3.3 | C | Composer: `@` files/MCP, `/` commands, mode + model chips, image paste | P3.2 | M | none | – | todo |
| P3.4 | C | Inspector: context, todo/plan, cost, subagent tree, files changed | P3.2 | M | none | – | todo |
| P3.5 | C | Approvals inbox + inline cards | P3.2, P1.10 | S | none | approve/deny round-trip | todo |
| P3.6 | C | Models screen: providers, keys, local models (Ollama pull), routing | P2.1, P2.5 | M | none | – | todo |
| P3.7 | C | Command palette, keyboard shortcuts, light/dark | P3.2 | S | none | – | todo |

## Phase 4 — Flow engine, server, instances  (milestone **M4: webhook → agent → action flow runs on hokkabazd**)

| ID | Lane | Task | Deps | Size | Test | DoD | Status |
|---|---|---|---|---|---|---|---|
| P4.1 📜 | D | `hk-flow` model: workflow JSON schema, node trait, items model, node registry, versioning | P1.1, P1.12 | M | unit (schema) | schema + registry documented | todo |
| P4.2 | D | Executor: DAG scheduling, per-item/batch, retries, continue-on-fail, timeouts, execution persistence | P4.1 | L | unit | branch/merge/retry cases tested | todo |
| P4.3 | D | Expressions (`minijinja`) + Code node (`rhai`) + control nodes (If, Switch, Merge, Batch, Loop, Wait, Sub-flow, Error trigger) | P4.2 | L | unit (expr, wait) | – | todo |
| P4.4 | D | Triggers: manual, cron/interval, app event, file watch; durable waits survive restart | P4.2 | M | unit (cron) | – | todo |
| P4.5 | D | AI nodes: Agent, LLM Prompt, Classifier, Extract (JSON schema), Embed | P4.2, P1.9 | M | none | agent node runs with tools | todo |
| P4.6 📜 | D | `hk-server` (axum): webhook routes + HMAC presets, REST v1, WS event/command mux, token scopes, rate limit | P4.2 | L | unit (auth) | OpenAPI doc generated | todo |
| P4.7 | D | `hokkabazd`: daemon profile, systemd unit, Dockerfile, pairing QR endpoint | P4.6 | M | none | docker run → webhook triggers flow | todo |
| P4.8 | C | `RemoteCore` impl of `CoreHandle` (WS/REST) + Instances screen + pairing | P4.6, P3.1 | M | none | desktop controls remote daemon | todo |
| P4.9 | C | Flows screen: list, canvas (pan/zoom, ports, bezier edges), node picker, params form from JSON Schema | P4.1, P3.1 | L | unit (canvas model) | build & save flow | todo |
| P4.10 | C | Test-run mode, pinned data, execution overlay; Runs screen | P4.9, P4.2 | M | none | – | todo |
| P4.11 | A | `flow_run` + `flow_author` agent tools | P4.2, P1.8 | S | none | agent drafts flow, user opens in canvas | todo |

## Phase 5 — Plugins + integrations  (milestone **M5: plugin SDK + T1 integrations**)

| ID | Lane | Task | Deps | Size | Test | DoD | Status |
|---|---|---|---|---|---|---|---|
| P5.1 📜 | E | WIT world `hokkabaz-plugin`: tools, nodes, triggers, hooks, settings; capability manifest `plugin.toml` | P1.7, P4.1 | M | none | WIT reviewed by A + D leads | todo |
| P5.2 | E | `hk-plugin` host on wasmtime: load, capability-scoped imports (http, kv, secrets, log, fs, emit), unload | P5.1 | L | unit (caps) | sample plugin adds tool + node | todo |
| P5.3 | E | Rust plugin SDK crate + template + `hk plugin new/build/install` | P5.2 | M | none | – | todo |
| P5.4 | E | Integration framework: credential schemas, node/trigger helpers, polling trigger base | P4.2 | M | none | – | todo |
| P5.5 | E | T1 batch A: HTTP Request, GitHub, Gitea/Forgejo, Git, n8n | P5.4 | L | none | – | todo |
| P5.6 | E | T1 batch B: Telegram, Matrix, ntfy, Gotify, Email IMAP/SMTP, RSS | P5.4 | L | none | – | todo |
| P5.7 | E | T1 batch C: SQLite, PostgreSQL, Redis, S3/MinIO, SearXNG (+ `web_search` tool), Ollama node | P5.4 | L | none | – | todo |
| P5.8 | C | Plugins screen: installed, MCP servers (trust prompt), integrations, credentials editor | P5.2, P5.4 | M | none | – | todo |
| P5.9 | D | n8n JSON importer (best-effort, common nodes) | P4.3, P5.5 | M | unit (fixture) | sample n8n flows import | todo |

## Phase 6 — Android APK  (milestone **M6: signed APK on device**)

| ID | Lane | Task | Deps | Size | Test | DoD | Status |
|---|---|---|---|---|---|---|---|
| P6.1 | F | `cargo-ndk` build for aarch64 + x86_64; cfg-gate desktop-only deps; mobile profile | P3.1 | M | none | `.so` builds in CI | todo |
| P6.2 | F | Android Keystore secrets bridge; app-private data dir; SAF file picks | P6.1, P1.12 | M | none | – | todo |
| P6.3 | C | Mobile layouts: bottom nav, chat full-screen, inspector sheet, approvals via notifications | P3.2 | L | none | – | todo |
| P6.4 | F | Foreground service for long runs / optional local webhook server; battery-aware | P6.1, P4.6 | M | none | – | todo |
| P6.5 | C | Remote-first UX: pair via QR camera scan, instance switcher | P4.8, P6.3 | S | none | phone controls daemon | todo |
| P6.6 | F | Release signing, F-Droid metadata, GitHub Releases APK | P6.1 | S | none | signed APK published | todo |

## Phase 7 — Hardening & extras  (milestone **M7: v1.0**)

| ID | Lane | Task | Deps | Size | Test | DoD | Status |
|---|---|---|---|---|---|---|---|
| P7.1 | B | On-device LLM (`local-llm`, llama-cpp-2, GGUF manager) desktop + Android | P2.5, P6.1 | L | none | offline chat on phone | todo |
| P7.2 | E | T2 integrations (GitLab, Home Assistant, Nextcloud, Discord, Docker, Qdrant, …) | P5.4 | L | none | – | todo |
| P7.3 | E | JS/Python plugin SDK docs (jco, componentize-py) | P5.3 | S | none | – | todo |
| P7.4 | A | Security pass: redaction audit, injection tagging, webhook fuzz (one-off) | P4.6 | M | unit | – | todo |
| P7.5 | F | Desktop packaging: AppImage/deb, dmg, msi; auto-update channel | P3.7 | M | none | – | todo |
| P7.6 | F | Docs: user guide, plugin guide, deploy guides (Docker, systemd, Railway, Pi) | M5 | M | none | – | todo |

---

## Critical path

`P0.1 → P1.1 → P1.3/P1.7 → P1.9 → P1.11 (M1) → P3.1 → P3.2 (M3) → P4.1 → P4.2 → P4.6 → P4.7 (M4) → P6.1 → P6.3 (M6)`

Parallel early: B lane (P1.4–P1.6, P2.x adapters) and C lane (P0.4, P0.5) run alongside A lane from day one.

## Ready set at start

`P0.1`, `P0.3`, `P0.4` → then `P0.2`, `P0.5`, `P1.1`, `P1.4`.

## Risks

| Risk | Mitigation |
|---|---|
| Upstream dsh is "developer preview", changes fast | Port concepts, not code; pin SHAs; ACP bridge gives live access to latest dsh |
| wasmtime JIT restrictions on some Android devices | `wasmi` interpreter fallback feature |
| Flutter canvas performance on large flows | Viewport culling, cached node layers; cap visible nodes |
| Android background limits kill local triggers | Phone = remote controller by default; foreground service opt-in |
| Provider API drift | Fixture tests per adapter; catalog-driven capabilities |
| Token spend during development | CLAUDE.md lean test policy, caveman comms, ≤3 parallel workers |
