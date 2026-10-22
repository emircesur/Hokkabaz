//! Gitea / Forgejo (`hokkabaz.gitea`): issues, repositories, files and releases over the
//! `/api/v1` REST API. Forgejo is API compatible, so one node serves both.
//!
//! Same shape as the GitHub node (see `github/mod.rs`): a [`RestNode`] driven by route
//! templates plus a `pre_send` hook; file edit/delete wrap in [`ShaFirst`]. Differences
//! that matter: files are created with POST and updated with PUT, issue labels are numeric
//! ids, and list endpoints page with `page`/`limit` (servers cap `limit`, 50 by default).

use std::sync::Arc;

use base64::Engine as _;
use serde_json::{Map, Value, json};

use hk_flow::{FlowError, NodeContext, NodeRegistry};

use crate::credentials::{Authenticate, CredentialTest, CredentialType, CredentialTypes};
use crate::describe::{Choice, NodeBuilder, Pagination, Prop, Routing, return_all_and_limit};
use crate::pathparam::{
    ShaFirst, collect_into, defaults_of, expand, nested_str, node_err, opt_str, person, put, query_from, req_str,
    string_list,
};
use crate::rest::{RestBody, RestNode, RestRequest, RestSpec};

pub const NODE_TYPE: &str = "hokkabaz.gitea";
pub const CREDENTIAL: &str = "giteaApi";

pub fn credential_type() -> CredentialType {
    CredentialType::new(CREDENTIAL, "Gitea API")
        .documentation("https://docs.gitea.com/development/api-usage#generating-and-listing-api-tokens")
        .property(
            Prop::string("server", "Server URL")
                .required()
                .placeholder("https://gitea.example.com")
                .description("Base URL of the Gitea or Forgejo instance, without /api/v1"),
        )
        .property(Prop::string("accessToken", "Access Token").password().required())
        .authenticate(Authenticate::header("Authorization", "=token {{$credentials.accessToken}}"))
        .test(CredentialTest::get("={{$credentials.server}}", "/api/v1/user"))
}

type Op = (&'static str, &'static str, &'static str, &'static str, &'static str, &'static str, bool);

const OPS: &[Op] = &[
    ("issue", "create", "Create", "Create an issue", "POST", "/api/v1/repos/{owner}/{repository}/issues", false),
    ("issue", "createComment", "Create Comment", "Comment on an issue", "POST", "/api/v1/repos/{owner}/{repository}/issues/{issueNumber}/comments", false),
    ("issue", "edit", "Edit", "Edit an issue", "PATCH", "/api/v1/repos/{owner}/{repository}/issues/{issueNumber}", false),
    ("issue", "get", "Get", "Get an issue", "GET", "/api/v1/repos/{owner}/{repository}/issues/{issueNumber}", false),
    ("issue", "getAll", "Get Many", "Get many issues of a repository", "GET", "/api/v1/repos/{owner}/{repository}/issues", true),
    ("issue", "lock", "Lock", "Lock an issue", "PUT", "/api/v1/repos/{owner}/{repository}/issues/{issueNumber}/lock", false),
    ("repository", "get", "Get", "Get a repository", "GET", "/api/v1/repos/{owner}/{repository}", false),
    ("repository", "getBranches", "Get Branches", "List the branches of a repository", "GET", "/api/v1/repos/{owner}/{repository}/branches", true),
    ("repository", "getPullRequests", "Get Pull Requests", "List the pull requests of a repository", "GET", "/api/v1/repos/{owner}/{repository}/pulls", true),
    ("file", "get", "Get", "Get a file", "GET", "/api/v1/repos/{owner}/{repository}/contents/{+filePath}", false),
    ("file", "create", "Create", "Create a file", "POST", "/api/v1/repos/{owner}/{repository}/contents/{+filePath}", false),
    ("file", "edit", "Edit", "Edit a file", "PUT", "/api/v1/repos/{owner}/{repository}/contents/{+filePath}", false),
    ("file", "delete", "Delete", "Delete a file", "DELETE", "/api/v1/repos/{owner}/{repository}/contents/{+filePath}", false),
    ("file", "list", "List", "List the entries of a directory", "GET", "/api/v1/repos/{owner}/{repository}/contents/{+filePath}", false),
    ("release", "create", "Create", "Create a release", "POST", "/api/v1/repos/{owner}/{repository}/releases", false),
    ("release", "get", "Get", "Get a release", "GET", "/api/v1/repos/{owner}/{repository}/releases/{releaseId}", false),
    ("release", "getAll", "Get Many", "Get many releases", "GET", "/api/v1/repos/{owner}/{repository}/releases", true),
    ("release", "update", "Update", "Update a release", "PATCH", "/api/v1/repos/{owner}/{repository}/releases/{releaseId}", false),
    ("release", "delete", "Delete", "Delete a release", "DELETE", "/api/v1/repos/{owner}/{repository}/releases/{releaseId}", false),
];

