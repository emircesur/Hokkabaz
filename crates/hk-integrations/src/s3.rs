//! S3 / MinIO node (`n8n-nodes-base.s3`): bucket create, delete, getAll; file copy, delete,
//! download, getAll, upload. Folder operations are not implemented.
//!
//! Requests are signed by [`crate::sigv4`] and sent with reqwest (rustls). Addressing: a
//! custom endpoint (MinIO, R2, Spaces, ...) always uses path style
//! (`<endpoint>/<bucket>/<key>`); against AWS the bucket goes in the host unless
//! `forcePathStyle` is set or the bucket name contains a dot (TLS wildcard mismatch).

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use base64::Engine;
use reqwest::Url;
use reqwest::header::HeaderMap;
use serde_json::{Map, Value, json};

use hk_flow::description::NodeTypeDescription;
use hk_flow::item::BinaryData;
use hk_flow::{FlowError, FlowNode, Item, NodeContext, NodeOutput, NodeRegistry};

use crate::credentials::{CredentialType, CredentialTypes};
use crate::describe::{Choice, NodeBuilder, Prop, return_all_and_limit};
use crate::kit::{self, node_err};
use crate::sigv4::{self, Keys, uri_encode};

pub const NODE_TYPE: &str = "n8n-nodes-base.s3";
pub const CREDENTIAL: &str = "s3";

const LIST_PAGE: usize = 1000;

pub fn credential_type() -> CredentialType {
    CredentialType::new(CREDENTIAL, "S3")
        .documentation(
            "https://docs.aws.amazon.com/IAM/latest/UserGuide/id_credentials_access-keys.html",
        )
        .property(Prop::string("region", "Region").default("us-east-1"))
        .property(Prop::string("accessKeyId", "Access Key ID").required())
        .property(
            Prop::string("secretAccessKey", "Secret Access Key")
                .password()
                .required(),
        )
        .property(
            Prop::string("sessionToken", "Session Token")
                .password()
                .description("Only for temporary credentials"),
        )
        .property(
            Prop::string("endpoint", "Endpoint")
                .placeholder("https://minio.example.com")
                .description(
                    "Custom S3-compatible endpoint. Empty: AWS (https://s3.<region>.amazonaws.com)",
                ),
        )
        .property(
            Prop::boolean("forcePathStyle", "Force Path Style")
                .description("Address buckets as <endpoint>/<bucket> also on AWS"),
        )
}

