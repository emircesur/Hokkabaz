//! Layered config: defaults -> profile -> user -> project -> env, TOML, patch-by-id arrays.
// port: design §3.2 (config layers; mirrors dsh profiles/bundles/patches concept, no source ported)
//!
//! Merge rules (later layer wins):
//! - tables merge recursively;
//! - arrays whose entries are all tables with a string `id` merge by id: matching id deep-merges
//!   into the existing entry, new id appends, `remove = true` deletes the entry. `enabled = false`
//!   is an ordinary field: the entry stays in the tree and `Config::entries` skips it. An empty
//!   patch array replaces (clears) the base;
//! - everything else (scalars, plain arrays) replaces.

use std::path::{Path, PathBuf};

use toml::{Table, Value};

const BASE_DEFAULTS: &str = r#"
[agent]
mode = "build"
max_steps = 50
max_parallel_subagents = 4

[mcp]
allow_stdio = true

[sandbox]
backend = "auto"

[acp]
enabled = true

[[plugins]]
id = "session"
enabled = true

[[plugins]]
id = "agent"
enabled = true

[[plugins]]
id = "tools"
enabled = true
"#;

const CLI_PROFILE: &str = r#"
[[plugins]]
id = "tui"
enabled = true
"#;

const DESKTOP_PROFILE: &str = r#"
[[plugins]]
id = "flow"
enabled = true

[[plugins]]
id = "ffi"
enabled = true
"#;

// Android: no landlock, no acp, no stdio MCP spawn (CLAUDE.md conventions).
const MOBILE_PROFILE: &str = r#"
[mcp]
allow_stdio = false

[sandbox]
backend = "none"

[acp]
enabled = false

[[plugins]]
id = "flow"
enabled = true

[[plugins]]
id = "ffi"
enabled = true
"#;

const DAEMON_PROFILE: &str = r#"
[[plugins]]
id = "flow"
enabled = true

[[plugins]]
id = "server"
enabled = true
"#;

const REDACTED: &str = "<redacted>";
const ENV_PREFIX: &str = "HK_";

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config layer `{layer}`: {msg}")]
    Parse { layer: String, msg: String },
    #[error("config file {path}: {msg}")]
    Io { path: PathBuf, msg: String },
    #[error("unknown profile `{0}` (expected cli|desktop|mobile|daemon)")]
    UnknownProfile(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Profile {
    Cli,
    Desktop,
    Mobile,
    Daemon,
}

impl Profile {
    pub fn as_str(self) -> &'static str {
        match self {
            Profile::Cli => "cli",
            Profile::Desktop => "desktop",
            Profile::Mobile => "mobile",
            Profile::Daemon => "daemon",
        }
    }

    /// Profile-specific layer (sits on top of built-in defaults).
    pub fn layer(self) -> Layer {
        let src = match self {
            Profile::Cli => CLI_PROFILE,
            Profile::Desktop => DESKTOP_PROFILE,
            Profile::Mobile => MOBILE_PROFILE,
            Profile::Daemon => DAEMON_PROFILE,
        };
        Layer::from_toml(format!("profile:{}", self.as_str()), src).expect("builtin profile toml")
    }
}

impl std::str::FromStr for Profile {
    type Err = ConfigError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "cli" => Ok(Profile::Cli),
            "desktop" => Ok(Profile::Desktop),
            "mobile" => Ok(Profile::Mobile),
            "daemon" => Ok(Profile::Daemon),
            _ => Err(ConfigError::UnknownProfile(s.to_string())),
        }
    }
}

/// One named config layer.
#[derive(Debug, Clone)]
pub struct Layer {
    pub name: String,
    pub table: Table,
}

impl Layer {
    pub fn from_toml(name: impl Into<String>, src: &str) -> Result<Self, ConfigError> {
        let name = name.into();
        let table = src.parse::<Table>().map_err(|e| ConfigError::Parse {
            layer: name.clone(),
            msg: e.to_string(),
        })?;
        Ok(Self { name, table })
    }

