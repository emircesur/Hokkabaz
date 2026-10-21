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
| P0.1 | F | Cargo workspace skeleton with all crates (empty lib + `todo!()`-free stubs), edition 2024, shared lints, `rust-toolchain.toml` | – | S | none | `cargo check --workspace` green | done — 14 crates + 2 apps, strict lints, clippy -D warnings clean |
| P0.2 | F | CI: `fmt`, `clippy`, `check` on Linux; tests only on changed crates (path filters); Android build job stub | P0.1 | S | none | CI green on main | wip (yaml done; green-on-main unverified) |
| P0.3 | F | `docs/UPSTREAM.md` (pinned SHAs, concept map), `THIRD_PARTY_NOTICES.md`; license files deferred (D10) | – | S | none | files exist | done — UPSTREAM.md + THIRD_PARTY_NOTICES.md; MIT texts to add before release; license deferred |
| P0.4 | C | Flutter app skeleton (`apps/flutter`), go_router + Riverpod, theme tokens from DESIGN §4.7, empty screens for 7 tabs | – | M | none | `flutter analyze` clean, runs on Linux desktop | todo |
| P0.5 | C | flutter_rust_bridge v2 wiring: `hk-ffi` exposes `core_version()` + a demo stream | P0.1, P0.4 | S | none | desktop app shows version from Rust | todo |

## Phase 1 — Core engine + DeepSeek chat in CLI  (milestone **M1: `hk chat` talks to DeepSeek with tools**)

| ID | Lane | Task | Deps | Size | Test | DoD | Status |
|---|---|---|---|---|---|---|---|
| P1.1 📜 | A | `hk-core`: Host, Ctx, service registry, typed EventBus with interceptors, Effect guards, plugin load/unload | P0.1 | L | unit | load/unload unwinds effects; interceptor reject/rewrite works | done |
| P1.2 📜 | A | `hk-core` config layers (defaults→profile→user→project→env), TOML, id-patch semantics, profiles `cli/desktop/mobile/daemon` | P1.1 | M | unit | layered config resolves; dump-config prints tree | todo |
| P1.3 📜 | A | `hk-session`: event types, JSONL writer (fsync on turn end), reader, SQLite index, `derive_messages`, fork | P1.1 | L | unit | replay of fixture log yields exact messages; fork at turn works | done — check+clippy clean, 11 tests pass; deps: hk-llm(path), rusqlite 0.32 bundled (matches sqlx 0.8 libsqlite3-sys), uuid v7; added `session/start`+`session/title` events |
| P1.4 📜 | B | `hk-llm`: `Provider` trait, `ChatRequest`, `StreamEvent`, `ModelInfo`, cancellation, usage/cost types | P0.1 | M | none | trait compiles, docs on each type | done — check+clippy clean; deps: async-trait, futures, serde, serde_json, thiserror, tokio-util |
| P1.5 | B | DeepSeek adapter: chat + reasoner, SSE parsing, `reasoning_content`, tool-call deltas, cache-hit usage, errors/retry | P1.4 | M | fixture | fixtures for text, reasoning, tool calls, error pass | done | `hk_llm::deepseek::DeepSeek`; 16 tests pass, clippy clean. Retry connect-phase only. FIM + dsh headers not done. |
| P1.6 | B | OpenAI-compatible adapter (base_url + headers config) | P1.4 | M | fixture | works vs fixture; reused by later providers | done |
| P1.7 📜 | A | `hk-tools`: registry, JSON-Schema arg validation, pipeline events `tools/pre-execute|execute|post-execute`, output truncation/spill | P1.1 | M | unit | guard reject + spill paths tested | done |
| P1.8 | A | Builtin tools: read, write, edit, glob, grep, bash (portable-pty), web_fetch, todo | P1.7 | L | unit (edit only) | each tool callable via registry | done |
| P1.9 | A | `hk-agent` loop: turn/step per DESIGN §3.5, inbox, inject, cancellation, missing tool results on failure, max steps | P1.3, P1.4, P1.7 | L | unit | state machine tests w/ fake provider | done |
| P1.10 | A | Modes `build` / `plan` + approval events (`approval/request|response`) | P1.9 | M | unit | plan mode blocks write, asks for bash | done — Ask call blocks until respond(); CLI must answer |
| P1.11 | C | `hk-cli`: `hk chat` (line mode) + `hk sessions ls/show`, key from env/keyring | P1.5, P1.10 | M | none | manual: multi-step tool turn with DeepSeek | done — mock-SSE tested only; needs manual real-DeepSeek run (M1 gate) |
| P1.12 | A | `hk-store`: SQLite migrations, secrets API (keyring desktop), settings table | P1.1 | M | unit (migrations) | keys stored/retrieved, never logged | done |