fn description() -> NodeTypeDescription {
    let bucket = |p: Prop| p.show("resource", &["file"]);
    let mut d = NodeBuilder::new(NODE_TYPE, "S3")
        .description("Sends data to any S3-compatible service")
        .subtitle("={{ $parameter.operation + \": \" + $parameter.resource }}")
        .icon("fa:hdd")
        .color("#ff9900")
        .credential(CREDENTIAL, true)
        .property(
            Prop::options(
                "resource",
                "Resource",
                vec![Choice::new("Bucket", "bucket"), Choice::new("File", "file")],
            )
            .default("file"),
        )
        .property(
            Prop::options(
                "operation",
                "Operation",
                vec![
                    Choice::new("Create", "create").action("Create a bucket"),
                    Choice::new("Delete", "delete").action("Delete a bucket"),
                    Choice::new("Get Many", "getAll").action("Get many buckets"),
                ],
            )
            .show("resource", &["bucket"]),
        )
        .property(
            Prop::options(
                "operation",
                "Operation",
                vec![
                    Choice::new("Copy", "copy").action("Copy a file"),
                    Choice::new("Delete", "delete").action("Delete a file"),
                    Choice::new("Download", "download").action("Download a file"),
                    Choice::new("Get Many", "getAll").action("Get many files"),
                    Choice::new("Upload", "upload").action("Upload a file"),
                ],
            )
            .default("upload")
            .show("resource", &["file"]),
        )
        .property(
            Prop::string("name", "Name")
                .required()
                .description("Bucket name")
                .show("resource", &["bucket"])
                .show("operation", &["create", "delete"]),
        )
        .property(
            Prop::string("sourcePath", "Source Path")
                .required()
                .placeholder("/bucket/path/to/file.ext")
                .show("resource", &["file"])
                .show("operation", &["copy"]),
        )
        .property(
            Prop::string("destinationPath", "Destination Path")
                .required()
                .placeholder("/bucket/path/to/copy.ext")
                .show("resource", &["file"])
                .show("operation", &["copy"]),
        )
        .property(
            bucket(Prop::string("bucketName", "Bucket Name").required())
                .show("operation", &["delete", "download", "getAll", "upload"]),
        )
        .property(
            bucket(Prop::string("fileKey", "File Key").required())
                .show("operation", &["delete", "download"]),
        )
        .property(
            bucket(
                Prop::string("fileName", "File Name")
                    .required()
                    .description("Object key to write"),
            )
            .show("operation", &["upload"]),
        )
        .property(
            bucket(Prop::boolean("binaryData", "Binary File").default(true))
                .show("operation", &["upload"]),
        )
        .property(
            bucket(Prop::string("fileContent", "File Content"))
                .show("operation", &["upload"])
                .show_bool("binaryData", false),
        )
        .property(
            bucket(Prop::string("binaryPropertyName", "Input Binary Field").default("data"))
                .show("operation", &["upload"])
                .show_bool("binaryData", true),
        )
        .property(
            bucket(Prop::string("binaryPropertyName", "Put Output File in Field").default("data"))
                .show("operation", &["download"]),
        )
        .property(Prop::collection(
            "additionalFields",
            "Additional Fields",
            vec![
                Prop::string("region", "Region")
                    .description("Bucket region for Create (default: the credential's)"),
                Prop::string("acl", "ACL").description("Canned ACL, e.g. private, public-read"),
                Prop::string("storageClass", "Storage Class"),
                Prop::string("contentType", "Content Type")
                    .description("Upload only; default: the binary's MIME type"),
                Prop::string("prefix", "Prefix")
                    .description("Get Many Files: only keys starting with this"),
            ],
        ));
    let all = return_all_and_limit(None, &["getAll"]);
    d = d.properties(all);
    let mut d = d.build();
    kit::serve_versions(&mut d, &[1.0]);
    d
}

// ------------------------------------------------------------------------------- XML

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Inner text of each `<tag>..</tag>` in `xml` (S3 responses carry no attributes on these).
fn blocks<'a>(xml: &'a str, tag: &str) -> Vec<&'a str> {
    let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(a) = rest.find(&open) {
        let after = &rest[a + open.len()..];
        let Some(b) = after.find(&close) else { break };
        out.push(&after[..b]);
        rest = &after[b + close.len()..];
    }
    out
}

fn text(xml: &str, tag: &str) -> Option<String> {
    blocks(xml, tag).first().map(|t| unescape(t))
}

// ------------------------------------------------------------------------------- client

struct Reply {
    headers: HeaderMap,
    body: Vec<u8>,
}

struct S3 {
    http: reqwest::Client,
    node: String,
    keys: Keys,
    endpoint: Url,
    custom: bool,
    force_path: bool,
}

impl S3 {
    fn new(node: &str, cred: &Map<String, Value>) -> Result<Self, FlowError> {
        let get = |k: &str| kit::cred_str(cred, k);
        let region = if get("region").is_empty() {
            "us-east-1".to_owned()
        } else {
            get("region")
        };
        let custom = !get("endpoint").trim().is_empty();
        let raw = if custom {
            get("endpoint").trim().to_owned()
        } else {
            format!("https://s3.{region}.amazonaws.com")
        };
        let endpoint =
            Url::parse(&raw).map_err(|_| kit::err(node, "The endpoint is not a valid URL"))?;
        if !matches!(endpoint.scheme(), "http" | "https") || endpoint.host_str().is_none() {
            return Err(kit::err(node, "The endpoint must be an http(s) URL"));
        }
        let (access_key, secret_key) = (get("accessKeyId"), get("secretAccessKey"));
        if access_key.is_empty() || secret_key.is_empty() {
            return Err(kit::err(
                node,
                "The credential needs an access key ID and a secret access key",
            ));
        }
        Ok(Self {
            http: kit::client(node, Duration::from_secs(300))?,
            node: node.to_owned(),
            keys: Keys {
                access_key,
                secret_key,
                region,
                session_token: Some(get("sessionToken")).filter(|t| !t.is_empty()),
            },
            endpoint,
            custom,
            force_path: matches!(cred.get("forcePathStyle"), Some(Value::Bool(true))),
        })
    }

