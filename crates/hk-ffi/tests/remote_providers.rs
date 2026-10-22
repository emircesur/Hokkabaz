//! A phone configuring the model providers of an instance: add an endpoint, set a key, choose
//! the default model, list models. Run-scoped tokens may read but not change.

use std::sync::Arc;

use hk_ffi::dto::{CoreProfile, ModeDto, PreferencesDto, ProviderInputDto, ProviderKindDto};
use hk_ffi::handle::CoreHandle;
use hk_ffi::remote::RemoteCore;
use hk_providers::{HubOptions, ProviderHub};
use hk_server::{AppState, Auth, LocalConfig, LocalRuntime, Scope, serve_listener};
use hk_store::{MemorySecretStore, Secret, Store};
use tokio::net::TcpListener;

const RUN: &str = "run-token-0123456789";
const ADMIN: &str = "admin-token-0123456789";

#[tokio::test(flavor = "multi_thread")]
async fn configure_providers_remotely() {
    let dir = tempfile::tempdir().unwrap();
    let hub = ProviderHub::open(
        Store::open_in_memory().await.unwrap(),
        HubOptions {
            secrets: Arc::new(MemorySecretStore::default()),
            extras: Vec::new(),
            catalog_path: dir.path().join("models.json"),
            offline: None,
            env: false,
        },
    )
    .await
    .unwrap();
    let runtime = Arc::new(
        LocalRuntime::new(LocalConfig {
            provider: hub.provider(),
            sessions_dir: dir.path().join("sessions"),
            spill_dir: dir.path().join("spill"),
            default_model: "lab/m1".into(),
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
    let state = AppState::new(runtime, auth).with_providers(hub);
    tokio::spawn(serve_listener(listener, state, std::future::pending()));

    let admin = RemoteCore::new(&url, Secret::new(ADMIN), CoreProfile::Mobile).unwrap();
    let s = admin.get_settings().await.unwrap();
    assert!(
        s.providers
            .iter()
            .any(|p| p.id == "anthropic" && !p.enabled)
    );

    let s = admin
        .save_provider(ProviderInputDto {
            id: "lab".into(),
            kind: ProviderKindDto::OpenaiCompat,
            base_url: Some("http://127.0.0.1:9/v1".into()),
            enabled: true,
            models: vec!["m1".into()],
        })
        .await
        .unwrap();
    assert!(s.providers.iter().any(|p| p.id == "lab" && p.enabled));

    let s = admin
        .set_api_key("anthropic".into(), Some("sk-ant-test".into()))
        .await
        .unwrap();
    assert!(s.providers.iter().any(|p| p.id == "anthropic" && p.has_key));
    // The key is write-only: nothing in the settings carries it.
    assert!(!format!("{s:?}").contains("sk-ant-test"));

    let s = admin
        .set_preferences(PreferencesDto {
            default_model: Some("lab/m1".into()),
            mode: ModeDto::Plan,
            workspace_root: None,
        })
        .await
        .unwrap();
    assert_eq!(s.default_model.as_deref(), Some("lab/m1"));
    assert!(matches!(s.mode, ModeDto::Plan));

    let models = admin.list_models().await.unwrap();
    assert!(models.models.iter().any(|m| m.reference == "lab/m1"));

    // A new session picks the instance's default model.
    let session = admin
        .create_session(hk_ffi::dto::CreateSessionDto {
            workspace_root: None,
            model: None,
            title: None,
        })
        .await
        .unwrap();
    assert_eq!(session.model.as_deref(), Some("lab/m1"));

    let bad = admin
        .save_provider(ProviderInputDto {
            id: "x".into(),
            kind: ProviderKindDto::Anthropic,
            base_url: None,
            enabled: true,
            models: vec![],
        })
        .await;
    assert!(
        bad.is_err(),
        "custom providers must be openai_compat/ollama"
    );

    let reader = RemoteCore::new(&url, Secret::new(RUN), CoreProfile::Mobile).unwrap();
    assert!(reader.get_settings().await.is_ok());
    assert!(reader.remove_provider("lab".into()).await.is_err());

    let s = admin.remove_provider("lab".into()).await.unwrap();
    assert!(s.providers.iter().all(|p| p.id != "lab"));
}