## Phase 2 — Providers breadth, local AI, MCP, sandbox  (milestone **M2: any model, local fallback, MCP tools**)

| ID | Lane | Task | Deps | Size | Test | DoD | Status |
|---|---|---|---|---|---|---|---|
| P2.1 | B | models.dev catalog fetch + cache + capability flags; custom endpoints | P1.4, P1.12 | M | fixture | `hk models ls` shows catalog + local | done — CLI `hk models ls` pending (lane C) |
| P2.2 | B | Ollama native adapter (list, pull progress, chat, tools) | P1.4 | M | fixture | local qwen/llama chat with tools | todo |
| P2.3 | B | Anthropic adapter (thinking, caching) | P1.4 | M | fixture | fixtures pass | todo |
| P2.4 | B | Gemini adapter | P1.4 | M | fixture | fixtures pass | todo |
| P2.5 | B | Router: per-agent model, fallback chain, retry/backoff, token meter + cost projection | P2.1 | M | unit | fallback on 5xx/timeout tested | done — no route-answered signal in StreamEvent (log only via tracing) |
| P2.6 | B | `hk-mcp` via rmcp: stdio + streamable HTTP, tools into registry, resources/prompts listing | P1.7 | M | none | filesystem MCP server usable in chat | done — tool names `mcp.<srv>.<tool>`: dots invalid for OpenAI-style APIs, map before send; rmcp pulls reqwest 0.13 (check android) |
| P2.7 | A | `hk-sandbox`: policy model, landlock backend, seatbelt backend, none+allowlist; bash wraps argv | P1.8 | L | unit (policy) | write outside root denied on Linux | done — hk-tools bash wiring pending; hosts call maybe_run_helper() |
| P2.8 | A | Subagents (`task` tool), parallel limit, linked child sessions | P1.9 | M | unit | parent receives child result | done — child approvals deny by default, 1 level deep |
| P2.9 | A | Compaction + guards (repeat-tool, timeouts, cost cap) | P1.9 | M | unit | compaction keeps log replayable | done |
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

## Phase 4 — n8n-compatible flow engine, server, instances  (milestone **M4: n8n webhook → AI agent → HTTP flow imports and runs on hokkabazd**)

| ID | Lane | Task | Deps | Size | Test | DoD | Status |
|---|---|---|---|---|---|---|---|
| P4.1 📜 | D | `hk-flow` model = n8n workflow JSON (lossless serde incl. unknown fields), node trait keyed by `(type, typeVersion)`, n8n items `{json, binary, pairedItem}`, node description schema, versions | P1.1, P1.12 | M | unit (round-trip) | n8n export → import → export equivalent (normalized) | done — FlowNode/NodeContext minimal, P4.2 extends |
| P4.1b | D | n8n compat corpus harness: fixture format (workflow + payload → expected runData), runner, 20 seed cases | P4.1 | M | fixture | harness runs, per-node diff | done — 21 cases all pending_executor; expectations from docs memory, re-verify when P4.2 runs them |
| P4.2 | D | Executor: n8n v1 execution order, multi in/out, `executeOnce`, `alwaysOutputData`, `retryOnFail`, `onError`, disabled nodes, pinData, staticData; runData persistence | P4.1 | L | fixture | seed corpus passes | todo |
| P4.3 | D | Expression engine: `rquickjs` + n8n runtime shim (`$json`, `$input`, `$('Name')`, `$node`, `$items`, `$workflow`, `$execution`, `$env`, `$vars`, `$now`/Luxon DateTime, `$jmespath`, extension methods); sandbox limits | P4.2 | L | unit (expr corpus) | n8n doc expression examples evaluate identically | todo |
| P4.3b | D | Code node (JS, run-once + per-item) on same engine | P4.3 | M | fixture | – | todo |
| P4.3c | D | Core n8n nodes: Set/Edit Fields, If, Switch, Filter, Merge (all modes), Loop Over Items, Wait (durable), Execute Workflow (+Trigger), Error Trigger, NoOp, Respond to Webhook, HTTP Request, Date & Time, Aggregate/Split Out, Sort, Limit, Remove Duplicates | P4.3 | L | fixture | corpus cases per node pass | todo |
| P4.4 | D | Triggers (n8n types): Manual, Schedule, Webhook, Form, Chat, Execute Workflow Trigger + native app event, file watch; durable waits survive restart | P4.2 | M | unit (cron) | – | todo |
| P4.5 | D | n8n LangChain nodes on hk-llm/hk-agent: AI Agent, chat model sub-nodes (DeepSeek, OpenAI, Anthropic, Gemini, Ollama, OpenRouter), memory, tool sub-nodes, output parsers, Basic LLM Chain, Text Classifier, Information Extractor; + native Hokkabaz Agent node | P4.3, P1.9 | L | fixture | n8n AI agent template imports and runs on DeepSeek | todo |
| P4.6a 📜 | D | `hk-server` agent API (first slice of P4.6): axum REST `/api/v1` sessions list\/show\/create, submit turn, inject, cancel, approval respond, models list; WS `/api/v1/ws` session event stream + commands; bearer token auth + scopes | P1.11 | M | unit (auth, routing) | curl creates session, streams events over WS, approves a tool call | todo |
| P4.6 📜 | D | `hk-server` (axum): n8n-shaped webhooks (`/webhook`, `/webhook-test`, response modes) + HMAC presets, REST v1 mirroring n8n public API (workflows, executions, credentials), WS event/command mux, token scopes, rate limit | P4.2 | L | unit (auth) | OpenAPI doc generated | todo |
| P4.7 | D | `hokkabazd`: daemon profile, systemd unit, Dockerfile, pairing QR endpoint | P4.6 | M | none | docker run → webhook triggers flow | todo |
| P4.8 | C | `RemoteCore` impl of `CoreHandle` (WS/REST) + Instances screen + pairing | P4.6, P3.1 | M | none | desktop controls remote daemon | todo |
| P4.9 | C | Flows screen: list, canvas (pan/zoom, ports, bezier edges), node picker, params form from node description, n8n paste/copy/import, ⚠ placeholder for unsupported nodes | P4.1, P3.1 | L | unit (canvas model) | n8n workflow pasted, edited, exported | todo |
| P4.10 | C | Test-run mode, pinned data, execution overlay; Runs screen | P4.9, P4.2 | M | none | – | todo |
| P4.11 | A | `flow_run` + `flow_author` agent tools | P4.2, P1.8 | S | none | agent drafts flow, user opens in canvas | todo |

