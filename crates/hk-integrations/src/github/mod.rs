//! GitHub (`n8n-nodes-base.github`) and GitHub Trigger (`n8n-nodes-base.githubTrigger`).
//!
//! Clean-room from the n8n docs: same node/parameter names so imported workflows run, REST
//! API v3 underneath. The action node is a [`RestNode`] (routing in the description) plus a
//! `pre_send` hook that fills in path placeholders, bodies and filter queries; file edit
//! and delete need the current blob sha, so the node is wrapped in [`ShaFirst`]. The
//! trigger polls the repository events API (no webhook is created on GitHub).

mod trigger;

use std::sync::Arc;

use base64::Engine as _;
use serde_json::{Map, Value, json};

use hk_flow::{FlowError, NodeContext, NodeRegistry};

use crate::credentials::{Authenticate, CredentialTest, CredentialType, CredentialTypes};
use crate::describe::{Choice, NodeBuilder, Pagination, Prop, Routing, return_all_and_limit};
use crate::pathparam::{
    ShaFirst, collect_into, defaults_of, expand, nested_str, node_err, opt_str, person, put,
    query_from, req_str, string_list,
};
use crate::poll::PollRegistry;
use crate::rest::{RestBody, RestNode, RestRequest, RestSpec};

pub use trigger::GithubTrigger;

pub const NODE_TYPE: &str = "n8n-nodes-base.github";
pub const TRIGGER_TYPE: &str = "n8n-nodes-base.githubTrigger";
pub const CREDENTIAL: &str = "githubApi";

pub fn credential_type() -> CredentialType {
    CredentialType::new(CREDENTIAL, "GitHub API")
        .documentation("https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/managing-your-personal-access-tokens")
        .property(
            Prop::string("server", "GitHub Server")
                .default("https://api.github.com")
                .description("The server to connect to. Change only for GitHub Enterprise"),
        )
        .property(Prop::string("user", "User").description("Informational; not used for authentication"))
        .property(Prop::string("accessToken", "Access Token").password().required())
        .authenticate(Authenticate::header("Authorization", "=token {{$credentials.accessToken}}"))
        .test(CredentialTest::get("={{$credentials.server}}", "/user"))
}

/// (resource, value, label, action, method, url, paged list). `{x}` are parameter placeholders.
type Op = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    bool,
);

