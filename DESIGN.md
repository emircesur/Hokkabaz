# Hokkabaz — Design

> *Hokkabaz* (Turkish): juggler, conjurer. One app that keeps many models, agents, tools and automations in the air at once.

Status: draft v0.1 · 2026-10-21

---

## 1. Product

Hokkabaz is a native, open-source AI agent harness and automation workbench.

- **Any model**: hosted APIs (DeepSeek first, then OpenAI-compatible, Anthropic, Gemini, OpenRouter, Mistral, Groq, xAI, …) and local runtimes (Ollama, llama.cpp server, LM Studio, vLLM, LocalAI; on-device llama.cpp later).
- **Agentic chat**: coding/ops agent with tools, subagents, plans, approvals, MCP.
- **Automations**: n8n-style node graphs with triggers (webhook, cron, event, manual), executions, retries, and an *agent node*.
- **Instances**: run headless `hokkabazd` on a server/VPS/home box, receive webhooks, manage several instances from one app (desktop or phone).
- **Plugins**: sandboxed WASM plugins + MCP servers + built-in integrations for open-source services.
- **Native, not web**: Rust core; Flutter UI on desktop (Linux/macOS/Windows) and Android APK. Plus `hk` CLI/TUI.

### Non-goals (v1)

- Web UI / hosted SaaS.
- iOS (architecture allows it; not planned).
- Running TypeScript upstream code inside the app on Android.
- Full n8n workflow compatibility (import is best-effort).
- Multi-user RBAC on instances beyond token scopes.

### Personas

| Persona | Needs |
|---|---|
| Solo dev | Coding agent on any model, cheap DeepSeek default, local fallback offline |
| Self-hoster | Automations triggered by webhooks from Gitea, Home Assistant, etc., running on a home server |
| Mobile operator | Phone app: chat with agents, approve tool calls, watch executions, trigger flows on remote instance |

---

## 2. Upstream strategy

Both upstreams are TypeScript (dsh: Node + pnpm + Cordis; opencode: Bun). Hokkabaz is Rust. We **port architecture and semantics**, not code.

| Concept | Source | Hokkabaz home |
|---|---|---|
| Everything-is-a-plugin, services + typed events + reversible effects (Cordis) | dsh `docs/architecture.md`, `cordis-primer.md` | `hk-core` |
| Append-only session event log, `deriveMessages()`, projections, "model-visible means logged" | dsh `packages/session`, `core/session` | `hk-session` |
| Turn/step loop, `agent/pre-step`, inbox, inject, cancellation, missing tool results on failure | dsh `core/agent-loop`, `docs/agent-lifecycle.md` | `hk-agent` |
| Tool pipeline `pre-execute → execute → post-execute`, scoped registry | dsh `core/tools`, `docs/tool-execution-pipeline.md` | `hk-tools` |
| Capability seams (fs, subprocess, shell, sandbox share one execution world) | dsh `docs/capability-seams.md` | `hk-tools`, `hk-sandbox` |
| DeepSeek adapter: reasoning content, prefix cache, wire extensions, retry, token meter | dsh `packages/llm/*` | `hk-llm::deepseek` |
| Guards: repeat-tool reminder, timeout policy | dsh `packages/guard` | `hk-agent::guards` |
| Compaction, spill (large tool outputs to disk) | dsh `packages/compaction`, `spill` | `hk-agent::compaction` |
| Schedule, webhook runtime, workflow (scripted subagent fan-out) | dsh `packages/schedule`, `experimental/webhook*`, `workflow` | `hk-flow`, `hk-server` |
| Goals, todo, plan | dsh `packages/goal`, `todo`, `plan` | `hk-agent` tools |
| Provider catalog from models.dev, provider config schema | opencode `packages/schema/src/models-dev.ts`, `config/provider.ts` | `hk-llm::catalog` |
| Agent modes: `build` (full), `plan` (read-only, ask before bash), `general` subagent | opencode README / agents | `hk-agent::modes` |
| Plugin hooks surface (event hooks, tool add) | opencode `packages/plugin` | `hk-plugin` WIT design input |
| Client/server split, event manifest | opencode `packages/server`, `schema/event-manifest.ts` | `hk-server` API shape |
| ACP (Agent Client Protocol) | dsh `packages/acp`, opencode | `hk-acp` (drive dsh/opencode as external engines on desktop) |

