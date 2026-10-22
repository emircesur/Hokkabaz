//! The WASM runtime: wasmtime components, a fresh sandboxed instance per call, wall-clock and
//! memory limits, and host services that check the manifest's capabilities on every call.
// port: none (Hokkabaz-native; see DESIGN §3.10)

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder};
use wasmtime_wasi::{FsPerms, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::error::{PluginError, Result};
use crate::manifest::{Capabilities, Manifest};
use crate::store::InstalledPlugin;

wasmtime::component::bindgen!({
    path: "../../wit/hokkabaz-plugin.wit",
    world: "plugin",
});

const EPOCH_TICK: Duration = Duration::from_millis(100);
/// Wall-clock limit of one guest call.
const CALL_TIMEOUT: Duration = Duration::from_secs(60);
const MEMORY_LIMIT: usize = 256 * 1024 * 1024;
const MAX_HTTP_PER_CALL: u32 = 50;
const MAX_HTTP_BODY: usize = 8 * 1024 * 1024;
const MAX_LOG_PER_CALL: u32 = 200;
const MAX_KV_BYTES: usize = 1024 * 1024;

/// Looks up secrets by name (the embedder's secret store).
pub trait SecretLookup: Send + Sync {
    fn get(&self, name: &str) -> Option<String>;
}

/// A trigger event a plugin emitted.
#[derive(Debug, Clone)]
pub struct EmitEvent {
    pub plugin_id: String,
    pub trigger: String,
    pub event: Value,
}

/// What the embedder gives the runtime.
#[derive(Clone)]
pub struct HostServices {
    pub secrets: Arc<dyn SecretLookup>,
    pub emit: Arc<dyn Fn(EmitEvent) + Send + Sync>,
    /// Per-plugin data lives in `<data_dir>/<plugin id>/`.
    pub data_dir: PathBuf,
}

impl fmt::Debug for HostServices {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostServices")
            .field("data_dir", &self.data_dir)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerDecl {
    pub name: String,
    #[serde(default = "default_interval")]
    pub interval_secs: u64,
}

fn default_interval() -> u64 {
    300
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolDeclJson {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    pub risk: String,
}

/// What `describe` returned.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Description {
    pub tools: Vec<ToolDeclJson>,
    pub nodes: Vec<Value>,
    pub triggers: Vec<TriggerDecl>,
    pub settings: Option<Value>,
}

/// Per-plugin key/value store, persisted to `kv.json`.
#[derive(Debug)]
struct Kv {
    path: PathBuf,
    map: Mutex<HashMap<String, String>>,
}

impl Kv {
    fn open(dir: PathBuf) -> Self {
        let path = dir.join("kv.json");
        let map = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self {
            path,
            map: Mutex::new(map),
        }
    }

    fn with<R>(&self, f: impl FnOnce(&mut HashMap<String, String>) -> R) -> R {
        f(&mut self.map.lock().unwrap_or_else(PoisonError::into_inner))
    }

    fn persist(&self, map: &HashMap<String, String>) -> std::result::Result<(), String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec(map).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &self.path).map_err(|e| e.to_string())
    }
}

/// Everything one guest instance can touch.
struct State {
    wasi: WasiCtx,
    table: ResourceTable,
    limits: StoreLimits,
    id: String,
    caps: Capabilities,
    svc: Arc<HostServices>,
    kv: Arc<Kv>,
    http_calls: u32,
    logs: u32,
}

impl WasiView for State {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

fn host_err(e: impl fmt::Display) -> String {
    e.to_string()
}

impl hokkabaz::plugin::host::Host for State {
    fn log(&mut self, level: u8, message: String) {
        if self.logs >= MAX_LOG_PER_CALL {
            return;
        }
        self.logs += 1;
        let msg: String = message.chars().take(2000).collect();
        match level {
            0 => tracing::trace!(plugin = %self.id, "{msg}"),
            1 => tracing::debug!(plugin = %self.id, "{msg}"),
            2 => tracing::info!(plugin = %self.id, "{msg}"),
            3 => tracing::warn!(plugin = %self.id, "{msg}"),
            _ => tracing::error!(plugin = %self.id, "{msg}"),
        }
    }