const OPS: &[Op] = &[
    (
        "issue",
        "create",
        "Create",
        "Create an issue",
        "POST",
        "/repos/{owner}/{repository}/issues",
        false,
    ),
    (
        "issue",
        "createComment",
        "Create Comment",
        "Create a comment on an issue",
        "POST",
        "/repos/{owner}/{repository}/issues/{issueNumber}/comments",
        false,
    ),
    (
        "issue",
        "edit",
        "Edit",
        "Edit an issue",
        "PATCH",
        "/repos/{owner}/{repository}/issues/{issueNumber}",
        false,
    ),
    (
        "issue",
        "get",
        "Get",
        "Get an issue",
        "GET",
        "/repos/{owner}/{repository}/issues/{issueNumber}",
        false,
    ),
    (
        "issue",
        "getAll",
        "Get Many",
        "Get many issues of a repository",
        "GET",
        "/repos/{owner}/{repository}/issues",
        true,
    ),
    (
        "issue",
        "lock",
        "Lock",
        "Lock an issue",
        "PUT",
        "/repos/{owner}/{repository}/issues/{issueNumber}/lock",
        false,
    ),
    (
        "repository",
        "get",
        "Get",
        "Get a repository",
        "GET",
        "/repos/{owner}/{repository}",
        false,
    ),
    (
        "repository",
        "getIssues",
        "Get Issues",
        "Get the issues of a repository",
        "GET",
        "/repos/{owner}/{repository}/issues",
        true,
    ),
    (
        "repository",
        "getPullRequests",
        "Get Pull Requests",
        "Get the pull requests of a repository",
        "GET",
        "/repos/{owner}/{repository}/pulls",
        true,
    ),
    (
        "repository",
        "getLicense",
        "Get License",
        "Get the license of a repository",
        "GET",
        "/repos/{owner}/{repository}/license",
        false,
    ),
    (
        "repository",
        "getProfile",
        "Get Community Profile",
        "Get the community profile of a repository",
        "GET",
        "/repos/{owner}/{repository}/community/profile",
        false,
    ),
    (
        "repository",
        "listPopularPaths",
        "List Popular Paths",
        "Get the top 10 popular content paths over the last 14 days",
        "GET",
        "/repos/{owner}/{repository}/traffic/popular/paths",
        false,
    ),
    (
        "repository",
        "listReferrers",
        "List Referrers",
        "Get the top 10 referrers over the last 14 days",
        "GET",
        "/repos/{owner}/{repository}/traffic/popular/referrers",
        false,
    ),
    (
        "file",
        "get",
        "Get",
        "Get the data of a file",
        "GET",
        "/repos/{owner}/{repository}/contents/{+filePath}",
        false,
    ),
    (
        "file",
        "create",
        "Create",
        "Create a new file",
        "PUT",
        "/repos/{owner}/{repository}/contents/{+filePath}",
        false,
    ),
    (
        "file",
        "edit",
        "Edit",
        "Edit a file",
        "PUT",
        "/repos/{owner}/{repository}/contents/{+filePath}",
        false,
    ),
    (
        "file",
        "delete",
        "Delete",
        "Delete a file",
        "DELETE",
        "/repos/{owner}/{repository}/contents/{+filePath}",
        false,
    ),
    (
        "file",
        "list",
        "List",
        "List the files in a directory",
        "GET",
        "/repos/{owner}/{repository}/contents/{+filePath}",
        false,
    ),
    (
        "release",
        "create",
        "Create",
        "Create a release",
        "POST",
        "/repos/{owner}/{repository}/releases",
        false,
    ),
    (
        "release",
        "get",
        "Get",
        "Get a release",
        "GET",
        "/repos/{owner}/{repository}/releases/{releaseId}",
        false,
    ),
    (
        "release",
        "getAll",
        "Get Many",
        "Get many releases",
        "GET",
        "/repos/{owner}/{repository}/releases",
        true,
    ),
    (
        "release",
        "update",
        "Update",
        "Update a release",
        "PATCH",
        "/repos/{owner}/{repository}/releases/{releaseId}",
        false,
    ),
    (
        "release",
        "delete",
        "Delete",
        "Delete a release",
        "DELETE",
        "/repos/{owner}/{repository}/releases/{releaseId}",
        false,
    ),
    (
        "review",
        "get",
        "Get",
        "Get a review for a pull request",
        "GET",
        "/repos/{owner}/{repository}/pulls/{prNumber}/reviews/{reviewId}",
        false,
    ),
    (
        "review",
        "getAll",
        "Get Many",
        "Get many reviews of a pull request",
        "GET",
        "/repos/{owner}/{repository}/pulls/{prNumber}/reviews",
        true,
    ),
    (
        "review",
        "create",
        "Create",
        "Create a review for a pull request",
        "POST",
        "/repos/{owner}/{repository}/pulls/{prNumber}/reviews",
        false,
    ),
    (
        "review",
        "update",
        "Update",
        "Update a review of a pull request",
        "PUT",
        "/repos/{owner}/{repository}/pulls/{prNumber}/reviews/{reviewId}",
        false,
    ),
    (
        "user",
        "getRepositories",
        "Get Repositories",
        "Get the public repositories of a user",
        "GET",
        "/users/{owner}/repos",
        true,
    ),
];

const RESOURCES: &[(&str, &str)] = &[
    ("Issue", "issue"),
    ("File", "file"),
    ("Release", "release"),
    ("Repository", "repository"),
    ("Review", "review"),
    ("User", "user"),
];

const LOCK_REASONS: &[&str] = &["off-topic", "too heated", "resolved", "spam"];

fn pagination() -> Pagination {
    Pagination::Page {
        page_param: "page".into(),
        size_param: "per_page".into(),
        page_size: 100,
        first_page: 1,
    }
}

fn operation_prop(resource: &str) -> Prop {
    let choices: Vec<Choice> = OPS
        .iter()
        .filter(|o| o.0 == resource)
        .map(|&(_, value, label, action, method, url, list)| {
            let mut r = Routing::request(method, url);
            // an empty output path is "the whole response", which marks list operations
            // for return-all / limit handling
            if list
                || matches!(
                    (resource, value),
                    ("repository", "listPopularPaths" | "listReferrers") | ("file", "list")
                )
            {
                r = r.output(Some(""), &[]);
            }
            if list {
                r = r.paginate(pagination());
            }
            Choice::new(label, value).action(action).routing(r)
        })
        .collect();
    let default = choices.first().map_or(Value::Null, |_| {
        Value::String(
            OPS.iter()
                .find(|o| o.0 == resource)
                .map_or("", |o| o.1)
                .to_owned(),
        )
    });
    Prop::options("operation", "Operation", choices)
        .default(default)
        .show("resource", &[resource])
}

