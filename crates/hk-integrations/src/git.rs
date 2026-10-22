//! Git node (`n8n-nodes-base.git`): clone, add, commit, push, pull, fetch, status, log,
//! checkout, tag and pushTags by driving the `git` CLI.
//!
//! Safety model (a workflow author must not be able to reach outside the workspace or run
//! commands):
//! - no shell: `tokio::process::Command` with an argument vector, user values only ever as
//!   separate arguments, ref-like values may not start with `-`, paths follow `--`;
//! - the repository path resolves under a workspace root ([`set_workspace_root`], env
//!   `HK_GIT_WORKSPACE_ROOT`, else the working directory) with symlinks resolved, and git
//!   repository discovery is capped at that path so a plain folder never falls through to a
//!   repository above it; a local clone source must live under the root too;
//! - `GIT_ALLOW_PROTOCOL` excludes `ext::`/`fd::` transports, repository hooks are disabled
//!   (`core.hooksPath=/dev/null`), prompts are off;
//! - the `gitPassword` credential travels in a git config env var, never in argv, and is
//!   scrubbed from error text.
//!
//! Desktop only: android builds register the node but every run fails with a clear
//! "not supported" error (no `git` binary, no process spawning in the app sandbox).

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use serde_json::json;

use hk_flow::description::NodeTypeDescription;
use hk_flow::{
    ErrorMode, FlowError, FlowNode, Item, NodeContext, NodeOutput, NodeRegistry, PairedItem,
};

use crate::credentials::{CredentialType, CredentialTypes};
use crate::describe::{Choice, NodeBuilder, Prop, return_all_and_limit};

pub const NODE_TYPE: &str = "n8n-nodes-base.git";
pub const CREDENTIAL: &str = "gitPassword";

static WORKSPACE_ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Set the directory all repositories must live under. First call wins; the daemon calls
/// this once at startup from its config.
pub fn set_workspace_root(root: impl Into<PathBuf>) {
    let _ = WORKSPACE_ROOT.set(root.into());
}