    fn http(&mut self, request: String) -> std::result::Result<String, String> {
        if self.http_calls >= MAX_HTTP_PER_CALL {
            return Err("too many http requests in one call".into());
        }
        self.http_calls += 1;
        let req: Value = serde_json::from_str(&request).map_err(host_err)?;
        let url = req["url"].as_str().ok_or("`url` is required")?;
        let url = self.caps.check_url(url).map_err(host_err)?;
        let method = req["method"].as_str().unwrap_or("GET").to_ascii_uppercase();
        let method = reqwest::Method::from_bytes(method.as_bytes()).map_err(host_err)?;
        let timeout = Duration::from_millis(
            req["timeout_ms"]
                .as_u64()
                .unwrap_or(15_000)
                .clamp(100, 60_000),
        );
        // Redirects are followed only to hosts the plugin was granted.
        let caps = self.caps.clone();
        let policy = reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= 5 || caps.check_url(attempt.url().as_str()).is_err() {
                attempt.stop()
            } else {
                attempt.follow()
            }
        });
        let client = reqwest::blocking::Client::builder()
            .timeout(timeout)
            .redirect(policy)
            .build()
            .map_err(host_err)?;
        let mut rb = client.request(method, url);
        if let Some(h) = req["headers"].as_object() {
            for (k, v) in h {
                if let Some(v) = v.as_str() {
                    rb = rb.header(k, v);
                }
            }
        }
        if let Some(b) = req["body"].as_str() {
            rb = rb.body(b.to_string());
        }
        let resp = rb.send().map_err(host_err)?;
        let status = resp.status().as_u16();
        let headers: serde_json::Map<String, Value> = resp
            .headers()
            .iter()
            .filter_map(|(k, v)| {
                v.to_str()
                    .ok()
                    .map(|v| (k.as_str().to_string(), Value::String(v.to_string())))
            })
            .collect();
        let mut body = Vec::new();
        std::io::Read::read_to_end(
            &mut std::io::Read::take(resp, MAX_HTTP_BODY as u64 + 1),
            &mut body,
        )
        .map_err(host_err)?;
        if body.len() > MAX_HTTP_BODY {
            return Err("response body is too large".into());
        }
        Ok(serde_json::json!({"status": status, "headers": headers, "body": String::from_utf8_lossy(&body)}).to_string())
    }

    fn kv_get(&mut self, key: String) -> std::result::Result<Option<String>, String> {
        self.caps.check_kv().map_err(host_err)?;
        Ok(self.kv.with(|m| m.get(&key).cloned()))
    }

    fn kv_set(&mut self, key: String, value: String) -> std::result::Result<(), String> {
        self.caps.check_kv().map_err(host_err)?;
        if key.is_empty() || key.len() > 256 {
            return Err("key must be 1-256 bytes".into());
        }
        self.kv.with(|m| {
            let used: usize = m
                .iter()
                .filter(|(k, _)| **k != key)
                .map(|(k, v)| k.len() + v.len())
                .sum();
            if used + key.len() + value.len() > MAX_KV_BYTES {
                return Err("key/value store is full (1 MiB)".to_string());
            }
            m.insert(key, value);
            self.kv.persist(m)
        })
    }

    fn kv_delete(&mut self, key: String) -> std::result::Result<(), String> {
        self.caps.check_kv().map_err(host_err)?;
        self.kv.with(|m| {
            m.remove(&key);
            self.kv.persist(m)
        })
    }

    fn secret(&mut self, name: String) -> std::result::Result<String, String> {
        self.caps.check_secret(&name).map_err(host_err)?;
        self.svc
            .secrets
            .get(&name)
            .ok_or_else(|| format!("secret `{name}` is not set"))
    }

    fn emit(&mut self, trigger: String, event: String) -> std::result::Result<(), String> {
        self.caps.check_emit().map_err(host_err)?;
        let event: Value = serde_json::from_str(&event).map_err(host_err)?;
        (self.svc.emit)(EmitEvent {
            plugin_id: self.id.clone(),
            trigger,
            event,
        });
        Ok(())
    }
}

