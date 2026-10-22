//! n8n credential type definitions: what fields a credential has, how it authenticates a
//! request and how to test it.
//!
//! Types describe *shape* only. Values live in hk-store secrets; the runtime resolves them
//! through hk-flow's `CredentialResolver` and hands the decrypted map to
//! [`CredentialType::apply`], which returns the request parts to attach. Nothing here logs
//! or stores values, and errors name fields, never values.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use indexmap::IndexMap;
use serde_json::{Map, Value, json};

use hk_flow::description::{NodeProperty, PropertyType};

use crate::describe::Prop;
use crate::template::{self, display};

#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    #[error("credential field `{0}` is missing")]
    MissingField(String),
    #[error("credential template: {0}")]
    Template(String),
    #[error("credential type `{0}` is not registered")]
    UnknownType(String),
    #[error("credential type `{0}` is registered twice")]
    Duplicate(String),
    #[error("credential data: {0}")]
    Invalid(String),
    #[error("credential test failed: {0}")]
    Test(String),
}

/// Request parts a credential contributes.
#[derive(Clone, Default, PartialEq)]
pub struct AppliedAuth {
    pub headers: Vec<(String, String)>,
    pub query: Vec<(String, String)>,
    /// Merged into a JSON/form body.
    pub body: Map<String, Value>,
    pub basic: Option<(String, String)>,
}

// values are secrets
impl fmt::Debug for AppliedAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AppliedAuth")
            .field("headers", &self.headers.len())
            .field("query", &self.query.len())
            .field("body", &self.body.len())
            .field("basic", &self.basic.is_some())
            .finish()
    }
}

/// Code-defined authenticate (n8n function form), for shapes templates cannot express.
pub type AuthFn =
    Arc<dyn Fn(&Map<String, Value>) -> Result<AppliedAuth, CredentialError> + Send + Sync>;

/// n8n `IAuthenticateGeneric.properties`: every value is a `$credentials.*` template.
#[derive(Debug, Clone, Default)]
pub struct GenericAuth {
    pub headers: IndexMap<String, String>,
    pub qs: IndexMap<String, String>,
    pub body: IndexMap<String, String>,
    /// `(username, password)` templates.
    pub basic: Option<(String, String)>,
}

#[derive(Clone)]
pub enum Authenticate {
    Generic(Box<GenericAuth>),
    /// n8n httpCustomAuth: a JSON field holding `{headers, qs, body}` as literal values.
    JsonBlob {
        field: String,
    },
    Function(AuthFn),
}

impl fmt::Debug for Authenticate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Generic(g) => f.debug_tuple("Generic").field(g).finish(),
            Self::JsonBlob { field } => f.debug_struct("JsonBlob").field("field", field).finish(),
            Self::Function(_) => f.write_str("Function(..)"),
        }
    }
}

impl Authenticate {
    pub fn header(name: &str, value: &str) -> Self {
        Self::Generic(Box::new(GenericAuth {
            headers: IndexMap::from([(name.to_owned(), value.to_owned())]),
            ..GenericAuth::default()
        }))
    }

    pub fn query(name: &str, value: &str) -> Self {
        Self::Generic(Box::new(GenericAuth {
            qs: IndexMap::from([(name.to_owned(), value.to_owned())]),
            ..GenericAuth::default()
        }))
    }

    pub fn basic(username: &str, password: &str) -> Self {
        Self::Generic(Box::new(GenericAuth {
            basic: Some((username.to_owned(), password.to_owned())),
            ..GenericAuth::default()
        }))
    }

    /// `Authorization: Bearer <token>`; `token` is a template (`={{$credentials.token}}`).
    pub fn bearer(token: &str) -> Self {
        Self::header(
            "Authorization",
            &format!("=Bearer {}", token.trim_start_matches('=')),
        )
    }

    pub fn function(
        f: impl Fn(&Map<String, Value>) -> Result<AppliedAuth, CredentialError> + Send + Sync + 'static,
    ) -> Self {
        Self::Function(Arc::new(f))
    }

