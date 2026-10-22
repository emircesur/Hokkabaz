//! Declarative builders for n8n node descriptions.
//!
//! [`NodeBuilder`] and [`Prop`] produce plain [`NodeTypeDescription`] / [`NodeProperty`]
//! values, so a node authored here renders on the canvas exactly like an imported n8n
//! node. "Routing-lite" ([`Routing`]) is the subset of n8n's declarative `routing` block
//! that [`RestNode`](crate::rest::RestNode) interprets: where a parameter goes in the HTTP
//! request, which request an operation makes, and how its response becomes items.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use hk_flow::description::{
    CredentialDescription, DisplayOptions, IoDecl, IoSpec, NodeDefaults, NodeProperty,
    NodeTypeDescription, PropertyType,
};
use hk_flow::version::{NodeVersions, Version};

// ------------------------------------------------------------------ routing-lite

/// Where a parameter's value goes in the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SendKind {
    Body,
    Query,
    Header,
}

/// `routing.send`: attach this parameter to the request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Send {
    #[serde(rename = "type")]
    pub kind: SendKind,
    /// Field name; dotted for nested body objects (`topic.name`). Defaults to the
    /// parameter name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub property: Option<String>,
    /// Fixed value / expression instead of the parameter value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    /// Drop `null` and `""` (default). `false` and `0` are always sent.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub skip_empty: bool,
}

fn yes() -> bool {
    true
}
fn is_true(b: &bool) -> bool {
    *b
}

impl Send {
    pub fn body(property: &str) -> Self {
        Self::new(SendKind::Body, property)
    }
    pub fn query(property: &str) -> Self {
        Self::new(SendKind::Query, property)
    }
    pub fn header(property: &str) -> Self {
        Self::new(SendKind::Header, property)
    }
    fn new(kind: SendKind, property: &str) -> Self {
        Self {
            kind,
            property: Some(property.to_owned()),
            value: None,
            skip_empty: true,
        }
    }
}

/// `routing.request`: the request an operation makes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RouteRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// Relative to the node's base URL, or absolute. Expressions allowed
    /// (`=/topics/{{ $parameter.topic }}`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub qs: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub body: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub headers: Map<String, Value>,
}

/// `routing.output`: response -> items.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RouteOutput {
    /// Dotted path of the result list (`data`, `result.items`). Absent: the whole
    /// response (an array becomes one item per element).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub property: Option<String>,
    /// Fields kept when the node's `simplify` parameter is on (dotted paths).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub simplify: Vec<String>,
}

/// How a list operation pages. Results are read from `output.property`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Pagination {
    /// `?limit=N&offset=M`
    #[serde(rename_all = "camelCase")]
    Offset {
        limit_param: String,
        offset_param: String,
        page_size: usize,
    },
    /// `?per_page=N&page=K` (1-based unless `first_page` says otherwise)
    #[serde(rename_all = "camelCase")]
    Page {
        page_param: String,
        size_param: String,
        page_size: usize,
        #[serde(default = "one")]
        first_page: u64,
    },
    /// The response carries an opaque cursor for the next page.
    #[serde(rename_all = "camelCase")]
    Cursor {
        cursor_param: String,
        /// Dotted path of the next cursor in the response; empty/absent = last page.
        next_path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size_param: Option<String>,
        page_size: usize,
    },
    /// The response carries the next page's URL.
    #[serde(rename_all = "camelCase")]
    NextUrl { next_path: String },
}

fn one() -> u64 {
    1
}

/// The `routing` block.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Routing {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<RouteRequest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send: Option<Send>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<RouteOutput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pagination: Option<Pagination>,
}

impl Routing {
    pub fn request(method: &str, url: &str) -> Self {
        Self {
            request: Some(RouteRequest {
                method: Some(method.to_owned()),
                url: Some(url.to_owned()),
                ..RouteRequest::default()
            }),
            ..Self::default()
        }
    }

    pub fn send(send: Send) -> Self {
        Self {
            send: Some(send),
            ..Self::default()
        }
    }

    /// Static query parameter (value may be an expression).
    pub fn qs(mut self, k: &str, v: impl Into<Value>) -> Self {
        if let Some(r) = &mut self.request {
            r.qs.insert(k.to_owned(), v.into());
        }
        self
    }

