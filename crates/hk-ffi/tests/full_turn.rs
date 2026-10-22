//! Whole turn through the DTO layer with a scripted provider: settings, session, streamed
//! events, tool approval, history. No network.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use hk_ffi::dto::{
    CoreProfile, CreateSessionDto, ModeDto, PreferencesDto, ProviderInputDto, ProviderKindDto,
    RoleDto, SessionEventDto, SessionEventKind as K, TurnEndReasonDto, TurnRequestDto,
};
use hk_ffi::error::HkErrorKind;
use hk_ffi::handle::CoreHandle;
use hk_ffi::local::{CoreOptions, LocalCore};
use hk_llm::{
    CancellationToken, Capabilities, ChatRequest, EventStream, FinishReason, ModelInfo, Provider,
    StreamEvent, Usage,
};
use hk_store::MemorySecretStore;

type Script = Vec<hk_llm::Result<StreamEvent>>;

/// One script per request, in order.
struct Scripted(Mutex<Vec<Script>>);

#[async_trait::async_trait]
impl Provider for Scripted {
    fn id(&self) -> &str {
        "fake"
    }

    async fn models(&self) -> hk_llm::Result<Vec<ModelInfo>> {
        Ok(vec![ModelInfo {
            id: "m".into(),
            provider: "fake".into(),
            name: "Fake".into(),
            context_window: 8192,
            max_output: None,
            capabilities: Capabilities {
                tools: true,
                reasoning: false,
                vision: false,
            },
            cost: None,
        }])
    }

    async fn stream(
        &self,
        _req: ChatRequest,
        _cancel: CancellationToken,
    ) -> hk_llm::Result<EventStream> {
        let script = {
            let mut v = self.0.lock().unwrap();
            if v.is_empty() {
                Vec::new()
            } else {
                v.remove(0)
            }
        };
        Ok(futures::stream::iter(script).boxed())
    }
}

fn scripts() -> Vec<Script> {
    vec![
        vec![
            Ok(StreamEvent::ToolCallDelta {
                index: 0,
                id: Some("c1".into()),
                name: Some("write".into()),
                arguments_delta: r#"{"path":"hello.txt","#.into(),
            }),
            Ok(StreamEvent::ToolCallDelta {
                index: 0,
                id: None,
                name: None,
                arguments_delta: r#""content":"hi"}"#.into(),
            }),
            Ok(StreamEvent::Finish(FinishReason::ToolCalls)),
        ],
        vec![
            Ok(StreamEvent::ReasoningDelta { text: "hmm".into() }),
            Ok(StreamEvent::TextDelta { text: "do".into() }),
            Ok(StreamEvent::TextDelta { text: "ne".into() }),
            Ok(StreamEvent::Usage(Usage {
                input_tokens: 7,
                output_tokens: 3,
                ..Usage::default()
            })),
            Ok(StreamEvent::Finish(FinishReason::Stop)),
        ],
    ]
}

async fn next(
    feed: &mut (impl futures::Stream<Item = SessionEventDto> + Unpin),
) -> SessionEventDto {
    tokio::time::timeout(Duration::from_secs(10), feed.next())
        .await
        .expect("event within 10s")
        .expect("feed open")
}