    /// Missing file -> `None` (layers are optional); unreadable/invalid file -> error.
    pub fn from_file(name: impl Into<String>, path: &Path) -> Result<Option<Self>, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(src) => Self::from_toml(name, &src).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(ConfigError::Io {
                path: path.to_path_buf(),
                msg: e.to_string(),
            }),
        }
    }

    /// Env layer: `HK_` prefix, `__` nests, keys lowercased (`HK_MCP__ALLOW_STDIO=false`
    /// -> `mcp.allow_stdio = false`). Values parse as TOML scalars/arrays, else plain string.
    pub fn from_env<I, K, V>(vars: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let mut table = Table::new();
        for (k, v) in vars {
            let Some(rest) = k.as_ref().strip_prefix(ENV_PREFIX) else {
                continue;
            };
            let path: Vec<String> = rest
                .split("__")
                .filter(|s| !s.is_empty())
                .map(str::to_ascii_lowercase)
                .collect();
            if path.is_empty() {
                continue;
            }
            insert_path(&mut table, &path, parse_env_value(v.as_ref()));
        }
        Self {
            name: "env".into(),
            table,
        }
    }
}

fn parse_env_value(raw: &str) -> Value {
    format!("v = {raw}")
        .parse::<Table>()
        .ok()
        .and_then(|mut t| t.remove("v"))
        .unwrap_or_else(|| Value::String(raw.to_string()))
}

fn insert_path(table: &mut Table, path: &[String], value: Value) {
    match path {
        [last] => {
            table.insert(last.clone(), value);
        }
        [head, rest @ ..] => {
            let entry = table
                .entry(head.clone())
                .or_insert_with(|| Value::Table(Table::new()));
            if !entry.is_table() {
                *entry = Value::Table(Table::new());
            }
            if let Value::Table(t) = entry {
                insert_path(t, rest, value);
            }
        }
        [] => {}
    }
}

/// Inputs for `Config::load`. Files are optional; `env` is passed in (not read from the
/// process) so resolution stays deterministic and testable.
#[derive(Debug, Clone)]
pub struct LoadOptions {
    pub profile: Profile,
    pub user_path: Option<PathBuf>,
    pub project_path: Option<PathBuf>,
    pub env: Vec<(String, String)>,
}

impl LoadOptions {
    pub fn new(profile: Profile) -> Self {
        Self {
            profile,
            user_path: None,
            project_path: None,
            env: Vec::new(),
        }
    }

    /// Use the current process environment (`HK_*` vars only).
    pub fn with_process_env(mut self) -> Self {
        self.env = std::env::vars()
            .filter(|(k, _)| k.starts_with(ENV_PREFIX))
            .collect();
        self
    }
}

/// Fully resolved config tree.
#[derive(Debug, Clone)]
pub struct Config {
    tree: Table,
    applied: Vec<String>,
}

impl Config {
    /// Built-in defaults layer.
    pub fn defaults_layer() -> Layer {
        Layer::from_toml("defaults", BASE_DEFAULTS).expect("builtin defaults toml")
    }

    /// Merge `layers` in order (later wins).
    pub fn from_layers(layers: impl IntoIterator<Item = Layer>) -> Self {
        let mut tree = Table::new();
        let mut applied = Vec::new();
        for l in layers {
            merge_table(&mut tree, l.table);
            applied.push(l.name);
        }
        Self { tree, applied }
    }

    /// defaults -> profile -> user -> project -> env.
    pub fn load(opts: &LoadOptions) -> Result<Self, ConfigError> {
        let mut layers = vec![Self::defaults_layer(), opts.profile.layer()];
        if let Some(p) = &opts.user_path {
            layers.extend(Layer::from_file("user", p)?);
        }
        if let Some(p) = &opts.project_path {
            layers.extend(Layer::from_file("project", p)?);
        }
        layers.push(Layer::from_env(opts.env.iter().map(|(k, v)| (k, v))));
        Ok(Self::from_layers(layers))
    }

    pub fn tree(&self) -> &Table {
        &self.tree
    }

    /// Names of layers merged, in order.
    pub fn applied_layers(&self) -> &[String] {
        &self.applied
    }

    /// Dotted-path lookup (`"mcp.allow_stdio"`).
    pub fn get(&self, path: &str) -> Option<&Value> {
        let mut parts = path.split('.');
        let mut cur = self.tree.get(parts.next()?)?;
        for p in parts {
            cur = cur.as_table()?.get(p)?;
        }
        Some(cur)
    }

    pub fn get_bool(&self, path: &str) -> Option<bool> {
        self.get(path)?.as_bool()
    }

    pub fn get_str(&self, path: &str) -> Option<&str> {
        self.get(path)?.as_str()
    }

    pub fn get_int(&self, path: &str) -> Option<i64> {
        self.get(path)?.as_integer()
    }