    pub fn body(mut self, k: &str, v: impl Into<Value>) -> Self {
        if let Some(r) = &mut self.request {
            r.body.insert(k.to_owned(), v.into());
        }
        self
    }

    pub fn header(mut self, k: &str, v: impl Into<Value>) -> Self {
        if let Some(r) = &mut self.request {
            r.headers.insert(k.to_owned(), v.into());
        }
        self
    }

    /// Result list lives at `property`; `simplify` names the fields kept when the
    /// node's `simplify` parameter is on.
    pub fn output(mut self, property: Option<&str>, simplify: &[&str]) -> Self {
        self.output = Some(RouteOutput {
            property: property.map(str::to_owned),
            simplify: simplify.iter().map(|s| (*s).to_owned()).collect(),
        });
        self
    }

    pub fn paginate(mut self, p: Pagination) -> Self {
        self.pagination = Some(p);
        self
    }

    fn to_value(&self) -> Value {
        // plain structs of strings/maps: cannot fail
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    /// The `routing` of a property or option value; `None` when absent or malformed.
    pub fn from_extra(extra: &Map<String, Value>) -> Option<Self> {
        extra
            .get("routing")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }
}

// ------------------------------------------------------------------- properties

/// One choice of an `options` property.
#[derive(Debug, Clone)]
pub struct Choice(Map<String, Value>);

impl Choice {
    pub fn new(name: &str, value: impl Into<Value>) -> Self {
        let mut m = Map::new();
        m.insert("name".into(), name.into());
        m.insert("value".into(), value.into());
        Self(m)
    }

    pub fn describe(mut self, text: &str) -> Self {
        self.0.insert("description".into(), text.into());
        self
    }

    /// Shown in the UI as the action name (n8n `action`).
    pub fn action(mut self, text: &str) -> Self {
        self.0.insert("action".into(), text.into());
        self
    }

    pub fn routing(mut self, r: Routing) -> Self {
        self.0.insert("routing".into(), r.to_value());
        self
    }
}

impl From<Choice> for Value {
    fn from(c: Choice) -> Self {
        Value::Object(c.0)
    }
}

/// Builder for a [`NodeProperty`].
#[derive(Debug, Clone)]
pub struct Prop(NodeProperty);

impl Prop {
    pub fn new(name: &str, display_name: &str, ty: PropertyType) -> Self {
        Self(NodeProperty {
            display_name: display_name.to_owned(),
            name: name.to_owned(),
            prop_type: ty,
            default: Some(match ty_default(&ty) {
                Some(d) => d,
                None => Value::Null,
            }),
            description: None,
            hint: None,
            placeholder: None,
            required: None,
            no_data_expression: None,
            options: None,
            display_options: None,
            type_options: None,
            extra: Map::new(),
        })
    }

    pub fn string(name: &str, display_name: &str) -> Self {
        Self::new(name, display_name, PropertyType::String)
    }
    pub fn number(name: &str, display_name: &str) -> Self {
        Self::new(name, display_name, PropertyType::Number)
    }
    pub fn boolean(name: &str, display_name: &str) -> Self {
        Self::new(name, display_name, PropertyType::Boolean)
    }
    pub fn json(name: &str, display_name: &str) -> Self {
        Self::new(name, display_name, PropertyType::Json)
    }
    pub fn hidden(name: &str, default: impl Into<Value>) -> Self {
        Self::new(name, name, PropertyType::Hidden).default(default)
    }
    pub fn notice(name: &str, text: &str) -> Self {
        Self::new(name, text, PropertyType::Notice)
    }

    /// `options` property. The default is the first choice unless overridden.
    pub fn options(name: &str, display_name: &str, choices: Vec<Choice>) -> Self {
        let first = choices.first().and_then(|c| c.0.get("value")).cloned();
        let mut p = Self::new(name, display_name, PropertyType::Options);
        p.0.options = Some(choices.into_iter().map(Value::from).collect());
        p.0.default = first.or(Some(Value::String(String::new())));
        p.0.no_data_expression = Some(true);
        p
    }

