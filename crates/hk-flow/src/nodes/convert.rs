//! Format nodes: HTML Extract, Markdown, XML, Crypto, Execute Command.
//!
//! Clean-room from the public n8n docs. Not implemented: HTML templates / tables, HTML to
//! Markdown, Crypto sign, XML builder options beyond the common ones.

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use serde_json::{Map, Value, json};

use super::util::*;
use crate::description::NodeTypeDescription;
use crate::error::FlowError;
use crate::item::Item;
use crate::node::{FlowNode, NodeContext, NodeOutput};

macro_rules! node_struct {
    ($name:ident) => {
        #[derive(Debug)]
        pub struct $name(NodeTypeDescription);
        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }
    };
}

/// Source text for a node reading `sourceData` (json property or binary field).
fn source_text(
    ctx: &NodeContext,
    item: &Item,
    i: usize,
    prop: &str,
    binary: bool,
) -> Result<String, FlowError> {
    if binary {
        let b = item
            .binary
            .as_ref()
            .and_then(|m| m.get(prop))
            .ok_or_else(|| node_err(ctx, format!("Item {i} has no binary field '{prop}'")))?;
        let bytes = B64
            .decode(b.data.as_bytes())
            .map_err(|e| node_err(ctx, format!("Binary data is not valid base64: {e}")))?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    } else {
        get_path(&item.json, prop, true)
            .map(to_display)
            .ok_or_else(|| {
                node_err(
                    ctx,
                    format!("No property named '{prop}' exists in item {i}"),
                )
            })
    }
}

// ----------------------------------------------------------------------- HTML

node_struct!(HtmlNode);

impl HtmlNode {
    pub fn new() -> Self {
        Self(describe(json!({
            "displayName": "HTML",
            "name": "n8n-nodes-base.html",
            "version": [1, 1.1, 1.2],
            "defaultVersion": 1.2,
            "description": "Work with HTML",
            "group": ["transform"],
            "defaults": {"name": "HTML", "color": "#FF9922"},
            "inputs": ["main"],
            "outputs": ["main"],
            "properties": [
                {"displayName": "Operation", "name": "operation", "type": "options", "default": "extractHtmlContent",
                 "options": [{"name": "Extract HTML Content", "value": "extractHtmlContent"}]},
                {"displayName": "Source Data", "name": "sourceData", "type": "options", "default": "json",
                 "options": [{"name": "Binary", "value": "binary"}, {"name": "JSON", "value": "json"}]},
                {"displayName": "Input Binary Field", "name": "dataPropertyName", "type": "string", "default": "data",
                 "displayOptions": {"show": {"sourceData": ["binary"]}}},
                {"displayName": "JSON Property", "name": "dataPropertyName", "type": "string", "default": "data",
                 "displayOptions": {"show": {"sourceData": ["json"]}}},
                {"displayName": "Extraction Values", "name": "extractionValues", "type": "fixedCollection", "default": {},
                 "typeOptions": {"multipleValues": true},
                 "options": [{"displayName": "Values", "name": "values", "values": [
                    {"displayName": "Key", "name": "key", "type": "string", "default": ""},
                    {"displayName": "CSS Selector", "name": "cssSelector", "type": "string", "default": ""},
                    {"displayName": "Return Value", "name": "returnValue", "type": "options", "default": "text",
                     "options": [{"name": "Attribute", "value": "attribute"}, {"name": "HTML", "value": "html"},
                                 {"name": "Text", "value": "text"}, {"name": "Value", "value": "value"}]},
                    {"displayName": "Attribute", "name": "attribute", "type": "string", "default": "",
                     "displayOptions": {"show": {"returnValue": ["attribute"]}}},
                    {"displayName": "Skip Selectors", "name": "skipSelectors", "type": "string", "default": "",
                     "displayOptions": {"show": {"returnValue": ["text"]}}},
                    {"displayName": "Return Array", "name": "returnArray", "type": "boolean", "default": false}]}]},
                {"displayName": "Options", "name": "options", "type": "collection", "default": {}, "placeholder": "Add option",
                 "options": [
                    {"displayName": "Trim Values", "name": "trimValues", "type": "boolean", "default": true},
                    {"displayName": "Clean Up Text", "name": "cleanUpText", "type": "boolean", "default": true}]}
            ]
        })))
    }
}