fn description() -> hk_flow::description::NodeTypeDescription {
    let issue_ops = ["createComment", "edit", "get", "lock"];
    let states = || {
        Prop::options(
            "state",
            "State",
            vec![Choice::new("Open", "open"), Choice::new("Closed", "closed")],
        )
        .default("open")
    };
    let issue_fields = || {
        vec![
            Prop::string("labels", "Labels").description("Comma-separated label names"),
            Prop::string("assignees", "Assignees").description("Comma-separated user logins"),
        ]
    };
    let mut edit_fields = vec![
        Prop::string("title", "Title"),
        Prop::string("body", "Body"),
        states(),
    ];
    edit_fields.extend(issue_fields());

    let list_filters = |name: &str, extra: Vec<Prop>| {
        let mut f = vec![
            states(),
            Prop::options(
                "sort",
                "Sort",
                vec![
                    Choice::new("Created", "created"),
                    Choice::new("Updated", "updated"),
                    Choice::new("Comments", "comments"),
                ],
            )
            .default("created"),
            Prop::options(
                "direction",
                "Direction",
                vec![
                    Choice::new("Ascending", "asc"),
                    Choice::new("Descending", "desc"),
                ],
            )
            .default("desc"),
        ];
        f.extend(extra);
        Prop::collection(name, "Filters", f)
    };
    let release_fields = |name: &str, with_tag: bool| {
        let mut f = vec![
            Prop::string("name", "Name"),
            Prop::string("body", "Body"),
            Prop::boolean("draft", "Draft").default(false),
            Prop::boolean("prerelease", "Prerelease").default(false),
            Prop::string("target_commitish", "Target Commitish"),
        ];
        if with_tag {
            f.push(Prop::string("tag_name", "Tag Name"));
        }
        Prop::collection(
            name,
            if with_tag {
                "Update Fields"
            } else {
                "Additional Fields"
            },
            f,
        )
    };

    let mut b = NodeBuilder::new(NODE_TYPE, "GitHub")
        .description("Consume the GitHub API")
        .subtitle("={{$parameter[\"operation\"] + \": \" + $parameter[\"resource\"]}}")
        .icon("fa:github")
        .color("#000000")
        .credential(CREDENTIAL, true)
        .property(
            Prop::options(
                "resource",
                "Resource",
                RESOURCES.iter().map(|(n, v)| Choice::new(n, *v)).collect(),
            )
            .default("issue"),
        );
    for (_, r) in RESOURCES {
        b = b.property(operation_prop(r));
    }
    b = b
        .property(
            Prop::string("owner", "Repository Owner")
                .required()
                .description("Login of the user or organization that owns the repository"),
        )
        .property(
            Prop::string("repository", "Repository Name")
                .required()
                .hide("resource", &["user"]),
        )
        // issue
        .property(
            Prop::number("issueNumber", "Issue Number")
                .required()
                .show("resource", &["issue"])
                .show("operation", &issue_ops),
        )
        .property(
            Prop::string("title", "Title")
                .required()
                .show("resource", &["issue"])
                .show("operation", &["create"]),
        )
        .property(
            Prop::string("body", "Body")
                .show("resource", &["issue"])
                .show("operation", &["create"]),
        )
        .property(
            Prop::string("body", "Comment")
                .required()
                .show("resource", &["issue"])
                .show("operation", &["createComment"]),
        );
    for p in issue_fields() {
        b = b.property(
            p.show("resource", &["issue"])
                .show("operation", &["create"]),
        );
    }
    b = b
        .property(
            Prop::collection("editFields", "Edit Fields", edit_fields)
                .show("resource", &["issue"])
                .show("operation", &["edit"]),
        )
        .property(
            Prop::options(
                "lockReason",
                "Lock Reason",
                LOCK_REASONS.iter().map(|r| Choice::new(r, *r)).collect(),
            )
            .default("resolved")
            .show("resource", &["issue"])
            .show("operation", &["lock"]),
        )
        .property(
            list_filters(
                "getRepositoryIssuesFilters",
                vec![
                    Prop::string("assignee", "Assignee"),
                    Prop::string("creator", "Creator"),
                    Prop::string("mentioned", "Mentioned"),
                    Prop::string("labels", "Labels").description("Comma-separated label names"),
                    Prop::string("since", "Updated Since").description("ISO 8601 timestamp"),
                ],
            )
            .show("resource", &["issue", "repository"])
            .show("operation", &["getAll", "getIssues"]),
        )
        .property(
            list_filters("getRepositoryPullRequestsFilters", Vec::new())
                .show("resource", &["repository"])
                .show("operation", &["getPullRequests"]),
        )
        // file
        .property(
            Prop::string("filePath", "File Path")
                .required()
                .show("resource", &["file"])
                .show("operation", &["get", "create", "edit", "delete"]),
        )
        .property(
            Prop::string("filePath", "Path")
                .show("resource", &["file"])
                .show("operation", &["list"]),
        )
        .property(
            Prop::string("fileContent", "File Content")
                .show("resource", &["file"])
                .show("operation", &["create", "edit"]),
        )
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
                    Prop::string("branch", "Branch")
                        .description("Defaults to the repository's default branch"),
                    Prop::string("reference", "Reference")
                        .description("Branch, tag or commit to read from"),
                    Prop::string("authorName", "Author Name"),
                    Prop::string("authorEmail", "Author Email"),
                    Prop::string("committerName", "Committer Name"),
                    Prop::string("committerEmail", "Committer Email"),
                ],
            )
            .show("resource", &["file"]),
        )
        // release
        .property(
            Prop::number("releaseId", "Release ID")
                .required()
                .show("resource", &["release"])
                .show("operation", &["get", "update", "delete"]),
        )
        .property(
            Prop::string("tag_name", "Tag Name")
                .required()
                .show("resource", &["release"])
                .show("operation", &["create"]),
        )
        .property(
            release_fields("additionalFields", false)
                .show("resource", &["release"])
                .show("operation", &["create"]),
        )
        .property(
            release_fields("updateFields", true)
                .show("resource", &["release"])
                .show("operation", &["update"]),
        )
        // review
        .property(
            Prop::number("prNumber", "PR Number")
                .required()
                .show("resource", &["review"]),
        )
        .property(
            Prop::number("reviewId", "Review ID")
                .required()
                .show("resource", &["review"])
                .show("operation", &["get", "update"]),
        )
        .property(
            Prop::options(
                "event",
                "Event",
                vec![
                    Choice::new("Approve", "APPROVE"),
                    Choice::new("Request Changes", "REQUEST_CHANGES"),
                    Choice::new("Comment", "COMMENT"),
                    Choice::new("Pending", "PENDING"),
                ],
            )
            .default("APPROVE")
            .show("resource", &["review"])
            .show("operation", &["create"]),
        )
        .property(
            Prop::string("body", "Body")
                .show("resource", &["review"])
                .show("operation", &["create", "update"]),
        )
        .properties(return_all_and_limit(
            None,
            &["getAll", "getIssues", "getPullRequests", "getRepositories"],
        ));
    b.build()
}