Pinned SHAs: dsh `687f282e` (2026-10-17), opencode `5fa426f` (2026-10-21). Details in `docs/UPSTREAM.md`.

**Escape hatch**: on desktop, `hk-acp` can run real `dsh --profile acp` or `opencode acp` as an *external engine* behind the same chat UI. This gives day-one parity while the native core catches up, and stays useful later for A/B comparing.

---

## 3. Architecture

```
┌───────────────────────── Flutter UI (Dart) ─────────────────────────┐
│ Chat · Flows canvas · Executions · Instances · Plugins · Models     │
└───────────────▲─────────────────────────────────────────────────────┘
                │ flutter_rust_bridge v2 (async fns + Streams)
┌───────────────┴──────────── hk-ffi ─────────────────────────────────┐
│ thin facade: commands in, event streams out; no logic               │
└───────────────▲─────────────────────────────────────────────────────┘
                │ in-process (local)  or  WS/REST client (remote instance)
┌───────────────┴──────────── Hokkabaz Core (Rust, tokio) ────────────┐
│ hk-core: Host · Services · EventBus · Effects · Config layers       │
│ ├ hk-agent ── hk-llm ── providers (deepseek, openai-compat, …)      │
│ ├ hk-tools ── hk-sandbox ── hk-mcp ── hk-plugin (wasmtime)          │
│ ├ hk-flow  ── hk-integrations                                       │
│ ├ hk-session (event log) ── hk-store (SQLite, secrets)              │
│ └ hk-server (axum) [desktop/daemon; optional on Android]            │
└─────────────────────────────────────────────────────────────────────┘
        ▲                         ▲
   hk-cli (TUI)            hk-daemon `hokkabazd`
```

### 3.1 Local vs remote

The UI talks to a `CoreHandle` trait with two impls:
- `LocalCore`: in-process via FFI (desktop default, Android light mode).
- `RemoteCore`: WS + REST to a `hokkabazd` instance (token auth). Same commands and events, serialized.

So the same screens control a local core or any remote instance. Instance switcher in the top bar.

### 3.2 hk-core (Cordis-like host)

- `Host` holds a tree of `Plugin`s. Each plugin gets a `Ctx` scoped to it.
- `ctx.provide::<S>(impl)` registers a service; `ctx.use::<S>()` resolves. Services keyed by `TypeId` + optional name.
- `ctx.on::<E>(handler)` subscribes to typed events; handlers may be *interceptors* (return `Continue | Rewrite(E) | Reject(reason)`) for `agent/*`, `tools/*`, `fs/*` domains.
- Every registration returns an `Effect` guard; dropping/unloading a plugin unwinds its effects in reverse order. Hot unload = drop scope.
- Config = ordered layers: built-in defaults → profile → user `hokkabaz.toml` → project `.hokkabaz/config.toml` → CLI/env overrides. Each row has an id; layers patch by id (mirrors dsh profiles/bundles/patches).
- Profiles: `desktop`, `mobile`, `daemon`, `cli`. Each lists the plugins it mounts.

### 3.3 hk-session

- Event log per session: `~/.hokkabaz/sessions/<id>/events.jsonl` (append-only, fsync on turn end) + SQLite index (title, model, cwd, counts, timestamps).
- Durable events: `turn/start|end`, `step/start|end`, `system/message`, `user/message`, `assistant/message`, `assistant/attempt`, `tool/call`, `tool/result`, `request/header`, `compaction/summary`, `approval/*`.
- Live (non-durable): `agent/assistant-stream` chunks.
- `derive_messages(log) -> Vec<Message>`: model history is always a projection of the log. Fork = new session seeded with prefix up to a turn boundary.
- Projections: incremental folds (`stateOf`) for UI (todo list, cost meter, file changes).

### 3.4 hk-llm