fn clean_text(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[async_trait]
impl FlowNode for HtmlNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }
    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        use scraper::{Html, Selector};
        let op = param_str(ctx, "operation", 0, "extractHtmlContent")?;
        if op != "extractHtmlContent" {
            return Err(node_err(
                ctx,
                format!("Operation '{op}' is not supported; only Extract HTML Content is"),
            ));
        }
        let mut out = Vec::new();
        for (i, item) in port(ctx, 0).iter().enumerate() {
            let binary = param_str(ctx, "sourceData", i, "json")? == "binary";
            let prop = param_str(ctx, "dataPropertyName", i, "data")?;
            let html = source_text(ctx, item, i, &prop, binary)?;
            let doc = Html::parse_document(&html);
            let trim = param_bool(ctx, "options.trimValues", i, true)?;
            let clean = param_bool(ctx, "options.cleanUpText", i, true)?;
            let mut res = Map::new();
            for k in 0..raw_array(ctx, "extractionValues.values").len() {
                let base = format!("extractionValues.values.{k}");
                let key = param_str(ctx, &format!("{base}.key"), i, "")?;
                let css = param_str(ctx, &format!("{base}.cssSelector"), i, "")?;
                let ret = param_str(ctx, &format!("{base}.returnValue"), i, "text")?;
                let attr = param_str(ctx, &format!("{base}.attribute"), i, "")?;
                let skip = param_str(ctx, &format!("{base}.skipSelectors"), i, "")?;
                let as_array = param_bool(ctx, &format!("{base}.returnArray"), i, false)?;
                if key.is_empty() {
                    continue;
                }
                let sel = Selector::parse(&css)
                    .map_err(|e| node_err(ctx, format!("Invalid CSS selector '{css}': {e}")))?;
                let skip_sel: Vec<Selector> = skip
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .filter_map(|s| Selector::parse(s).ok())
                    .collect();
                let mut vals: Vec<String> = Vec::new();
                for el in doc.select(&sel) {
                    let v = match ret.as_str() {
                        "html" => el.inner_html(),
                        "attribute" => el.value().attr(&attr).unwrap_or_default().to_owned(),
                        "value" => el.value().attr("value").unwrap_or_default().to_owned(),
                        _ => {
                            let mut skipped = std::collections::HashSet::new();
                            for s in &skip_sel {
                                for e in el.select(s) {
                                    skipped.insert(e.id());
                                }
                            }
                            el.descendants()
                                .filter(|n| {
                                    !n.ancestors().any(|a| skipped.contains(&a.id()))
                                        && !skipped.contains(&n.id())
                                })
                                .filter_map(|n| n.value().as_text().map(|t| t.text.to_string()))
                                .collect::<String>()
                        }
                    };
                    let v = if clean && ret == "text" {
                        clean_text(&v)
                    } else {
                        v
                    };
                    vals.push(if trim { v.trim().to_owned() } else { v });
                }
                let value = if as_array {
                    Value::Array(vals.into_iter().map(Value::String).collect())
                } else {
                    match vals.len() {
                        0 => Value::Null,
                        1 => Value::String(vals.remove(0)),
                        _ => Value::String(vals.join(" ")),
                    }
                };
                res.insert(key, value);
            }
            let mut o = Item::new(res);
            o.paired_item = paired(i);
            out.push(o);
        }
        Ok(vec![out])
    }
}

// ------------------------------------------------------------------- Markdown

node_struct!(MarkdownNode);

impl MarkdownNode {
    pub fn new() -> Self {
        Self(describe(json!({
            "displayName": "Markdown",
            "name": "n8n-nodes-base.markdown",
            "version": [1],
            "description": "Convert data between Markdown and HTML",
            "group": ["output"],
            "defaults": {"name": "Markdown"},
            "inputs": ["main"],
            "outputs": ["main"],
            "properties": [
                {"displayName": "Mode", "name": "mode", "type": "options", "default": "markdownToHtml",
                 "options": [{"name": "Markdown to HTML", "value": "markdownToHtml"}, {"name": "HTML to Markdown", "value": "htmlToMarkdown"}]},
                {"displayName": "Markdown", "name": "markdown", "type": "string", "default": "",
                 "displayOptions": {"show": {"mode": ["markdownToHtml"]}}},
                {"displayName": "HTML", "name": "html", "type": "string", "default": "",
                 "displayOptions": {"show": {"mode": ["htmlToMarkdown"]}}},
                {"displayName": "Destination Key", "name": "destinationKey", "type": "string", "default": "data"}
            ]
        })))
    }
}