/// Configured root, else `HK_GIT_WORKSPACE_ROOT`, else the working directory.
pub fn workspace_root() -> PathBuf {
    if let Some(r) = WORKSPACE_ROOT.get() {
        return r.clone();
    }
    if let Some(r) = std::env::var_os("HK_GIT_WORKSPACE_ROOT").filter(|v| !v.is_empty()) {
        return PathBuf::from(r);
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

pub fn credential_type() -> CredentialType {
    CredentialType::new(CREDENTIAL, "Git")
        .documentation("https://docs.n8n.io/integrations/builtin/credentials/git/")
        .property(Prop::string("username", "Username"))
        .property(
            Prop::string("password", "Password")
                .password()
                .description("Password or personal access token"),
        )
}

const OPERATIONS: &[(&str, &str, &str)] = &[
    ("Add", "add", "Add files to the index"),
    (
        "Checkout",
        "checkout",
        "Switch to a branch, optionally creating it",
    ),
    ("Clone", "clone", "Clone a repository into the workspace"),
    ("Commit", "commit", "Commit the index"),
    ("Fetch", "fetch", "Fetch from a remote"),
    ("Log", "log", "List commits"),
    ("Pull", "pull", "Pull from a remote"),
    ("Push", "push", "Push to a remote"),
    ("Push Tags", "pushTags", "Push all tags to a remote"),
    ("Status", "status", "Show the working tree status"),
    ("Tag", "tag", "Create a tag"),
];

const NETWORK_OPS: &[&str] = &["clone", "fetch", "pull", "push", "pushTags"];

fn description() -> NodeTypeDescription {
    NodeBuilder::new(NODE_TYPE, "Git")
        .description("Control Git repositories in the workspace")
        .subtitle("={{$parameter[\"operation\"]}}")
        .icon("fa:code-branch")
        .color("#808080")
        .credential(CREDENTIAL, false)
        .property(
            Prop::options(
                "operation",
                "Operation",
                OPERATIONS
                    .iter()
                    .map(|(n, v, d)| Choice::new(n, *v).action(d))
                    .collect(),
            )
            .default("status"),
        )
        .property(
            Prop::string("repositoryPath", "Repository Path")
                .required()
                .description("Relative to the workspace root (the clone destination for Clone)"),
        )
        .property(
            Prop::string("sourceRepository", "Source Repository")
                .required()
                .show("operation", &["clone"]),
        )
        .property(
            Prop::options(
                "authentication",
                "Authentication",
                vec![
                    Choice::new("None", "none"),
                    Choice::new("Basic Auth (git password)", "gitPassword"),
                ],
            )
            .default("none")
            .show("operation", NETWORK_OPS),
        )
        .property(
            Prop::string("pathsToAdd", "Paths to Add")
                .default(".")
                .description("Comma-separated paths relative to the repository")
                .show("operation", &["add"]),
        )
        .property(
            Prop::string("message", "Message")
                .required()
                .show("operation", &["commit"]),
        )
        .property(
            Prop::collection(
                "options",
                "Options",
                vec![
                    Prop::string("authorName", "Author Name"),
                    Prop::string("authorEmail", "Author Email"),
                    Prop::boolean("allowEmpty", "Allow Empty").default(false),
                ],
            )
            .show("operation", &["commit"]),
        )
        .property(
            Prop::string("branch", "Branch")
                .required()
                .show("operation", &["checkout"]),
        )
        .property(
            Prop::string("branch", "Branch")
                .description("Empty: the configured upstream")
                .show("operation", &["push", "pull"]),
        )
        .property(
            Prop::boolean("createBranch", "Create Branch")
                .default(false)
                .show("operation", &["checkout"]),
        )
        .property(
            Prop::string("remote", "Remote")
                .default("origin")
                .show("operation", &["fetch", "pull", "push", "pushTags"]),
        )
        .property(
            Prop::string("name", "Tag Name")
                .required()
                .show("operation", &["tag"]),
        )
        .properties(return_all_and_limit(None, &["log"]))
        .property(
            Prop::collection(
                "logOptions",
                "Options",
                vec![Prop::string("file", "File").description("Only commits touching this path")],
            )
            .show("operation", &["log"]),
        )
        .build()
}

fn node_err(ctx: &NodeContext, message: impl Into<String>) -> FlowError {
    FlowError::Node {
        node: ctx.node.name.clone(),
        message: message.into(),
    }
}

pub struct GitNode {
    desc: NodeTypeDescription,
    root: Option<PathBuf>,
    creds: CredentialTypes,
}

impl std::fmt::Debug for GitNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitNode")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl GitNode {
    /// `root`: workspace root; `None` reads [`workspace_root`] on every run.
    pub fn new(root: Option<PathBuf>, creds: &CredentialTypes) -> Self {
        Self {
            desc: description(),
            root,
            creds: creds.clone(),
        }
    }
}

/// Node + credential type into the shared registries. Features: `int-git`.
pub fn register(nodes: &mut NodeRegistry, creds: &CredentialTypes) -> Result<(), FlowError> {
    register_with_root(nodes, creds, None)
}

/// Like [`register`] with an explicit workspace root.
pub fn register_with_root(
    nodes: &mut NodeRegistry,
    creds: &CredentialTypes,
    root: Option<PathBuf>,
) -> Result<(), FlowError> {
    creds.register_if_absent(credential_type());
    nodes.register(Arc::new(GitNode::new(root, creds)))
}

#[async_trait]
impl FlowNode for GitNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.desc
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let carry = matches!(
            ctx.node.error_mode(),
            ErrorMode::ContinueRegularOutput | ErrorMode::ContinueErrorOutput
        );
        let n = ctx.input.first().map_or(0, Vec::len);
        let mut out = Vec::new();
        for i in 0..n {
            match self.run_item(ctx, i).await {
                Ok(items) => out.extend(items.into_iter().map(|mut o| {
                    o.paired_item = Some(PairedItem::Index(i));
                    o
                })),
                Err(FlowError::Node { message, .. }) if carry => {
                    let mut o = Item::from_json(json!({ "error": message }));
                    o.error = Some(json!({ "message": message }));
                    o.paired_item = Some(PairedItem::Index(i));
                    out.push(o);
                }
                Err(e) => return Err(e),
            }
        }
        Ok(vec![out])
    }
}