```rust
trait Provider: Send + Sync {
    fn id(&self) -> &str;
    async fn models(&self) -> Result<Vec<ModelInfo>>;
    async fn stream(&self, req: ChatRequest, cancel: CancellationToken)
        -> Result<BoxStream<'static, Result<StreamEvent>>>;
}
enum StreamEvent { TextDelta, ReasoningDelta, ToolCallDelta, Usage, Finish(FinishReason), ... }
```

Adapters (order of delivery):
1. `deepseek` — chat + reasoner, `reasoning_content`, tool calls, prefix-cache usage fields, FIM (for editor features later), dsh wire-extension headers optional (off by default; our own `user-agent`).
2. `openai_compat` — covers OpenAI, OpenRouter, Groq, Mistral, xAI, Together, vLLM, LM Studio, LocalAI, llama.cpp server.
3. `ollama` — native API (model pull/list, keep-alive).
4. `anthropic` — messages API, thinking, prompt caching.
5. `gemini` — generateContent streaming.
6. `local` (feature `local-llm`) — on-device llama.cpp via `llama-cpp-2`, GGUF models; Android aarch64 + desktop.

Catalog: models.dev JSON cached in SQLite, refreshed daily; user overrides for custom endpoints. Capabilities per model: tools, reasoning, vision, context, cost → used by UI and router.

Router: per-agent model, fallback chain (e.g. `deepseek-chat → openrouter/… → ollama/qwen`), retry with backoff (port dsh `llm-retry`), token meter + cost per session.

### 3.5 hk-agent

- Loop exactly per dsh turn flow: claim inbox → `agent/pre-step` interceptors → assemble prompt sections + tool schemas → `agent/request` → stream → append durable events → execute tool calls through pipeline → step end → continue if tools owe a request.
- Modes (opencode): `build` (all tools), `plan` (read-only fs, bash needs approval), custom modes in config (prompt + tool allowlist + model).
- Subagents: `task` tool spawns child agent with its own session linked to parent; results returned as tool result. Parallel fan-out limit configurable.
- Guards: repeat-tool reminder, step/turn timeouts, max steps, cost cap per turn.
- Compaction: summarize old turns when context > threshold; spill tool outputs > N KB to files and reference them.
- Approvals: tool calls with `risk >= ask` emit `approval/request`; UI (desktop or phone) answers. Remote approval works through instance WS.

### 3.6 hk-tools + hk-sandbox

Builtin tools: `read`, `write`, `edit` (search/replace + patch), `glob`, `grep` (ripgrep lib), `bash` (PTY via `portable-pty`, desktop), `web_fetch`, `web_search` (SearXNG integration), `todo`, `task`, `flow_run`, `flow_author`.

Pipeline: `tools/pre-execute` (policy, approval, arg validation via JSON Schema) → `execute` → `tools/post-execute` (redaction, spill, truncation) → `tool/result`.

Sandbox backends: `landlock` (Linux), `seatbelt` (macOS), `none` + path allowlist (Windows/Android). Policy: write roots, network on/off, env allowlist. Android: no shell by default; file tools scoped to app storage + SAF picks.

### 3.7 hk-mcp

Client via `rmcp` (official Rust MCP SDK). Transports: stdio (desktop/daemon only), streamable HTTP (all, incl. Android). MCP tools join the registry under `mcp.<server>.<tool>`; resources and prompts exposed in chat `@` picker.

### 3.8 hk-plugin (WASM)

- Runtime: `wasmtime` component model; WIT world in `wit/hokkabaz-plugin.wit`.
- A plugin may export: tools, flow nodes, triggers, event hooks (interceptors on `agent/*`, `tools/*`), settings schema.
- Host imports granted by capability manifest: `http` (allowlist hosts), `kv` (plugin-scoped), `secrets` (named handles only), `log`, `fs` (scoped dir), `emit-event`.
- Package: `plugin.toml` + `.wasm`, signed hash, installed into `~/.hokkabaz/plugins/`. Registry = git repo index (later).
- Android: wasmtime supports aarch64; fallback to `wasmi` interpreter behind feature if JIT blocked.
- Authoring SDKs: Rust (`hokkabaz-plugin` crate), JS/TS via `jco componentize`, Python via `componentize-py`.

