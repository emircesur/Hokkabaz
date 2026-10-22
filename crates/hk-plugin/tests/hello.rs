//! The sample plugin (plugins/hello, built for wasm32-wasip2) through the whole host:
//! install, load, tools, node, trigger, capability enforcement.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use hk_plugin::host::{EmitEvent, HostServices, PluginHost, SecretLookup};
use hk_plugin::store::PluginStore;
use hk_tools::{Risk, ToolCtx};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

struct NoSecrets;
impl SecretLookup for NoSecrets {
    fn get(&self, _name: &str) -> Option<String> {
        None
    }
}

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
async fn hello_plugin_end_to_end() {
    let Some(wasm) = build_hello() else {
        return; // wasm32-wasip2 target missing: nothing to test here
    };
    let dir = tempfile::tempdir().unwrap();
    // Install with the http capability pointed at a loopback server.
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    let manifest = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/hello/plugin.toml"),
    )
    .unwrap()
    .replace("\"httpbin.org\"", "\"127.0.0.1\"");
    std::fs::write(src.join("plugin.toml"), manifest).unwrap();
    std::fs::copy(&wasm, src.join("hello_plugin.wasm")).unwrap();
    let store = PluginStore::new(dir.path().join("plugins"));
    let installed = store.install_dir(&src).unwrap();

    let emitted: Arc<Mutex<Vec<EmitEvent>>> = Arc::default();
    let sink = emitted.clone();
    let host = PluginHost::new(HostServices {
        secrets: Arc::new(NoSecrets),
        emit: Arc::new(move |e| sink.lock().unwrap().push(e)),
        data_dir: dir.path().join("data"),
    })
    .unwrap();
    let plugin = host.load(&installed).unwrap();

    // describe
    let d = &plugin.description;
    assert_eq!(
        d.tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        ["hello_greet", "hello_fetch"]
    );
    assert_eq!(d.nodes.len(), 1);
    assert_eq!(d.triggers[0].name, "hello_tick");

    // tools through the agent adapter
    let tools = hk_plugin::tool::plugin_tools(&plugin);
    let greet = tools
        .iter()
        .find(|t| t.spec().name == "hello_greet")
        .unwrap();
    let fetch = tools
        .iter()
        .find(|t| t.spec().name == "hello_fetch")
        .unwrap();
    assert_eq!(fetch.risk(&json!({})), Risk::Ask);
    let out = greet
        .execute(ToolCtx::default(), json!({"name": "Ada"}))
        .await
        .unwrap();
    assert_eq!(out.content, "Hello, Ada!");
    let bad = greet.execute(ToolCtx::default(), json!({})).await.unwrap();
    assert!(
        bad.is_error && bad.content.contains("name"),
        "plugin errors reach the model as tool errors: {bad:?}"
    );

    // capabilities: a granted host works, anything else is denied (without trapping the plugin)
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else {
                return;
            };
            let mut b = vec![0u8; 4096];
            let _ = s.read(&mut b).await;
            let _ = s
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\nconnection: close\r\n\r\nhello",
                )
                .await;
        }
    });
    let ok = fetch
        .execute(
            ToolCtx::default(),
            json!({"url": format!("http://127.0.0.1:{port}/x")}),
        )
        .await
        .unwrap();
    assert_eq!(ok.content, "200 5 bytes", "{ok:?}");
    let denied = fetch
        .execute(ToolCtx::default(), json!({"url": "http://example.com/"}))
        .await
        .unwrap();
    assert!(
        denied.is_error && denied.content.contains("not granted"),
        "{denied:?}"
    );
    let scheme = fetch
        .execute(ToolCtx::default(), json!({"url": "file:///etc/passwd"}))
        .await
        .unwrap();
    assert!(
        scheme.is_error && scheme.content.contains("scheme"),
        "{scheme:?}"
    );

    // node
    let mut reg = hk_flow::NodeRegistry::new();
    assert_eq!(hk_plugin::node::register_plugin_nodes(&mut reg, &plugin), 1);
    let v = plugin
        .execute_node(
            "hello.greet",
            json!({"params": {"name": "Bo"}, "items": [{"json": {"a": 1}}, {"json": {"a": 2}}]}),
        )
        .unwrap();
    assert_eq!(v["items"][0][1]["json"]["greeting"], "Hello, Bo!");
    assert_eq!(v["items"][0][0]["json"]["a"], 1);
    assert!(plugin.execute_node("nope", json!({})).is_err());

    // trigger + key/value capability
    let (events, state) = plugin.poll_trigger("hello_tick", &Value::Null).unwrap();
    assert_eq!(events, vec![json!({"tick": 1})]);
    let (events, _) = plugin.poll_trigger("hello_tick", &state).unwrap();
    assert_eq!(events, vec![json!({"tick": 2})]);
    let kv = std::fs::read_to_string(dir.path().join("data/dev.hokkabaz.hello/kv.json")).unwrap();
    assert!(kv.contains("\"last_tick\":\"2\""), "{kv}");

    // unload
    assert!(host.unload("dev.hokkabaz.hello"));
    assert!(host.get("dev.hokkabaz.hello").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn without_capabilities_the_plugin_is_boxed_in() {
    let Some(wasm) = build_hello() else { return };
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    // Same wasm, no capabilities at all.
    let manifest = "id = \"dev.hokkabaz.hello\"\nname = \"Hello\"\nversion = \"0.1.0\"\nwasm = \"hello_plugin.wasm\"\n";
    std::fs::write(src.join("plugin.toml"), manifest).unwrap();
    std::fs::copy(&wasm, src.join("hello_plugin.wasm")).unwrap();
    let installed = PluginStore::new(dir.path().join("p"))
        .install_dir(&src)
        .unwrap();
    let host = PluginHost::new(HostServices {
        secrets: Arc::new(NoSecrets),
        emit: Arc::new(|_| {}),
        data_dir: dir.path().join("d"),
    })
    .unwrap();
    let plugin = host.load(&installed).unwrap();
    // The trigger writes to kv: denied, so the plugin reports an error.
    let err = plugin
        .poll_trigger("hello_tick", &Value::Null)
        .unwrap_err()
        .to_string();
    assert!(err.contains("not granted"), "{err}");
    // Pure computation still works.
    assert_eq!(
        plugin.call_tool("hello_greet", r#"{"name":"x"}"#).unwrap(),
        "Hello, x!"
    );
}