    /// Id-keyed array entries that are not disabled (`enabled = false`).
    pub fn entries(&self, path: &str) -> Vec<&Table> {
        self.get(path)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_table)
                    .filter(|t| t.get("enabled").and_then(Value::as_bool) != Some(false))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Resolved tree with secret-looking keys redacted.
    pub fn dump_table(&self) -> Table {
        redact_table(&self.tree)
    }

    /// Resolved tree as TOML, secrets redacted (library side of `dump-config`).
    pub fn dump(&self) -> String {
        toml::to_string(&self.dump_table()).unwrap_or_default()
    }
}

fn is_secret_key(k: &str) -> bool {
    let k = k.to_ascii_lowercase();
    ["key", "token", "secret"].iter().any(|s| k.contains(s))
}

fn redact_table(t: &Table) -> Table {
    t.iter()
        .map(|(k, v)| {
            let v = if is_secret_key(k) {
                Value::String(REDACTED.into())
            } else {
                redact_value(v)
            };
            (k.clone(), v)
        })
        .collect()
}

fn redact_value(v: &Value) -> Value {
    match v {
        Value::Table(t) => Value::Table(redact_table(t)),
        Value::Array(a) => Value::Array(a.iter().map(redact_value).collect()),
        other => other.clone(),
    }
}

fn entry_id(v: &Value) -> Option<&str> {
    v.as_table()?.get("id")?.as_str()
}

fn is_id_array(a: &[Value]) -> bool {
    a.iter().all(|v| entry_id(v).is_some())
}

fn merge_table(base: &mut Table, patch: Table) {
    for (k, pv) in patch {
        match (base.get_mut(&k), pv) {
            (Some(Value::Table(b)), Value::Table(p)) => merge_table(b, p),
            (Some(Value::Array(b)), Value::Array(p))
                if !p.is_empty() && is_id_array(b) && is_id_array(&p) =>
            {
                merge_id_array(b, p)
            }
            (_, pv) => {
                base.insert(k, pv);
            }
        }
    }
}

