//! n8n node (`n8n-nodes-base.n8n`): call another n8n instance's public API to list, read,
//! activate and deactivate workflows and to list, read and delete executions. Clean-room
//! from the public API docs; the credential `n8nApi` keeps n8n's shape (`apiKey` and
//! `baseUrl` that already includes `/api/v1`) so imported workflows resolve it unchanged.

use std::sync::Arc;

use serde_json::{Map, Value};

use hk_flow::{FlowError, NodeContext, NodeRegistry};

use crate::credentials::{Authenticate, CredentialTest, CredentialType, CredentialTypes};
use crate::describe::{Choice, NodeBuilder, Pagination, Prop, Routing, return_all_and_limit};
use crate::pathparam::{defaults_of, expand, query_from, req_str};
use crate::rest::{RestNode, RestRequest, RestSpec};

pub const NODE_TYPE: &str = "n8n-nodes-base.n8n";
pub const CREDENTIAL: &str = "n8nApi";

pub fn credential_type() -> CredentialType {
    CredentialType::new(CREDENTIAL, "n8n API")
        .documentation("https://docs.n8n.io/api/authentication/")
        .property(Prop::string("apiKey", "API Key").password().required())
        .property(
            Prop::string("baseUrl", "Base URL")
                .required()
                .placeholder("https://<name>.app.n8n.cloud/api/v1")
                .description("The API base of the instance, including /api/v1"),
        )
        .authenticate(Authenticate::header("X-N8N-API-KEY", "={{$credentials.apiKey}}"))
        .test(CredentialTest::get("={{$credentials.baseUrl}}", "/workflows?limit=1"))
}

fn cursor() -> Pagination {
    Pagination::Cursor {
        cursor_param: "cursor".into(),
        next_path: "nextCursor".into(),
        size_param: Some("limit".into()),
        page_size: 100,
    }
}

fn description() -> hk_flow::description::NodeTypeDescription {
    let list = |r: Routing| r.output(Some("data"), &[]).paginate(cursor());
    let wf_ops = vec![
        Choice::new("Activate", "activate")
            .action("Activate a workflow")
            .routing(Routing::request("POST", "/workflows/{workflowId}/activate")),
        Choice::new("Deactivate", "deactivate")
            .action("Deactivate a workflow")
            .routing(Routing::request("POST", "/workflows/{workflowId}/deactivate")),
        Choice::new("Get", "get").action("Get a workflow").routing(Routing::request("GET", "/workflows/{workflowId}")),
        Choice::new("Get Many", "getAll").action("Get many workflows").routing(list(Routing::request("GET", "/workflows"))),
    ];
    let ex_ops = vec![
        Choice::new("Delete", "delete")
            .action("Delete an execution")
            .routing(Routing::request("DELETE", "/executions/{executionId}")),
        Choice::new("Get", "get").action("Get an execution").routing(Routing::request("GET", "/executions/{executionId}")),
        Choice::new("Get Many", "getAll").action("Get many executions").routing(list(Routing::request("GET", "/executions"))),
    ];
    NodeBuilder::new(NODE_TYPE, "n8n")
        .description("Handle events and perform actions on another n8n instance")
        .subtitle("={{$parameter[\"operation\"] + \": \" + $parameter[\"resource\"]}}")
        .icon("fa:network-wired")
        .color("#ff6d5a")
        .credential(CREDENTIAL, true)
        .property(
            Prop::options("resource", "Resource", vec![Choice::new("Workflow", "workflow"), Choice::new("Execution", "execution")])
                .default("workflow"),
        )
        .property(Prop::options("operation", "Operation", wf_ops).default("getAll").show("resource", &["workflow"]))
        .property(Prop::options("operation", "Operation", ex_ops).default("getAll").show("resource", &["execution"]))
        .property(
            Prop::string("workflowId", "Workflow ID")
                .required()
                .show("resource", &["workflow"])
                .show("operation", &["activate", "deactivate", "get"]),
        )
        .property(
            Prop::string("executionId", "Execution ID")
                .required()
                .show("resource", &["execution"])
                .show("operation", &["delete", "get"]),
        )
        .properties(return_all_and_limit(None, &["getAll"]))
        .property(
            Prop::collection(
                "filters",
                "Filters",
                vec![
                    Prop::boolean("active", "Active"),
                    Prop::string("name", "Name"),
                    Prop::string("tags", "Tags").description("Comma-separated tag names"),
                ],
            )
            .show("resource", &["workflow"])
            .show("operation", &["getAll"]),
        )
        .property(
            Prop::collection(
                "filters",
                "Filters",
                vec![
                    Prop::options(
                        "status",
                        "Status",
                        vec![Choice::new("Error", "error"), Choice::new("Success", "success"), Choice::new("Waiting", "waiting")],
                    ),
                    Prop::string("workflowId", "Workflow ID"),
                    Prop::boolean("includeData", "Include Data").default(false),
                ],
            )
            .show("resource", &["execution"])
            .show("operation", &["getAll"]),
        )
        .property(
            Prop::collection("options", "Options", vec![Prop::boolean("includeData", "Include Execution Data").default(false)])
                .show("resource", &["execution"])
                .show("operation", &["get"]),
        )
        .build()
}

fn route_hook(
    defaults: Map<String, Value>,
) -> impl Fn(&mut RestRequest, &NodeContext, usize) -> Result<(), FlowError> + Send + Sync + 'static {
    move |req, ctx, i| {
        let d = &defaults;
        let resource = req_str(ctx, d, "resource", i)?;
        let op = req_str(ctx, d, "operation", i)?;
        expand(req, ctx, d, i)?;
        match (resource.as_str(), op.as_str()) {
            ("workflow", "getAll") => query_from(ctx, i, "filters", &["active", "name", "tags"], req)?,
            ("execution", "getAll") => query_from(ctx, i, "filters", &["status", "workflowId", "includeData"], req)?,
            ("execution", "get") => query_from(ctx, i, "options", &["includeData"], req)?,
            _ => {}
        }
        Ok(())
    }
}

/// Node + credential type into the shared registries. Features: `int-n8n`.
pub fn register(nodes: &mut NodeRegistry, creds: &CredentialTypes) -> Result<(), FlowError> {
    creds.register_if_absent(credential_type());
    let desc = description();
    let defaults = defaults_of(&desc);
    let spec = RestSpec::new("={{$credentials.baseUrl}}")
        .credential(CREDENTIAL, true)
        .header("Accept", "application/json")
        .error_path("message")
        .pre_send(route_hook(defaults));
    nodes.register(Arc::new(RestNode::new(desc, spec, creds)))
}
