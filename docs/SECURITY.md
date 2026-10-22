# Security notes (P7.4 pass)

## Boundaries

| Surface | Control |
|---|---|
| Secrets (provider keys, credentials, MCP tokens, instance tokens) | `hk-store::secrets`; `Secret` never prints; Android: Keystore-wrapped key + ChaCha20-Poly1305 file; desktop: OS keyring, else 0600 file. Never in the session log, flow execution data or logs (audited: no `tracing` call carries a secret field; HTTP errors drop URLs). Credential values are write-only from the UI. |
| Tool output | `post-execute` redaction runs before truncate/spill. |
| Prompt injection | Output from the web (`web_fetch`, `web_search`), MCP servers and plugins is wrapped in `<untrusted-content source="...">`; the closing tag cannot be forged by the content, and the system prompt tells the model to treat it as data. Not a guarantee: approvals (`Risk::Ask`) stay the backstop for web/MCP/plugin tools. |
| WASM plugins | Deny-by-default capabilities checked on every host call, fresh instance per call, time + memory limits, secrets only from the `plugin/` namespace, SHA-256-verified registry installs. |
| MCP servers | Untrusted until the user approves; any config change resets trust; secrets stored encrypted. stdio servers are desktop only. |
| Code node | JavaScript in QuickJS (no fs/net/process, memory/stack/time caps). Python mode is a plain subprocess, so it is **off** unless `HOKKABAZ_ALLOW_PYTHON=1`. |
| n8n delegation | Opt-in; items leave the device; the UI says so. |
| Webhooks | Route matching and body parsing (JSON, form, multipart, text, binary) under a deterministic fuzz test (`hk-flow/tests/webhook_fuzz.rs`, 20k hostile requests; seed via `HK_FUZZ_SEED`). Instance auth by API token; pairing tokens are one-time. |
| Local models | Downloads only from the catalog URL, size and SHA-256 checked, resumable; custom GGUF is used in place after a header check. |

## Known gaps

* A tool can still be talked into misuse by injected text if the user approves it blindly; keep `Ask` approvals on for web-facing agents.
* Python Code node and stdio MCP servers run with the user's privileges.
* Android has no process sandbox beyond the OS app sandbox (landlock is Linux desktop only).
* The plugin registry trusts whoever can merge to `index.json`; users see the capability list before installing.