### 3.9 hk-flow (automation engine)

n8n-inspired, Rust-native.

- **Workflow** = versioned JSON doc: `nodes[]`, `connections[]`, `settings`, `pinned_data`. Stored in SQLite; export/import as `.hkflow.json`. Best-effort n8n JSON importer for common nodes.
- **Node** = `{ id, type, version, params, credentials_ref, position }`. Types come from registry (builtin, integrations, WASM plugins, MCP tools-as-nodes).
- **Items model**: nodes pass `Vec<Item>` (JSON + binary refs), like n8n. Each node runs per item or per batch.
- **Expressions**: `minijinja` templates in params (`{{ $json.title }}`, `{{ $node["Fetch"].json.id }}`); **Code node** uses `rhai` (sandboxed, Android-safe).
- **Control nodes**: If, Switch, Merge, Split/Batch, Loop, Wait (durable timer), Error Trigger, Sub-workflow.
- **AI nodes**: Agent (runs `hk-agent` with chosen model/mode/tools, returns final + transcript ref), LLM Prompt, Classifier, Extract (JSON schema), Embed.
- **Triggers**: Webhook (via `hk-server`), Cron (`tokio-cron-scheduler`), Interval, Manual, App event (session/agent events), File watch (desktop), Integration triggers (polling or webhook).
- **Executions**: each run persisted (status, per-node input/output, timings, error). Retry per node (count, backoff), continue-on-fail, timeout. Queue with concurrency limit per instance. Durable waits survive restart.
- **Agent ↔ flow**: flows can be exposed as agent tools (`flow_run`); agents can draft flows (`flow_author` writes a draft that the user reviews in canvas).

### 3.10 hk-server + instances

- `axum` server inside `hokkabazd` (and optionally desktop app).
- Routes: `POST /webhook/:path` (+ `/webhook-test/:path`), `GET/POST /api/v1/...` (sessions, flows, executions, plugins, models), `GET /api/v1/events` WS (multiplexed event stream + commands), `/health`.
- Auth: instance tokens with scopes (`read`, `chat`, `flows`, `admin`); webhook paths have own secret/HMAC verification (GitHub/Gitea style signature presets).
- Pairing: desktop/phone pairs with an instance via QR code (URL + one-time token).
- Deploy targets documented: systemd, Docker image, Railway/Fly template, Raspberry Pi (aarch64).
- n8n interop: `n8n` integration node calls n8n webhooks/REST; Hokkabaz webhooks can be targets of n8n.

### 3.11 hk-integrations (built-in OSS connectors)

Each integration = credential schema + nodes (actions/triggers) + optional agent tools. Feature-gated `int-<name>`.

| Tier | Integrations |
|---|---|
| T1 (v1) | HTTP Request, Webhook, Git (gitoxide), GitHub, Gitea/Forgejo, Ollama, SearXNG, ntfy, Gotify, Telegram, Matrix, SQLite, PostgreSQL, Redis, S3/MinIO, RSS, Email (IMAP/SMTP), n8n |
| T2 | GitLab, Home Assistant, Nextcloud (WebDAV), Discord, Mattermost, Docker/Podman API, Qdrant, Meilisearch, Paperless-ngx, Uptime Kuma, Obsidian vault (fs), Immich |
| T3 | Jellyfin, Grafana/Prometheus query, Vaultwarden (read), Mastodon, Kafka/NATS, Elasticsearch/OpenSearch, Jira-alt (Plane), Penpot |

### 3.12 hk-store

- SQLite via `sqlx` (migrations embedded). Tables: sessions index, flows, flow_versions, executions, execution_nodes, credentials (encrypted), plugins, instances, settings, model_catalog.
- Secrets: OS keyring (`keyring` crate) on desktop; Android Keystore via FFI callback; daemon: age-encrypted file with master key from env.
- Data dir: `~/.hokkabaz/` (desktop/daemon), app-private storage on Android.

### 3.13 hk-ffi + Flutter