    pub fn apply(&self, data: &Map<String, Value>) -> Result<AppliedAuth, CredentialError> {
        match self {
            Self::Generic(g) => {
                let mut out = AppliedAuth::default();
                for (k, v) in &g.headers {
                    out.headers
                        .push((template::render(k, data)?, template::render(v, data)?));
                }
                for (k, v) in &g.qs {
                    out.query
                        .push((template::render(k, data)?, template::render(v, data)?));
                }
                for (k, v) in &g.body {
                    out.body.insert(
                        template::render(k, data)?,
                        Value::String(template::render(v, data)?),
                    );
                }
                if let Some((u, p)) = &g.basic {
                    out.basic = Some((template::render(u, data)?, template::render(p, data)?));
                }
                Ok(out)
            }
            Self::JsonBlob { field } => {
                let raw = data
                    .get(field)
                    .ok_or_else(|| CredentialError::MissingField(field.clone()))?;
                // never echo the blob: it holds the secrets
                let spec: Value = match raw {
                    Value::String(s) => serde_json::from_str(s).map_err(|_| {
                        CredentialError::Invalid(format!("field `{field}` is not valid JSON"))
                    })?,
                    other => other.clone(),
                };
                let mut out = AppliedAuth::default();
                let pairs = |k: &str| -> Vec<(String, String)> {
                    spec.get(k)
                        .and_then(Value::as_object)
                        .map(|o| o.iter().map(|(a, b)| (a.clone(), display(b))).collect())
                        .unwrap_or_default()
                };
                out.headers = pairs("headers");
                out.query = pairs("qs");
                if let Some(b) = spec.get("body").and_then(Value::as_object) {
                    out.body = b.clone();
                }
                Ok(out)
            }
            Self::Function(f) => f(data),
        }
    }
}

/// n8n `ICredentialTestRequest`: a cheap authenticated call that proves the credential.
#[derive(Debug, Clone)]
pub struct CredentialTest {
    pub method: String,
    /// Template; may use `$credentials.*` (`={{$credentials.serverUrl}}`).
    pub base_url: String,
    pub url: String,
    /// Accepted statuses; empty = any 2xx.
    pub ok_status: Vec<u16>,
}