    /// Canonical URL for `bucket` / `key` with a (pre-sorted not required) query.
    fn url(
        &self,
        bucket: Option<&str>,
        key: Option<&str>,
        query: &[(&str, String)],
    ) -> Result<Url, FlowError> {
        let scheme = self.endpoint.scheme();
        let host = self.endpoint.host_str().unwrap_or_default();
        let port = self
            .endpoint
            .port()
            .map(|p| format!(":{p}"))
            .unwrap_or_default();
        let prefix = self.endpoint.path().trim_end_matches('/');
        let virtual_host =
            bucket.is_some_and(|b| !self.custom && !self.force_path && !b.contains('.'));
        let (host, path) = match bucket {
            Some(b) if virtual_host => (format!("{b}.{host}"), String::new()),
            Some(b) => (host.to_owned(), format!("/{}", uri_encode(b, false))),
            None => (host.to_owned(), String::new()),
        };
        let key_part = key
            .map(|k| format!("/{}", uri_encode(k.trim_start_matches('/'), true)))
            .unwrap_or_default();
        let mut full = format!("{scheme}://{host}{port}{prefix}{path}{key_part}");
        if key_part.is_empty() {
            full.push('/');
        }
        if !query.is_empty() {
            let q: Vec<String> = query
                .iter()
                .map(|(k, v)| format!("{}={}", uri_encode(k, false), uri_encode(v, false)))
                .collect();
            full.push('?');
            full.push_str(&q.join("&"));
        }
        Url::parse(&full).map_err(|_| kit::err(&self.node, "Could not build the request URL"))
    }

    async fn send(
        &self,
        method: &str,
        url: Url,
        extra: Vec<(String, String)>,
        body: Vec<u8>,
    ) -> Result<Reply, FlowError> {
        let date = sigv4::amz_date(SystemTime::now());
        let signed = sigv4::sign(&self.keys, "s3", method, &url, &extra, &body, &date);
        let m = reqwest::Method::from_bytes(method.as_bytes())
            .map_err(|_| kit::err(&self.node, "bad method"))?;
        let mut rb = self.http.request(m, url);
        for (k, v) in extra.iter().chain(signed.iter()) {
            rb = rb.header(k.as_str(), v.as_str());
        }
        if !body.is_empty() {
            rb = rb.body(body);
        }
        let res = rb.send().await.map_err(|e| {
            kit::err(
                &self.node,
                format!("The request failed: {}", e.without_url()),
            )
        })?;
        let status = res.status();
        let headers = res.headers().clone();
        let body = res
            .bytes()
            .await
            .map_err(|e| {
                kit::err(
                    &self.node,
                    format!("Could not read the response: {}", e.without_url()),
                )
            })?
            .to_vec();
        if status.is_success() {
            return Ok(Reply { headers, body });
        }
        let xml = String::from_utf8_lossy(&body);
        let mut msg = format!("S3 returned {}", status.as_u16());
        if let Some(code) = text(&xml, "Code") {
            msg.push_str(&format!(": {code}"));
            if let Some(m) = text(&xml, "Message") {
                msg.push_str(&format!(": {}", m.chars().take(300).collect::<String>()));
            }
        }
        if matches!(status.as_u16(), 401 | 403) {
            msg.push_str(" (check the credentials)");
        }
        Err(kit::err(&self.node, msg))
    }
}

// ------------------------------------------------------------------------------- node

/// `/bucket/key/with/slashes` -> (bucket, key).
fn split_path(p: &str) -> Option<(String, String)> {
    let (b, k) = p.trim_start_matches('/').split_once('/')?;
    (!b.is_empty() && !k.is_empty()).then(|| (b.to_owned(), k.to_owned()))
}

#[derive(Debug)]
struct S3Node {
    desc: NodeTypeDescription,
    creds: CredentialTypes,
}

impl S3Node {
    fn req(
        &self,
        ctx: &NodeContext,
        name: &str,
        label: &str,
        i: usize,
    ) -> Result<String, FlowError> {
        let v = kit::str_param(ctx, &self.desc, name, i)?;
        if v.is_empty() {
            return Err(node_err(ctx, format!("Parameter '{label}' is required")));
        }
        Ok(v)
    }

