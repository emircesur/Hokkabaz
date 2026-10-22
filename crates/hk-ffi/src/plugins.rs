//! Plugins and MCP servers of the in-process core: what is installed, what is trusted, what
//! runs. WASM plugins add agent tools and flow nodes; MCP servers add agent tools. Both need an
//! explicit user decision first (capability list for plugins, trust prompt for MCP servers).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use hk_mcp::{McpClient, McpServerConfig, McpTool, McpTransportConfig};
use hk_plugin::manifest::Manifest;
use hk_plugin::registry::{self, RegistryEntry, RegistryIndex};
use hk_plugin::store::PluginStore;
use hk_store::{Secret, SecretStore};
use hk_tools::Tool;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::{HkError, HkErrorKind};

type Connected = (Arc<McpClient>, Vec<Arc<dyn Tool>>);
type AddTools = Arc<dyn Fn(Vec<Arc<dyn Tool>>) + Send + Sync>;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct McpEntry {
    config: McpServerConfig,
    #[serde(default)]
    trusted: bool,
    #[serde(default = "yes")]
    enabled: bool,
}

fn yes() -> bool {
    true
}

#[derive(Default)]
struct McpRuntime {
    clients: HashMap<String, Arc<McpClient>>,
    tools: HashMap<String, Vec<String>>,
    errors: HashMap<String, String>,
}

impl std::fmt::Debug for Manager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("plugins::Manager")
    }
}

pub struct Manager {
    store: PluginStore,
    secrets: Arc<dyn SecretStore>,
    add_tools: AddTools,
    mcp_path: PathBuf,
    #[cfg(not(target_os = "android"))]
    host: hk_plugin::PluginHost,
    errors: Mutex<HashMap<String, String>>,
    registered_tools: Mutex<HashSet<String>>,
    mcp: tokio::sync::Mutex<McpRuntime>,
}

static MANAGER: RwLock<Option<Arc<Manager>>> = RwLock::new(None);

pub fn install(m: Arc<Manager>) {
    *MANAGER.write().unwrap_or_else(PoisonError::into_inner) = Some(m);
}

pub fn uninstall() {
    *MANAGER.write().unwrap_or_else(PoisonError::into_inner) = None;
}

pub fn manager() -> Result<Arc<Manager>, HkError> {
    MANAGER
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
        .ok_or_else(|| {
            HkError::new(
                HkErrorKind::NotInitialized,
                "plugins need the in-process core (core_init)",
            )
        })
}