    /// `collection` ("Additional Fields"): optional sub-parameters, stored by name.
    pub fn collection(name: &str, display_name: &str, fields: Vec<Prop>) -> Self {
        let mut p = Self::new(name, display_name, PropertyType::Collection);
        p.0.options = Some(fields.into_iter().map(|f| f.build_value()).collect());
        p.0.default = Some(json!({}));
        p.0.placeholder = Some("Add Field".into());
        p
    }

    pub fn default(mut self, v: impl Into<Value>) -> Self {
        self.0.default = Some(v.into());
        self
    }
    pub fn description(mut self, t: &str) -> Self {
        self.0.description = Some(t.to_owned());
        self
    }
    pub fn hint(mut self, t: &str) -> Self {
        self.0.hint = Some(t.to_owned());
        self
    }
    pub fn placeholder(mut self, t: &str) -> Self {
        self.0.placeholder = Some(t.to_owned());
        self
    }
    pub fn required(mut self) -> Self {
        self.0.required = Some(true);
        self
    }
    /// Mask the value in the UI (secrets).
    pub fn password(self) -> Self {
        self.type_option("password", true)
    }
    pub fn min(self, v: f64) -> Self {
        self.type_option("minValue", v)
    }
    pub fn max(self, v: f64) -> Self {
        self.type_option("maxValue", v)
    }

    pub fn type_option(mut self, k: &str, v: impl Into<Value>) -> Self {
        let slot = self.0.type_options.get_or_insert_with(|| json!({}));
        if let Value::Object(m) = slot {
            m.insert(k.to_owned(), v.into());
        }
        self
    }

    /// Show only when parameter `key` is one of `values` (n8n `displayOptions.show`).
    pub fn show(mut self, key: &str, values: &[&str]) -> Self {
        let d = self.0.display_options.get_or_insert_with(DisplayOptions::default);
        d.show
            .get_or_insert_with(IndexMap::new)
            .insert(key.to_owned(), json!(values));
        self
    }

    /// Hide when parameter `key` is one of `values`.
    pub fn hide(mut self, key: &str, values: &[&str]) -> Self {
        let d = self.0.display_options.get_or_insert_with(DisplayOptions::default);
        d.hide
            .get_or_insert_with(IndexMap::new)
            .insert(key.to_owned(), json!(values));
        self
    }

    /// Same as [`show`](Self::show) with a boolean condition value.
    pub fn show_bool(mut self, key: &str, value: bool) -> Self {
        let d = self.0.display_options.get_or_insert_with(DisplayOptions::default);
        d.show
            .get_or_insert_with(IndexMap::new)
            .insert(key.to_owned(), json!([value]));
        self
    }

    pub fn routing(mut self, r: Routing) -> Self {
        self.0.extra.insert("routing".into(), r.to_value());
        self
    }

    /// Send this parameter in the request (shorthand for `routing(Routing::send(..))`).
    pub fn send(self, s: Send) -> Self {
        self.routing(Routing::send(s))
    }

    pub fn build(self) -> NodeProperty {
        self.0
    }