#[cfg(target_os = "android")]
impl GitNode {
    async fn run_item(&self, ctx: &NodeContext, _i: usize) -> Result<Vec<Item>, FlowError> {
        let _ = &self.creds;
        let _ = &self.root;
        Err(node_err(
            ctx,
            "The Git node is not supported on Android: there is no git binary here. Run this workflow on a desktop instance",
        ))
    }
}

#[cfg(not(target_os = "android"))]
impl GitNode {
    async fn run_item(&self, ctx: &NodeContext, i: usize) -> Result<Vec<Item>, FlowError> {
        imp::run_item(self, ctx, i).await
    }
}

#[cfg(not(target_os = "android"))]
mod imp {
    use std::collections::HashMap;
    use std::path::{Component, Path};
    use std::process::Stdio;
    use std::time::Duration;

    use base64::Engine as _;
    use serde_json::Value;
    use tokio::process::Command;

    use super::*;
    use crate::template::display;

    const TIMEOUT: Duration = Duration::from_secs(300);
    const MAX_OUTPUT: usize = 4 * 1024 * 1024;

    fn str_param(ctx: &NodeContext, name: &str, i: usize) -> Result<String, FlowError> {
        Ok(ctx
            .eval_param_at(name, i)?
            .map(|v| display(&v))
            .unwrap_or_default()
            .trim()
            .to_owned())
    }

    fn req_param(ctx: &NodeContext, name: &str, i: usize) -> Result<String, FlowError> {
        let v = str_param(ctx, name, i)?;
        if v.is_empty() {
            return Err(node_err(ctx, format!("Parameter `{name}` is required")));
        }
        Ok(v)
    }

