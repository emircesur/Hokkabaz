//! S3 node against a local fake: SigV4 headers, path-style routing, XML parsing, paging.
#![cfg(feature = "int-s3")]

mod common;

use std::sync::Arc;

use hk_flow::{Item, Node, NodeContext, NodeRegistry};
use hk_integrations::{CredentialTypes, register_integrations};
use serde_json::{Value, json};

use common::{Creds, http_resp, serve};

fn setup() -> NodeRegistry {
    let mut nodes = NodeRegistry::new();
    register_integrations(&mut nodes, &CredentialTypes::new()).unwrap();
    nodes
}

async fn run(endpoint: &str, params: Value, input: Vec<Item>) -> Result<Vec<Item>, hk_flow::FlowError> {
    let nodes = setup();
    let node: Node = serde_json::from_value(json!({
        "name": "S3", "type": "n8n-nodes-base.s3", "typeVersion": 1, "position": [0, 0],
        "parameters": params,
        "credentials": {"s3": {"id": "c1", "name": "s3"}}
    }))
    .unwrap();
    let mut c = NodeContext::new(node, vec![input], 0);
    let cred = json!({"endpoint": endpoint, "accessKeyId": "AKID", "secretAccessKey": "SECRET", "region": "us-east-1"});
    c.hooks.credentials = Some(Arc::new(Creds(cred)));
    nodes.latest("n8n-nodes-base.s3").unwrap().execute(&c).await.map(|mut o| o.remove(0))
}

fn one() -> Vec<Item> {
    vec![Item::from_json(json!({}))]
}

fn header(req: &str, name: &str) -> Option<String> {
    req.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.eq_ignore_ascii_case(name).then(|| v.trim().to_owned())
    })
}

#[tokio::test]
async fn upload_is_signed_and_path_style() {
    let resp = "HTTP/1.1 200 OK\r\nETag: \"abc123\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned();
    let (url, seen) = serve(vec![resp]).await;
    let params = json!({
        "resource": "file", "operation": "upload", "bucketName": "bkt", "fileName": "dir/a b.txt",
        "binaryData": false, "fileContent": "hello",
        "additionalFields": {"acl": "public-read", "contentType": "text/x-test"}
    });
    let out = run(&url, params, one()).await.unwrap();
    assert_eq!(out[0].json["etag"], "abc123");
    assert_eq!(out[0].json["size"], 5);
    let req = seen.lock().unwrap()[0].clone();
    assert!(req.starts_with("PUT /bkt/dir/a%20b.txt HTTP/1.1"), "{req}");
    let auth = header(&req, "authorization").expect("authorization header");
    assert!(auth.starts_with("AWS4-HMAC-SHA256 Credential=AKID/"), "{auth}");
    assert!(auth.contains("/us-east-1/s3/aws4_request,SignedHeaders="), "{auth}");
    assert!(auth.contains("content-type;host;x-amz-acl;x-amz-content-sha256;x-amz-date"), "{auth}");
    // sha256("hello")
    assert_eq!(header(&req, "x-amz-content-sha256").unwrap(), "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824");
    assert_eq!(header(&req, "x-amz-acl").as_deref(), Some("public-read"));
    assert_eq!(header(&req, "content-type").as_deref(), Some("text/x-test"));
    assert!(header(&req, "x-amz-date").unwrap().ends_with('Z'));
    assert!(req.ends_with("hello"));
    // the secret never goes on the wire
    assert!(!req.contains("SECRET"));
}

