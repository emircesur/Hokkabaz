//! Real child-process transport against `fake-acp-agent`.

use std::sync::Arc;

use async_trait::async_trait;
use hk_acp::{
    AcpEngine, AcpError, ApprovalPrompt, Approver, Decision, EngineEvent, EngineOptions,
    EngineSpec, StopReason,
};

struct Allow;

#[async_trait]
impl Approver for Allow {
    async fn decide(&self, _: &ApprovalPrompt) -> Decision {
        Decision::AllowOnce
    }
}

#[tokio::test]
async fn spawn_handshake_prompt_and_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let mut spec = EngineSpec::new("fake", env!("CARGO_BIN_EXE_fake-acp-agent"), &[]);
    spec.env.insert("FAKE_ACP_GREETING".into(), "hey".into());
    let mut engine = AcpEngine::spawn(
        &spec,
        EngineOptions::new(dir.path().to_path_buf(), Arc::new(Allow)),
    )
    .await
    .unwrap();
    assert_eq!(engine.agent_info().unwrap().name, "fake-acp-agent");
    let mut rx = engine.take_events().unwrap();

    assert_eq!(engine.prompt("ping").await.unwrap(), StopReason::EndTurn);
    // second turn on the same session
    assert_eq!(engine.prompt("again").await.unwrap(), StopReason::EndTurn);

    let mut text = String::new();
    while let Ok(e) = rx.try_recv() {
        if let EngineEvent::Text(t) = e {
            text.push_str(&t);
        }
    }
    assert!(text.contains("hey echo: ping [allowed]"), "{text}");
    assert!(text.contains("hey echo: again [allowed]"), "{text}");
    engine.shutdown().await;
}

#[tokio::test]
async fn missing_command_is_spawn_error() {
    let dir = tempfile::tempdir().unwrap();
    let spec = EngineSpec::new("nope", "/definitely/not/a/binary", &[]);
    let err = AcpEngine::spawn(
        &spec,
        EngineOptions::new(dir.path().to_path_buf(), Arc::new(Allow)),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, AcpError::Spawn { .. }), "{err}");
}