    /// Branch, remote or tag name: becomes a separate argv entry, so only option injection
    /// and control characters matter.
    fn ref_like(ctx: &NodeContext, what: &str, v: String) -> Result<String, FlowError> {
        if v.starts_with('-') || v.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(node_err(ctx, format!("Invalid {what} `{v}`")));
        }
        Ok(v)
    }

    /// Resolve `raw` under `root` with symlinks resolved. `must_exist = false` allows the
    /// final component to be missing (clone destination).
    fn resolve_under(root: &Path, raw: &str, must_exist: bool) -> Result<PathBuf, String> {
        let p = Path::new(raw);
        if p.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err("path may not contain `..`".into());
        }
        let croot = root
            .canonicalize()
            .map_err(|e| format!("workspace root {} is not usable: {e}", root.display()))?;
        let joined = if p.is_absolute() {
            p.to_path_buf()
        } else {
            croot.join(p)
        };
        let resolved = if must_exist {
            joined
                .canonicalize()
                .map_err(|e| format!("{}: {e}", joined.display()))?
        } else {
            let parent = joined.parent().ok_or("path has no parent")?;
            let name = joined.file_name().ok_or("path has no name")?;
            parent
                .canonicalize()
                .map_err(|e| format!("{}: {e}", parent.display()))?
                .join(name)
        };
        if !resolved.starts_with(&croot) {
            return Err(format!(
                "path is outside the workspace root {}",
                croot.display()
            ));
        }
        Ok(resolved)
    }

    /// A clone source that is a local path (not a URL / scp-style address).
    fn is_local_source(src: &str) -> bool {
        if src.contains("://") {
            return false;
        }
        // scp-like `user@host:path`; a drive-less unix path has no ':' before the first '/'
        match (src.find(':'), src.find('/')) {
            (Some(c), Some(s)) => c > s,
            (Some(_), None) => false,
            _ => true,
        }
    }

    struct Out {
        stdout: String,
    }

    async fn git(
        ctx: &NodeContext,
        cwd: &Path,
        ceiling: Option<&Path>,
        secrets: &[String],
        extra_env: &HashMap<String, String>,
        args: &[String],
    ) -> Result<Out, FlowError> {
        let mut cmd = Command::new("git");
        cmd.arg("-c")
            .arg("core.hooksPath=/dev/null")
            .arg("-c")
            .arg("protocol.ext.allow=never")
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_EDITOR", "true")
            .env("GIT_MERGE_AUTOEDIT", "no")
            .env("GIT_ALLOW_PROTOCOL", "file:git:http:https:ssh")
            .env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes")
            .env("LC_ALL", "C");
        for k in ["PATH", "HOME", "USERPROFILE", "SSH_AUTH_SOCK", "SYSTEMROOT"] {
            if let Some(v) = std::env::var_os(k) {
                cmd.env(k, v);
            }
        }
        if let Some(c) = ceiling {
            cmd.env("GIT_CEILING_DIRECTORIES", c);
        }
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        let scrub = |s: &str| {
            let mut s = s.to_owned();
            for secret in secrets.iter().filter(|x| !x.is_empty()) {
                s = s.replace(secret.as_str(), "***");
            }
            s
        };
        let child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                node_err(ctx, "`git` was not found in PATH")
            } else {
                node_err(ctx, format!("Could not start git: {e}"))
            }
        })?;
        let res = tokio::time::timeout(TIMEOUT, child.wait_with_output())
            .await
            .map_err(|_| node_err(ctx, "git timed out"))?
            .map_err(|e| node_err(ctx, format!("git failed: {e}")))?;
        if !res.status.success() {
            let err = String::from_utf8_lossy(&res.stderr);
            let msg: String = scrub(err.trim()).chars().take(800).collect();
            return Err(node_err(
                ctx,
                format!(
                    "git {} failed: {msg}",
                    args.first().map_or("", String::as_str)
                ),
            ));
        }
        let mut stdout = res.stdout;
        stdout.truncate(MAX_OUTPUT);
        Ok(Out {
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
        })
    }

    fn ok(op: &str) -> Vec<Item> {
        vec![Item::from_json(json!({ "success": true, "operation": op }))]
    }

    fn s(v: &str) -> String {
        v.to_owned()
    }

    pub(super) async fn run_item(
        node: &GitNode,
        ctx: &NodeContext,
        i: usize,
    ) -> Result<Vec<Item>, FlowError> {
        let op = req_param(ctx, "operation", i)?;
        if !OPERATIONS.iter().any(|(_, v, _)| *v == op) {
            return Err(node_err(ctx, format!("Unknown operation `{op}`")));
        }
        let root = node.root.clone().unwrap_or_else(workspace_root);
        let repo_raw = req_param(ctx, "repositoryPath", i)?;
        let repo = resolve_under(&root, &repo_raw, op != "clone")
            .map_err(|m| node_err(ctx, format!("Repository path: {m}")))?;

        // credentials only for network operations that ask for them
        let mut env = HashMap::new();
        let mut secrets = Vec::new();
        if NETWORK_OPS.contains(&op.as_str())
            && str_param(ctx, "authentication", i)? == "gitPassword"
        {
            let reference = ctx
                .node
                .credentials
                .as_ref()
                .and_then(|m| m.get(CREDENTIAL))
                .ok_or_else(|| {
                    node_err(
                        ctx,
                        format!("No credentials of type `{CREDENTIAL}` selected"),
                    )
                })?;
            let resolver =
                ctx.hooks.credentials.as_ref().ok_or_else(|| {
                    node_err(ctx, "Credentials are not available in this runtime")
                })?;
            let raw = resolver.resolve(CREDENTIAL, reference).await?;
            let data = node.creds.with_defaults(CREDENTIAL, &raw);
            let get = |k: &str| data.get(k).map(display).unwrap_or_default();
            let token = base64::engine::general_purpose::STANDARD.encode(format!(
                "{}:{}",
                get("username"),
                get("password")
            ));
            // config via env, not argv: argv is world-readable in the process table
            env.insert("GIT_CONFIG_COUNT".into(), "1".into());
            env.insert("GIT_CONFIG_KEY_0".into(), "http.extraHeader".into());
            env.insert(
                "GIT_CONFIG_VALUE_0".into(),
                format!("Authorization: Basic {token}"),
            );
            secrets = vec![token, get("password")];
        }

        if op == "clone" {
            let src = req_param(ctx, "sourceRepository", i)?;
            if src.starts_with('-') || src.chars().any(char::is_control) {
                return Err(node_err(ctx, "Invalid source repository"));
            }
            if src.starts_with("ext::") || src.starts_with("fd::") {
                return Err(node_err(ctx, "Unsupported source repository transport"));
            }
            let src = if is_local_source(&src) {
                resolve_under(&root, &src, true)
                    .map_err(|m| node_err(ctx, format!("Local source repository: {m}")))?
                    .to_string_lossy()
                    .into_owned()
            } else if src.starts_with("file://") {
                return Err(node_err(
                    ctx,
                    "file:// sources are not allowed; use a path under the workspace root",
                ));
            } else {
                src
            };
            let parent = repo.parent().unwrap_or(&root);
            git(
                ctx,
                parent,
                Some(parent),
                &secrets,
                &env,
                &[
                    s("clone"),
                    s("--"),
                    src,
                    repo.to_string_lossy().into_owned(),
                ],
            )
            .await?;
            return Ok(ok(&op));
        }

        let ceiling = repo.parent();
        let run = |args: Vec<String>| {
            let (secrets, env, repo) = (&secrets, &env, &repo);
            async move { git(ctx, repo, ceiling, secrets, env, &args).await }
        };

        match op.as_str() {
            "add" => {
                let paths = str_param(ctx, "pathsToAdd", i)?;
                let mut args = vec![s("add"), s("--")];
                let list: Vec<&str> = paths
                    .split(',')
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .collect();
                args.extend(if list.is_empty() {
                    vec![s(".")]
                } else {
                    list.into_iter().map(s).collect()
                });
                run(args).await?;
            }
            "commit" => {
                let message = req_param(ctx, "message", i)?;
                let mut args = Vec::new();
                let name = str_param(ctx, "options.authorName", i)?;
                let email = str_param(ctx, "options.authorEmail", i)?;
                if !name.is_empty() {
                    args.extend([s("-c"), format!("user.name={name}")]);
                }
                if !email.is_empty() {
                    args.extend([s("-c"), format!("user.email={email}")]);
                }
                args.extend([s("commit"), s("-m"), message]);
                if matches!(
                    ctx.eval_param_at("options.allowEmpty", i)?,
                    Some(Value::Bool(true))
                ) {
                    args.push(s("--allow-empty"));
                }
                run(args).await?;
            }
            "checkout" => {
                let branch = ref_like(ctx, "branch", req_param(ctx, "branch", i)?)?;
                let create = matches!(
                    ctx.eval_param_at("createBranch", i)?,
                    Some(Value::Bool(true))
                );
                let args = if create {
                    vec![s("checkout"), s("-b"), branch]
                } else {
                    vec![s("checkout"), branch, s("--")]
                };
                run(args).await?;
            }
            "fetch" | "pull" | "push" | "pushTags" => {
                let remote = match str_param(ctx, "remote", i)? {
                    r if r.is_empty() => s("origin"),
                    r => ref_like(ctx, "remote", r)?,
                };
                let mut args = vec![op.replace("Tags", "")];
                if op == "pushTags" {
                    args.extend([remote, s("--tags")]);
                } else {
                    let branch = str_param(ctx, "branch", i)?;
                    if op == "fetch" || !branch.is_empty() {
                        args.push(remote);
                    }
                    if op != "fetch" && !branch.is_empty() {
                        args.push(ref_like(ctx, "branch", branch)?);
                    }
                }
                run(args).await?;
            }
            "tag" => {
                let name = ref_like(ctx, "tag name", req_param(ctx, "name", i)?)?;
                run(vec![s("tag"), name]).await?;
            }
            "status" => {
                let o = run(vec![s("status"), s("--porcelain=v1"), s("-b"), s("-z")]).await?;
                return Ok(vec![Item::from_json(parse_status(&o.stdout))]);
            }
            "log" => {
                let mut args = vec![s("log"), s("--format=%H%x1f%an%x1f%ae%x1f%aI%x1f%s%x1e")];
                let all = matches!(ctx.eval_param_at("returnAll", i)?, Some(Value::Bool(true)));
                if !all {
                    let limit = ctx
                        .eval_param_at("limit", i)?
                        .and_then(|v| v.as_u64())
                        .unwrap_or(100)
                        .max(1);
                    args.push(format!("-n{limit}"));
                }
                let file = str_param(ctx, "logOptions.file", i)?;
                if !file.is_empty() {
                    args.extend([s("--"), file]);
                }
                let o = run(args).await?;
                return Ok(parse_log(&o.stdout));
            }
            _ => return Err(node_err(ctx, format!("Unknown operation `{op}`"))),
        }
        Ok(ok(&op))
    }

    fn parse_log(out: &str) -> Vec<Item> {
        out.split('\x1e')
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .filter_map(|r| {
                let mut f = r.split('\x1f');
                Some(Item::from_json(json!({
                    "hash": f.next()?,
                    "author_name": f.next()?,
                    "author_email": f.next()?,
                    "date": f.next()?,
                    "message": f.next()?,
                })))
            })
            .collect()
    }

    pub(super) fn parse_status(out: &str) -> Value {
        let mut entries = out.split('\0').filter(|e| !e.is_empty());
        let (mut current, mut tracking, mut ahead, mut behind) =
            (Value::Null, Value::Null, 0u64, 0u64);
        if let Some(h) = entries.next().and_then(|h| h.strip_prefix("## ")) {
            if let Some(b) = h.strip_prefix("No commits yet on ") {
                current = json!(b);
            } else {
                let (names, counts) = h.split_once(" [").unwrap_or((h, ""));
                match names.split_once("...") {
                    Some((c, t)) => {
                        current = json!(c);
                        tracking = json!(t);
                    }
                    None => current = json!(names),
                }
                for part in counts.trim_end_matches(']').split(", ") {
                    if let Some(n) = part.strip_prefix("ahead ") {
                        ahead = n.parse().unwrap_or(0);
                    } else if let Some(n) = part.strip_prefix("behind ") {
                        behind = n.parse().unwrap_or(0);
                    }
                }
            }
        }
        let mut files = Vec::new();
        while let Some(e) = entries.next() {
            if e.len() < 4 {
                continue;
            }
            let (xy, path) = e.split_at(3);
            let (x, y) = (&xy[..1], &xy[1..2]);
            // renames and copies carry the original path as the next entry
            if x == "R" || x == "C" {
                entries.next();
            }
            files.push(json!({ "path": path, "index": x.trim(), "working_dir": y.trim() }));
        }
        json!({ "current": current, "tracking": tracking, "ahead": ahead, "behind": behind,
                "clean": files.is_empty(), "files": files })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn status_parsing() {
            let v = parse_status(
                "## main...origin/main [ahead 2, behind 1]\0 M a.txt\0?? b c.txt\0R  new\0old\0",
            );
            assert_eq!(v["current"], "main");
            assert_eq!(v["tracking"], "origin/main");
            assert_eq!(
                (v["ahead"].as_u64(), v["behind"].as_u64()),
                (Some(2), Some(1))
            );
            assert_eq!(v["files"].as_array().unwrap().len(), 3);
            assert_eq!(v["files"][1]["path"], "b c.txt");
            assert_eq!(v["files"][1]["index"], "?");
            assert_eq!(v["files"][2]["path"], "new");
            assert_eq!(parse_status("## No commits yet on main\0")["clean"], true);
        }

        #[test]
        fn sandbox_paths() {
            let dir = std::env::temp_dir().join(format!("hk-git-sb-{}", std::process::id()));
            std::fs::create_dir_all(dir.join("a")).unwrap();
            assert!(resolve_under(&dir, "a", true).is_ok());
            assert!(resolve_under(&dir, "new", false).is_ok());
            assert!(resolve_under(&dir, "../x", false).is_err());
            assert!(resolve_under(&dir, "/etc", true).is_err());
            assert!(resolve_under(&dir, "a/../..", true).is_err());
            #[cfg(unix)]
            {
                let _ = std::fs::remove_file(dir.join("link"));
                std::os::unix::fs::symlink("/", dir.join("link")).unwrap();
                assert!(resolve_under(&dir, "link", true).is_err());
                assert!(resolve_under(&dir, "link/tmp", false).is_err());
            }
            assert!(is_local_source("repos/a.git") && !is_local_source("git@host:a/b.git"));
            assert!(!is_local_source("https://x/y"));
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