    fn build_value(self) -> Value {
        serde_json::to_value(self.0).unwrap_or(Value::Null)
    }
}

impl From<Prop> for NodeProperty {
    fn from(p: Prop) -> Self {
        p.0
    }
}

fn ty_default(t: &PropertyType) -> Option<Value> {
    Some(match t {
        PropertyType::String => json!(""),
        PropertyType::Number => json!(0),
        PropertyType::Boolean => json!(false),
        PropertyType::Collection | PropertyType::FixedCollection => json!({}),
        PropertyType::MultiOptions => json!([]),
        _ => return None,
    })
}

/// Standard "Return All" + "Limit" pair for list operations. Pass the operation values
/// they belong to (`operation` parameter).
pub fn return_all_and_limit(resource: Option<&str>, operations: &[&str]) -> Vec<Prop> {
    let scope = |p: Prop| {
        let p = match resource {
            Some(r) => p.show("resource", &[r]),
            None => p,
        };
        p.show("operation", operations)
    };
    vec![
        scope(
            Prop::boolean("returnAll", "Return All")
                .description("Whether to return all results or only up to a given limit"),
        ),
        scope(
            Prop::number("limit", "Limit")
                .default(50)
                .min(1.0)
                .description("Max number of results to return")
                .show_bool("returnAll", false),
        ),
    ]
}

/// `simplify` toggle: results are cut down to the operation's `routing.output.simplify`.
pub fn simplify_prop() -> Prop {
    Prop::boolean("simplify", "Simplify")
        .default(true)
        .description("Whether to return a simplified version of the response instead of the raw data")
}

/// n8n polling trigger interval (`pollTimes`): `{ item: [{ mode, value, unit }] }`.
/// [`PollingHost`](crate::poll::PollingHost) reads `everyMinute`, `everyHour` and `everyX`.
pub fn poll_times_prop() -> Prop {
    let mut p = Prop::new("pollTimes", "Poll Times", PropertyType::FixedCollection);
    p.0.default = Some(json!({"item": [{"mode": "everyMinute"}]}));
    p.0.type_options = Some(json!({"multipleValues": true, "multipleValueButtonText": "Add Poll Time"}));
    p.0.options = Some(vec![json!({
        "name": "item", "displayName": "Item",
        "values": [
            {"displayName": "Mode", "name": "mode", "type": "options", "default": "everyMinute",
             "options": [
                {"name": "Every Minute", "value": "everyMinute"},
                {"name": "Every Hour", "value": "everyHour"},
                {"name": "Every X", "value": "everyX"}]},
            {"displayName": "Value", "name": "value", "type": "number", "default": 5,
             "displayOptions": {"show": {"mode": ["everyX"]}}},
            {"displayName": "Unit", "name": "unit", "type": "options", "default": "minutes",
             "displayOptions": {"show": {"mode": ["everyX"]}},
             "options": [
                {"name": "Seconds", "value": "seconds"},
                {"name": "Minutes", "value": "minutes"},
                {"name": "Hours", "value": "hours"}]}
        ]
    })]);
    p
}

// ----------------------------------------------------------------------- node

/// Builder for a [`NodeTypeDescription`].
#[derive(Debug, Clone)]
pub struct NodeBuilder(NodeTypeDescription);

impl NodeBuilder {
    /// `name` is the full node type (`hokkabaz.ntfy`).
    pub fn new(name: &str, display_name: &str) -> Self {
        let v = Version::new(1.0).unwrap_or_else(|| unreachable_version());
        Self(NodeTypeDescription {
            display_name: display_name.to_owned(),
            name: name.to_owned(),
            version: NodeVersions::Single(v),
            default_version: None,
            description: String::new(),
            subtitle: None,
            group: vec!["output".into()],
            icon: None,
            defaults: NodeDefaults {
                name: display_name.to_owned(),
                color: None,
                extra: Map::new(),
            },
            inputs: IoDecl::List(vec![IoSpec::Simple("main".into())]),
            outputs: IoDecl::List(vec![IoSpec::Simple("main".into())]),
            output_names: None,
            credentials: Vec::new(),
            properties: Vec::new(),
            webhooks: None,
            polling: None,
            codex: None,
            usable_as_tool: None,
            hidden: None,
            extra: Map::new(),
        })
    }

    pub fn version(mut self, v: f64) -> Self {
        if let Some(v) = Version::new(v) {
            self.0.version = NodeVersions::Single(v);
        }
        self
    }

    pub fn description(mut self, t: &str) -> Self {
        self.0.description = t.to_owned();
        self
    }

    pub fn subtitle(mut self, t: &str) -> Self {
        self.0.subtitle = Some(t.to_owned());
        self
    }

    pub fn group(mut self, g: &[&str]) -> Self {
        self.0.group = g.iter().map(|s| (*s).to_owned()).collect();
        self
    }

    pub fn icon(mut self, icon: &str) -> Self {
        self.0.icon = Some(Value::String(icon.to_owned()));
        self
    }

    pub fn color(mut self, c: &str) -> Self {
        self.0.defaults.color = Some(c.to_owned());
        self
    }

    pub fn credential(mut self, name: &str, required: bool) -> Self {
        self.0.credentials.push(CredentialDescription {
            name: name.to_owned(),
            required: Some(required),
            display_options: None,
            extra: Map::new(),
        });
        self
    }

    pub fn property(mut self, p: Prop) -> Self {
        self.0.properties.push(p.build());
        self
    }