fn merge_id_array(base: &mut Vec<Value>, patch: Vec<Value>) {
    for pv in patch {
        let Value::Table(mut pt) = pv else { continue };
        let id = pt
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let remove = pt.remove("remove").and_then(|v| v.as_bool()) == Some(true);
        let pos = base.iter().position(|b| entry_id(b) == Some(id.as_str()));
        match (pos, remove) {
            (Some(i), true) => {
                base.remove(i);
            }
            (None, true) => {}
            (Some(i), false) => {
                if let Value::Table(bt) = &mut base[i] {
                    merge_table(bt, pt);
                }
            }
            (None, false) => base.push(Value::Table(pt)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(name: &str, src: &str) -> Layer {
        Layer::from_toml(name, src).unwrap()
    }

    fn ids(c: &Config, path: &str) -> Vec<String> {
        c.get(path)
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .map(|v| entry_id(v).unwrap().to_string())
            .collect()
    }

    #[test]
    fn config_layer_order_later_wins() {
        let c = Config::from_layers([
            l("a", "[agent]\nmax_steps = 1\nmode = \"x\""),
            l("b", "[agent]\nmax_steps = 2"),
            l("c", "[agent]\nmax_steps = 3"),
        ]);
        assert_eq!(c.get_int("agent.max_steps"), Some(3));
        assert_eq!(c.get_str("agent.mode"), Some("x")); // untouched keys survive
        assert_eq!(c.applied_layers(), ["a", "b", "c"]);
    }

    #[test]
    fn config_full_stack_order() {
        let dir = std::env::temp_dir().join(format!("hk-core-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let user = dir.join("user.toml");
        let project = dir.join("project.toml");
        std::fs::write(&user, "[agent]\nmax_steps = 10\nmode = \"user\"").unwrap();
        std::fs::write(&project, "[agent]\nmax_steps = 20").unwrap();
        let mut o = LoadOptions::new(Profile::Cli);
        o.user_path = Some(user);
        o.project_path = Some(project);
        o.env = vec![("HK_AGENT__MAX_PARALLEL_SUBAGENTS".into(), "9".into())];
        let c = Config::load(&o).unwrap();
        assert_eq!(c.get_int("agent.max_steps"), Some(20)); // project > user > defaults
        assert_eq!(c.get_str("agent.mode"), Some("user"));
        assert_eq!(c.get_int("agent.max_parallel_subagents"), Some(9)); // env > defaults
        assert_eq!(
            c.applied_layers(),
            ["defaults", "profile:cli", "user", "project", "env"]
        );
        // missing files are skipped
        o.user_path = Some(dir.join("nope.toml"));
        assert!(Config::load(&o).is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn config_id_patch_merge_append_remove_disable() {
        let c = Config::from_layers([
            l(
                "base",
                "[[p]]\nid=\"a\"\nmodel=\"m1\"\nenabled=true\n[[p]]\nid=\"b\"\nmodel=\"m2\"\n[[p]]\nid=\"c\"",
            ),
            l(
                "patch",
                "[[p]]\nid=\"a\"\nmodel=\"m9\"\n[[p]]\nid=\"b\"\nremove=true\n[[p]]\nid=\"c\"\nenabled=false\n[[p]]\nid=\"d\"",
            ),
        ]);
        assert_eq!(ids(&c, "p"), ["a", "c", "d"]); // b removed, order kept, d appended
        let a = c.entries("p");
        assert_eq!(a.len(), 2); // c disabled
        assert_eq!(a[0]["model"].as_str(), Some("m9"));
        assert_eq!(a[0]["enabled"].as_bool(), Some(true)); // untouched field survives
        assert!(
            c.tree()["p"]
                .as_array()
                .unwrap()
                .iter()
                .all(|v| v.get("remove").is_none())
        );
    }

    #[test]
    fn config_plain_arrays_replace() {
        let c = Config::from_layers([l("a", "x=[1,2,3]"), l("b", "x=[9]")]);
        assert_eq!(c.get("x").unwrap().as_array().unwrap().len(), 1);
    }

    #[test]
    fn config_env_prefix_and_nesting() {
        let env = Layer::from_env([
            ("HK_MCP__ALLOW_STDIO", "false"),
            ("HK_AGENT__MAX_STEPS", "7"),
            ("HK_PROVIDERS__DEEPSEEK__BASE_URL", "http://x"),
            ("OTHER", "ignored"),
        ]);
        let c = Config::from_layers([Config::defaults_layer(), env]);
        assert_eq!(c.get_bool("mcp.allow_stdio"), Some(false));
        assert_eq!(c.get_int("agent.max_steps"), Some(7));
        assert_eq!(c.get_str("providers.deepseek.base_url"), Some("http://x"));
        assert!(c.get("other").is_none());
    }

    #[test]
    fn config_mobile_profile_differs() {
        let desk = Config::load(&LoadOptions::new(Profile::Desktop)).unwrap();
        let mob = Config::load(&LoadOptions::new(Profile::Mobile)).unwrap();
        assert_eq!(desk.get_bool("mcp.allow_stdio"), Some(true));
        assert_eq!(mob.get_bool("mcp.allow_stdio"), Some(false));
        assert_eq!(desk.get_str("sandbox.backend"), Some("auto"));
        assert_eq!(mob.get_str("sandbox.backend"), Some("none"));
        assert_eq!(mob.get_bool("acp.enabled"), Some(false));
        // user layer can still override a profile default
        let mut o = LoadOptions::new(Profile::Mobile);
        o.env = vec![("HK_SANDBOX__BACKEND".into(), "auto".into())];
        assert_eq!(
            Config::load(&o).unwrap().get_str("sandbox.backend"),
            Some("auto")
        );
        assert_eq!("Mobile".parse::<Profile>().unwrap(), Profile::Mobile);
        assert!("nope".parse::<Profile>().is_err());
    }

    #[test]
    fn config_dump_redacts_secrets() {
        let c = Config::from_layers([l(
            "a",
            "[providers.ds]\napi_key=\"sk-123\"\nbase_url=\"u\"\n[[hooks]]\nid=\"h\"\nAuth_Token=\"t\"\nclient_secret=\"s\"",
        )]);
        let d = c.dump();
        assert!(!d.contains("sk-123") && !d.contains("\"t\"") && !d.contains("\"s\""));
        assert!(d.contains("base_url"));
        assert_eq!(d.matches(REDACTED).count(), 3);
        // underlying tree untouched
        assert_eq!(c.get_str("providers.ds.api_key"), Some("sk-123"));
    }

    #[test]
    fn config_bad_toml_names_layer() {
        let e = Layer::from_toml("user", "a = = 1").unwrap_err();
        assert!(e.to_string().contains("user"));
    }
}
