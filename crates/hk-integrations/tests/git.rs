//! Git node against a temporary local bare repository (skipped when `git` is missing).
#![cfg(all(feature = "int-git", not(target_os = "android")))]

use std::path::{Path, PathBuf};
use std::process::Command;

use hk_flow::{Item, Node, NodeContext, NodeRegistry};
use hk_integrations::CredentialTypes;
use hk_integrations::git::register_with_root;
use serde_json::{Value, json};

struct Dir(PathBuf);
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn ctx(params: Value) -> NodeContext {
    let node: Node = serde_json::from_value(json!({
        "name": "Git", "type": "n8n-nodes-base.git", "typeVersion": 1, "position": [0, 0], "parameters": params
    }))
    .unwrap();
    NodeContext::new(node, vec![vec![Item::from_json(json!({}))]], 0)
}

fn bare_git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).trim().to_owned()
}

#[tokio::test]
async fn clone_commit_push_log_status_tag_roundtrip() {
    if !git_available() {
        return;
    }
    let root = Dir(std::env::temp_dir().join(format!("hk-git-it-{}", std::process::id())));
    std::fs::create_dir_all(&root.0).unwrap();
    let root_path = root.0.canonicalize().unwrap();
    bare_git(&root_path, &["init", "--bare", "-q", "origin.git"]);

    let mut nodes = NodeRegistry::new();
    register_with_root(&mut nodes, &CredentialTypes::new(), Some(root_path.clone())).unwrap();
    let git = nodes.latest("n8n-nodes-base.git").unwrap();
    let run = |p: Value| {
        let git = git.clone();
        async move { git.execute(&ctx(p)).await }
    };

    run(json!({"operation": "clone", "repositoryPath": "work", "sourceRepository": "origin.git"}))
        .await
        .unwrap();
    run(json!({"operation": "checkout", "repositoryPath": "work", "branch": "main", "createBranch": true})).await.unwrap();
    std::fs::write(root_path.join("work/a.txt"), "hello").unwrap();

    let st = run(json!({"operation": "status", "repositoryPath": "work"}))
        .await
        .unwrap();
    assert_eq!(st[0][0].json["clean"], false);
    assert_eq!(st[0][0].json["files"][0]["path"], "a.txt");

    run(json!({"operation": "add", "repositoryPath": "work", "pathsToAdd": "a.txt"}))
        .await
        .unwrap();
    run(json!({
        "operation": "commit", "repositoryPath": "work", "message": "first",
        "options": {"authorName": "T", "authorEmail": "t@x.io"}
    }))
    .await
    .unwrap();
    run(json!({"operation": "tag", "repositoryPath": "work", "name": "v1"}))
        .await
        .unwrap();
    run(json!({"operation": "push", "repositoryPath": "work", "branch": "main"}))
        .await
        .unwrap();
    run(json!({"operation": "pushTags", "repositoryPath": "work"}))
        .await
        .unwrap();

    let log = run(json!({"operation": "log", "repositoryPath": "work", "returnAll": true}))
        .await
        .unwrap();
    assert_eq!(log[0].len(), 1);
    assert_eq!(log[0][0].json["message"], "first");
    assert_eq!(log[0][0].json["author_email"], "t@x.io");

    // the bare repo really received branch and tag
    assert_eq!(
        bare_git(
            &root_path.join("origin.git"),
            &["log", "--format=%s", "main"]
        ),
        "first"
    );
    assert_eq!(bare_git(&root_path.join("origin.git"), &["tag"]), "v1");

    run(json!({"operation": "fetch", "repositoryPath": "work"}))
        .await
        .unwrap();
    run(json!({"operation": "pull", "repositoryPath": "work", "branch": "main"}))
        .await
        .unwrap();
    let st = run(json!({"operation": "status", "repositoryPath": "work"}))
        .await
        .unwrap();
    assert_eq!(st[0][0].json["clean"], true);
    assert_eq!(st[0][0].json["current"], "main");
}

#[tokio::test]
async fn paths_and_arguments_are_confined() {
    if !git_available() {
        return;
    }
    let root = Dir(std::env::temp_dir().join(format!("hk-git-sb-it-{}", std::process::id())));
    std::fs::create_dir_all(root.0.join("plain")).unwrap();
    let mut nodes = NodeRegistry::new();
    register_with_root(&mut nodes, &CredentialTypes::new(), Some(root.0.clone())).unwrap();
    let git = nodes.latest("n8n-nodes-base.git").unwrap();
    let err = |p: Value| {
        let git = git.clone();
        async move { git.execute(&ctx(p)).await.unwrap_err().to_string() }
    };

    assert!(
        err(json!({"operation": "status", "repositoryPath": "../elsewhere"}))
            .await
            .contains("`..`")
    );
    assert!(
        err(json!({"operation": "status", "repositoryPath": "/etc"}))
            .await
            .contains("outside")
    );
    // option injection through ref-like values and clone sources
    assert!(
        err(json!({"operation": "checkout", "repositoryPath": "plain", "branch": "--orphan"}))
            .await
            .contains("Invalid branch")
    );
    assert!(
        err(json!({"operation": "clone", "repositoryPath": "c", "sourceRepository": "--upload-pack=touch x"}))
            .await
            .contains("Invalid source")
    );
    assert!(
        err(json!({"operation": "clone", "repositoryPath": "c", "sourceRepository": "ext::sh -c touch% x"}))
            .await
            .contains("Unsupported")
    );
    assert!(
        err(json!({"operation": "clone", "repositoryPath": "c", "sourceRepository": "/etc"}))
            .await
            .contains("outside")
    );
    // a folder that is not a repository must not fall through to a repository above it
    let e = err(json!({"operation": "status", "repositoryPath": "plain"})).await;
    assert!(e.contains("not a git repository"), "{e}");
}
