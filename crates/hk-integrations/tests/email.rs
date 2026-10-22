//! Email registration and failure paths. No SMTP/IMAP server is involved: message building
//! and parsing are unit-tested in `src/email`.
#![cfg(feature = "int-email")]

mod common;

use std::sync::Arc;

use hk_flow::{Item, Node, NodeContext, NodeRegistry};
use hk_integrations::{CredentialTypes, PollRegistry, register_integrations_with_polling};
use serde_json::{Value, json};

use common::Creds;

fn setup() -> (NodeRegistry, CredentialTypes, PollRegistry) {
    let mut nodes = NodeRegistry::new();
    let creds = CredentialTypes::new();
    let mut polls = PollRegistry::new();
    register_integrations_with_polling(&mut nodes, &creds, &mut polls).unwrap();
    (nodes, creds, polls)
}

#[test]
fn registers_send_trigger_and_credentials() {
    let (nodes, creds, polls) = setup();
    assert!(nodes.latest("n8n-nodes-base.emailSend").is_some());
    assert!(polls.get("n8n-nodes-base.emailReadImap").is_some());
    // the trigger node is also a plain node (manual test run)
    assert!(nodes.latest("n8n-nodes-base.emailReadImap").is_some());
    let smtp = creds.schema("smtp").unwrap();
    assert_eq!(smtp["properties"]["port"]["default"], 465);
    assert_eq!(smtp["properties"]["secure"]["default"], true);
    assert_eq!(smtp["required"], json!(["host"]));
    let imap = creds.schema("imap").unwrap();
    assert_eq!(imap["properties"]["port"]["default"], 993);
    assert_eq!(imap["required"], json!(["host", "user", "password"]));
    // n8n versions resolve exactly
    for v in [1, 2] {
        let n: Node = serde_json::from_value(json!({
            "name": "m", "type": "n8n-nodes-base.emailSend", "typeVersion": v, "position": [0, 0]
        }))
        .unwrap();
        assert!(nodes.is_supported(&n));
    }
}

fn send_ctx(cred: Value, params: Value) -> NodeContext {
    let node: Node = serde_json::from_value(json!({
        "name": "Mail", "type": "n8n-nodes-base.emailSend", "typeVersion": 2.1, "position": [0, 0],
        "parameters": params,
        "credentials": {"smtp": {"id": "c1", "name": "smtp"}}
    }))
    .unwrap();
    let mut c = NodeContext::new(node, vec![vec![Item::from_json(json!({}))]], 0);
    c.hooks.credentials = Some(Arc::new(Creds(cred)));
    c
}

#[tokio::test]
async fn send_failures_are_reported_without_the_password() {
    let (nodes, ..) = setup();
    let n = nodes.latest("n8n-nodes-base.emailSend").unwrap();
    let cred = json!({"host": "127.0.0.1", "port": 1, "secure": false, "disableStartTls": true, "user": "u", "password": "smtp_SECRET"});
    let params = json!({"fromEmail": "a@example.com", "toEmail": "b@example.org", "subject": "s", "emailFormat": "text", "text": "t"});

    let e = n
        .execute(&send_ctx(cred.clone(), params.clone()))
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("Sending the email failed"), "{e}");
    assert!(!e.contains("smtp_SECRET"));

    // bad recipient never reaches the network
    let mut bad = params.clone();
    bad["toEmail"] = json!("nobody");
    let e = n
        .execute(&send_ctx(cred.clone(), bad))
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("Invalid To address"), "{e}");

    // missing attachment property
    let mut att = params;
    att["options"] = json!({"attachments": "data"});
    let e = n
        .execute(&send_ctx(cred, att))
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("no binary property `data`"), "{e}");
}