impl CredentialTest {
    pub fn get(base_url: &str, url: &str) -> Self {
        Self {
            method: "GET".into(),
            base_url: base_url.to_owned(),
            url: url.to_owned(),
            ok_status: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CredentialType {
    pub name: String,
    pub display_name: String,
    pub documentation_url: Option<String>,
    /// Parent types (`oAuth2Api`): their properties come first.
    pub extends: Vec<String>,
    pub properties: Vec<NodeProperty>,
    pub authenticate: Option<Authenticate>,
    pub test: Option<CredentialTest>,
    /// Offered by the HTTP Request node's "Generic Credential Type" picker.
    pub generic_auth: bool,
}

impl CredentialType {
    pub fn new(name: &str, display_name: &str) -> Self {
        Self {
            name: name.to_owned(),
            display_name: display_name.to_owned(),
            documentation_url: None,
            extends: Vec::new(),
            properties: Vec::new(),
            authenticate: None,
            test: None,
            generic_auth: false,
        }
    }

    pub fn property(mut self, p: Prop) -> Self {
        self.properties.push(p.build());
        self
    }

    pub fn authenticate(mut self, a: Authenticate) -> Self {
        self.authenticate = Some(a);
        self
    }

    pub fn test(mut self, t: CredentialTest) -> Self {
        self.test = Some(t);
        self
    }

    pub fn generic(mut self) -> Self {
        self.generic_auth = true;
        self
    }

    pub fn extends(mut self, parent: &str) -> Self {
        self.extends.push(parent.to_owned());
        self
    }

    pub fn documentation(mut self, url: &str) -> Self {
        self.documentation_url = Some(url.to_owned());
        self
    }

    /// Copy of `data` with property defaults filled in for absent fields.
    pub fn with_defaults(&self, data: &Map<String, Value>) -> Map<String, Value> {
        let mut out = data.clone();
        for p in &self.properties {
            if !out.contains_key(&p.name)
                && let Some(d) = &p.default
                && !d.is_null()
                // an empty default means "no value": a missing secret must stay missing
                && !matches!(d, Value::String(x) if x.is_empty())
            {
                out.insert(p.name.clone(), d.clone());
            }
        }
        out
    }

    /// Request parts for decrypted credential `data` (defaults applied first).
    pub fn apply(&self, data: &Map<String, Value>) -> Result<AppliedAuth, CredentialError> {
        match &self.authenticate {
            Some(a) => a.apply(&self.with_defaults(data)),
            None => Ok(AppliedAuth::default()),
        }
    }

    /// JSON schema of the credential data (`GET /credentials/schema/{type}`): flat
    /// object, unknown keys rejected, `required` from the properties that say so.
    pub fn schema(&self) -> Value {
        let mut props = Map::new();
        let mut required = Vec::new();
        for p in &self.properties {
            let mut s = match p.prop_type {
                PropertyType::Number => json!({"type": "number"}),
                PropertyType::Boolean => json!({"type": "boolean"}),
                PropertyType::Collection | PropertyType::FixedCollection | PropertyType::Json => {
                    json!({"type": "object"})
                }
                PropertyType::Options => {
                    let vals: Vec<Value> = p
                        .options
                        .iter()
                        .flatten()
                        .filter_map(|o| o.get("value").cloned())
                        .collect();
                    json!({"type": "string", "enum": vals})
                }
                PropertyType::Notice | PropertyType::Hidden | PropertyType::Button => {
                    json!({"type": "string"})
                }
                _ => json!({"type": "string"}),
            };
            if let (Some(d), Value::Object(m)) = (&p.default, &mut s)
                && !d.is_null()
                && !matches!(d, Value::String(x) if x.is_empty())
            {
                m.insert("default".into(), d.clone());
            }
            if p.required == Some(true) && !matches!(p.prop_type, PropertyType::Notice) {
                required.push(Value::String(p.name.clone()));
            }
            if !matches!(p.prop_type, PropertyType::Notice) {
                props.insert(p.name.clone(), s);
            }
        }
        json!({
            "additionalProperties": false,
            "type": "object",
            "properties": props,
            "required": required,
        })
    }

    /// n8n `ICredentialType` JSON (no secrets, no functions) for the credential-type list.
    pub fn describe(&self) -> Value {
        let mut v = json!({
            "name": self.name,
            "displayName": self.display_name,
            "properties": self.properties,
        });
        if let Value::Object(m) = &mut v {
            if let Some(u) = &self.documentation_url {
                m.insert("documentationUrl".into(), json!(u));
            }
            if !self.extends.is_empty() {
                m.insert("extends".into(), json!(self.extends));
            }
            if self.generic_auth {
                m.insert("genericAuth".into(), json!(true));
            }
            if self.test.is_some() {
                m.insert("test".into(), json!(true));
            }
        }
        v
    }
}

fn secret(name: &str, display: &str) -> Prop {
    Prop::string(name, display).password().required()
}

/// Registry of credential types. Cheap to clone; all clones share the table, so an
/// integration registered through one handle is visible to nodes built from another.
#[derive(Clone, Default)]
pub struct CredentialTypes(Arc<RwLock<BTreeMap<String, Arc<CredentialType>>>>);

impl fmt::Debug for CredentialTypes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CredentialTypes")
            .field("names", &self.names())
            .finish()
    }
}

impl CredentialTypes {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registry preloaded with the generic types ([`builtin_types`]).
    pub fn with_builtins() -> Self {
        let t = Self::new();
        for ty in builtin_types() {
            // names are unique by construction
            let _ = t.register(ty);
        }
        t
    }

    pub fn register(&self, ty: CredentialType) -> Result<(), CredentialError> {
        let mut g = self
            .0
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if g.contains_key(&ty.name) {
            return Err(CredentialError::Duplicate(ty.name));
        }
        g.insert(ty.name.clone(), Arc::new(ty));
        Ok(())
    }

    /// Register unless the name is taken (idempotent integration setup).
    pub fn register_if_absent(&self, ty: CredentialType) {
        let _ = self.register(ty);
    }

    pub fn get(&self, name: &str) -> Option<Arc<CredentialType>> {
        self.0
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(name)
            .cloned()
    }

    pub fn names(&self) -> Vec<String> {
        self.0
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .keys()
            .cloned()
            .collect()
    }