const RESOURCES: &[(&str, &str)] =
    &[("Issue", "issue"), ("Repository", "repository"), ("File", "file"), ("Release", "release")];

fn operation_prop(resource: &str) -> Prop {
    let choices: Vec<Choice> = OPS
        .iter()
        .filter(|o| o.0 == resource)
        .map(|&(res, value, label, action, method, url, list)| {
            let mut r = Routing::request(method, url);
            // an empty output path is "the whole response", which marks list operations
            if list || (res, value) == ("file", "list") {
                r = r.output(Some(""), &[]);
            }
            if list {
                r = r.paginate(Pagination::Page {
                    page_param: "page".into(),
                    size_param: "limit".into(),
                    page_size: 50,
                    first_page: 1,
                });
            }
            Choice::new(label, value).action(action).routing(r)
        })
        .collect();
    let first = OPS.iter().find(|o| o.0 == resource).map_or("", |o| o.1);
    Prop::options("operation", "Operation", choices).default(first).show("resource", &[resource])
}

fn release_fields(name: &str, label: &str, with_tag: bool) -> Prop {
    let mut f = vec![
        Prop::string("name", "Title"),
        Prop::string("body", "Note"),
        Prop::boolean("draft", "Draft").default(false),
        Prop::boolean("prerelease", "Prerelease").default(false),
        Prop::string("target_commitish", "Target Commitish"),
    ];
    if with_tag {
        f.push(Prop::string("tag_name", "Tag Name"));
    }
    Prop::collection(name, label, f)
}

