//! Templates (D19): registry items that install a flow, agent, prompt, studio project, settings
//! preset, or a bundle of other templates. They share the plugin registry (D13): same
//! `index.json` (under `templates`), same id/version rules, same sha256-verified `.tar.gz`.
//!
//! A package holds `template.toml` plus payload files:
//!
//! ```toml
//! id = "dev.hokkabaz.webhook-to-telegram"
//! name = "Webhook to Telegram"
//! version = "1.0.0"
//! kind = "flow"                      # flow | agent | prompt | studio | settings_preset | bundle
//! payload = ["flow.json"]
//!
//! [[inputs]]
//! key = "bot"
//! label = "Telegram bot"
//! kind = "credential"                # credential | model | string | number | bool | select | file
//! credential_type = "telegramApi"
//! required = true
//! ```
//!
//! Payload strings may contain `{{inputs.<key>}}`. A string that is exactly one placeholder takes
//! the input's typed value; otherwise the value is spliced in as text. Other `{{ … }}` (n8n
//! expressions) are left alone. Credential inputs carry a credential id, never the secret.

use std::collections::{BTreeMap, HashSet};
use std::io::Read;
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::{PluginError, Result};
use crate::manifest::valid_id;
use crate::registry::{TemplateEntry, is_newer, parse_version};

pub const MANIFEST_FILE: &str = "template.toml";
const MAX_PACKAGE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_FILES: usize = 256;