#[tokio::test]
async fn full_turn_with_approval_over_dto_stream() {
    let dir = tempfile::tempdir().unwrap();
    let core = LocalCore::open(
        dir.path().join("data"),
        CoreProfile::Mobile,
        CoreOptions {
            secrets: Some(Arc::new(MemorySecretStore::default())),
            extra_providers: vec![Arc::new(Scripted(Mutex::new(scripts())))],
            refresh_catalog: false,
        },
    )
    .await
    .unwrap();

    // Settings: a key goes in, only `has_key` comes out.
    let s = core
        .set_api_key("deepseek".into(), Some("sk-topsecret".into()))
        .await
        .unwrap();
    let ds = s.providers.iter().find(|p| p.id == "deepseek").unwrap();
    assert!(ds.has_key && ds.enabled && ds.error.is_none());
    assert!(!format!("{s:?}").contains("topsecret"));
    // Disabled-by-default built-ins with no key stay quiet; an enabled one without key reports.
    let s = core
        .save_provider(ProviderInputDto {
            id: "anthropic".into(),
            kind: ProviderKindDto::Anthropic,
            base_url: None,
            enabled: true,
            models: vec![],
        })
        .await
        .unwrap();
    assert!(
        s.providers
            .iter()
            .find(|p| p.id == "anthropic")
            .unwrap()
            .error
            .is_some()
    );
    let bad = core
        .save_provider(ProviderInputDto {
            id: "mine".into(),
            kind: ProviderKindDto::OpenaiCompat,
            base_url: None,
            enabled: true,
            models: vec![],
        })
        .await
        .unwrap_err();
    assert_eq!(bad.kind, HkErrorKind::InvalidArgument);
    core.set_preferences(PreferencesDto {
        default_model: Some("fake/m".into()),
        mode: ModeDto::Build,
        workspace_root: None,
    })
    .await
    .unwrap();

    let models = core.list_models().await.unwrap();
    assert!(
        models
            .models
            .iter()
            .any(|m| m.reference == "fake/m" && m.tools)
    );
    assert!(models.errors.iter().any(|e| e.provider_id == "anthropic"));

    // Session in an explicit workspace; subscribe before submitting.
    let ws = dir.path().join("ws");
    let info = core
        .create_session(CreateSessionDto {
            workspace_root: Some(ws.display().to_string()),
            ..CreateSessionDto::default()
        })
        .await
        .unwrap();
    assert_eq!(info.model.as_deref(), Some("fake/m"));
    let mut feed = core.session_events(&info.id).unwrap();
    core.submit_turn(
        &info.id,
        TurnRequestDto {
            prompt: "write a file".into(),
            mode: Some(ModeDto::Build),
            model: None,
        },
    )
    .await
    .unwrap();

    let mut seen = Vec::new();
    let mut text = String::new();
    let mut args = String::new();
    loop {
        let ev = next(&mut feed).await;
        seen.push(ev.kind);
        match ev.kind {
            K::TextDelta => text.push_str(ev.text.as_deref().unwrap()),
            K::ToolCallArgs => args.push_str(ev.text.as_deref().unwrap()),
            K::ApprovalRequest => {
                assert_eq!(ev.tool.as_deref(), Some("write"));
                assert!(!ws.join("hello.txt").exists(), "ran before approval");
                let pending = core.get_session(&info.id).await.unwrap().pending_approvals;
                assert_eq!(pending.len(), 1);
                core.respond_approval(&info.id, ev.approval_id.as_deref().unwrap(), true, None)
                    .await
                    .unwrap();
            }
            K::Usage => {
                let u = ev.usage.unwrap();
                assert_eq!((u.input_tokens, u.output_tokens), (7, 3));
            }
            K::TurnEnded => {
                assert_eq!(ev.end_reason, Some(TurnEndReasonDto::Completed));
                break;
            }
            K::Error => panic!("unexpected error event: {:?}", ev.text),
            _ => {}
        }
    }

    for k in [
        K::TurnStarted,
        K::UserMessage,
        K::ToolCallStart,
        K::ToolCallArgs,
        K::ToolCall,
        K::ApprovalRequest,
        K::ApprovalResolved,
        K::ToolResult,
        K::ReasoningDelta,
        K::AssistantMessage,
        K::Usage,
    ] {
        assert!(seen.contains(&k), "missing {k:?} in {seen:?}");
    }
    assert_eq!(text, "done");
    assert_eq!(args, r#"{"path":"hello.txt","content":"hi"}"#);
    assert_eq!(std::fs::read_to_string(ws.join("hello.txt")).unwrap(), "hi");

    let detail = core.get_session(&info.id).await.unwrap();
    assert!(detail.pending_approvals.is_empty());
    assert!(
        detail
            .messages
            .iter()
            .any(|m| m.role == RoleDto::User && m.content == "write a file")
    );
    assert!(detail.messages.iter().any(|m| m.role == RoleDto::Tool));
    assert!(
        detail
            .messages
            .iter()
            .any(|m| m.role == RoleDto::Assistant && m.content == "done")
    );
    assert_eq!(core.list_sessions(10, 0).await.unwrap().len(), 1);

    core.shutdown().await;
    assert!(feed.next().await.is_none(), "feed ends on shutdown");
}