fn description() -> hk_flow::description::NodeTypeDescription {
    let mut b = NodeBuilder::new(NODE_TYPE, "Gitea")
        .description("Work with Gitea and Forgejo repositories")
        .subtitle("={{$parameter[\"operation\"] + \": \" + $parameter[\"resource\"]}}")
        .icon("fa:code-branch")
        .color("#609926")
        .credential(CREDENTIAL, true)
        .property(
            Prop::options("resource", "Resource", RESOURCES.iter().map(|(n, v)| Choice::new(n, *v)).collect())
                .default("issue"),
        );
    for (_, r) in RESOURCES {
        b = b.property(operation_prop(r));
    }
    b.property(Prop::string("owner", "Repository Owner").required())
        .property(Prop::string("repository", "Repository Name").required())
        .property(
            Prop::number("issueNumber", "Issue Number")
                .required()
                .show("resource", &["issue"])
                .show("operation", &["createComment", "edit", "get", "lock"]),
        )
        .property(Prop::string("title", "Title").required().show("resource", &["issue"]).show("operation", &["create"]))
        .property(Prop::string("body", "Body").show("resource", &["issue"]).show("operation", &["create"]))
        .property(Prop::string("body", "Comment").required().show("resource", &["issue"]).show("operation", &["createComment"]))
        .property(
            Prop::string("labels", "Label IDs")
                .description("Comma-separated numeric label ids")
                .show("resource", &["issue"])
                .show("operation", &["create"]),
        )
        .property(
            Prop::string("assignees", "Assignees")
                .description("Comma-separated user logins")
                .show("resource", &["issue"])
                .show("operation", &["create"]),
        )
        .property(
            Prop::collection(
                "editFields",
                "Edit Fields",
                vec![
                    Prop::string("title", "Title"),
                    Prop::string("body", "Body"),
                    Prop::options("state", "State", vec![Choice::new("Open", "open"), Choice::new("Closed", "closed")]),
                    Prop::string("assignees", "Assignees").description("Comma-separated user logins"),
                ],
            )
            .show("resource", &["issue"])
            .show("operation", &["edit"]),
        )
        .property(
            Prop::string("lockReason", "Lock Reason")
                .description("e.g. off-topic, too heated, resolved, spam")
                .show("resource", &["issue"])
                .show("operation", &["lock"]),
        )
        .property(
            Prop::collection(
                "filters",
                "Filters",
                vec![
                    Prop::options(
                        "state",
                        "State",
                        vec![Choice::new("Open", "open"), Choice::new("Closed", "closed"), Choice::new("All", "all")],
                    ),
                    Prop::string("labels", "Labels").description("Comma-separated label names"),
                    Prop::string("q", "Search Text"),
                    Prop::options("type", "Type", vec![Choice::new("Issues", "issues"), Choice::new("Pull Requests", "pulls")]),
                ],
            )
            .show("resource", &["issue"])
            .show("operation", &["getAll"]),
        )
        .property(
            Prop::string("filePath", "File Path")
                .required()
                .show("resource", &["file"])
                .show("operation", &["get", "create", "edit", "delete"]),
        )
        .property(Prop::string("filePath", "Path").show("resource", &["file"]).show("operation", &["list"]))
        .property(Prop::string("fileContent", "File Content").show("resource", &["file"]).show("operation", &["create", "edit"]))
        .property(
            Prop::string("commitMessage", "Commit Message")
                .required()
                .show("resource", &["file"])
                .show("operation", &["create", "edit", "delete"]),
        )
        .property(
            Prop::collection(
                "additionalParameters",
                "Additional Parameters",
                vec![
                    Prop::string("branch", "Branch").description("Defaults to the repository's default branch"),
                    Prop::string("reference", "Reference").description("Branch, tag or commit to read from"),
                    Prop::string("authorName", "Author Name"),
                    Prop::string("authorEmail", "Author Email"),
                    Prop::string("committerName", "Committer Name"),
                    Prop::string("committerEmail", "Committer Email"),
                ],
            )
            .show("resource", &["file"]),
        )
        .property(Prop::number("releaseId", "Release ID").required().show("resource", &["release"]).show("operation", &["get", "update", "delete"]))
        .property(Prop::string("tag_name", "Tag Name").required().show("resource", &["release"]).show("operation", &["create"]))
        .property(release_fields("additionalFields", "Additional Fields", false).show("resource", &["release"]).show("operation", &["create"]))
        .property(release_fields("updateFields", "Update Fields", true).show("resource", &["release"]).show("operation", &["update"]))
        .properties(return_all_and_limit(None, &["getAll", "getBranches", "getPullRequests"]))
        .build()
}