fn bad<T>(msg: impl Into<String>) -> Result<T> {
    Err(PluginError::Template(msg.into()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TemplateKind {
    Flow,
    Agent,
    Prompt,
    Studio,
    SettingsPreset,
    Bundle,
}

/// What an input asks for; drives the install wizard's widget and value checks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InputKind {
    /// Value: a stored credential's id of this n8n credential type.
    Credential {
        credential_type: String,
    },
    /// Value: a model id; the picker filters on these capabilities (`tools`, `vision`, …).
    Model {
        #[serde(default)]
        capabilities: Vec<String>,
    },
    String,
    Number,
    Bool,
    Select {
        options: Vec<String>,
    },
    /// Value: a path picked by the user.
    File,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemplateInput {
    pub key: String,
    pub label: String,
    #[serde(flatten)]
    pub kind: InputKind,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    #[serde(default)]
    pub help: String,
}

impl TemplateInput {
    fn check_value(&self, v: &Value) -> Result<()> {
        let ok = match &self.kind {
            InputKind::Credential { .. } | InputKind::Model { .. } => {
                v.as_str().is_some_and(|s| !s.trim().is_empty())
            }
            InputKind::String | InputKind::File => v.is_string(),
            InputKind::Number => v.is_number(),
            InputKind::Bool => v.is_boolean(),
            InputKind::Select { options } => {
                v.as_str().is_some_and(|s| options.iter().any(|o| o == s))
            }
        };
        if ok {
            Ok(())
        } else {
            bad(format!(
                "input `{}`: value {v} does not fit its kind",
                self.key
            ))
        }
    }
}

/// Another registry template pulled in by a bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleItem {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemplateManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub kind: TemplateKind,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub author: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_hokkabaz_version: Option<String>,
    #[serde(default)]
    pub inputs: Vec<TemplateInput>,
    /// Package-relative payload files. Empty for bundles.
    #[serde(default)]
    pub payload: Vec<String>,
    /// Bundles only. Inputs declared on the bundle are offered to every item.
    #[serde(default)]
    pub bundle_items: Vec<BundleItem>,
}

impl TemplateManifest {
    /// Parse and validate `template.toml` (no payload checks; see [`TemplatePackage`]).
    pub fn parse(text: &str) -> Result<Self> {
        let m: Self = toml::from_str(text).map_err(|e| PluginError::Template(e.to_string()))?;
        m.validate()?;
        Ok(m)
    }

    pub fn validate(&self) -> Result<()> {
        if !valid_id(&self.id) {
            return bad("`id` must be 1-100 chars of [a-z0-9._-] and not start with '.' or '-'");
        }
        if self.name.trim().is_empty() {
            return bad("`name` is required");
        }
        if parse_version(&self.version).is_none() {
            return bad("`version` must be MAJOR.MINOR.PATCH");
        }
        if let Some(v) = &self.min_hokkabaz_version
            && parse_version(v).is_none()
        {
            return bad("`min_hokkabaz_version` must be MAJOR.MINOR.PATCH");
        }
        let mut keys = HashSet::new();
        for i in &self.inputs {
            if i.key.is_empty()
                || !i.key.starts_with(|c: char| c.is_ascii_alphabetic())
                || !i.key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                return bad(format!(
                    "input key `{}` must match [A-Za-z][A-Za-z0-9_]*",
                    i.key
                ));
            }
            if !keys.insert(i.key.as_str()) {
                return bad(format!("duplicate input key `{}`", i.key));
            }
            if i.label.trim().is_empty() {
                return bad(format!("input `{}` needs a label", i.key));
            }
            match &i.kind {
                InputKind::Credential { credential_type } if credential_type.trim().is_empty() => {
                    return bad(format!("input `{}`: empty credential_type", i.key));
                }
                InputKind::Select { options } => {
                    let uniq: HashSet<_> = options.iter().collect();
                    if options.is_empty() || uniq.len() != options.len() {
                        return bad(format!("input `{}`: select needs unique options", i.key));
                    }
                }
                _ => {}
            }
            if let Some(d) = &i.default {
                i.check_value(d)?;
            }
        }
        if self.kind == TemplateKind::Bundle {
            if self.bundle_items.is_empty() || !self.payload.is_empty() {
                return bad("a bundle lists `bundle_items` and has no `payload`");
            }
            let mut ids = HashSet::new();
            for b in &self.bundle_items {
                if !valid_id(&b.id) || b.id == self.id || !ids.insert(b.id.as_str()) {
                    return bad(format!("bad or duplicate bundle item `{}`", b.id));
                }
                if b.min_version
                    .as_deref()
                    .is_some_and(|v| parse_version(v).is_none())
                {
                    return bad(format!("bundle item `{}`: bad min_version", b.id));
                }
            }
        } else {
            if self.payload.is_empty() || !self.bundle_items.is_empty() {
                return bad("non-bundle templates list `payload` files and no `bundle_items`");
            }
            let json_only = !matches!(self.kind, TemplateKind::Prompt);
            let mut seen = HashSet::new();
            for p in &self.payload {
                if !safe_rel_path(p) || !seen.insert(p.as_str()) || p == MANIFEST_FILE {
                    return bad(format!("bad or duplicate payload path `{p}`"));
                }
                if json_only && !p.ends_with(".json") {
                    return bad(format!("payload `{p}` must be a .json file"));
                }
            }
        }
        Ok(())
    }

    /// Host `version` meets `min_hokkabaz_version`.
    pub fn supports_host(&self, version: &str) -> bool {
        self.min_hokkabaz_version
            .as_deref()
            .is_none_or(|min| !is_newer(min, version))
    }

    fn input(&self, key: &str) -> Option<&TemplateInput> {
        self.inputs.iter().find(|i| i.key == key)
    }

    /// Final input values: provided, else default, else `null` for optional inputs.
    /// Missing required inputs, undeclared keys and ill-typed values are errors.
    pub fn resolve_inputs(&self, values: &Map<String, Value>) -> Result<BTreeMap<String, Value>> {
        if let Some(k) = values.keys().find(|k| self.input(k).is_none()) {
            return bad(format!("unknown input `{k}`"));
        }
        let mut out = BTreeMap::new();
        for i in &self.inputs {
            let v = match values
                .get(&i.key)
                .filter(|v| !v.is_null())
                .or(i.default.as_ref())
            {
                Some(v) => {
                    i.check_value(v)?;
                    v.clone()
                }
                None if i.required => return bad(format!("missing required input `{}`", i.key)),
                None => Value::Null,
            };
            out.insert(i.key.clone(), v);
        }
        Ok(out)
    }
}

/// Package path: relative, `/`-separated, no `..`.
fn safe_rel_path(p: &str) -> bool {
    let path = Path::new(p);
    !p.is_empty()
        && !p.contains('\\')
        && path.components().all(|c| matches!(c, Component::Normal(_)))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum PayloadContent {
    Json(Value),
    Text(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderedPayload {
    pub path: String,
    pub content: PayloadContent,
}

/// A template with inputs applied, ready for the installer of its kind.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstantiatedTemplate {
    pub id: String,
    pub version: String,
    pub kind: TemplateKind,
    pub inputs: BTreeMap<String, Value>,
    pub payload: Vec<RenderedPayload>,
    /// Bundles: items the caller resolves from the index and renders with `inputs`.
    pub bundle_items: Vec<BundleItem>,
}

impl InstantiatedTemplate {
    /// Flow payloads as n8n workflows.
    pub fn workflows(&self) -> Result<Vec<hk_flow::Workflow>> {
        self.payload
            .iter()
            .filter_map(|p| match &p.content {
                PayloadContent::Json(v) if self.kind == TemplateKind::Flow => Some((p, v)),
                _ => None,
            })
            .map(|(p, v)| parse_workflow(&p.path, v.clone()))
            .collect()
    }
}

fn parse_workflow(path: &str, v: Value) -> Result<hk_flow::Workflow> {
    let wf = hk_flow::Workflow::from_json_value(v)
        .and_then(|w| w.validate().map(|()| w))
        .map_err(|e| PluginError::Template(format!("`{path}` is not a valid n8n workflow: {e}")))?;
    Ok(wf)
}

/// A validated template package: manifest + files by package path.
#[derive(Debug, Clone, PartialEq)]
pub struct TemplatePackage {
    pub manifest: TemplateManifest,
    pub files: BTreeMap<String, Vec<u8>>,
}

impl TemplatePackage {
    pub fn from_files(files: BTreeMap<String, Vec<u8>>) -> Result<Self> {
        let Some(raw) = files.get(MANIFEST_FILE) else {
            return bad(format!("package has no {MANIFEST_FILE}"));
        };
        let text = std::str::from_utf8(raw)
            .map_err(|_| PluginError::Template(format!("{MANIFEST_FILE} is not utf-8")))?;
        let manifest = TemplateManifest::parse(text)?;
        let pkg = Self { manifest, files };
        pkg.validate_payload()?;
        Ok(pkg)
    }

    /// Load an unpacked package directory (recursive).
    pub fn from_dir(dir: &Path) -> Result<Self> {
        fn walk(
            root: &Path,
            dir: &Path,
            out: &mut BTreeMap<String, Vec<u8>>,
            total: &mut u64,
        ) -> Result<()> {
            for e in std::fs::read_dir(dir)? {
                let e = e?;
                let ft = e.file_type()?;
                let path = e.path();
                if ft.is_dir() {
                    walk(root, &path, out, total)?;
                } else if ft.is_file() {
                    let rel = path
                        .strip_prefix(root)
                        .map_err(|_| PluginError::Template("path outside package".into()))?;
                    let rel = rel
                        .components()
                        .map(|c| c.as_os_str().to_string_lossy())
                        .collect::<Vec<_>>()
                        .join("/");
                    let bytes = std::fs::read(&path)?;
                    *total += bytes.len() as u64;
                    if *total > MAX_PACKAGE_BYTES || out.len() >= MAX_FILES {
                        return bad("package is too large");
                    }
                    out.insert(rel, bytes);
                }
            }
            Ok(())
        }
        let mut files = BTreeMap::new();
        walk(dir, dir, &mut files, &mut 0)?;
        Self::from_files(files)
    }

    /// Load a `.tar.gz` package. Only regular files with safe relative paths are accepted.
    pub fn from_archive(bytes: &[u8]) -> Result<Self> {
        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
        let mut files = BTreeMap::new();
        let mut total = 0u64;
        for entry in tar.entries()? {
            let mut entry = entry?;
            let kind = entry.header().entry_type();
            if kind.is_dir() {
                continue;
            }
            let path = entry.path()?.to_string_lossy().into_owned();
            let path = path.trim_start_matches("./").to_string();
            if !kind.is_file() || !safe_rel_path(&path) {
                return bad(format!(
                    "archive entry `{path}` is not a plain relative file"
                ));
            }
            total += entry.size();
            if total > MAX_PACKAGE_BYTES || files.len() >= MAX_FILES {
                return bad("package is too large");
            }
            let mut buf = Vec::with_capacity(entry.size() as usize);
            entry.read_to_end(&mut buf)?;
            files.insert(path, buf);
        }
        Self::from_files(files)
    }

    /// Package agrees with the index entry it was downloaded for.
    pub fn check_entry(&self, entry: &TemplateEntry) -> Result<()> {
        let m = &self.manifest;
        if m.id != entry.id || m.version != entry.version || m.kind != entry.kind {
            return bad(format!(
                "package {}@{} ({:?}) does not match index entry {}@{} ({:?})",
                m.id, m.version, m.kind, entry.id, entry.version, entry.kind
            ));
        }
        Ok(())
    }

    fn load(&self, path: &str) -> Result<PayloadContent> {
        let Some(raw) = self.files.get(path) else {
            return bad(format!("payload `{path}` is missing from the package"));
        };
        let text = std::str::from_utf8(raw)
            .map_err(|_| PluginError::Template(format!("payload `{path}` is not utf-8")))?;
        if path.ends_with(".json") {
            serde_json::from_str(text)
                .map(PayloadContent::Json)
                .map_err(|e| PluginError::Template(format!("payload `{path}`: {e}")))
        } else {
            Ok(PayloadContent::Text(text.to_string()))
        }
    }

    fn validate_payload(&self) -> Result<()> {
        for path in &self.manifest.payload {
            let content = self.load(path)?;
            check_kind(self.manifest.kind, path, &content)?;
            let mut refs = Vec::new();
            match &content {
                PayloadContent::Json(v) => collect_refs(v, &mut refs),
                PayloadContent::Text(t) => refs.extend(placeholders(t).map(|(_, k)| k.to_string())),
            }
            if let Some(k) = refs.iter().find(|k| self.manifest.input(k).is_none()) {
                return bad(format!("payload `{path}` uses undeclared input `{k}`"));
            }
        }
        Ok(())
    }

    /// Apply input values to every payload file. See the module docs for substitution rules.
    pub fn render(&self, values: &Map<String, Value>) -> Result<InstantiatedTemplate> {
        let m = &self.manifest;
        let inputs = m.resolve_inputs(values)?;
        let mut payload = Vec::with_capacity(m.payload.len());
        for path in &m.payload {
            let content = match self.load(path)? {
                PayloadContent::Json(v) => PayloadContent::Json(substitute_value(v, &inputs)),
                PayloadContent::Text(t) => PayloadContent::Text(substitute_text(&t, &inputs)),
            };
            // Typed substitution can still break shape (e.g. a node name from an input).
            check_kind(m.kind, path, &content)?;
            payload.push(RenderedPayload {
                path: path.clone(),
                content,
            });
        }
        Ok(InstantiatedTemplate {
            id: m.id.clone(),
            version: m.version.clone(),
            kind: m.kind,
            inputs,
            payload,
            bundle_items: m.bundle_items.clone(),
        })
    }
}

/// Render convenience matching the install wizard's call shape.
pub fn render(pkg: &TemplatePackage, values: &Map<String, Value>) -> Result<InstantiatedTemplate> {
    pkg.render(values)
}

fn check_kind(kind: TemplateKind, path: &str, content: &PayloadContent) -> Result<()> {
    match (kind, content) {
        (TemplateKind::Flow, PayloadContent::Json(v)) => parse_workflow(path, v.clone()).map(drop),
        (TemplateKind::Agent, PayloadContent::Json(v)) => {
            if v.get("name")
                .and_then(Value::as_str)
                .is_some_and(|n| !n.trim().is_empty())
            {
                Ok(())
            } else {
                bad(format!("agent payload `{path}` needs a non-empty `name`"))
            }
        }
        (TemplateKind::Studio | TemplateKind::SettingsPreset, PayloadContent::Json(v)) => {
            if v.is_object() {
                Ok(())
            } else {
                bad(format!("payload `{path}` must be a json object"))
            }
        }
        (TemplateKind::Prompt, _) => Ok(()),
        _ => bad(format!(
            "payload `{path}` has the wrong format for {kind:?}"
        )),
    }
}

/// `{{inputs.key}}` / `{{ inputs.key }}` occurrences: (byte range, key).
fn placeholders(s: &str) -> impl Iterator<Item = (std::ops::Range<usize>, &str)> {
    let mut pos = 0;
    std::iter::from_fn(move || {
        while let Some(off) = s[pos..].find("{{") {
            let start = pos + off;
            let Some(len) = s[start + 2..].find("}}") else {
                pos = s.len();
                return None;
            };
            let end = start + 2 + len + 2;
            let inner = s[start + 2..end - 2].trim();
            if let Some(key) = inner.strip_prefix("inputs.")
                && !key.is_empty()
                && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                pos = end;
                return Some((start..end, key));
            }
            pos = start + 2;
        }
        None
    })
}

fn collect_refs(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) => out.extend(placeholders(s).map(|(_, k)| k.to_string())),
        Value::Array(a) => a.iter().for_each(|v| collect_refs(v, out)),
        Value::Object(o) => o.values().for_each(|v| collect_refs(v, out)),
        _ => {}
    }
}