#[async_trait]
impl FlowNode for MarkdownNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }
    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let mut out = Vec::new();
        for (i, item) in port(ctx, 0).iter().enumerate() {
            let mode = param_str(ctx, "mode", i, "markdownToHtml")?;
            if mode != "markdownToHtml" {
                return Err(node_err(
                    ctx,
                    "HTML to Markdown is not supported yet; use a Code node or an n8n instance",
                ));
            }
            let md = param_str(ctx, "markdown", i, "")?;
            let mut html = String::new();
            let opts = pulldown_cmark::Options::ENABLE_TABLES
                | pulldown_cmark::Options::ENABLE_STRIKETHROUGH
                | pulldown_cmark::Options::ENABLE_TASKLISTS;
            pulldown_cmark::html::push_html(&mut html, pulldown_cmark::Parser::new_ext(&md, opts));
            let mut o = item.clone();
            o.json.insert(
                param_str(ctx, "destinationKey", i, "data")?,
                Value::String(html.trim_end().to_owned()),
            );
            o.paired_item = paired(i);
            out.push(o);
        }
        Ok(vec![out])
    }
}

// ------------------------------------------------------------------------ XML

node_struct!(XmlNode);

impl XmlNode {
    pub fn new() -> Self {
        Self(describe(json!({
            "displayName": "XML",
            "name": "n8n-nodes-base.xml",
            "version": [1],
            "description": "Convert data from and to XML",
            "group": ["transform"],
            "defaults": {"name": "XML", "color": "#333377"},
            "inputs": ["main"],
            "outputs": ["main"],
            "properties": [
                {"displayName": "Mode", "name": "mode", "type": "options", "default": "xmlToJson",
                 "options": [{"name": "XML to JSON", "value": "xmlToJson"}, {"name": "JSON to XML", "value": "jsonToXml"}]},
                {"displayName": "Property Name", "name": "dataPropertyName", "type": "string", "default": "data"},
                {"displayName": "Options", "name": "options", "type": "collection", "default": {}, "placeholder": "Add option",
                 "options": [
                    {"displayName": "Explicit Array", "name": "explicitArray", "type": "boolean", "default": false},
                    {"displayName": "Explicit Root", "name": "explicitRoot", "type": "boolean", "default": true},
                    {"displayName": "Ignore Attributes", "name": "ignoreAttrs", "type": "boolean", "default": false},
                    {"displayName": "Merge Attributes", "name": "mergeAttrs", "type": "boolean", "default": true},
                    {"displayName": "Root Name", "name": "rootName", "type": "string", "default": "root"},
                    {"displayName": "Headless", "name": "headless", "type": "boolean", "default": false}]}
            ]
        })))
    }
}

struct XmlOpts {
    explicit_array: bool,
    ignore_attrs: bool,
    merge_attrs: bool,
}