- `flutter_rust_bridge` v2. API surface grouped: `core::{init, shutdown}`, `chat::*`, `flows::*`, `instances::*`, `plugins::*`, `models::*`, `settings::*`. Streams for events.
- Dart state: Riverpod. Navigation: go_router. Canvas: custom `CustomPainter` + `InteractiveViewer` (no web views).
- Android build: `cargo-ndk` for `aarch64-linux-android` (+ `x86_64` for emulator). Foreground service for long runs / local webhook server; otherwise phone acts as remote controller.

---

## 4. UX design

### 4.1 Principles

1. **Keyboard-first desktop, thumb-first mobile.** Same information architecture, different density.
2. **Transparency.** Every model call, tool call, cost and approval is visible and inspectable.
3. **Calm by default.** Streaming text, not spinners. Collapse tool noise; expand on demand.
4. **One object model.** Sessions, flows, executions, instances are all lists → detail → inspector.

### 4.2 Information architecture

Nav rail (desktop) / bottom bar (mobile, 4 tabs + "More"):

| Screen | Purpose |
|---|---|
| **Chat** | Agentic chat sessions |
| **Flows** | Automation list + canvas editor |
| **Runs** | Flow executions + agent background jobs |
| **Instances** | Local + remote instances, pairing, health |
| **Plugins** | WASM plugins, MCP servers, integrations, credentials |
| **Models** | Providers, API keys, local models, routing/fallbacks |
| **Settings** | Profiles, sandbox policy, appearance, data |

Top bar: instance switcher · model/mode quick picker · command palette (`Ctrl/Cmd+K`) · approvals inbox badge.

### 4.3 Chat screen

Desktop 3-pane:
```
┌ rail ┬ sessions ─────┬ conversation ───────────────────────┬ inspector ─────┐
│ ●    │ search        │ [mode: build ▾] [deepseek-chat ▾]   │ Context        │
│ Chat │ ─ today       │                                      │  files, MCP    │
│ Flows│  fix webhook  │ user: …                              │ Todo / plan    │
│ Runs │  refactor llm │ assistant: … (reasoning ▸ collapsed) │ Cost / tokens  │
│ …    │ ─ yesterday   │  ┌ tool: edit src/x.rs  ✓ diff ▸ ┐   │ Subagents tree │
│      │               │  └ tool: bash cargo check ⏳ ask ┘   │ Files changed  │
│      │               │ [ composer: @file /cmd ⏎ ]          │                │
└──────┴───────────────┴──────────────────────────────────────┴────────────────┘
```
- Tool calls render as compact cards: name, target, status, expand for args/output/diff.
- Reasoning shown collapsed with token count.
- Approval cards inline + global inbox (approve once / always for this session / deny).
- Composer: `@` files/MCP resources, `/` commands, `#` flows, image paste, model/mode chips.
- Session actions: fork at turn, export (md/jsonl), rerun turn on another model (compare).

Mobile: sessions list → conversation full screen; inspector as bottom sheet; approvals as notifications.

### 4.4 Flows screen (automation schema)

```
┌ flows list ┬ canvas ─────────────────────────────────────────┬ node panel ───┐
│ + New      │  (Webhook)──►(If)──►(Agent: triage)──►(Gitea)   │ Agent: triage │
│ triage-bot │              └────►(ntfy)                        │ model ▾ mode ▾│
│ nightly-bk │                                                  │ prompt {{…}}  │
│            │  [▶ Test] [⟲ Pin data] [Activate ●]  zoom ─○──  │ tools [x][x]  │
└────────────┴──────────────────────────────────────────────────┴───────────────┘
```
- Nodes: rounded rectangles, category color stripe, input/output ports, item count badges after test run.
- Edges: bezier, animated dashes while executing; red on error.
- Add node: `Tab` / long-press opens searchable node picker (builtin, integrations, plugins, MCP).
- Node panel: params form generated from node JSON Schema; expression editor with live preview against pinned data.
- Test mode runs from a selected node with pinned input. Execution overlay shows per-node in/out.
- Mobile: canvas view + edit params, pan/zoom; full graph editing desktop-first, mobile supported later.

### 4.5 Runs screen