fn as_text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn substitute_text(s: &str, inputs: &BTreeMap<String, Value>) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last = 0;
    for (r, key) in placeholders(s) {
        out.push_str(&s[last..r.start]);
        out.push_str(&inputs.get(key).map(as_text).unwrap_or_default());
        last = r.end;
    }
    out.push_str(&s[last..]);
    out
}

fn substitute_value(v: Value, inputs: &BTreeMap<String, Value>) -> Value {
    match v {
        Value::String(s) => {
            let mut it = placeholders(&s);
            if let (Some((r, key)), None) = (it.next(), it.next())
                && r == (0..s.len())
            {
                return inputs.get(key).cloned().unwrap_or(Value::Null);
            }
            Value::String(substitute_text(&s, inputs))
        }
        Value::Array(a) => {
            Value::Array(a.into_iter().map(|v| substitute_value(v, inputs)).collect())
        }
        Value::Object(o) => Value::Object(
            o.into_iter()
                .map(|(k, v)| (k, substitute_value(v, inputs)))
                .collect(),
        ),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const FLOW_TOML: &str = r#"
id = "dev.test.hook"
name = "Hook"
version = "1.0.0"
kind = "flow"
min_hokkabaz_version = "0.2.0"
payload = ["flow.json"]

[[inputs]]
key = "bot"
label = "Bot"
kind = "credential"
credential_type = "telegramApi"
required = true

[[inputs]]
key = "chat"
label = "Chat id"
kind = "string"
required = true

[[inputs]]
key = "retries"
label = "Retries"
kind = "number"
default = 3

[[inputs]]
key = "tone"
label = "Tone"
kind = "select"
options = ["short", "long"]
"#;

    fn flow_json() -> Value {
        json!({
            "name": "Hook",
            "nodes": [
                {"name": "Webhook", "type": "n8n-nodes-base.webhook", "typeVersion": 2,
                 "position": [0, 0], "parameters": {"path": "hook"}},
                {"name": "Send", "type": "n8n-nodes-base.telegram", "typeVersion": 1.2,
                 "position": [200, 0],
                 "parameters": {"chatId": "{{inputs.chat}}", "text": "={{ $json.body.msg }} ({{ inputs.tone }})",
                                "maxTries": "{{inputs.retries}}"},
                 "credentials": {"telegramApi": {"id": "{{inputs.bot}}", "name": "bot"}}}
            ],
            "connections": {"Webhook": {"main": [[{"node": "Send", "type": "main", "index": 0}]]}}
        })
    }

    fn pkg(toml: &str, payload: &[(&str, String)]) -> Result<TemplatePackage> {
        let mut files = BTreeMap::new();
        files.insert(MANIFEST_FILE.to_string(), toml.as_bytes().to_vec());
        for (p, c) in payload {
            files.insert(p.to_string(), c.as_bytes().to_vec());
        }
        TemplatePackage::from_files(files)
    }

    fn flow_pkg() -> TemplatePackage {
        pkg(FLOW_TOML, &[("flow.json", flow_json().to_string())]).unwrap()
    }

    fn vals(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn manifest_validation() {
        let m = TemplateManifest::parse(FLOW_TOML).unwrap();
        assert_eq!(
            m.inputs[0].kind,
            InputKind::Credential {
                credential_type: "telegramApi".into()
            }
        );
        assert!(m.supports_host("0.2.0") && !m.supports_host("0.1.9"));
        let cases = [
            (FLOW_TOML.replace("1.0.0", "1.0"), "short version"),
            (FLOW_TOML.replace("0.2.0", "x"), "bad min version"),
            (
                FLOW_TOML.replace("key = \"chat\"", "key = \"bot\""),
                "duplicate key",
            ),
            (
                FLOW_TOML.replace("key = \"chat\"", "key = \"9chat\""),
                "bad key",
            ),
            (
                FLOW_TOML.replace("default = 3", "default = \"3\""),
                "ill-typed default",
            ),
            (
                FLOW_TOML.replace("[\"short\", \"long\"]", "[]"),
                "empty select",
            ),
            (
                FLOW_TOML.replace("flow.json", "flow.yaml"),
                "flow payload must be json",
            ),
            (
                FLOW_TOML.replace("flow.json", "../flow.json"),
                "path escape",
            ),
            (
                FLOW_TOML.replace("kind = \"flow\"", "kind = \"bundle\""),
                "bundle with payload",
            ),
            (
                FLOW_TOML.replace("kind = \"flow\"", "kind = \"nope\""),
                "unknown kind",
            ),
            (
                FLOW_TOML.replace("payload =", "surprise = 1\npayload ="),
                "unknown field",
            ),
        ];
        for (text, why) in cases {
            assert!(TemplateManifest::parse(&text).is_err(), "{why}");
        }
        let bundle = r#"id = "dev.test.b"
name = "B"
version = "0.1.0"
kind = "bundle"
bundle_items = [{ id = "dev.test.hook", min_version = "1.0.0" }, { id = "dev.test.agent" }]"#;
        assert_eq!(
            TemplateManifest::parse(bundle).unwrap().bundle_items.len(),
            2
        );
        assert!(
            TemplateManifest::parse(&bundle.replace("dev.test.agent", "dev.test.hook")).is_err()
        );
    }

    #[test]
    fn package_validation() {
        flow_pkg();
        assert!(pkg(FLOW_TOML, &[]).is_err(), "payload missing");
        assert!(
            pkg(FLOW_TOML, &[("flow.json", "{\"connections\":{}}".into())]).is_err(),
            "not a workflow"
        );
        let undeclared = flow_json()
            .to_string()
            .replace("inputs.chat", "inputs.room");
        assert!(
            pkg(FLOW_TOML, &[("flow.json", undeclared)]).is_err(),
            "undeclared placeholder"
        );
        let agent = FLOW_TOML
            .replace("kind = \"flow\"", "kind = \"agent\"")
            .replace("flow.json", "a.json");
        assert!(
            pkg(&agent, &[("a.json", "{\"model\":\"x\"}".into())]).is_err(),
            "agent without name"
        );
        assert!(pkg(&agent, &[("a.json", "{\"name\":\"x\"}".into())]).is_ok());
    }

    #[test]
    fn render_substitutes_typed_and_text() {
        let out = flow_pkg()
            .render(&vals(
                json!({"bot": "cred_1", "chat": "42", "tone": "short"}),
            ))
            .unwrap();
        let PayloadContent::Json(v) = &out.payload[0].content else {
            panic!()
        };
        let send = &v["nodes"][1];
        assert_eq!(send["parameters"]["chatId"], "42");
        assert_eq!(send["parameters"]["maxTries"], 3, "default, typed");
        assert_eq!(
            send["parameters"]["text"], "={{ $json.body.msg }} (short)",
            "n8n expr untouched"
        );
        assert_eq!(send["credentials"]["telegramApi"]["id"], "cred_1");
        assert_eq!(out.workflows().unwrap().len(), 1);
        assert_eq!(out.inputs["tone"], "short");
    }

    #[test]
    fn render_rejects_missing_extra_and_ill_typed_inputs() {
        let p = flow_pkg();
        let err = p
            .render(&vals(json!({"bot": "cred_1"})))
            .unwrap_err()
            .to_string();
        assert!(err.contains("missing required input `chat`"), "{err}");
        let err = p
            .render(&vals(json!({"bot": "c", "chat": "1", "extra": 1})))
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown input `extra`"), "{err}");
        assert!(
            p.render(&vals(json!({"bot": "c", "chat": 1}))).is_err(),
            "number for string"
        );
        assert!(
            p.render(&vals(json!({"bot": "c", "chat": "1", "tone": "loud"})))
                .is_err(),
            "select"
        );
        assert!(
            p.render(&vals(json!({"bot": "", "chat": "1"}))).is_err(),
            "empty credential"
        );
        // Optional input left out renders as empty text / null.
        let out = p.render(&vals(json!({"bot": "c", "chat": "1"}))).unwrap();
        let PayloadContent::Json(v) = &out.payload[0].content else {
            panic!()
        };
        assert_eq!(
            v["nodes"][1]["parameters"]["text"],
            "={{ $json.body.msg }} ()"
        );
    }

    #[test]
    fn prompt_text_payload() {
        let toml = r#"id = "dev.test.p"
name = "P"
version = "1.0.0"
kind = "prompt"
payload = ["prompt.md"]
[[inputs]]
key = "lang"
label = "Language"
kind = "string"
default = "Go"
"#;
        let p = pkg(
            toml,
            &[(
                "prompt.md",
                "Review this {{inputs.lang}} code. {{ $keep }}".into(),
            )],
        )
        .unwrap();
        let out = p.render(&Map::new()).unwrap();
        assert_eq!(
            out.payload[0].content,
            PayloadContent::Text("Review this Go code. {{ $keep }}".into())
        );
    }

    #[test]
    fn archive_round_trip_and_index_entry() {
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        {
            let mut tar = tar::Builder::new(&mut gz);
            for (name, data) in [
                (MANIFEST_FILE, FLOW_TOML.to_string()),
                ("flow.json", flow_json().to_string()),
            ] {
                let mut h = tar::Header::new_gnu();
                h.set_size(data.len() as u64);
                h.set_mode(0o644);
                h.set_cksum();
                tar.append_data(&mut h, name, data.as_bytes()).unwrap();
            }
            tar.finish().unwrap();
        }
        let bytes = gz.finish().unwrap();
        let entry = TemplateEntry::for_archive(&bytes, "https://example.com/t.tar.gz").unwrap();
        assert_eq!(
            (entry.kind, entry.size),
            (TemplateKind::Flow, bytes.len() as u64)
        );
        crate::registry::verify_template(&entry, &bytes).unwrap();
        TemplatePackage::from_archive(&bytes)
            .unwrap()
            .check_entry(&entry)
            .unwrap();
        let idx = format!(
            r#"{{"version":1,"plugins":[],"templates":[{}]}}"#,
            serde_json::to_string(&entry).unwrap()
        );
        let idx = crate::registry::RegistryIndex::parse(&idx).unwrap();
        assert_eq!(
            idx.find_template("dev.test.hook").unwrap().kind,
            TemplateKind::Flow
        );
    }

    #[test]
    fn registry_template_examples_are_valid() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../registry-template/templates");
        let mut n = 0;
        for e in std::fs::read_dir(&dir).unwrap() {
            let p = TemplatePackage::from_dir(&e.unwrap().path()).unwrap();
            let required: Map<String, Value> = p
                .manifest
                .inputs
                .iter()
                .filter(|i| i.required && i.default.is_none())
                .map(|i| {
                    let v = match &i.kind {
                        InputKind::Number => json!(1),
                        InputKind::Bool => json!(true),
                        InputKind::Select { options } => json!(options[0]),
                        _ => json!("x"),
                    };
                    (i.key.clone(), v)
                })
                .collect();
            p.render(&required).unwrap();
            n += 1;
        }
        assert!(n >= 2);
    }
}
