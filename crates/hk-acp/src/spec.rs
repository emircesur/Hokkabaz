//! External engine launch specs: built-in `dsh` / `opencode`, overridable via
//! `[acp.engines.<name>]` (`command`, `args`, `env`) in the layered config.

use std::collections::BTreeMap;

use hk_core::Config;

#[derive(Debug, thiserror::Error)]
pub enum SpecError {
    #[error("acp is disabled (acp.enabled = false)")]
    Disabled,
    #[error("engine `{0}`: {1}")]
    Invalid(String, String),
    #[error("empty engine command")]
    Empty,
}

#[derive(Clone, PartialEq, Eq)]
pub struct EngineSpec {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    /// Extra environment for the child. Values may be secrets: never logged (see `Debug`).
    pub env: BTreeMap<String, String>,
}

impl std::fmt::Debug for EngineSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineSpec")
            .field("name", &self.name)
            .field("command", &self.command)
            .field("args", &self.args)
            .field("env_keys", &self.env.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl EngineSpec {
    pub fn new(name: &str, command: &str, args: &[&str]) -> Self {
        Self {
            name: name.to_owned(),
            command: command.to_owned(),
            args: args.iter().map(|s| (*s).to_owned()).collect(),
            env: BTreeMap::new(),
        }
    }

    pub fn builtin(name: &str) -> Option<Self> {
        match name {
            "dsh" => Some(Self::new("dsh", "dsh", &["--profile", "acp"])),
            "opencode" => Some(Self::new("opencode", "opencode", &["acp"])),
            _ => None,
        }
    }

    /// `acp.engines.<name>` from config, if present. Missing `command` is an error.
    pub fn from_config(cfg: &Config, name: &str) -> Result<Option<Self>, SpecError> {
        let Some(t) = cfg.get(&format!("acp.engines.{name}")).and_then(|v| v.as_table()) else {
            return Ok(None);
        };
        let bad = |m: &str| SpecError::Invalid(name.to_owned(), m.to_owned());
        let command = t
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| bad("missing string `command`"))?
            .to_owned();
        let args = match t.get("args") {
            None => Vec::new(),
            Some(v) => v
                .as_array()
                .ok_or_else(|| bad("`args` must be an array"))?
                .iter()
                .map(|a| a.as_str().map(str::to_owned).ok_or_else(|| bad("`args` must be strings")))
                .collect::<Result<_, _>>()?,
        };
        let env = match t.get("env") {
            None => BTreeMap::new(),
            Some(v) => v
                .as_table()
                .ok_or_else(|| bad("`env` must be a table"))?
                .iter()
                .map(|(k, v)| {
                    v.as_str()
                        .map(|s| (k.clone(), s.to_owned()))
                        .ok_or_else(|| bad("`env` values must be strings"))
                })
                .collect::<Result<_, _>>()?,
        };
        Ok(Some(Self {
            name: name.to_owned(),
            command,
            args,
            env,
        }))
    }

    /// Config entry, else built-in name, else `arg` is treated as a command line.
    pub fn resolve(cfg: Option<&Config>, arg: &str) -> Result<Self, SpecError> {
        if let Some(c) = cfg {
            if c.get_bool("acp.enabled") == Some(false) {
                return Err(SpecError::Disabled);
            }
            if let Some(s) = Self::from_config(c, arg)? {
                return Ok(s);
            }
        }
        if let Some(s) = Self::builtin(arg) {
            return Ok(s);
        }
        Self::from_command_line(arg)
    }

    pub fn from_command_line(line: &str) -> Result<Self, SpecError> {
        let mut words = split_words(line).into_iter();
        let command = words.next().ok_or(SpecError::Empty)?;
        Ok(Self {
            name: command.clone(),
            command,
            args: words.collect(),
            env: BTreeMap::new(),
        })
    }
}

/// Whitespace split honoring single/double quotes (no escapes: enough for `--engine "cmd 'a b'"`).
fn split_words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut have = false;
    for c in s.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                have = true;
            }
            (None, c) if c.is_whitespace() => {
                if have || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    have = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if have || !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use hk_core::{Config as C, Layer};

    fn cfg(extra: &str) -> Config {
        C::from_layers([C::defaults_layer(), Layer::from_toml("t", extra).unwrap()])
    }

    #[test]
    fn builtin_and_command_line() {
        let s = EngineSpec::resolve(None, "dsh").unwrap();
        assert_eq!((s.command.as_str(), s.args.as_slice()), ("dsh", &["--profile".to_owned(), "acp".to_owned()][..]));
        let s = EngineSpec::resolve(None, "mycmd --flag 'a b'").unwrap();
        assert_eq!(s.args, ["--flag", "a b"]);
        assert!(matches!(EngineSpec::resolve(None, "  "), Err(SpecError::Empty)));
    }

    #[test]
    fn config_overrides_builtin() {
        let c = cfg("[acp.engines.dsh]\ncommand = \"/opt/dsh\"\nargs = [\"--profile\", \"acp\", \"-v\"]\n[acp.engines.dsh.env]\nFOO = \"bar\"\n");
        let s = EngineSpec::resolve(Some(&c), "dsh").unwrap();
        assert_eq!(s.command, "/opt/dsh");
        assert_eq!(s.args.len(), 3);
        assert_eq!(s.env["FOO"], "bar");
        assert!(!format!("{s:?}").contains("bar"));
    }

    #[test]
    fn invalid_and_disabled() {
        let c = cfg("[acp.engines.x]\nargs = []\n");
        assert!(matches!(EngineSpec::resolve(Some(&c), "x"), Err(SpecError::Invalid(..))));
        let c = cfg("[acp]\nenabled = false\n");
        assert!(matches!(EngineSpec::resolve(Some(&c), "dsh"), Err(SpecError::Disabled)));
    }
}