## Phase 5 — Plugins + integrations as n8n nodes  (milestone **M5: plugin SDK + T1 integrations, top n8n template coverage**)

| ID | Lane | Task | Deps | Size | Test | DoD | Status |
|---|---|---|---|---|---|---|---|
| P5.1 📜 | E | WIT world `hokkabaz-plugin`: tools, nodes, triggers, hooks, settings; capability manifest `plugin.toml` | P1.7, P4.1 | M | none | WIT reviewed by A + D leads | todo |
| P5.2 | E | `hk-plugin` host on wasmtime: load, capability-scoped imports (http, kv, secrets, log, fs, emit), unload | P5.1 | L | unit (caps) | sample plugin adds tool + node | todo |
| P5.3 | E | Rust plugin SDK crate + template + `hk plugin new/build/install` | P5.2 | M | none | – | todo |
| P5.3b | E | GitHub plugin registry: `hokkabaz-plugins` repo layout (index.json, manifests), CI manifest/hash validation, Releases artifacts; in-app browse/install/update | P5.3 | M | none | plugin installed from GitHub registry | todo |
| P5.4 | E | Integration framework: n8n credential types, node/trigger helpers matching n8n node descriptions, polling trigger base | P4.3c | M | none | – | todo |
| P5.5 | E | T1 batch A: HTTP Request, GitHub, Gitea/Forgejo, Git, n8n | P5.4 | L | none | – | todo |
| P5.6 | E | T1 batch B: Telegram, Matrix, ntfy, Gotify, Email IMAP/SMTP, RSS | P5.4 | L | none | – | todo |
| P5.7 | E | T1 batch C: SQLite, PostgreSQL, Redis, S3/MinIO, SearXNG (+ `web_search` tool), Ollama node | P5.4 | L | none | – | todo |
| P5.8 | C | Plugins screen: installed, MCP servers (trust prompt), integrations, credentials editor | P5.2, P5.4 | M | none | – | todo |
| P5.9 | D | n8n instance sync: pull/push workflows + credential metadata via n8n API; credential import mapping | P4.6 | M | none | flows pulled from live n8n run locally | todo |
| P5.10 | D | Remote delegation: run unimplemented node types on configured n8n instance | P5.9 | M | none | flow with unsupported node runs end-to-end | todo |
| P5.11 | D | Coverage report: rank node types by usage in n8n public templates, track % supported in `docs/N8N_COVERAGE.md`; drives integration order | P4.1b | S | none | report generated | todo |