// ------------------------------------------------------------------- request

fn route_hook(
    defaults: Map<String, Value>,
) -> impl Fn(&mut RestRequest, &NodeContext, usize) -> Result<(), FlowError> + Send + Sync + 'static
{
    move |req, ctx, i| {
        let d = &defaults;
        let resource = req_str(ctx, d, "resource", i)?;
        let op = req_str(ctx, d, "operation", i)?;
        let mut body = Map::new();

        if resource == "file" && op == "list" && opt_str(ctx, d, "filePath", i)?.is_none() {
            req.url = req.url.trim_end_matches("/{+filePath}").to_owned();
        }
        expand(req, ctx, d, i)?;

        match (resource.as_str(), op.as_str()) {
            ("issue", "create") => {
                put(
                    &mut body,
                    "title",
                    Some(json!(req_str(ctx, d, "title", i)?)),
                );
                put(
                    &mut body,
                    "body",
                    opt_str(ctx, d, "body", i)?.map(Value::from),
                );
                for k in ["labels", "assignees"] {
                    if let Some(v) = ctx.eval_param_at(k, i)? {
                        let l = string_list(&v);
                        if !l.is_empty() {
                            body.insert(k.to_owned(), json!(l));
                        }
                    }
                }
            }
            ("issue", "createComment") => {
                put(&mut body, "body", Some(json!(req_str(ctx, d, "body", i)?)))
            }
            ("issue", "edit") => collect_into(
                ctx,
                i,
                "editFields",
                &["title", "body", "state", "labels", "assignees"],
                &mut body,
            )?,
            ("issue", "lock") => {
                put(
                    &mut body,
                    "lock_reason",
                    opt_str(ctx, d, "lockReason", i)?.map(Value::from),
                );
            }
            ("issue", "getAll") | ("repository", "getIssues") => query_from(
                ctx,
                i,
                "getRepositoryIssuesFilters",
                &[
                    "state",
                    "sort",
                    "direction",
                    "assignee",
                    "creator",
                    "mentioned",
                    "labels",
                    "since",
                ],
                req,
            )?,
            ("repository", "getPullRequests") => {
                query_from(
                    ctx,
                    i,
                    "getRepositoryPullRequestsFilters",
                    &["state", "sort", "direction"],
                    req,
                )?;
            }
            ("file", "get") => {
                let hidden = opt_str(ctx, d, "__ref", i)?;
                let reference = match hidden {
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
                put(
                    &mut body,
                    "message",
                    Some(json!(req_str(ctx, d, "commitMessage", i)?)),
                );
                if op != "delete" {
                    let content = opt_str(ctx, d, "fileContent", i)?.unwrap_or_default();
                    body.insert(
                        "content".into(),
                        json!(base64::engine::general_purpose::STANDARD.encode(content.as_bytes())),
                    );
                }
                if op != "create" {
                    let sha = opt_str(ctx, d, "__sha", i)?
                        .ok_or_else(|| node_err(ctx, "File sha is missing"))?;
                    body.insert("sha".into(), json!(sha));
                }
                if let Some(extra) = ctx.eval_param_at("additionalParameters", i)? {
                    put(
                        &mut body,
                        "branch",
                        nested_str(&extra, "branch", "branch").map(Value::from),
                    );
                    put(&mut body, "author", person(&extra, "author"));
                    put(&mut body, "committer", person(&extra, "committer"));
                }
            }
            ("release", "create") => {
                put(
                    &mut body,
                    "tag_name",
                    Some(json!(req_str(ctx, d, "tag_name", i)?)),
                );
                collect_into(
                    ctx,
                    i,
                    "additionalFields",
                    &["name", "body", "draft", "prerelease", "target_commitish"],
                    &mut body,
                )?;
            }
            ("release", "update") => collect_into(
                ctx,
                i,
                "updateFields",
                &[
                    "tag_name",
                    "name",
                    "body",
                    "draft",
                    "prerelease",
                    "target_commitish",
                ],
                &mut body,
            )?,
            ("review", "create") => {
                put(
                    &mut body,
                    "event",
                    Some(json!(req_str(ctx, d, "event", i)?)),
                );
                put(
                    &mut body,
                    "body",
                    opt_str(ctx, d, "body", i)?.map(Value::from),
                );
            }
            ("review", "update") => {
                put(&mut body, "body", Some(json!(req_str(ctx, d, "body", i)?)))
            }
            _ => {}
        }
        if !body.is_empty() {
            req.body = RestBody::Json(Value::Object(body));
        }
        Ok(())
    }
}

/// Node, trigger and credential type into the shared registries. Features: `int-github`.
pub fn register(
    nodes: &mut NodeRegistry,
    creds: &CredentialTypes,
    polls: &mut PollRegistry,
) -> Result<(), FlowError> {
    creds.register_if_absent(credential_type());
    let desc = description();
    let defaults = defaults_of(&desc);
    let spec = RestSpec::new("={{$credentials.server}}")
        .credential(CREDENTIAL, true)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        // GitHub rejects requests without a User-Agent
        .header("User-Agent", "hokkabaz")
        .error_path("message")
        .pre_send(route_hook(defaults));
    nodes.register(Arc::new(ShaFirst::new(
        RestNode::new(desc, spec, creds),
        &["edit", "delete"],
    )))?;
    polls.register(nodes, Arc::new(GithubTrigger::new(creds)))?;
    Ok(())
}