    /// Type with the properties of its `extends` parents merged in (parents first).
    pub fn resolved(&self, name: &str) -> Option<CredentialType> {
        let mut ty = (*self.get(name)?).clone();
        let mut props = Vec::new();
        for parent in &ty.extends {
            if let Some(p) = self.resolved(parent) {
                for pp in p.properties {
                    if !props.iter().any(|x: &NodeProperty| x.name == pp.name) {
                        props.push(pp);
                    }
                }
            }
        }
        for p in std::mem::take(&mut ty.properties) {
            props.retain(|x| x.name != p.name);
            props.push(p);
        }
        ty.properties = props;
        Some(ty)
    }

    /// Schema for the REST route (`CredentialSchemas::schema` in hk-server).
    pub fn schema(&self, name: &str) -> Option<Value> {
        self.resolved(name).map(|t| t.schema())
    }

    pub fn apply(
        &self,
        name: &str,
        data: &Map<String, Value>,
    ) -> Result<AppliedAuth, CredentialError> {
        self.resolved(name)
            .ok_or_else(|| CredentialError::UnknownType(name.to_owned()))?
            .apply(data)
    }

    /// Defaults-applied credential data (so templates like `serverUrl` resolve).
    pub fn with_defaults(&self, name: &str, data: &Map<String, Value>) -> Map<String, Value> {
        match self.resolved(name) {
            Some(t) => t.with_defaults(data),
            None => data.clone(),
        }
    }

    /// All types as n8n `ICredentialType` JSON.
    pub fn describe_all(&self) -> Vec<Value> {
        self.names()
            .iter()
            .filter_map(|n| self.resolved(n))
            .map(|t| t.describe())
            .collect()
    }

    /// Run the type's test request. `Ok` = the service accepted the credential.
    pub async fn test(&self, name: &str, data: &Map<String, Value>) -> Result<(), CredentialError> {
        let ty = self
            .resolved(name)
            .ok_or_else(|| CredentialError::UnknownType(name.to_owned()))?;
        let t = ty
            .test
            .clone()
            .ok_or_else(|| CredentialError::Test("type has no test request".into()))?;
        let data = ty.with_defaults(data);
        let auth = ty.apply(&data)?;
        let base = template::render(&t.base_url, &data)?;
        let url = format!(
            "{}/{}",
            base.trim_end_matches('/'),
            t.url.trim_start_matches('/')
        );
        let method = reqwest::Method::from_bytes(t.method.to_uppercase().as_bytes())
            .map_err(|_| CredentialError::Test("bad method".into()))?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|_| CredentialError::Test("cannot build http client".into()))?;
        let mut rb = client.request(method, &url);
        for (k, v) in &auth.headers {
            rb = rb.header(k.as_str(), v.as_str());
        }
        if !auth.query.is_empty() {
            rb = rb.query(&auth.query);
        }
        if let Some((u, p)) = &auth.basic {
            rb = rb.basic_auth(u, Some(p));
        }
        let res = rb
            .send()
            .await
            .map_err(|e| CredentialError::Test(e.without_url().to_string()))?;
        let s = res.status().as_u16();
        let ok = if t.ok_status.is_empty() {
            (200..300).contains(&s)
        } else {
            t.ok_status.contains(&s)
        };
        if ok {
            Ok(())
        } else {
            Err(CredentialError::Test(format!("service answered {s}")))
        }
    }
}

