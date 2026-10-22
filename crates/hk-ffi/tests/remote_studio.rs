//! A phone driving a server's Video Studio: upload a video, import it, read the project,
//! build a proxy, stream it back with ranges, download it. Real ffmpeg, real hk-server over
//! loopback; skipped when ffmpeg is missing.
#![allow(clippy::print_stderr)]

use std::process::Command;
use std::sync::{Arc, Mutex};

use hk_ffi::dto::CoreProfile;
use hk_ffi::handle::CoreHandle;
use hk_ffi::remote::RemoteCore;
use hk_server::{AppState, Auth, LocalConfig, LocalRuntime, Scope, serve_listener};
use hk_store::{MemorySecretStore, Secret, Store};
use serde_json::json;
use tokio::net::TcpListener;

const RUN: &str = "run-token-0123456789";
const ADMIN: &str = "admin-token-0123456789";

fn make_video(path: &std::path::Path) -> bool {
    Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=320x240:rate=15:duration=2",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=2",
            "-shortest",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(path)
        .status()
        .is_ok_and(|s| s.success())
}

#[tokio::test(flavor = "multi_thread")]
async fn upload_import_stream_and_download() {
    let dir = tempfile::tempdir().unwrap();
    let video = dir.path().join("in.mp4");
    if !make_video(&video) {
        eprintln!("ffmpeg not available; skipping");
        return;
    }

    // the daemon side
    let server_dir = dir.path().join("server");
    let store = Store::open_in_memory().await.unwrap();
    let secrets = Arc::new(MemorySecretStore::default());
    let provider: Arc<dyn hk_llm::Provider> = Arc::new(hk_llm::Router::new());
    let studio = hk_daemon::studio_api::build(&server_dir, store, secrets, provider)
        .await
        .unwrap();
    let runtime = Arc::new(
        LocalRuntime::new(LocalConfig {
            provider: Arc::new(hk_llm::Router::new()),
            sessions_dir: server_dir.join("sessions"),
            spill_dir: server_dir.join("spill"),
            default_model: "m".into(),
            default_cwd: None,
            max_steps: 1,
        })
        .unwrap(),
    );
    let auth = Auth::new()
        .with_token(Scope::Run, RUN)
        .with_token(Scope::Admin, ADMIN);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let state =
        AppState::new(runtime, auth).with_studio(Arc::new(hk_daemon::studio_api::Api(studio)));
    tokio::spawn(serve_listener(listener, state, std::future::pending()));

    // the phone side
    let core = RemoteCore::new(&url, Secret::new(RUN), CoreProfile::Mobile).unwrap();
    assert!(core.studio_is_remote());
    let tools = core.studio_rpc("check_tools", json!({})).await.unwrap();
    assert_eq!(tools["ffmpeg"], true);

    let progress = Mutex::new(Vec::new());
    let server_path = core
        .studio_upload(video.to_str().unwrap(), &|sent, total| {
            progress.lock().unwrap().push((sent, total))
        })
        .await
        .unwrap();
    assert!(server_path.contains("_uploads"), "{server_path}");
    let last = *progress.lock().unwrap().last().unwrap();
    assert_eq!(last.0, last.1);
    assert_eq!(last.1, std::fs::metadata(&video).unwrap().len());

    let project = core
        .studio_rpc("import", json!({"path": server_path, "name": "demo"}))
        .await
        .unwrap();
    let id = project["id"].as_str().unwrap().to_owned();
    assert_eq!(project["name"], "demo");
    assert!(project["source"]["duration"].as_f64().unwrap() > 1.5);
    let listed = core.studio_rpc("projects", json!({})).await.unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 1);

    // a player gets a URL it can range over without the API token
    let proxy = core.studio_rpc("proxy", json!({"id": id})).await.unwrap();
    let media = core.studio_media(proxy.as_str().unwrap()).await.unwrap();
    assert!(media.contains("ticket=") && !media.contains(RUN));
    let http = reqwest::Client::new();
    let part = http
        .get(&media)
        .header("range", "bytes=0-99")
        .send()
        .await
        .unwrap();
    assert_eq!(part.status(), 206);
    assert_eq!(part.bytes().await.unwrap().len(), 100);

    // a path the Studio does not own is not served
    let nope = core.studio_media("/etc/passwd").await.unwrap();
    assert_eq!(http.get(&nope).send().await.unwrap().status(), 404);

    // download = byte-identical copy
    let dest = dir.path().join("out.mp4");
    core.studio_download(proxy.as_str().unwrap(), dest.to_str().unwrap())
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(&dest).unwrap(),
        std::fs::read(proxy.as_str().unwrap()).unwrap()
    );

    // errors keep their kind across the wire
    let err = core
        .studio_rpc("project", json!({"id": "does-not-exist"}))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err.kind,
            hk_ffi::error::HkErrorKind::NotFound | hk_ffi::error::HkErrorKind::InvalidArgument
        ),
        "{err:?}"
    );
}