#[tokio::test]
async fn list_files_follows_continuation_tokens() {
    let page1 = r#"<?xml version="1.0"?><ListBucketResult><IsTruncated>true</IsTruncated><NextContinuationToken>tok/1</NextContinuationToken>
        <Contents><Key>a.txt</Key><LastModified>2024-01-01T00:00:00.000Z</LastModified><ETag>&quot;e1&quot;</ETag><Size>10</Size><StorageClass>STANDARD</StorageClass></Contents></ListBucketResult>"#;
    let page2 = r#"<ListBucketResult><IsTruncated>false</IsTruncated>
        <Contents><Key>b &amp; c.txt</Key><ETag>&quot;e2&quot;</ETag><Size>7</Size></Contents></ListBucketResult>"#;
    let (url, seen) = serve(vec![http_resp("200 OK", page1), http_resp("200 OK", page2)]).await;
    let params = json!({"resource": "file", "operation": "getAll", "bucketName": "bkt", "returnAll": true, "additionalFields": {"prefix": "logs/"}});
    let out = run(&url, params, one()).await.unwrap();
    let keys: Vec<_> = out.iter().map(|i| i.json["Key"].as_str().unwrap().to_owned()).collect();
    assert_eq!(keys, ["a.txt", "b & c.txt"]);
    assert_eq!(out[0].json["ETag"], "e1");
    assert_eq!(out[0].json["Size"], 10);
    let reqs = seen.lock().unwrap().clone();
    assert!(reqs[0].starts_with("GET /bkt/?list-type=2&max-keys=1000&prefix=logs%2F HTTP/1.1"), "{}", reqs[0]);
    assert!(reqs[1].contains("continuation-token=tok%2F1"), "{}", reqs[1]);
}

#[tokio::test]
async fn buckets_download_and_errors() {
    let buckets = r#"<ListAllMyBucketsResult><Buckets><Bucket><Name>one</Name><CreationDate>2024-01-01T00:00:00.000Z</CreationDate></Bucket><Bucket><Name>two</Name><CreationDate>x</CreationDate></Bucket></Buckets></ListAllMyBucketsResult>"#;
    let (url, seen) = serve(vec![http_resp("200 OK", buckets)]).await;
    let out = run(&url, json!({"resource": "bucket", "operation": "getAll", "returnAll": true}), one()).await.unwrap();
    assert_eq!(out.iter().map(|i| i.json["Name"].as_str().unwrap()).collect::<Vec<_>>(), ["one", "two"]);
    assert!(seen.lock().unwrap()[0].starts_with("GET / HTTP/1.1"));

    let raw = "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc".to_owned();
    let (url, _) = serve(vec![raw]).await;
    let params = json!({"resource": "file", "operation": "download", "bucketName": "bkt", "fileKey": "img/x.png"});
    let out = run(&url, params, one()).await.unwrap();
    let bin = &out[0].binary.as_ref().unwrap()["data"];
    assert_eq!(bin.data, "YWJj");
    assert_eq!(bin.mime_type, "image/png");
    assert_eq!(bin.file_name.as_deref(), Some("x.png"));

    let denied = r#"<Error><Code>NoSuchBucket</Code><Message>The specified bucket does not exist</Message></Error>"#;
    let (url, _) = serve(vec![http_resp("404 Not Found", denied)]).await;
    let err = run(&url, json!({"resource": "file", "operation": "delete", "bucketName": "nope", "fileKey": "k"}), one()).await.unwrap_err();
    assert!(err.to_string().contains("NoSuchBucket: The specified bucket does not exist"), "{err}");
}

#[tokio::test]
async fn copy_sends_the_encoded_source_header() {
    let (url, seen) = serve(vec![http_resp("200 OK", "<CopyObjectResult><ETag>&quot;e&quot;</ETag><LastModified>t</LastModified></CopyObjectResult>")]).await;
    let params = json!({"resource": "file", "operation": "copy", "sourcePath": "/src/a b.txt", "destinationPath": "/dst/copy.txt"});
    let out = run(&url, params, one()).await.unwrap();
    assert_eq!(out[0].json["etag"], "e");
    let req = seen.lock().unwrap()[0].clone();
    assert!(req.starts_with("PUT /dst/copy.txt HTTP/1.1"), "{req}");
    assert_eq!(header(&req, "x-amz-copy-source").as_deref(), Some("/src/a%20b.txt"));
}