/// Generic types every install has (n8n names; `apiKey` is Hokkabaz's own).
pub fn builtin_types() -> Vec<CredentialType> {
    vec![
        CredentialType::new("httpHeaderAuth", "Header Auth")
            .documentation("https://docs.n8n.io/integrations/builtin/credentials/httprequest/")
            .generic()
            .property(Prop::string("name", "Name").required().default("Authorization").placeholder("X-API-KEY"))
            .property(secret("value", "Value"))
            .authenticate(Authenticate::header("={{$credentials.name}}", "={{$credentials.value}}")),
        CredentialType::new("httpBasicAuth", "Basic Auth")
            .generic()
            .property(Prop::string("user", "User").required())
            .property(secret("password", "Password"))
            .authenticate(Authenticate::basic("={{$credentials.user}}", "={{$credentials.password}}")),
        CredentialType::new("httpBearerAuth", "Bearer Auth")
            .generic()
            .property(secret("token", "Bearer Token"))
            .authenticate(Authenticate::bearer("{{$credentials.token}}")),
        CredentialType::new("httpQueryAuth", "Query Auth")
            .generic()
            .property(Prop::string("name", "Name").required())
            .property(secret("value", "Value"))
            .authenticate(Authenticate::query("={{$credentials.name}}", "={{$credentials.value}}")),
        CredentialType::new("httpCustomAuth", "Custom Auth")
            .generic()
            .property(
                Prop::json("json", "JSON")
                    .required()
                    .default("{\n\t\"headers\": {\n\t\t\"key\": \"value\"\n\t},\n\t\"qs\": {\n\t\t\"key\": \"value\"\n\t}\n}")
                    .description("Use the JSON field to specify headers, qs and body")
                    .password(),
            )
            .authenticate(Authenticate::JsonBlob { field: "json".into() }),
        // shape only: the OAuth dance (authorize, token refresh) is not implemented here;
        // a credential whose `oauthTokenData` was stored by the server authenticates as bearer
        CredentialType::new("oAuth2Api", "OAuth2 API")
            .generic()
            .property(
                Prop::options(
                    "grantType",
                    "Grant Type",
                    vec![
                        crate::describe::Choice::new("Authorization Code", "authorizationCode"),
                        crate::describe::Choice::new("Client Credentials", "clientCredentials"),
                        crate::describe::Choice::new("PKCE", "pkce"),
                    ],
                )
                .default("authorizationCode"),
            )
            .property(Prop::string("authUrl", "Authorization URL").show("grantType", &["authorizationCode", "pkce"]))
            .property(Prop::string("accessTokenUrl", "Access Token URL").required())
            .property(Prop::string("clientId", "Client ID").required())
            .property(secret("clientSecret", "Client Secret"))
            .property(Prop::string("scope", "Scope"))
            .property(Prop::string("authQueryParameters", "Auth URI Query Parameters"))
            .property(
                Prop::options(
                    "authentication",
                    "Authentication",
                    vec![
                        crate::describe::Choice::new("Header", "header"),
                        crate::describe::Choice::new("Body", "body"),
                    ],
                )
                .default("header"),
            )
            .property(Prop::json("oauthTokenData", "Token Data").default(json!({})).type_option("hidden", true))
            .authenticate(Authenticate::function(|d| {
                let tok = d
                    .get("oauthTokenData")
                    .and_then(|t| t.get("access_token"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| CredentialError::MissingField("oauthTokenData.access_token".into()))?;
                Ok(AppliedAuth {
                    headers: vec![("Authorization".into(), format!("Bearer {tok}"))],
                    ..AppliedAuth::default()
                })
            })),
        CredentialType::new("apiKey", "API Key")
            .generic()
            .property(secret("apiKey", "API Key"))
            .property(
                Prop::options(
                    "in",
                    "Send In",
                    vec![
                        crate::describe::Choice::new("Header", "header"),
                        crate::describe::Choice::new("Query", "query"),
                    ],
                )
                .default("header"),
            )
            .property(Prop::string("name", "Name").default("X-API-Key").description("Header or query parameter name"))
            .property(
                Prop::string("prefix", "Value Prefix")
                    .default("")
                    .description("Prepended to the key, e.g. `Bearer `"),
            )
            .authenticate(Authenticate::function(|d| {
                let get = |k: &str| d.get(k).map(display).unwrap_or_default();
                let key = d.get("apiKey").map(display).ok_or_else(|| CredentialError::MissingField("apiKey".into()))?;
                let name = get("name");
                let name = if name.is_empty() { "X-API-Key".to_owned() } else { name };
                let value = format!("{}{key}", get("prefix"));
                let mut out = AppliedAuth::default();
                if get("in") == "query" {
                    out.query.push((name, value));
                } else {
                    out.headers.push((name, value));
                }
                Ok(out)
            })),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(v: Value) -> Map<String, Value> {
        match v {
            Value::Object(m) => m,
            _ => unreachable!(),
        }
    }

    #[test]
    fn generic_types_authenticate() {
        let t = CredentialTypes::with_builtins();
        let a = t
            .apply("httpHeaderAuth", &m(json!({"name": "X-K", "value": "v"})))
            .unwrap();
        assert_eq!(a.headers, vec![("X-K".to_owned(), "v".to_owned())]);
        let a = t
            .apply("httpBearerAuth", &m(json!({"token": "tok"})))
            .unwrap();
        assert_eq!(
            a.headers,
            vec![("Authorization".to_owned(), "Bearer tok".to_owned())]
        );
        let a = t
            .apply("httpBasicAuth", &m(json!({"user": "u", "password": "p"})))
            .unwrap();
        assert_eq!(a.basic, Some(("u".into(), "p".into())));
        let a = t
            .apply("httpQueryAuth", &m(json!({"name": "k", "value": "v"})))
            .unwrap();
        assert_eq!(a.query, vec![("k".to_owned(), "v".to_owned())]);
        let a = t
            .apply("httpCustomAuth", &m(json!({"json": "{\"headers\":{\"A\":\"1\"},\"qs\":{\"b\":2},\"body\":{\"c\":3}}"})))
            .unwrap();
        assert_eq!(a.headers, vec![("A".to_owned(), "1".to_owned())]);
        assert_eq!(a.query, vec![("b".to_owned(), "2".to_owned())]);
        assert_eq!(a.body["c"], 3);
        let a = t
            .apply(
                "apiKey",
                &m(json!({"apiKey": "k", "in": "query", "name": "key"})),
            )
            .unwrap();
        assert_eq!(a.query, vec![("key".to_owned(), "k".to_owned())]);
        let a = t
            .apply("apiKey", &m(json!({"apiKey": "k", "prefix": "Bearer "})))
            .unwrap();
        assert_eq!(
            a.headers,
            vec![("X-API-Key".to_owned(), "Bearer k".to_owned())]
        );
        let a = t
            .apply(
                "oAuth2Api",
                &m(json!({"oauthTokenData": {"access_token": "at"}})),
            )
            .unwrap();
        assert_eq!(a.headers[0].1, "Bearer at");
    }

    #[test]
    fn errors_do_not_leak_values() {
        let t = CredentialTypes::with_builtins();
        let e = t
            .apply("httpCustomAuth", &m(json!({"json": "{not json SECRET"})))
            .unwrap_err()
            .to_string();
        assert!(!e.contains("SECRET"));
        assert!(matches!(
            t.apply("httpBearerAuth", &m(json!({}))),
            Err(CredentialError::MissingField(_))
        ));
        assert!(matches!(
            t.apply("nope", &m(json!({}))),
            Err(CredentialError::UnknownType(_))
        ));
        let dbg = format!(
            "{:?}",
            AppliedAuth {
                headers: vec![("a".into(), "SECRET".into())],
                ..AppliedAuth::default()
            }
        );
        assert!(!dbg.contains("SECRET"));
    }

    #[test]
    fn schema_shape() {
        let t = CredentialTypes::with_builtins();
        let s = t.schema("httpBasicAuth").unwrap();
        assert_eq!(s["additionalProperties"], false);
        assert_eq!(s["required"], json!(["user", "password"]));
        assert_eq!(s["properties"]["user"]["type"], "string");
        let s = t.schema("apiKey").unwrap();
        assert_eq!(s["properties"]["in"]["enum"], json!(["header", "query"]));
        assert_eq!(s["required"], json!(["apiKey"]));
        assert!(t.schema("nope").is_none());
    }

    #[test]
    fn extends_merges_parent_properties() {
        let t = CredentialTypes::with_builtins();
        t.register(
            CredentialType::new("myOAuth", "My OAuth")
                .extends("oAuth2Api")
                .property(Prop::hidden("scope", "read")),
        )
        .unwrap();
        let r = t.resolved("myOAuth").unwrap();
        assert!(r.properties.iter().any(|p| p.name == "clientId"));
        assert_eq!(r.properties.iter().filter(|p| p.name == "scope").count(), 1);
        assert!(t.register(CredentialType::new("myOAuth", "x")).is_err());
    }
}