fn route_hook(
    defaults: Map<String, Value>,
) -> impl Fn(&mut RestRequest, &NodeContext, usize) -> Result<(), FlowError> + Send + Sync + 'static {
    move |req, ctx, i| {
        let d = &defaults;
        let resource = req_str(ctx, d, "resource", i)?;
        let op = req_str(ctx, d, "operation", i)?;
        let mut body = Map::new();

        // tolerate a server URL that already ends in /api/v1
        req.url = req.url.replace("/api/v1/api/v1", "/api/v1");
        if resource == "file" && op == "list" && opt_str(ctx, d, "filePath", i)?.is_none() {
            req.url = req.url.trim_end_matches("/{+filePath}").to_owned();
        }
        expand(req, ctx, d, i)?;

        match (resource.as_str(), op.as_str()) {
            ("issue", "create") => {
                put(&mut body, "title", Some(json!(req_str(ctx, d, "title", i)?)));
                put(&mut body, "body", opt_str(ctx, d, "body", i)?.map(Value::from));
                if let Some(v) = ctx.eval_param_at("labels", i)? {
                    let ids: Vec<i64> = string_list(&v).iter().filter_map(|s| s.parse().ok()).collect();
                    if !ids.is_empty() {
                        body.insert("labels".into(), json!(ids));
                    }
                }
                if let Some(v) = ctx.eval_param_at("assignees", i)? {
                    let l = string_list(&v);
                    if !l.is_empty() {
                        body.insert("assignees".into(), json!(l));
                    }
                }
            }
            ("issue", "createComment") => put(&mut body, "body", Some(json!(req_str(ctx, d, "body", i)?))),
            ("issue", "edit") => collect_into(ctx, i, "editFields", &["title", "body", "state", "assignees"], &mut body)?,
            ("issue", "lock") => put(&mut body, "lock_reason", opt_str(ctx, d, "lockReason", i)?.map(Value::from)),
            ("issue", "getAll") => query_from(ctx, i, "filters", &["state", "labels", "q", "type"], req)?,
            ("file", "get") => {
                let reference = match opt_str(ctx, d, "__ref", i)? {
                    Some(r) => Some(r),
                    None => ctx
                        .eval_param_at("additionalParameters", i)?
                        .and_then(|v| nested_str(&v, "reference", "reference")),
                };
                if let Some(r) = reference {
                    req.query.push(("ref".into(), r));
                }
            }
            ("file", "create" | "edit" | "delete") => {
                put(&mut body, "message", Some(json!(req_str(ctx, d, "commitMessage", i)?)));
                if op != "delete" {
                    let content = opt_str(ctx, d, "fileContent", i)?.unwrap_or_default();
                    body.insert(
                        "content".into(),
                        json!(base64::engine::general_purpose::STANDARD.encode(content.as_bytes())),
                    );
                }
                if op != "create" {
                    let sha = opt_str(ctx, d, "__sha", i)?.ok_or_else(|| node_err(ctx, "File sha is missing"))?;
                    body.insert("sha".into(), json!(sha));
                }
                if let Some(extra) = ctx.eval_param_at("additionalParameters", i)? {
                    // Gitea's file API names the target branch `branch`
                    put(&mut body, "branch", nested_str(&extra, "branch", "branch").map(Value::from));
                    put(&mut body, "author", person(&extra, "author"));
                    put(&mut body, "committer", person(&extra, "committer"));
                }
            }
            ("release", "create") => {
                put(&mut body, "tag_name", Some(json!(req_str(ctx, d, "tag_name", i)?)));
                collect_into(ctx, i, "additionalFields", &["name", "body", "draft", "prerelease", "target_commitish"], &mut body)?;
            }
            ("release", "update") => collect_into(
                ctx,
                i,
                "updateFields",
                &["tag_name", "name", "body", "draft", "prerelease", "target_commitish"],
                &mut body,
            )?,
            _ => {}
        }
        if !body.is_empty() {
            req.body = RestBody::Json(Value::Object(body));
        }
        Ok(())
    }
}

/// Node + credential type into the shared registries. Features: `int-gitea`.
pub fn register(nodes: &mut NodeRegistry, creds: &CredentialTypes) -> Result<(), FlowError> {
    creds.register_if_absent(credential_type());
    let desc = description();
    let defaults = defaults_of(&desc);
    let spec = RestSpec::new("={{$credentials.server}}")
        .credential(CREDENTIAL, true)
        .header("Accept", "application/json")
        .error_path("message")
        .pre_send(route_hook(defaults));
    nodes.register(Arc::new(ShaFirst::new(RestNode::new(desc, spec, creds), &["edit", "delete"])))
}
