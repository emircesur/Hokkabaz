# Hokkabaz agent API v1

Served by `hk-server` (`hokkabazd`). Base path `/api/v1`. JSON, snake_case fields, event names
`domain/action`. Scope of this document: agent API (P4.6a). Webhook and n8n-shaped routes are
not part of v1.

Start: `hokkabazd [--listen 127.0.0.1:7878] [--data-dir DIR]`. Print a token: `hokkabazd token --scope admin|run|read`.

## Auth

`Authorization: Bearer <token>`. Tokens are random, generated on first start, stored via
`hk-store` secrets (OS keyring, else a 0600 file in the data dir) and never logged.
Scopes are ordered: `read` < `run` < `admin`.

| Scope | Allows |
|-------|--------|
| `read` | GET routes, WS event stream, WS `subscribe`/`unsubscribe` |
| `run` | `read` + create session, turns, inject, cancel, approvals, WS commands |
| `admin` | `run` + instance administration (reserved) |

`/health` needs no token. Missing/invalid token: `401` + `WWW-Authenticate: Bearer`. Valid token
with too low a scope: `403`.

Errors: `{"error": {"code": "unauthorized|forbidden|not_found|bad_request|conflict|internal", "message": "..."}}`.

## Routes

| Method | Path | Scope | Body / query | Response |
|--------|------|-------|--------------|----------|
| GET | `/health` | none | | `200 {"status":"ok","version":"x.y.z"}` |
| GET | `/sessions` | read | `?limit=50&offset=0` (limit max 200) | `200 {"sessions":[SessionInfo]}` newest first |
| POST | `/sessions` | run | `{"cwd"?, "model"?, "title"?}` (body optional) | `201 SessionInfo` |
| GET | `/sessions/{id}` | read | `?after_seq=N` | `200 SessionDetail` |
| POST | `/sessions/{id}/turns` | run | `{"prompt", "mode"?: "build"\|"plan", "model"?}` | `202 {"session_id","queued"}` |
| POST | `/sessions/{id}/inject` | run | `{"text"}` | `202` (steers the running turn; idle = like a turn) |
| POST | `/sessions/{id}/cancel` | run | | `202` (no-op when idle) |
| POST | `/approvals/{approval_id}` | run | `{"approved", "reason"?, "session_id"?}` | `200 {"ok":true}`; `404` if nothing pending; `409` if the id is pending in several sessions and `session_id` is missing |
| GET | `/models` | read | | `200 {"models":[ModelInfo]}` from the configured provider/router |
| GET | `/ws` | read | `?access_token=` fallback for header-less clients | WebSocket, see below |

`submit` is asynchronous: it queues the prompt and returns. Follow progress over WS or poll
`GET /sessions/{id}?after_seq=<last seen seq>`. A prompt sent while a turn runs is queued
(`"queued": true`) and runs as the next turn. `mode` / `model` switch from that turn on and stay
until changed.

### Shapes

```
SessionInfo   { id, title|null, model|null, cwd|null, created_ms, updated_ms,
                event_count, turn_count, running }
SessionDetail { session: SessionInfo,
                events:   [SessionEvent],   // seq > after_seq (all without cursor)
                messages: [Message],        // derive_messages(log): what the model sees
                pending_approvals: [approval_id] }
SessionEvent  { seq, ts_ms, type: "domain/action", ...payload }   // hk-session log line
ModelInfo     { id, provider, name, context_window, max_output|null,
                capabilities: {tools, reasoning, vision}, cost|null }
```

Logged event types: `session/start`, `session/title`, `turn/start`, `turn/end`, `step/start`,
`step/end`, `system/message`, `user/message`, `assistant/message`, `assistant/attempt`,
`tool/call`, `tool/result`, `request/header`, `compaction/summary`, `approval/request`,
`approval/response`.

## WebSocket `/api/v1/ws`

Auth before upgrade (header preferred; `?access_token=` for browsers, beware URL logging).

### Server -> client

Every frame is `{"event": "domain/action", "session_id": "...", "data": {...}}`.

- Every event appended to any session log, as it is written: `event` = the log `type`,
  `data` = the full `SessionEvent` (including `seq`). Use `seq` to de-duplicate against
  `GET /sessions/{id}` and to resume after a reconnect.
- `agent/assistant-stream`: live-only model chunks, never logged.
  `data = {"turn_id", "step", "event": {"type": "text_delta"|"reasoning_delta"|"tool_call_delta"|"usage"|"finish", ...}}`.
- `session/error`: agent loop failed (log unusable), `data.message`.
- `stream/lagged`: client was too slow, `data.skipped` frames dropped; re-fetch via REST.
- `command/ack` / `command/error` (see below).

All sessions are streamed unless the client sends `subscribe` for at least one session.

### Client -> server

JSON text frames, optional `id` echoed in the reply.

```
{"cmd":"submit","session_id":"..","prompt":"..","mode":"build|plan","model":".."}
{"cmd":"inject","session_id":"..","text":".."}
{"cmd":"cancel","session_id":".."}
{"cmd":"approve","approval_id":"appr_0","approved":true,"reason":"..","session_id":".."}
{"cmd":"subscribe","session_id":".."}      // narrow the feed to these sessions
{"cmd":"unsubscribe","session_id":".."}
```

Replies: `{"event":"command/ack","id":..}` or
`{"event":"command/error","id":..,"data":{"code":"forbidden|not_found|bad_request|...","message":".."}}`.
Mutating commands need `run`; a `read` socket gets `forbidden`.

### Approval flow

1. Model calls a risky tool; `approval/request` appears in the feed
   (`data.approval_id`, `tool`, `detail`).
2. Client answers via `POST /approvals/{id}` or WS `approve`.
3. `approval/response` is logged, then the tool runs or is denied.

Approval ids are per session (`appr_N`); pass `session_id` when ambiguous.