    fn limit(&self, ctx: &NodeContext, i: usize) -> Result<usize, FlowError> {
        if kit::bool_param(ctx, &self.desc, "returnAll", i)? {
            return Ok(usize::MAX);
        }
        Ok(kit::param(ctx, &self.desc, "limit", i)?
            .and_then(|v| v.as_u64())
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n > 0)
            .unwrap_or(50))
    }

    async fn run_item(&self, ctx: &NodeContext, s3: &S3, i: usize) -> Result<Vec<Item>, FlowError> {
        let resource = kit::str_param(ctx, &self.desc, "resource", i)?;
        let op = kit::str_param(ctx, &self.desc, "operation", i)?;
        match (resource.as_str(), op.as_str()) {
            ("bucket", "create") => {
                let name = self.req(ctx, "name", "Name", i)?;
                let region = kit::field_str(ctx, "additionalFields.region", i)?
                    .unwrap_or_else(|| s3.keys.region.clone());
                let body = if region == "us-east-1" {
                    Vec::new()
                } else {
                    format!(
                        "<CreateBucketConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><LocationConstraint>{}</LocationConstraint></CreateBucketConfiguration>",
                        region.replace('&', "&amp;").replace('<', "&lt;")
                    )
                    .into_bytes()
                };
                s3.send("PUT", s3.url(Some(&name), None, &[])?, Vec::new(), body)
                    .await?;
                Ok(vec![Item::from_json(
                    json!({ "success": true, "bucket": name }),
                )])
            }
            ("bucket", "delete") => {
                let name = self.req(ctx, "name", "Name", i)?;
                s3.send(
                    "DELETE",
                    s3.url(Some(&name), None, &[])?,
                    Vec::new(),
                    Vec::new(),
                )
                .await?;
                Ok(vec![Item::from_json(
                    json!({ "success": true, "bucket": name }),
                )])
            }
            ("bucket", "getAll") => {
                let limit = self.limit(ctx, i)?;
                let r = s3
                    .send("GET", s3.url(None, None, &[])?, Vec::new(), Vec::new())
                    .await?;
                let xml = String::from_utf8_lossy(&r.body);
                Ok(blocks(&xml, "Bucket")
                    .into_iter()
                    .take(limit)
                    .map(|b| {
                        Item::from_json(json!({
                            "Name": text(b, "Name").unwrap_or_default(),
                            "CreationDate": text(b, "CreationDate").unwrap_or_default(),
                        }))
                    })
                    .collect())
            }
            ("file", "copy") => {
                let src = self.req(ctx, "sourcePath", "Source Path", i)?;
                let dst = self.req(ctx, "destinationPath", "Destination Path", i)?;
                let (sb, sk) = split_path(&src)
                    .ok_or_else(|| node_err(ctx, "Source Path must look like /bucket/key"))?;
                let (db, dk) = split_path(&dst)
                    .ok_or_else(|| node_err(ctx, "Destination Path must look like /bucket/key"))?;
                let copy_source = format!("/{}/{}", uri_encode(&sb, false), uri_encode(&sk, true));
                let r = s3
                    .send(
                        "PUT",
                        s3.url(Some(&db), Some(&dk), &[])?,
                        vec![("x-amz-copy-source".into(), copy_source)],
                        Vec::new(),
                    )
                    .await?;
                let xml = String::from_utf8_lossy(&r.body);
                Ok(vec![Item::from_json(json!({
                    "success": true, "bucket": db, "key": dk,
                    "etag": text(&xml, "ETag").map(|e| e.trim_matches('"').to_owned()),
                    "lastModified": text(&xml, "LastModified"),
                }))])
            }
            ("file", "delete") => {
                let (bucket, key) = (
                    self.req(ctx, "bucketName", "Bucket Name", i)?,
                    self.req(ctx, "fileKey", "File Key", i)?,
                );
                s3.send(
                    "DELETE",
                    s3.url(Some(&bucket), Some(&key), &[])?,
                    Vec::new(),
                    Vec::new(),
                )
                .await?;
                Ok(vec![Item::from_json(
                    json!({ "success": true, "bucket": bucket, "key": key }),
                )])
            }
            ("file", "download") => {
                let (bucket, key) = (
                    self.req(ctx, "bucketName", "Bucket Name", i)?,
                    self.req(ctx, "fileKey", "File Key", i)?,
                );
                let prop = kit::str_param(ctx, &self.desc, "binaryPropertyName", i)?;
                let prop = if prop.is_empty() {
                    "data".to_owned()
                } else {
                    prop
                };
                let r = s3
                    .send(
                        "GET",
                        s3.url(Some(&bucket), Some(&key), &[])?,
                        Vec::new(),
                        Vec::new(),
                    )
                    .await?;
                let header = |h: &str| {
                    r.headers
                        .get(h)
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_owned)
                };
                let name = key.rsplit('/').next().unwrap_or(&key).to_owned();
                let mime =
                    header("content-type").unwrap_or_else(|| "application/octet-stream".into());
                let mut item = Item::from_json(json!({
                    "bucket": bucket, "key": key, "size": r.body.len(),
                    "contentType": mime, "etag": header("etag").map(|e| e.trim_matches('"').to_owned()),
                }));
                let bin = BinaryData {
                    data: base64::engine::general_purpose::STANDARD.encode(&r.body),
                    mime_type: mime,
                    file_extension: name.rsplit_once('.').map(|(_, e)| e.to_owned()),
                    file_name: Some(name),
                    ..BinaryData::default()
                };
                item.binary = Some(std::iter::once((prop, bin)).collect());
                Ok(vec![item])
            }
            ("file", "getAll") => {
                let bucket = self.req(ctx, "bucketName", "Bucket Name", i)?;
                let limit = self.limit(ctx, i)?;
                let prefix = kit::field_str(ctx, "additionalFields.prefix", i)?;
                let mut out: Vec<Item> = Vec::new();
                let mut token: Option<String> = None;
                loop {
                    let mut q: Vec<(&str, String)> = vec![
                        ("list-type", "2".into()),
                        (
                            "max-keys",
                            limit
                                .saturating_sub(out.len())
                                .clamp(1, LIST_PAGE)
                                .to_string(),
                        ),
                    ];
                    if let Some(p) = &prefix {
                        q.push(("prefix", p.clone()));
                    }
                    if let Some(t) = &token {
                        q.push(("continuation-token", t.clone()));
                    }
                    let r = s3
                        .send(
                            "GET",
                            s3.url(Some(&bucket), None, &q)?,
                            Vec::new(),
                            Vec::new(),
                        )
                        .await?;
                    let xml = String::from_utf8_lossy(&r.body);
                    for c in blocks(&xml, "Contents") {
                        out.push(Item::from_json(json!({
                            "Key": text(c, "Key").unwrap_or_default(),
                            "LastModified": text(c, "LastModified").unwrap_or_default(),
                            "ETag": text(c, "ETag").unwrap_or_default().trim_matches('"'),
                            "Size": text(c, "Size").and_then(|s| s.parse::<u64>().ok()).unwrap_or(0),
                            "StorageClass": text(c, "StorageClass").unwrap_or_default(),
                        })));
                    }
                    token = text(&xml, "NextContinuationToken");
                    if out.len() >= limit
                        || text(&xml, "IsTruncated").as_deref() != Some("true")
                        || token.is_none()
                    {
                        break;
                    }
                }
                out.truncate(limit);
                Ok(out)
            }
            ("file", "upload") => {
                let (bucket, key) = (
                    self.req(ctx, "bucketName", "Bucket Name", i)?,
                    self.req(ctx, "fileName", "File Name", i)?,
                );
                let (data, mut mime) = if kit::bool_param(ctx, &self.desc, "binaryData", i)? {
                    let prop = kit::str_param(ctx, &self.desc, "binaryPropertyName", i)?;
                    let item = ctx
                        .input
                        .first()
                        .and_then(|p| p.get(i))
                        .cloned()
                        .unwrap_or_default();
                    let f =
                        kit::binary_file(ctx, &item, if prop.is_empty() { "data" } else { &prop })?;
                    (f.data, f.mime)
                } else {
                    (
                        kit::str_param(ctx, &self.desc, "fileContent", i)?.into_bytes(),
                        "text/plain".into(),
                    )
                };
                if let Some(ct) = kit::field_str(ctx, "additionalFields.contentType", i)? {
                    mime = ct;
                }
                let mut extra = vec![("content-type".to_owned(), mime)];
                if let Some(acl) = kit::field_str(ctx, "additionalFields.acl", i)? {
                    extra.push(("x-amz-acl".into(), acl));
                }
                if let Some(sc) = kit::field_str(ctx, "additionalFields.storageClass", i)? {
                    extra.push(("x-amz-storage-class".into(), sc));
                }
                let size = data.len();
                let r = s3
                    .send("PUT", s3.url(Some(&bucket), Some(&key), &[])?, extra, data)
                    .await?;
                let etag = r
                    .headers
                    .get("etag")
                    .and_then(|v| v.to_str().ok())
                    .map(|e| e.trim_matches('"').to_owned());
                Ok(vec![Item::from_json(
                    json!({ "success": true, "bucket": bucket, "key": key, "size": size, "etag": etag }),
                )])
            }
            (r, o) => Err(node_err(
                ctx,
                format!("Unsupported operation `{o}` for resource `{r}`"),
            )),
        }
    }
}

