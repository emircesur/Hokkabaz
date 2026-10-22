//! ntfy (https://ntfy.sh): send a notification. Demo integration for the framework: a
//! credential type plus a declarative node, no hand-written HTTP.
//!
//! Publishes with ntfy's JSON API (`POST <server>/` with `{topic, message, ...}`).

use std::sync::Arc;

use serde_json::{Map, Value};

use hk_flow::{FlowError, NodeRegistry};

use crate::credentials::{
    AppliedAuth, Authenticate, CredentialError, CredentialTest, CredentialType, CredentialTypes,
};
use crate::describe::{Choice, NodeBuilder, Prop, RouteSend, Routing};
use crate::rest::{RestBody, RestNode, RestSpec};
use crate::template::display;

pub const NODE_TYPE: &str = "hokkabaz.ntfy";
pub const CREDENTIAL: &str = "ntfyApi";

pub fn credential_type() -> CredentialType {
    CredentialType::new(CREDENTIAL, "ntfy API")
        .documentation("https://docs.ntfy.sh/publish/#authentication")
        .property(
            Prop::string("serverUrl", "Server URL")
                .default("https://ntfy.sh")
                .description("Base URL of the ntfy server"),
        )
        .property(
            Prop::string("accessToken", "Access Token")
                .password()
                .description("Preferred over user and password"),
        )
        .property(Prop::string("user", "User"))
        .property(Prop::string("password", "Password").password())
        .authenticate(Authenticate::function(|d: &Map<String, Value>| {
            let get = |k: &str| d.get(k).map(display).unwrap_or_default();
            let mut out = AppliedAuth::default();
            if !get("accessToken").is_empty() {
                out.headers.push((
                    "Authorization".into(),
                    format!("Bearer {}", get("accessToken")),
                ));
            } else if !get("user").is_empty() {
                out.basic = Some((get("user"), get("password")));
            }
            Ok::<_, CredentialError>(out)
        }))
        .test(CredentialTest::get(
            "={{$credentials.serverUrl}}",
            "/v1/account",
        ))
}

fn description() -> hk_flow::description::NodeTypeDescription {
    let priority = Prop::options(
        "priority",
        "Priority",
        vec![
            Choice::new("Min", 1),
            Choice::new("Low", 2),
            Choice::new("Default", 3),
            Choice::new("High", 4),
            Choice::new("Max", 5),
        ],
    )
    .default(3)
    .send(RouteSend::body("priority"));

    NodeBuilder::new(NODE_TYPE, "ntfy")
        .description("Send push notifications through ntfy")
        .subtitle("={{ $parameter.operation }}")
        .icon("fa:bell")
        .color("#338574")
        .credential(CREDENTIAL, false)
        .property(Prop::options(
            "resource",
            "Resource",
            vec![Choice::new("Message", "message")],
        ))
        .property(
            Prop::options(
                "operation",
                "Operation",
                vec![
                    Choice::new("Send", "send")
                        .action("Send a notification")
                        .routing(Routing::request("POST", "/")),
                ],
            )
            .show("resource", &["message"]),
        )
        .property(
            Prop::string("topic", "Topic")
                .required()
                .send(RouteSend::body("topic")),
        )
        .property(
            Prop::string("message", "Message")
                .required()
                .send(RouteSend::body("message")),
        )
        .property(Prop::collection(
            "additionalFields",
            "Additional Fields",
            vec![
                Prop::string("title", "Title").send(RouteSend::body("title")),
                priority,
                Prop::string("tags", "Tags")
                    .description("Comma-separated tags or emoji short codes")
                    .send(RouteSend::body("tags")),
                Prop::string("click", "Click URL").send(RouteSend::body("click")),
                Prop::string("icon", "Icon URL").send(RouteSend::body("icon")),
                Prop::string("delay", "Delay")
                    .placeholder("30m")
                    .send(RouteSend::body("delay")),
                Prop::boolean("markdown", "Markdown").send(RouteSend::body("markdown")),
            ],
        ))
        .build()
}

/// Node + credential type into the shared registries. Features: `int-ntfy`.
pub fn register(nodes: &mut NodeRegistry, creds: &CredentialTypes) -> Result<(), FlowError> {
    creds.register_if_absent(credential_type());
    let spec = RestSpec::new("={{$credentials.serverUrl}}")
        .credential(CREDENTIAL, false)
        .error_path("error")
        .pre_send(|req, _ctx, _i| {
            // JSON publish wants tags as an array; the UI field is a comma list
            if let RestBody::Json(Value::Object(o)) = &mut req.body
                && let Some(Value::String(s)) = o.get("tags")
            {
                let tags: Vec<Value> = s
                    .split(',')
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .map(|t| Value::String(t.to_owned()))
                    .collect();
                o.insert("tags".into(), Value::Array(tags));
            }
            Ok(())
        });
    nodes.register(Arc::new(RestNode::new(description(), spec, creds)))
}