fn xml_to_value(xml: &str, o: &XmlOpts) -> Result<Value, String> {
    use quick_xml::Reader;
    use quick_xml::events::Event;
    struct Frame {
        name: String,
        obj: Map<String, Value>,
        text: String,
    }
    fn add(parent: &mut Map<String, Value>, name: &str, v: Value, explicit_array: bool) {
        match parent.get_mut(name) {
            Some(Value::Array(a)) => a.push(v),
            Some(prev) => {
                let first = prev.take();
                *prev = Value::Array(vec![first, v]);
            }
            None => {
                parent.insert(
                    name.to_owned(),
                    if explicit_array {
                        Value::Array(vec![v])
                    } else {
                        v
                    },
                );
            }
        }
    }
    fn finish(f: Frame) -> Value {
        let text = f.text.trim().to_owned();
        if f.obj.is_empty() {
            return if text.is_empty() {
                Value::String(String::new())
            } else {
                Value::String(text)
            };
        }
        let mut obj = f.obj;
        if !text.is_empty() {
            obj.insert("_".into(), Value::String(text));
        }
        Value::Object(obj)
    }
    let mut reader = Reader::from_str(xml);
    let mut stack: Vec<Frame> = Vec::new();
    let mut root = Map::new();
    let attrs = |e: &quick_xml::events::BytesStart<'_>| -> Result<Map<String, Value>, String> {
        let mut m = Map::new();
        if o.ignore_attrs {
            return Ok(m);
        }
        for a in e.attributes() {
            let a = a.map_err(|e| e.to_string())?;
            m.insert(
                String::from_utf8_lossy(a.key.as_ref()).into_owned(),
                Value::String(
                    a.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                        .map_err(|e| e.to_string())?
                        .into_owned(),
                ),
            );
        }
        Ok(m)
    };
    loop {
        match reader
            .read_event()
            .map_err(|e| format!("Invalid XML at byte {}: {e}", reader.buffer_position()))?
        {
            Event::Start(e) => {
                let a = attrs(&e)?;
                let obj = if o.merge_attrs {
                    a
                } else if a.is_empty() {
                    Map::new()
                } else {
                    Map::from_iter([("$".to_owned(), Value::Object(a))])
                };
                stack.push(Frame {
                    name: String::from_utf8_lossy(e.name().as_ref()).into_owned(),
                    obj,
                    text: String::new(),
                });
            }
            Event::Empty(e) => {
                let a = attrs(&e)?;
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                let v = if a.is_empty() {
                    Value::String(String::new())
                } else if o.merge_attrs {
                    Value::Object(a)
                } else {
                    json!({"$": a})
                };
                match stack.last_mut() {
                    Some(p) => add(&mut p.obj, &name, v, o.explicit_array),
                    None => add(&mut root, &name, v, false),
                }
            }
            Event::End(_) => {
                let f = stack.pop().ok_or("Unbalanced XML")?;
                let name = f.name.clone();
                let v = finish(f);
                match stack.last_mut() {
                    Some(p) => add(&mut p.obj, &name, v, o.explicit_array),
                    None => add(&mut root, &name, v, false),
                }
            }
            Event::Text(t) => {
                if let Some(f) = stack.last_mut() {
                    f.text.push_str(
                        &t.xml_content(quick_xml::XmlVersion::Implicit1_0)
                            .map_err(|e| e.to_string())?,
                    );
                }
            }
            Event::CData(t) => {
                if let Some(f) = stack.last_mut() {
                    f.text.push_str(&String::from_utf8_lossy(&t));
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !stack.is_empty() {
        return Err("Unclosed XML element".into());
    }
    Ok(Value::Object(root))
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn value_to_xml(name: &str, v: &Value, out: &mut String) {
    match v {
        Value::Array(a) => {
            for x in a {
                value_to_xml(name, x, out);
            }
        }
        Value::Object(m) => {
            out.push('<');
            out.push_str(name);
            if let Some(Value::Object(attrs)) = m.get("$") {
                for (k, a) in attrs {
                    out.push_str(&format!(
                        " {k}=\"{}\"",
                        esc(&to_display(a)).replace('"', "&quot;")
                    ));
                }
            }
            out.push('>');
            for (k, c) in m {
                match k.as_str() {
                    "$" => {}
                    "_" => out.push_str(&esc(&to_display(c))),
                    _ => value_to_xml(k, c, out),
                }
            }
            out.push_str(&format!("</{name}>"));
        }
        Value::Null => out.push_str(&format!("<{name}/>")),
        other => out.push_str(&format!("<{name}>{}</{name}>", esc(&to_display(other)))),
    }
}

#[async_trait]
impl FlowNode for XmlNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }
    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let mut out = Vec::new();
        for (i, item) in port(ctx, 0).iter().enumerate() {
            let mode = param_str(ctx, "mode", i, "xmlToJson")?;
            let prop = param_str(ctx, "dataPropertyName", i, "data")?;
            let mut o = Item {
                paired_item: paired(i),
                ..Item::default()
            };
            if mode == "xmlToJson" {
                let xml = get_path(&item.json, &prop, true)
                    .map(to_display)
                    .ok_or_else(|| {
                        node_err(
                            ctx,
                            format!("No property named '{prop}' exists in item {i}"),
                        )
                    })?;
                let opts = XmlOpts {
                    explicit_array: param_bool(ctx, "options.explicitArray", i, false)?,
                    ignore_attrs: param_bool(ctx, "options.ignoreAttrs", i, false)?,
                    merge_attrs: param_bool(ctx, "options.mergeAttrs", i, true)?,
                };
                let v = xml_to_value(&xml, &opts).map_err(|m| node_err(ctx, m))?;
                let Value::Object(mut m) = v else {
                    unreachable!("root is an object")
                };
                if !param_bool(ctx, "options.explicitRoot", i, true)? && m.len() == 1 {
                    if let Some(Value::Object(inner)) = m.values().next().cloned() {
                        m = inner;
                    }
                }
                o.json = m;
            } else {
                let root = param_str(ctx, "options.rootName", i, "root")?;
                let mut s = String::new();
                if !param_bool(ctx, "options.headless", i, false)? {
                    s.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n");
                }
                value_to_xml(&root, &Value::Object(item.json.clone()), &mut s);
                o.json.insert(prop, Value::String(s));
            }
            out.push(o);
        }
        Ok(vec![out])
    }
}

// --------------------------------------------------------------------- Crypto

#[cfg(feature = "expr")]
node_struct!(CryptoNode);

#[cfg(feature = "expr")]
impl CryptoNode {
    pub fn new() -> Self {
        Self(describe(json!({
            "displayName": "Crypto",
            "name": "n8n-nodes-base.crypto",
            "version": [1],
            "description": "Provide cryptographic utilities",
            "group": ["transform"],
            "defaults": {"name": "Crypto", "color": "#408000"},
            "inputs": ["main"],
            "outputs": ["main"],
            "properties": [
                {"displayName": "Action", "name": "action", "type": "options", "default": "hash",
                 "options": [{"name": "Generate", "value": "generate"}, {"name": "Hash", "value": "hash"}, {"name": "Hmac", "value": "hmac"}]},
                {"displayName": "Type", "name": "type", "type": "options", "default": "MD5",
                 "displayOptions": {"show": {"action": ["hash", "hmac"]}},
                 "options": [{"name": "MD5", "value": "MD5"}, {"name": "SHA1", "value": "SHA1"}, {"name": "SHA256", "value": "SHA256"},
                             {"name": "SHA384", "value": "SHA384"}, {"name": "SHA512", "value": "SHA512"}]},
                {"displayName": "Value", "name": "value", "type": "string", "default": "",
                 "displayOptions": {"show": {"action": ["hash", "hmac"]}}},
                {"displayName": "Property Name", "name": "dataPropertyName", "type": "string", "default": "data"},
                {"displayName": "Secret", "name": "secret", "type": "string", "default": "",
                 "displayOptions": {"show": {"action": ["hmac"]}}},
                {"displayName": "Encoding", "name": "encoding", "type": "options", "default": "hex",
                 "displayOptions": {"show": {"action": ["hash", "hmac"]}},
                 "options": [{"name": "BASE64", "value": "base64"}, {"name": "HEX", "value": "hex"}]},
                {"displayName": "Encoding", "name": "encodingType", "type": "options", "default": "uuid",
                 "displayOptions": {"show": {"action": ["generate"]}},
                 "options": [{"name": "ASCII", "value": "ascii"}, {"name": "BASE64", "value": "base64"}, {"name": "HEX", "value": "hex"}, {"name": "UUID", "value": "uuid"}]},
                {"displayName": "Length", "name": "stringLength", "type": "number", "default": 32,
                 "displayOptions": {"show": {"action": ["generate"], "encodingType": ["ascii", "base64", "hex"]}}}
            ]
        })))
    }
}

#[cfg(feature = "expr")]
fn digest(kind: &str, data: &[u8], secret: Option<&[u8]>) -> Result<Vec<u8>, String> {
    use hmac::{Hmac, Mac};
    macro_rules! go {
        ($h:ty) => {
            match secret {
                Some(k) => {
                    let mut m = <Hmac<$h> as Mac>::new_from_slice(k).map_err(|e| e.to_string())?;
                    m.update(data);
                    m.finalize().into_bytes().to_vec()
                }
                None => <$h as sha2::Digest>::digest(data).to_vec(),
            }
        };
    }
    Ok(match kind {
        "MD5" => go!(md5::Md5),
        "SHA1" => go!(sha1::Sha1),
        "SHA256" => go!(sha2::Sha256),
        "SHA384" => go!(sha2::Sha384),
        "SHA512" => go!(sha2::Sha512),
        other => return Err(format!("Unknown hash type '{other}'")),
    })
}

#[cfg(feature = "expr")]
fn random_bytes(n: usize) -> Vec<u8> {
    // OS entropy via getrandom is not a dependency here; mix time, counter and address
    // entropy through SHA-256 blocks. Fine for ids and nonces, not for key material.
    use sha2::Digest as _;
    use std::sync::atomic::{AtomicU64, Ordering};
    static CTR: AtomicU64 = AtomicU64::new(0);
    let mut out = Vec::with_capacity(n + 32);
    while out.len() < n {
        let mut h = sha2::Sha256::new();
        h.update(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
                .to_le_bytes(),
        );
        h.update(CTR.fetch_add(1, Ordering::Relaxed).to_le_bytes());
        h.update(std::process::id().to_le_bytes());
        let x = 0u8;
        h.update((std::ptr::addr_of!(x) as usize).to_le_bytes());
        out.extend_from_slice(&h.finalize());
    }
    out.truncate(n);
    out
}

#[cfg(feature = "expr")]
#[async_trait]
impl FlowNode for CryptoNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }
    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let mut out = Vec::new();
        for (i, item) in port(ctx, 0).iter().enumerate() {
            let action = param_str(ctx, "action", i, "hash")?;
            let prop = param_str(ctx, "dataPropertyName", i, "data")?;
            let value = match action.as_str() {
                "hash" | "hmac" => {
                    let data = param_str(ctx, "value", i, "")?;
                    let secret = (action == "hmac")
                        .then(|| param_str(ctx, "secret", i, ""))
                        .transpose()?;
                    let d = digest(
                        &param_str(ctx, "type", i, "MD5")?,
                        data.as_bytes(),
                        secret.as_deref().map(str::as_bytes),
                    )
                    .map_err(|m| node_err(ctx, m))?;
                    if param_str(ctx, "encoding", i, "hex")? == "base64" {
                        B64.encode(d)
                    } else {
                        hex_lower(&d)
                    }
                }
                "generate" => {
                    let len = param_usize(ctx, "stringLength", i, 32)?.clamp(1, 4096);
                    match param_str(ctx, "encodingType", i, "uuid")?.as_str() {
                        "uuid" => {
                            let mut b = random_bytes(16);
                            b[6] = (b[6] & 0x0f) | 0x40;
                            b[8] = (b[8] & 0x3f) | 0x80;
                            let h = hex_lower(&b);
                            format!(
                                "{}-{}-{}-{}-{}",
                                &h[0..8],
                                &h[8..12],
                                &h[12..16],
                                &h[16..20],
                                &h[20..32]
                            )
                        }
                        "hex" => hex_lower(&random_bytes(len.div_ceil(2)))[..len].to_owned(),
                        "base64" => B64.encode(random_bytes(len)).chars().take(len).collect(),
                        _ => random_bytes(len)
                            .iter()
                            .map(|b| (b'a' + b % 26) as char)
                            .collect(),
                    }
                }
                other => return Err(node_err(ctx, format!("Action '{other}' is not supported"))),
            };
            let mut o = item.clone();
            o.json.insert(prop, Value::String(value));
            o.paired_item = paired(i);
            out.push(o);
        }
        Ok(vec![out])
    }
}