Integration order after core + AI nodes: implementing agent decides (D14), using P5.11 report + own judgment. T1/T2 lists in DESIGN §3.11 are suggestions, not commitments.

## Phase 6 — Android APK + on-device LLM  (milestone **M6: signed APK on device, offline chat**)

| ID | Lane | Task | Deps | Size | Test | DoD | Status |
|---|---|---|---|---|---|---|---|
| P6.1 | F | `cargo-ndk` build for aarch64 + x86_64; cfg-gate desktop-only deps; mobile profile | P3.1 | M | none | `.so` builds in CI | todo |
| P6.2 | F | Android Keystore secrets bridge; app-private data dir; SAF file picks | P6.1, P1.12 | M | none | – | todo |
| P6.3 | C | Mobile layouts: bottom nav, chat full-screen, inspector sheet, approvals via notifications | P3.2 | L | none | – | todo |
| P6.4 | F | Foreground service for long runs / optional local webhook server; battery-aware | P6.1, P4.6 | M | none | – | todo |
| P6.5 | C | Remote-first UX: pair via QR camera scan, instance switcher | P4.8, P6.3 | S | none | phone controls daemon | todo |
| P6.7 | B | On-device LLM (`local-llm`, llama-cpp-2): GGUF model manager (HF download, resume, checksum, fit check), desktop + Android aarch64, router offline fallback | P2.5, P6.1 | L | none | offline chat on phone | todo |
| P6.8 | C | Local models UI: browse/download/delete, device-class recommendations, offline indicator | P6.7, P3.6 | M | none | – | todo |
| P6.6 | F | Release signing, F-Droid metadata, GitHub Releases APK | P6.1 | S | none | signed APK published | todo |

## Phase 7 — Hardening & extras  (milestone **M7: v1.0**)

| ID | Lane | Task | Deps | Size | Test | DoD | Status |
|---|---|---|---|---|---|---|---|
| P7.2 | E | T2 integrations as n8n node types, ordered by P5.11 report (GitLab, Home Assistant, Nextcloud, Discord, Docker, Qdrant, …) | P5.4 | L | fixture | ≥ 80% of top-500 template node usages supported | todo |
| P7.2b | D | Code node Python mode | P4.3b | M | fixture | – | todo |
| P7.3 | E | JS/Python plugin SDK docs (jco, componentize-py) | P5.3 | S | none | – | todo |
| P7.4 | A | Security pass: redaction audit, injection tagging, webhook fuzz (one-off) | P4.6 | M | unit | – | todo |
| P7.5 | F | Desktop packaging: AppImage/deb, dmg, msi; auto-update channel | P3.7 | M | none | – | todo |
| P7.6 | F | Docs: user guide, plugin guide, deploy guides (Docker, systemd, Railway, Pi) | M5 | M | none | – | todo |

---

## Critical path

`P0.1 → P1.1 → P1.3/P1.7 → P1.9 → P1.11 (M1) → P3.1 → P3.2 (M3) → P4.1 → P4.2 → P4.3 → P4.3c → P4.6 → P4.7 (M4) → P6.1 → P6.3/P6.7 (M6)`

n8n compat is the largest lane-D workload: P4.1–P4.3 may start right after P1.1 + P1.12, parallel to M1–M3.

Parallel early: B lane (P1.4–P1.6, P2.x adapters) and C lane (P0.4, P0.5) run alongside A lane from day one.

## Ready set at start

`P0.1`, `P0.3`, `P0.4` → then `P0.2`, `P0.5`, `P1.1`, `P1.4`.

## Risks

| Risk | Mitigation |
|---|---|
| Upstream dsh is "developer preview", changes fast | Port concepts, not code; pin SHAs; ACP bridge gives live access to latest dsh |
| wasmtime JIT restrictions on some Android devices | `wasmi` interpreter fallback feature |
| Flutter canvas performance on large flows | Viewport culling, cached node layers; cap visible nodes |
| On-device LLM on low-RAM phones | Device-class model recommendations, fit check before download, small Q4 models, fall back to remote |
| Android background limits kill local triggers | Phone = remote controller by default; foreground service opt-in |
| Provider API drift | Fixture tests per adapter; catalog-driven capabilities |
| n8n semantics large, partly undocumented (execution order, expression extensions) | Fixture corpus recorded from real n8n runs; coverage report; remote delegation fallback |
| n8n Sustainable Use License | Clean-room from docs/JSON formats; no n8n source; legal check before shipping node descriptions |
| QuickJS perf on heavy expressions | Cache compiled expressions; reuse context per execution |
| Token spend during development | CLAUDE.md lean test policy, caveman comms, ≤3 parallel workers |