fn perr(e: hk_plugin::PluginError) -> HkError {
    use hk_plugin::PluginError as P;
    match e {
        P::NotFound(m) => HkError::new(HkErrorKind::NotFound, m),
        P::Manifest(m) | P::Denied(m) => HkError::invalid(m),
        P::Registry(m) => HkError::new(HkErrorKind::Unavailable, m),
        P::Unsupported(m) => HkError::new(HkErrorKind::Unsupported, m),
        other => HkError::internal(other.to_string()),
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

struct SecretsBridge(Arc<dyn SecretStore>);

#[cfg(not(target_os = "android"))]
impl hk_plugin::SecretLookup for SecretsBridge {
    fn get(&self, name: &str) -> Option<String> {
        // Plugins see only secrets stored under the `plugin/` namespace.
        self.0
            .get(&format!("plugin/{name}"))
            .ok()
            .flatten()
            .map(|s| s.expose().to_owned())
    }
}

impl Manager {
    pub fn new(
        data_dir: &std::path::Path,
        secrets: Arc<dyn SecretStore>,
        add_tools: AddTools,
    ) -> Result<Arc<Self>, HkError> {
        let store = PluginStore::new(data_dir.join("plugins"));
        #[cfg(not(target_os = "android"))]
        let host = hk_plugin::PluginHost::new(hk_plugin::HostServices {
            secrets: Arc::new(SecretsBridge(secrets.clone())),
            emit: Arc::new(|e| tracing::info!(plugin = %e.plugin_id, trigger = %e.trigger, "plugin event (no listener in the local core)")),
            data_dir: data_dir.join("plugin-data"),
        })
        .map_err(perr)?;
        Ok(Arc::new(Self {
            store,
            secrets,
            add_tools,
            mcp_path: data_dir.join("mcp.json"),
            #[cfg(not(target_os = "android"))]
            host,
            errors: Mutex::new(HashMap::new()),
            registered_tools: Mutex::new(HashSet::new()),
            mcp: tokio::sync::Mutex::new(McpRuntime::default()),
        }))
    }

    // ------------------------------------------------------------------ plugins

    /// Load every enabled plugin; failures are recorded per plugin and do not stop the others.
    pub fn load_all(&self) {
        match self.store.list() {
            Ok(list) => {
                for p in list.into_iter().filter(|p| p.enabled) {
                    if let Err(e) = self.load_one(&p) {
                        tracing::warn!(plugin = %p.manifest.id, error = %e, "plugin failed to load");
                        lock(&self.errors).insert(p.manifest.id.clone(), e.to_string());
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "cannot list plugins"),
        }
        self.rebuild_nodes();
    }

    #[cfg(not(target_os = "android"))]
    fn load_one(&self, p: &hk_plugin::store::InstalledPlugin) -> Result<(), HkError> {
        lock(&self.errors).remove(&p.manifest.id);
        let lp = self.host.load(p).map_err(perr)?;
        let tools: Vec<_> = hk_plugin::tool::plugin_tools(&lp)
            .into_iter()
            .filter(|t| lock(&self.registered_tools).insert(t.spec().name))
            .collect();
        if !tools.is_empty() {
            (self.add_tools)(tools);
        }
        Ok(())
    }

    #[cfg(target_os = "android")]
    fn load_one(&self, _p: &hk_plugin::store::InstalledPlugin) -> Result<(), HkError> {
        Err(HkError::new(
            HkErrorKind::Unsupported,
            "WASM plugins cannot run on Android yet",
        ))
    }

    fn rebuild_nodes(&self) {
        #[cfg(not(target_os = "android"))]
        {
            let loaded = self.host.loaded();
            if let Err(e) = crate::nodes::rebuild(|reg| {
                for p in &loaded {
                    hk_plugin::node::register_plugin_nodes(reg, p);
                }
            }) {
                tracing::warn!(error = %e, "node registry rebuild failed");
            }
        }
    }

    /// Installed plugins with their state, as JSON.
    pub fn list_json(&self) -> Result<Value, HkError> {
        let installed = self.store.list().map_err(perr)?;
        let errors = lock(&self.errors).clone();
        let mut out = Vec::new();
        for p in installed {
            let m = &p.manifest;
            #[allow(unused_mut)]
            let mut v = json!({
                "id": m.id, "name": m.name, "version": m.version, "description": m.description,
                "author": m.author, "license": m.license, "homepage": m.homepage,
                "enabled": p.enabled, "loaded": false,
                "capabilities": m.capability_summary(),
                "error": errors.get(&m.id),
                "tools": [], "nodes": [], "triggers": [], "settings_schema": null,
            });
            #[cfg(not(target_os = "android"))]
            if let Some(lp) = self.host.get(&m.id) {
                v["loaded"] = json!(true);
                v["tools"] = json!(
                    lp.description
                        .tools
                        .iter()
                        .map(|t| &t.name)
                        .collect::<Vec<_>>()
                );
                v["nodes"] = json!(
                    lp.description
                        .nodes
                        .iter()
                        .filter_map(|n| n["displayName"].as_str())
                        .collect::<Vec<_>>()
                );
                v["triggers"] = json!(
                    lp.description
                        .triggers
                        .iter()
                        .map(|t| &t.name)
                        .collect::<Vec<_>>()
                );
                v["settings_schema"] = lp.description.settings.clone().unwrap_or(Value::Null);
                v["settings"] = lp.settings();
            }
            #[cfg(target_os = "android")]
            {
                v["error"] = json!("WASM plugins cannot run on Android yet");
            }
            out.push(v);
        }
        Ok(Value::Array(out))
    }

    /// Install an archive (`.tar.gz`) and load it. Returns the plugin id.
    pub fn install_archive(&self, bytes: &[u8]) -> Result<String, HkError> {
        let p = self.store.install_archive(bytes).map_err(perr)?;
        self.after_install(p)
    }

    /// Install from a directory with `plugin.toml` + wasm (development, `hk plugin build` output).
    pub fn install_dir(&self, dir: &std::path::Path) -> Result<String, HkError> {
        let p = self.store.install_dir(dir).map_err(perr)?;
        self.after_install(p)
    }

    fn after_install(&self, p: hk_plugin::store::InstalledPlugin) -> Result<String, HkError> {
        let id = p.manifest.id.clone();
        #[cfg(not(target_os = "android"))]
        self.host.unload(&id);
        if p.enabled {
            if let Err(e) = self.load_one(&p) {
                lock(&self.errors).insert(id.clone(), e.to_string());
                self.rebuild_nodes();
                return Err(e);
            }
        }
        self.rebuild_nodes();
        Ok(id)
    }

    pub async fn registry_json(&self, index_url: Option<&str>) -> Result<Value, HkError> {
        let url = index_url
            .filter(|u| !u.trim().is_empty())
            .unwrap_or(registry::DEFAULT_INDEX_URL);
        let idx: RegistryIndex = registry::fetch_index(url).await.map_err(perr)?;
        let installed: BTreeMap<String, String> = self
            .store
            .list()
            .map_err(perr)?
            .into_iter()
            .map(|p| (p.manifest.id, p.manifest.version))
            .collect();
        let entries: Vec<Value> = idx
            .plugins
            .iter()
            .map(|e| {
                let cur = installed.get(&e.id);
                let mut v = serde_json::to_value(e).unwrap_or(Value::Null);
                v["installed_version"] = json!(cur);
                v["update_available"] =
                    json!(cur.is_some_and(|c| registry::is_newer(&e.version, c)));
                v["capability_summary"] = json!(
                    Manifest {
                        id: e.id.clone(),
                        name: e.name.clone(),
                        version: e.version.clone(),
                        description: String::new(),
                        wasm: "x.wasm".into(),
                        author: String::new(),
                        license: String::new(),
                        homepage: String::new(),
                        capabilities: e.capabilities.clone()
                    }
                    .capability_summary()
                );
                v
            })
            .collect();
        Ok(json!({"url": url, "plugins": entries}))
    }

    pub async fn install_from_registry(
        &self,
        id: &str,
        index_url: Option<&str>,
    ) -> Result<String, HkError> {
        let url = index_url
            .filter(|u| !u.trim().is_empty())
            .unwrap_or(registry::DEFAULT_INDEX_URL);
        let idx = registry::fetch_index(url).await.map_err(perr)?;
        let entry: &RegistryEntry = idx.find(id).ok_or_else(|| {
            HkError::new(
                HkErrorKind::NotFound,
                format!("`{id}` is not in the registry"),
            )
        })?;
        let bytes = registry::download(entry).await.map_err(perr)?;
        let p = self.store.install_archive(&bytes).map_err(perr)?;
        // The artifact must be the plugin the index promised.
        if p.manifest.id != entry.id {
            let _ = self.store.remove(&p.manifest.id);
            return Err(HkError::invalid(format!(
                "artifact contains `{}`, expected `{}`",
                p.manifest.id, entry.id
            )));
        }
        self.after_install(p)
    }

    pub fn remove(&self, id: &str) -> Result<(), HkError> {
        #[cfg(not(target_os = "android"))]
        self.host.unload(id);
        self.store.remove(id).map_err(perr)?;
        lock(&self.errors).remove(id);
        self.rebuild_nodes();
        Ok(())
    }

    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<(), HkError> {
        self.store.set_enabled(id, enabled).map_err(perr)?;
        let p = self.store.get(id).map_err(perr)?;
        #[cfg(not(target_os = "android"))]
        self.host.unload(id);
        lock(&self.errors).remove(id);
        if enabled {
            if let Err(e) = self.load_one(&p) {
                lock(&self.errors).insert(id.to_owned(), e.to_string());
            }
        }
        self.rebuild_nodes();
        Ok(())
    }

    pub fn set_settings(&self, id: &str, settings: &Value) -> Result<(), HkError> {
        self.store.get(id).map_err(perr)?;
        #[cfg(not(target_os = "android"))]
        if let Some(lp) = self.host.get(id) {
            return lp.set_settings(settings).map_err(perr);
        }
        Err(HkError::new(
            HkErrorKind::Unsupported,
            "plugin is not loaded",
        ))
    }

    // --------------------------------------------------------------------- MCP

    fn read_mcp(&self) -> Vec<McpEntry> {
        std::fs::read(&self.mcp_path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn write_mcp(&self, v: &[McpEntry]) -> Result<(), HkError> {
        let tmp = self.mcp_path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(v)?)
            .map_err(|e| HkError::internal(e.to_string()))?;
        std::fs::rename(&tmp, &self.mcp_path).map_err(|e| HkError::internal(e.to_string()))
    }

    fn mcp_secret(name: &str, what: &str) -> String {
        format!("mcp/{name}/{what}")
    }

    /// Config with its secrets filled in from the secret store.
    fn resolved(&self, cfg: &McpServerConfig) -> McpServerConfig {
        let mut c = cfg.clone();
        let get = |k: String| {
            self.secrets
                .get(&k)
                .ok()
                .flatten()
                .map(|s| s.expose().to_owned())
        };
        match &mut c.transport {
            McpTransportConfig::Http(h) => {
                h.bearer_token = get(Self::mcp_secret(&cfg.name, "token"))
            }
            McpTransportConfig::Stdio(s) => {
                for k in s.env.keys().cloned().collect::<Vec<_>>() {
                    if let Some(v) = get(Self::mcp_secret(&cfg.name, &format!("env/{k}"))) {
                        s.env.insert(k, v);
                    }
                }
            }
        }
        c
    }

    /// Servers, their trust and connection state. Secrets are never included.
    pub async fn mcp_json(&self) -> Value {
        let rt = self.mcp.lock().await;
        let entries = self.read_mcp();
        Value::Array(
            entries
                .iter()
                .map(|e| {
                    let (kind, target, env_keys) = match &e.config.transport {
                        McpTransportConfig::Stdio(s) => ("stdio", format!("{} {}", s.command, s.args.join(" ")).trim().to_owned(), s.env.keys().cloned().collect::<Vec<_>>()),
                        McpTransportConfig::Http(h) => ("http", h.url.clone(), Vec::new()),
                    };
                    json!({
                        "name": e.config.name, "transport": kind, "target": target, "env_keys": env_keys,
                        "trusted": e.trusted, "enabled": e.enabled,
                        "connected": rt.clients.contains_key(&e.config.name),
                        "tools": rt.tools.get(&e.config.name),
                        "error": rt.errors.get(&e.config.name),
                        "stdio_supported": cfg!(not(target_os = "android")),
                    })
                })
                .collect(),
        )
    }

    /// Add or change a server. `spec`: `{name, transport:"stdio"|"http", command, args[], env{}, cwd,
    /// url, bearer_token, headers{}, timeout_secs}`. Secrets (`bearer_token`, `env` values) go to the
    /// secret store. Any change resets trust: the user must approve what will actually run.
    pub async fn mcp_save(&self, spec: &Value) -> Result<(), HkError> {
        let mut cfg: McpServerConfig = serde_json::from_value(spec.clone())
            .map_err(|e| HkError::invalid(format!("bad MCP server: {e}")))?;
        cfg.validate()
            .map_err(|e| HkError::invalid(e.to_string()))?;
        let name = cfg.name.clone();
        let put = |k: String, v: &str| {
            self.secrets
                .set(&k, &Secret::new(v))
                .map_err(|e| HkError::internal(format!("secret store: {e}")))
        };
        match &mut cfg.transport {
            McpTransportConfig::Http(h) => {
                if let Some(t) = h.bearer_token.take().filter(|t| !t.is_empty()) {
                    put(Self::mcp_secret(&name, "token"), &t)?;
                }
            }
            McpTransportConfig::Stdio(s) => {
                for (k, v) in s.env.iter_mut() {
                    if !v.is_empty() {
                        put(Self::mcp_secret(&name, &format!("env/{k}")), v)?;
                        v.clear();
                    }
                }
            }
        }
        let mut entries = self.read_mcp();
        let was_trusted = entries
            .iter()
            .find(|e| e.config.name == name)
            .is_some_and(|e| {
                e.trusted && serde_json::to_value(&e.config).ok() == serde_json::to_value(&cfg).ok()
            });
        entries.retain(|e| e.config.name != name);
        entries.push(McpEntry {
            config: cfg,
            trusted: was_trusted,
            enabled: true,
        });
        entries.sort_by(|a, b| a.config.name.cmp(&b.config.name));
        self.write_mcp(&entries)?;
        self.mcp.lock().await.clients.remove(&name);
        Ok(())
    }

    pub async fn mcp_set_trusted(&self, name: &str, trusted: bool) -> Result<(), HkError> {
        let mut entries = self.read_mcp();
        entries
            .iter_mut()
            .find(|e| e.config.name == name)
            .ok_or_else(|| HkError::new(HkErrorKind::NotFound, format!("MCP server {name}")))?
            .trusted = trusted;
        self.write_mcp(&entries)?;
        if trusted {
            self.mcp_connect(name).await
        } else {
            self.mcp.lock().await.clients.remove(name);
            Ok(())
        }
    }

    pub async fn mcp_set_enabled(&self, name: &str, enabled: bool) -> Result<(), HkError> {
        let mut entries = self.read_mcp();
        entries
            .iter_mut()
            .find(|e| e.config.name == name)
            .ok_or_else(|| HkError::new(HkErrorKind::NotFound, format!("MCP server {name}")))?
            .enabled = enabled;
        self.write_mcp(&entries)?;
        if !enabled {
            self.mcp.lock().await.clients.remove(name);
        }
        Ok(())
    }

    pub async fn mcp_remove(&self, name: &str) -> Result<(), HkError> {
        let mut entries = self.read_mcp();
        let before = entries.len();
        entries.retain(|e| e.config.name != name);
        if entries.len() == before {
            return Err(HkError::new(
                HkErrorKind::NotFound,
                format!("MCP server {name}"),
            ));
        }
        self.write_mcp(&entries)?;
        let mut rt = self.mcp.lock().await;
        rt.clients.remove(name);
        rt.tools.remove(name);
        rt.errors.remove(name);
        let _ = self.secrets.delete(&Self::mcp_secret(name, "token"));
        Ok(())
    }

    /// Connect one trusted, enabled server and offer its tools to agents (new sessions).
    pub async fn mcp_connect(&self, name: &str) -> Result<(), HkError> {
        let entry = self
            .read_mcp()
            .into_iter()
            .find(|e| e.config.name == name)
            .ok_or_else(|| HkError::new(HkErrorKind::NotFound, format!("MCP server {name}")))?;
        if !entry.trusted {
            return Err(HkError::invalid("this MCP server has not been trusted yet"));
        }
        if !entry.enabled {
            return Ok(());
        }
        let cfg = self.resolved(&entry.config);
        let result: Result<Connected, String> = async {
            let client = Arc::new(McpClient::connect(&cfg).await.map_err(|e| e.to_string())?);
            let infos = client.list_tools().await.map_err(|e| e.to_string())?;
            let tools = infos
                .into_iter()
                .map(|i| Arc::new(McpTool::new(client.clone(), i)) as Arc<dyn Tool>)
                .collect();
            Ok((client, tools))
        }
        .await;
        let mut rt = self.mcp.lock().await;
        match result {
            Ok((client, tools)) => {
                rt.errors.remove(name);
                let names: Vec<String> = tools.iter().map(|t| t.spec().name).collect();
                let fresh: Vec<_> = tools
                    .into_iter()
                    .filter(|t| lock(&self.registered_tools).insert(t.spec().name))
                    .collect();
                if !fresh.is_empty() {
                    (self.add_tools)(fresh);
                }
                rt.tools.insert(name.to_owned(), names);
                rt.clients.insert(name.to_owned(), client);
                Ok(())
            }
            Err(e) => {
                rt.errors.insert(name.to_owned(), e.clone());
                Err(HkError::new(HkErrorKind::Provider, e))
            }
        }
    }

    /// Connect every trusted + enabled server (startup; errors are recorded per server).
    pub async fn mcp_connect_all(&self) {
        for e in self
            .read_mcp()
            .into_iter()
            .filter(|e| e.trusted && e.enabled)
        {
            if let Err(err) = self.mcp_connect(&e.config.name).await {
                tracing::warn!(server = %e.config.name, error = %err, "MCP server did not connect");
            }
        }
    }
}
