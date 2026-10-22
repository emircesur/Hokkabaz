# Plugins

Hokkabaz extends in four ways. Pick by what you need:

| Mechanism | Use for | Runs |
|---|---|---|
| **WASM plugin** | agent tools, flow nodes, polling triggers | sandboxed in-process (wasmtime); desktop only for now |
| **MCP server** | tools from an existing MCP ecosystem | separate process (stdio, desktop) or remote (HTTP) |
| **Integration nodes** | built-in REST services as n8n node types | in-process, all platforms |
| **Credentials** | secrets referenced by flow nodes | encrypted store; values never reach the UI |

## Security model

* Deny by default. A plugin gets what `plugin.toml` `[capabilities]` declares, checked on **every** host call:
  `http` (host patterns), `kv`, `secrets` (names; read from the `plugin/<name>` namespace only), `emit`, `fs` (host dirs, read or read-write).
* Each call runs in a fresh instance with an epoch (time) limit and a memory limit. No state survives except `kv`.
* The Plugins screen shows the capability list before install and for installed plugins.
* Registry installs verify SHA-256 of the artifact against `index.json`; archives are unpacked flat (no paths).
* MCP servers are **untrusted until you trust them**; any config change resets trust. Secrets (bearer token, env values) are stored encrypted, not in `mcp.json`.

## Writing a plugin (Rust)

```sh
hk plugin new my-plugin --id dev.me.my-plugin     # scaffold
cd my-plugin
hk plugin build                                   # -> dist/ (wasm32-wasip2; rustup target add wasm32-wasip2)
hk plugin install dist                            # or the .tar.gz
```

`src/lib.rs` implements `hk_plugin_sdk::Plugin`:

```rust
impl Plugin for MyPlugin {
    fn describe() -> Description { /* tools, nodes (n8n node JSON), triggers, settings schema */ }
    fn call_tool(name: &str, args: Value) -> Result<String, String>;
    fn execute_node(_n: NodeInput) -> Result<NodeOutput, String>;   // optional
    fn poll_trigger(_name: &str, _last: Value) -> Result<PollOutput, String>; // optional
}
```

Host functions (`hk_plugin_sdk::host`): `log`, `http` / `get`, `kv_get` / `kv_set` / `kv_delete`, `secret`, `emit`.
Tool risk (`Risk::Safe | Ask | Dangerous`) feeds the agent's approval flow. A working example lives in `plugins/hello`.

The ABI is `wit/hokkabaz-plugin.wit` (WASM component model, JSON-string payloads, version 1).
Any language that targets components works: JS via `jco componentize`, Python via `componentize-py`,
Go via TinyGo + `wit-bindgen-go`. Implement the `plugin` world and ship the `.wasm` with a `plugin.toml`.

### `plugin.toml`

```toml
id = "dev.me.my-plugin"        # [a-z0-9._-]
name = "My plugin"
version = "0.1.0"              # MAJOR.MINOR.PATCH
description = ""
wasm = "my_plugin.wasm"
[capabilities]
http = ["api.example.com", "*.example.org"]
kv = true
secrets = ["my-plugin-token"]  # set in the secret store as plugin/my-plugin-token
emit = false
fs = [{ path = "/data/shared", write = false }]
```

## On a headless instance

`hokkabazd` loads every enabled plugin from `<data dir>/plugins` at start (install with `hk plugin install` using the same
`--data-dir`/`HOKKABAZ_HOME`, then restart). Tools join the agent, nodes join the flow registry, and each trigger the plugin
declares becomes a polling trigger node `<plugin id>.<trigger>`: activating a workflow that contains it polls the plugin every
`interval_secs` (or the node's Poll Times) and starts the workflow with the returned events. Events carrying an `id` are
deduplicated across polls. Polling triggers of the built-in integrations (GitHub, Telegram, RSS, email) use the same machinery
and now fire on the daemon too.

## Registry

`index.json` (see `registry-template/`) lists entries `{id, name, version, description, capabilities, download_url (https), sha256, size}`.
The default registry is `emircesur/hokkabaz-plugins`; the Plugins screen can point at another URL.

* `hk plugin index-entry <archive> --url <asset url>` prints a ready entry.
* `hk plugin verify-index index.json --download` is what the registry's CI runs.

## MCP servers

Plugins screen > MCP servers > Add. HTTP servers work everywhere; command (stdio) servers are desktop only.
After adding, review the command/URL and press **Review & trust**: only then does the server connect and its tools join the agent.

## Credentials

Plugins screen > Credentials. Types come from the integrations registry (`hk-integrations`); node panels show a
picker for the credential types a node declares. In flow JSON a node references `credentials: { <type>: { id, name } }`.