#[cfg(feature = "expr")]
fn hex_lower(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

// ------------------------------------------------------------ Execute Command

node_struct!(ExecuteCommandNode);

impl ExecuteCommandNode {
    pub fn new() -> Self {
        Self(describe(json!({
            "displayName": "Execute Command",
            "name": "n8n-nodes-base.executeCommand",
            "version": [1],
            "description": "Executes a shell command",
            "group": ["transform"],
            "defaults": {"name": "Execute Command", "color": "#886644"},
            "inputs": ["main"],
            "outputs": ["main"],
            "properties": [
                {"displayName": "Execute Once", "name": "executeOnce", "type": "boolean", "default": true},
                {"displayName": "Command", "name": "command", "type": "string", "default": "", "typeOptions": {"rows": 5}}
            ]
        })))
    }
}

/// Whether shell commands may run: hook (set by the host for trusted contexts) or env opt-in.
fn exec_allowed() -> bool {
    std::env::var_os("HOKKABAZ_ALLOW_EXEC").is_some_and(|v| v == "1")
}

#[async_trait]
impl FlowNode for ExecuteCommandNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }
    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        #[cfg(target_os = "android")]
        {
            let _ = ctx;
            Err(node_err(ctx, "Execute Command is not available on Android"))
        }
        #[cfg(not(target_os = "android"))]
        {
            if !exec_allowed() {
                return Err(node_err(
                    ctx,
                    "Shell commands run with your privileges, so Execute Command is off by default. Set HOKKABAZ_ALLOW_EXEC=1 where the flow runs to enable it.",
                ));
            }
            let once = param_bool(ctx, "executeOnce", 0, true)?;
            let items = port(ctx, 0);
            let runs: Vec<usize> = if once {
                vec![0]
            } else {
                (0..items.len().max(1)).collect()
            };
            let mut out = Vec::new();
            for i in runs {
                let cmd = param_str(ctx, "command", i, "")?;
                let (shell, flag) = if cfg!(windows) {
                    ("cmd", "/C")
                } else {
                    ("sh", "-c")
                };
                let res = tokio::time::timeout(
                    std::time::Duration::from_secs(300),
                    tokio::process::Command::new(shell)
                        .arg(flag)
                        .arg(&cmd)
                        .kill_on_drop(true)
                        .output(),
                )
                .await
                .map_err(|_| node_err(ctx, "Command timed out after 300s"))?
                .map_err(|e| node_err(ctx, format!("Cannot run the command: {e}")))?;
                let mut m = Map::new();
                m.insert("exitCode".into(), json!(res.status.code().unwrap_or(-1)));
                m.insert(
                    "stderr".into(),
                    Value::String(String::from_utf8_lossy(&res.stderr).trim_end().to_owned()),
                );
                m.insert(
                    "stdout".into(),
                    Value::String(String::from_utf8_lossy(&res.stdout).trim_end().to_owned()),
                );
                let mut it = Item::new(m);
                it.paired_item = paired(i);
                out.push(it);
            }
            Ok(vec![out])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_round_trip_shapes() {
        let o = XmlOpts {
            explicit_array: false,
            ignore_attrs: false,
            merge_attrs: true,
        };
        let v = xml_to_value(r#"<a id="1"><b>x</b><b>y</b><c/><d k="v">t</d></a>"#, &o).unwrap();
        assert_eq!(
            v,
            json!({"a": {"id": "1", "b": ["x", "y"], "c": "", "d": {"k": "v", "_": "t"}}})
        );
        let mut s = String::new();
        value_to_xml(
            "root",
            &json!({"n": 1, "list": [1, 2], "t": {"$": {"a": "q\"1"}, "_": "x<y"}}),
            &mut s,
        );
        assert_eq!(
            s,
            "<root><n>1</n><list>1</list><list>2</list><t a=\"q&quot;1\">x&lt;y</t></root>"
        );
        assert!(xml_to_value("<a><b></a>", &o).is_err());
    }

    #[cfg(feature = "expr")]
    #[test]
    fn digests_match_known_vectors() {
        assert_eq!(
            hex_lower(&digest("MD5", b"abc", None).unwrap()),
            "900150983cd24fb0d6963f7d28e17f72"
        );
        assert_eq!(
            hex_lower(&digest("SHA256", b"abc", None).unwrap()),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex_lower(&digest("SHA256", b"what do ya want for nothing?", Some(b"Jefe")).unwrap()),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }
}