/// A plugin ready to run.
pub struct LoadedPlugin {
    pub manifest: Manifest,
    pub description: Description,
    engine: Engine,
    pre: PluginPre<State>,
    svc: Arc<HostServices>,
    kv: Arc<Kv>,
    settings_path: PathBuf,
}

impl fmt::Debug for LoadedPlugin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoadedPlugin")
            .field("id", &self.manifest.id)
            .field("version", &self.manifest.version)
            .finish_non_exhaustive()
    }
}

impl LoadedPlugin {
    fn new_store(&self) -> std::result::Result<Store<State>, PluginError> {
        let mut wasi = WasiCtxBuilder::new();
        // No stdio, no env, no network, no clocks beyond what WASI always gives; files only where granted.
        for g in &self.manifest.capabilities.fs {
            let perms = if g.write {
                FsPerms::ReadWrite
            } else {
                FsPerms::ReadOnly
            };
            wasi.preopened_dir(&g.path, g.path.to_string_lossy(), perms)
                .map_err(|e| PluginError::Runtime(format!("fs grant {}: {e}", g.path.display())))?;
        }
        let state = State {
            wasi: wasi.build(),
            table: ResourceTable::new(),
            limits: StoreLimitsBuilder::new()
                .memory_size(MEMORY_LIMIT)
                .instances(16)
                .tables(16)
                .memories(8)
                .build(),
            id: self.manifest.id.clone(),
            caps: self.manifest.capabilities.clone(),
            svc: self.svc.clone(),
            kv: self.kv.clone(),
            http_calls: 0,
            logs: 0,
        };
        let mut store = Store::new(&self.engine, state);
        store.limiter(|s| &mut s.limits);
        store.set_epoch_deadline(CALL_TIMEOUT.as_millis() as u64 / EPOCH_TICK.as_millis() as u64);
        Ok(store)
    }

    fn run<R>(
        &self,
        f: impl FnOnce(&Plugin, &mut Store<State>) -> wasmtime::Result<R>,
    ) -> Result<R> {
        let mut store = self.new_store()?;
        let inst = self
            .pre
            .instantiate(&mut store)
            .map_err(|e| PluginError::Runtime(format!("{e:#}")))?;
        f(&inst, &mut store).map_err(|e| PluginError::Runtime(format!("{e:#}")))
    }

    /// Per-plugin settings JSON (`settings.json`), `null` when never saved.
    pub fn settings(&self) -> Value {
        std::fs::read(&self.settings_path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or(Value::Null)
    }

    pub fn set_settings(&self, v: &Value) -> Result<()> {
        if let Some(p) = self.settings_path.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(
            &self.settings_path,
            serde_json::to_vec(v).map_err(|e| PluginError::Runtime(e.to_string()))?,
        )?;
        Ok(())
    }

    /// Blocking: run it on a blocking thread from async code.
    pub fn call_tool(&self, name: &str, args: &str) -> Result<String> {
        self.run(|p, s| p.hokkabaz_plugin_guest().call_call_tool(s, name, args))?
            .map_err(PluginError::Plugin)
    }

    /// Blocking. `input` is the JSON described in the WIT file; settings are added when missing.
    pub fn execute_node(&self, node_type: &str, mut input: Value) -> Result<Value> {
        if input.get("settings").is_none() {
            input["settings"] = self.settings();
        }
        let text = input.to_string();
        let out = self
            .run(|p, s| {
                p.hokkabaz_plugin_guest()
                    .call_execute_node(s, node_type, &text)
            })?
            .map_err(PluginError::Plugin)?;
        serde_json::from_str(&out)
            .map_err(|e| PluginError::Plugin(format!("node returned invalid JSON: {e}")))
    }

    /// Blocking. Returns `(events, new_state)`.
    pub fn poll_trigger(&self, name: &str, state: &Value) -> Result<(Vec<Value>, Value)> {
        let st = state.to_string();
        let out = self
            .run(|p, s| p.hokkabaz_plugin_guest().call_poll_trigger(s, name, &st))?
            .map_err(PluginError::Plugin)?;
        let v: Value = serde_json::from_str(&out)
            .map_err(|e| PluginError::Plugin(format!("trigger returned invalid JSON: {e}")))?;
        Ok((
            v["events"].as_array().cloned().unwrap_or_default(),
            v["state"].clone(),
        ))
    }
}

/// Owns the engine and the loaded plugins.
pub struct PluginHost {
    engine: Engine,
    linker: Linker<State>,
    svc: Arc<HostServices>,
    loaded: RwLock<HashMap<String, Arc<LoadedPlugin>>>,
    stop: Arc<AtomicBool>,
}

impl fmt::Debug for PluginHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PluginHost")
            .field("loaded", &self.loaded.read().map(|l| l.len()).unwrap_or(0))
            .finish_non_exhaustive()
    }
}

