//! Plugins on the daemon: installed plugins load at start; their nodes and polling triggers
//! register. Uses the sample plugin (built for wasm32-wasip2; skipped when the target is missing).
#![cfg(feature = "plugins")]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use hk_flow::{Node, NodeRegistry};
use hk_integrations::PollRegistry;
use hk_integrations::poll::PollContext;
use hk_plugin::store::PluginStore;
use hk_store::MemorySecretStore;
use serde_json::json;

fn build_hello() -> Option<PathBuf> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/hello");
    let ok = Command::new("cargo")
        .args(["build", "--release", "--target", "wasm32-wasip2", "--quiet"])
        .current_dir(&src)
        .status()
        .ok()?
        .success();
    let wasm = src.join("target/wasm32-wasip2/release/hello_plugin.wasm");
    (ok && wasm.is_file()).then_some(wasm)
}

#[tokio::test(flavor = "multi_thread")]
async fn installed_plugins_register_nodes_tools_and_triggers() {
    let Some(wasm) = build_hello() else { return };
    let data = tempfile::tempdir().unwrap();
    let src = data.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/hello/plugin.toml"),
        src.join("plugin.toml"),
    )
    .unwrap();
    std::fs::copy(&wasm, src.join("hello_plugin.wasm")).unwrap();
    PluginStore::new(data.path().join("plugins"))
        .install_dir(&src)
        .unwrap();

    let mut nodes = NodeRegistry::new();
    let mut polls = PollRegistry::new();
    let loaded = hk_daemon::plugins::load(
        data.path(),
        Arc::new(MemorySecretStore::default()),
        &mut nodes,
        &mut polls,
    );

    assert!(nodes.latest("hello.greet").is_some(), "plugin node");
    assert!(
        loaded.tools.iter().any(|t| t.spec().name == "hello_greet"),
        "plugin tool"
    );
    let ty = "dev.hokkabaz.hello.hello_tick";
    assert!(nodes.latest(ty).is_some(), "trigger node");

    let trig = polls.get(ty).expect("polling trigger");
    let node: Node = serde_json::from_value(
        json!({"name": "Tick", "type": ty, "typeVersion": 1, "position": [0, 0], "parameters": {}}),
    )
    .unwrap();
    let cx = PollContext::for_node(&node, Some("wf"), None);
    let first = trig.poll(&cx, None).await.unwrap();
    let second = trig.poll(&cx, first.cursor.as_ref()).await.unwrap();
    assert!(!first.items.is_empty() && !second.items.is_empty());
    assert_ne!(
        first.cursor, second.cursor,
        "the plugin's own state advances between polls"
    );
}