    pub fn properties(mut self, ps: impl IntoIterator<Item = Prop>) -> Self {
        self.0.properties.extend(ps.into_iter().map(Prop::build));
        self
    }

    /// Trigger node: no inputs, `trigger` group, polling flag.
    pub fn polling_trigger(mut self) -> Self {
        self.0.group = vec!["trigger".into()];
        self.0.inputs = IoDecl::List(Vec::new());
        self.0.polling = Some(true);
        self
    }

    pub fn build(self) -> NodeTypeDescription {
        self.0
    }
}

// 1.0 is finite; the branch exists only to avoid `expect` in a lib
#[cold]
fn unreachable_version() -> Version {
    Version::new(1.0).unwrap_or_else(|| unreachable_version())
}

// ------------------------------------------------------------- display options

/// n8n `displayOptions` evaluation against current parameter values. `lookup` resolves a
/// parameter name (a leading `/` is stripped). Conditions are `[values]` or
/// `[{_cnd: {eq|not|exists|...}}]`; unknown condition kinds count as not matching.
pub fn is_visible(d: Option<&DisplayOptions>, lookup: &dyn Fn(&str) -> Option<Value>) -> bool {
    let Some(d) = d else { return true };
    let matches = |conds: &IndexMap<String, Value>| {
        conds.iter().all(|(k, want)| {
            let have = lookup(k.trim_start_matches('/'));
            cond_matches(want, have.as_ref())
        })
    };
    if let Some(show) = &d.show
        && !matches(show)
    {
        return false;
    }
    if let Some(hide) = &d.hide
        && !hide.is_empty()
        && matches(hide)
    {
        return false;
    }
    true
}

fn cond_matches(want: &Value, have: Option<&Value>) -> bool {
    let Value::Array(alts) = want else {
        return false;
    };
    alts.iter().any(|alt| match alt {
        Value::Object(o) if o.contains_key("_cnd") => {
            let Some(Value::Object(c)) = o.get("_cnd") else {
                return false;
            };
            c.iter().all(|(op, v)| match (op.as_str(), have) {
                ("eq", Some(h)) => h == v,
                ("not", h) => h != Some(v),
                ("exists", h) => h.is_some_and(|x| !x.is_null()) == v.as_bool().unwrap_or(true),
                _ => false,
            })
        }
        other => have.is_some_and(|h| loose_eq(h, other)),
    })
}

fn loose_eq(a: &Value, b: &Value) -> bool {
    a == b
        || match (a, b) {
            (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
            _ => false,
        }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_n8n_shaped_description() {
        let d = NodeBuilder::new("hokkabaz.demo", "Demo")
            .description("demo")
            .credential("demoApi", true)
            .property(
                Prop::options(
                    "operation",
                    "Operation",
                    vec![Choice::new("Get", "get").routing(Routing::request("GET", "/x"))],
                )
                .show("resource", &["a"]),
            )
            .property(Prop::string("id", "ID").required().send(Send::query("id")))
            .build();
        let v = serde_json::to_value(&d).unwrap();
        assert_eq!(v["name"], "hokkabaz.demo");
        assert_eq!(v["properties"][0]["options"][0]["routing"]["request"]["url"], "/x");
        assert_eq!(v["properties"][0]["displayOptions"]["show"]["resource"], json!(["a"]));
        assert_eq!(v["properties"][1]["routing"]["send"]["type"], "query");
        // parses back through the hk-flow schema
        let back: NodeTypeDescription = serde_json::from_value(v).unwrap();
        assert_eq!(back, d);
    }

    #[test]
    fn display_options() {
        let d = DisplayOptions {
            show: Some(IndexMap::from([("resource".to_owned(), json!(["a", "b"]))])),
            hide: Some(IndexMap::from([("x".to_owned(), json!([true]))])),
            extra: Map::new(),
        };
        let l = |m: Value| move |k: &str| m.get(k).cloned();
        assert!(is_visible(Some(&d), &l(json!({"resource": "b"}))));
        assert!(!is_visible(Some(&d), &l(json!({"resource": "c"}))));
        assert!(!is_visible(Some(&d), &l(json!({"resource": "a", "x": true}))));
        assert!(is_visible(None, &l(json!({}))));
    }
}