#[async_trait]
impl FlowNode for S3Node {
    fn description(&self) -> &NodeTypeDescription {
        &self.desc
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let input_len = ctx.input.first().map_or(0, Vec::len);
        if input_len == 0 {
            return Ok(vec![Vec::new()]);
        }
        let cred = kit::credential(ctx, &self.creds, CREDENTIAL).await?;
        let s3 = S3::new(&ctx.node.name, &cred)?;
        let mut out = Vec::new();
        for i in 0..input_len {
            let res = self.run_item(ctx, &s3, i).await;
            kit::settle(ctx, i, res, &mut out)?;
        }
        Ok(vec![out])
    }
}

/// Node and credential type into the shared registries. Features: `int-s3`.
pub fn register(nodes: &mut NodeRegistry, creds: &CredentialTypes) -> Result<(), FlowError> {
    creds.register_if_absent(credential_type());
    nodes.register(Arc::new(S3Node {
        desc: description(),
        creds: creds.clone(),
    }))?;
    // n8n's own name for the same node
    let mut aws = description();
    aws.name = "n8n-nodes-base.awsS3".to_owned();
    aws.display_name = "AWS S3".to_owned();
    nodes.register(Arc::new(S3Node {
        desc: aws,
        creds: creds.clone(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(cred: Value) -> S3 {
        S3::new("S3", cred.as_object().unwrap()).unwrap()
    }

    #[test]
    fn addressing_styles() {
        let aws =
            client(json!({"accessKeyId": "a", "secretAccessKey": "b", "region": "eu-west-1"}));
        assert_eq!(
            aws.url(Some("bkt"), Some("a b/c.txt"), &[])
                .unwrap()
                .as_str(),
            "https://bkt.s3.eu-west-1.amazonaws.com/a%20b/c.txt"
        );
        assert_eq!(
            aws.url(Some("my.bkt"), None, &[]).unwrap().as_str(),
            "https://s3.eu-west-1.amazonaws.com/my.bkt/"
        );
        assert_eq!(
            aws.url(None, None, &[]).unwrap().as_str(),
            "https://s3.eu-west-1.amazonaws.com/"
        );
        let minio = client(
            json!({"accessKeyId": "a", "secretAccessKey": "b", "endpoint": "http://127.0.0.1:9000"}),
        );
        let u = minio
            .url(
                Some("bkt"),
                Some("k"),
                &[("list-type", "2".into()), ("prefix", "a/b".into())],
            )
            .unwrap();
        assert_eq!(
            u.as_str(),
            "http://127.0.0.1:9000/bkt/k?list-type=2&prefix=a%2Fb"
        );
    }

    #[test]
    fn xml_helpers() {
        let xml = "<R><Contents><Key>a&amp;b</Key><Size>3</Size></Contents><Contents><Key>c</Key></Contents></R>";
        let b = blocks(xml, "Contents");
        assert_eq!(b.len(), 2);
        assert_eq!(text(b[0], "Key").as_deref(), Some("a&b"));
        assert_eq!(
            split_path("/bkt/dir/file.txt"),
            Some(("bkt".into(), "dir/file.txt".into()))
        );
        assert_eq!(split_path("/bkt"), None);
    }
}