Table: flow/agent, trigger, status, started, duration, cost. Detail = timeline of nodes or agent steps, inputs/outputs, logs, retry button.

### 4.6 Instances screen

Cards: name, URL, version, status dot, active flows, queue depth. Actions: pair (QR), set default, view logs, open webhook URLs list.

### 4.7 Visual design system

- **Theme**: dark-first, light supported. Material 3 base, custom tokens.
- **Palette** (tokens, not hardcoded):
  - `bg` #0E0F13 / light #FAFAF7
  - `surface` #16181E / #FFFFFF
  - `border` #262A33 / #E4E4DF
  - `text` #E8E8EA / #17181C; `text-muted` #9A9CA5 / #5F616B
  - `accent` (juggler saffron) #F2A33A
  - `accent-2` (conjurer violet) #8B7CF6
  - `ok` #3FB27F · `warn` #E3B341 · `err` #E5534B · `info` #4C9AFF
  - Node category stripes: trigger=saffron, AI=violet, logic=slate, data=teal #2BB3A3, integration=blue.
- **Type**: Inter (UI), JetBrains Mono (code/tool output). Scale 12/13/14/16/20/24.
- **Spacing**: 4-pt grid. Radius 6 (cards), 10 (sheets), 999 (chips).
- **Motion**: 120–180 ms ease-out; streaming caret; no bouncing.
- **Logo**: three orbiting dots (saffron, violet, teal) around a small spark — "juggled" models/agents. Loading indicator reuses it.
- **Density**: desktop compact (32 px rows), mobile comfortable (48 px touch targets).
- **Accessibility**: WCAG AA contrast, full keyboard nav, screen-reader labels on canvas nodes, reduced motion respected.

---

## 5. Security model

- API keys and credentials only via `hk-store::secrets`; redacted from logs, session events, executions.
- Tools default-deny dangerous ops in `plan` mode; `build` mode asks for bash/network/out-of-root writes unless allowed in policy.
- WASM plugins capability-scoped; MCP servers listed with explicit trust prompt on first use.
- Webhooks: random path + optional HMAC/secret header; rate limiting per path.
- Instance API: TLS expected behind reverse proxy; token scopes; pairing tokens single-use, 10 min TTL.
- Prompt-injection hygiene: tool outputs and webhook payloads tagged untrusted in prompt assembly; flows can't escalate tool permissions beyond the flow's declared set.

---

## 6. Decisions (ADR summary)

| # | Decision | Why | Alternatives rejected |
|---|---|---|---|
| D1 | Port dsh/opencode semantics to Rust; no TS runtime in-app | Android APK, single binary, perf | Bundling Node (impossible/heavy on Android) |
| D2 | Flutter UI for desktop **and** Android via flutter_rust_bridge | One UI codebase; native rendering, not web | Separate Rust GUI (egui/Slint) + Flutter mobile = 2 UIs to maintain |
| D3 | `hk` TUI (ratatui) as the pure-Rust app surface | Fast dev loop, servers, SSH | — |
| D4 | dsh-style event-log session as source of truth | Replay, fork, audit, remote sync | Mutable message arrays |
| D5 | WASM component plugins + MCP; no native dylib plugins | Sandboxed, portable to Android | dylibs (unsafe, ABI pain), Lua |
| D6 | `minijinja` expressions + `rhai` code node | Pure Rust, sandboxable, Android-safe | Embedded JS (boa/quickjs) — keep as later option |
| D7 | SQLite + JSONL logs | Local-first, zero ops | Postgres (daemon may add later) |
| D8 | ACP bridge to real dsh/opencode on desktop | Day-one parity, comparison | — |
| D9 | DeepSeek is default provider and first adapter | User priority, cost | — |
| D10 | License MIT OR Apache-2.0 | Rust norm, compatible with MIT upstreams | — (open question for owner) |

## 7. Open questions

1. Desktop GUI: confirm Flutter (D2) vs pure-Rust GUI (Slint/egui) for desktop.
2. License: MIT/Apache dual OK?
3. On-device LLM on Android in v1 or v2?
4. Plugin registry hosting (git index on GitHub vs self-hosted).
5. n8n import scope: which node types matter most.