impl Drop for PluginHost {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl PluginHost {
    pub fn new(services: HostServices) -> Result<Self> {
        let mut cfg = Config::new();
        cfg.wasm_component_model(true);
        cfg.epoch_interruption(true);
        let engine = Engine::new(&cfg).map_err(|e| PluginError::Runtime(format!("{e:#}")))?;
        let mut linker: Linker<State> = Linker::new(&engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
            .map_err(|e| PluginError::Runtime(format!("{e:#}")))?;
        Plugin::add_to_linker::<State, wasmtime::component::HasSelf<State>>(&mut linker, |s| s)
            .map_err(|e| PluginError::Runtime(format!("{e:#}")))?;
        // Epoch ticker: bounds every guest call by wall-clock time.
        let stop = Arc::new(AtomicBool::new(false));
        {
            let (engine, stop) = (engine.clone(), stop.clone());
            std::thread::Builder::new()
                .name("hk-plugin-epoch".into())
                .spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        std::thread::sleep(EPOCH_TICK);
                        engine.increment_epoch();
                    }
                })
                .map_err(PluginError::Io)?;
        }
        Ok(Self {
            engine,
            linker,
            svc: Arc::new(services),
            loaded: RwLock::new(HashMap::new()),
            stop,
        })
    }

    /// Compile and instantiate-check a plugin, ask it to describe itself, keep it loaded.
    pub fn load(&self, p: &InstalledPlugin) -> Result<Arc<LoadedPlugin>> {
        let component = Component::from_file(&self.engine, p.wasm_path())
            .map_err(|e| PluginError::Runtime(format!("{}: {e:#}", p.manifest.wasm)))?;
        let pre = self
            .linker
            .instantiate_pre(&component)
            .map_err(|e| PluginError::Runtime(format!("{e:#}")))?;
        let pre = PluginPre::new(pre).map_err(|e| PluginError::Runtime(format!("{e:#}")))?;
        let data = self.svc.data_dir.join(&p.manifest.id);
        let lp = LoadedPlugin {
            manifest: p.manifest.clone(),
            description: Description::default(),
            engine: self.engine.clone(),
            pre,
            svc: self.svc.clone(),
            kv: Arc::new(Kv::open(data.clone())),
            settings_path: data.join("settings.json"),
        };
        let raw = lp.run(|inst, s| inst.hokkabaz_plugin_guest().call_describe(s))?;
        let description: Description = serde_json::from_str(&raw)
            .map_err(|e| PluginError::Plugin(format!("describe returned invalid JSON: {e}")))?;
        for t in &description.tools {
            if t.name.is_empty()
                || t.name.len() > 64
                || !t
                    .name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
            {
                return Err(PluginError::Plugin(format!("bad tool name `{}`", t.name)));
            }
        }
        let lp = Arc::new(LoadedPlugin { description, ..lp });
        self.loaded
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(p.manifest.id.clone(), lp.clone());
        Ok(lp)
    }

    pub fn unload(&self, id: &str) -> bool {
        self.loaded
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id)
            .is_some()
    }

    pub fn get(&self, id: &str) -> Option<Arc<LoadedPlugin>> {
        self.loaded
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id)
            .cloned()
    }

    pub fn loaded(&self) -> Vec<Arc<LoadedPlugin>> {
        let mut v: Vec<_> = self
            .loaded
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .cloned()
            .collect();
        v.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
        v
    }
}
