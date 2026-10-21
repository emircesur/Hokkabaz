//! Dart-facing data types. Plain structs/enums only (no hk-agent / hk-llm types leak), so a
//! `RemoteCore` over hk-server can produce the same shapes later.
//!
//! Counts and timestamps are `i64` (a Dart `int`); `u64` would become `BigInt` on the Dart side.

/// Where the core runs; picks the secret backend and tool policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreProfile {
    /// Desktop app: OS keyring when available.
    Desktop,
    /// Android / iOS: app-private secret file, no sandbox, no shell tool.
    Mobile,
}

/// Tool policy for a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeDto {
    /// All tools; risky calls ask for approval.
    Build,
    /// Read-only filesystem.
    Plan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKindDto {
    Deepseek,
    /// Any OpenAI-compatible endpoint (needs `base_url`).
    OpenaiCompat,
    Anthropic,
    Gemini,
    Ollama,
}

/// One configured provider. The API key itself never crosses FFI: only `has_key`.
#[derive(Debug, Clone)]
pub struct ProviderDto {
    pub id: String,
    pub kind: ProviderKindDto,
    pub base_url: Option<String>,
    pub enabled: bool,
    pub has_key: bool,
    /// One of the four fixed providers (cannot be removed, only disabled).
    pub builtin: bool,
    /// Fixed model ids (custom endpoints; empty = ask the provider).
    pub models: Vec<String>,
    /// Why the provider is not usable right now (missing key, bad endpoint), if so.
    pub error: Option<String>,
}

/// Create/update a provider (`save_provider`). Keys go through `set_api_key`.
#[derive(Debug, Clone)]
pub struct ProviderInputDto {
    pub id: String,
    pub kind: ProviderKindDto,
    pub base_url: Option<String>,
    pub enabled: bool,
    pub models: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SettingsDto {
    pub providers: Vec<ProviderDto>,
    /// `provider/model` used for new sessions.
    pub default_model: Option<String>,
    pub mode: ModeDto,
    /// Effective workspace root handed to tools (default: `<data_dir>/workspace`).
    pub workspace_root: String,
    pub profile: CoreProfile,
    pub data_dir: String,
}

#[derive(Debug, Clone)]
pub struct PreferencesDto {
    pub default_model: Option<String>,
    pub mode: ModeDto,
    /// `None` resets to the default workspace.
    pub workspace_root: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ModelDto {
    /// `provider/id`: what `submit_turn` / `create_session` take as model.
    pub reference: String,
    pub id: String,
    pub provider: String,
    pub name: String,
    pub context_window: i64,
    pub max_output: Option<i64>,
    pub tools: bool,
    pub reasoning: bool,
    pub vision: bool,
    /// USD per million tokens.
    pub input_per_mtok: Option<f64>,
    pub output_per_mtok: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct ProviderErrorDto {
    pub provider_id: String,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct ModelListDto {
    pub models: Vec<ModelDto>,
    /// Providers that failed to build or list; the rest are still usable.
    pub errors: Vec<ProviderErrorDto>,
}

#[derive(Debug, Clone)]
pub struct SessionInfoDto {
    pub id: String,
    pub title: Option<String>,
    pub model: Option<String>,
    pub cwd: Option<String>,
    pub created_ms: i64,
    pub updated_ms: i64,
    pub event_count: i64,
    pub turn_count: i64,
    /// A turn is executing right now.
    pub running: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleDto {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone)]
pub struct ToolCallDto {
    pub call_id: String,
    pub name: String,
    /// Arguments as JSON text.
    pub arguments_json: String,
}

/// One message of the history the model would see.
#[derive(Debug, Clone)]
pub struct MessageDto {
    pub role: RoleDto,
    pub content: String,
    pub reasoning: Option<String>,
    pub tool_calls: Vec<ToolCallDto>,
    /// For `Tool` messages: the call this answers.
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PendingApprovalDto {
    pub approval_id: String,
    pub call_id: String,
    pub tool: String,
    pub detail: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SessionDetailDto {
    pub session: SessionInfoDto,
    pub messages: Vec<MessageDto>,
    /// Approvals parked right now, for UIs attaching late.
    pub pending_approvals: Vec<PendingApprovalDto>,
}

#[derive(Debug, Clone, Default)]
pub struct CreateSessionDto {
    pub title: Option<String>,
    /// `provider/model`; default from settings.
    pub model: Option<String>,
    /// Default from settings.
    pub workspace_root: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TurnRequestDto {
    pub prompt: String,
    /// Switch mode from this turn on; `None` = the settings default.
    pub mode: Option<ModeDto>,
    /// Switch model (`provider/model`) from this turn on; `None` keeps the current one.
    pub model: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TurnAcceptedDto {
    pub session_id: String,
    /// Another turn was running; the prompt waits behind it.
    pub queued: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnEndReasonDto {
    Completed,
    Cancelled,
    Failed,
    MaxSteps,
}

/// Live session feed. Subscribe (`session_events`) before `submit_turn`: the feed carries only
/// what happens after subscribing; use `get_session` for history.
///
/// Streaming order inside a step: `TextDelta` / `ReasoningDelta` / `ToolCallStart` /
/// `ToolCallArgs` as the model streams, then the durable events (`AssistantMessage`,
/// `ToolCall`, `ApprovalRequest`, `ToolResult`, `Usage`, ...) a moment later. Durable events
/// are the source of truth; deltas are for live display.
#[derive(Debug, Clone)]
pub enum SessionEventDto {
    TurnStarted {
        turn_id: String,
    },
    UserMessage {
        content: String,
    },
    TextDelta {
        text: String,
    },
    ReasoningDelta {
        text: String,
    },
    /// Model began a tool call. `index` keys the following `ToolCallArgs`.
    ToolCallStart {
        index: u32,
        call_id: String,
        name: String,
    },
    /// Raw JSON fragment of the call's arguments, in order.
    ToolCallArgs {
        index: u32,
        delta: String,
    },
    /// Finished assistant message (replaces the accumulated deltas).
    AssistantMessage {
        content: String,
        reasoning: Option<String>,
    },
    /// Call with parsed arguments, about to run (or wait for approval).
    ToolCall {
        call_id: String,
        name: String,
        arguments_json: String,
    },
    ToolResult {
        call_id: String,
        content: String,
        is_error: bool,
    },
    ApprovalRequest {
        approval_id: String,
        call_id: String,
        tool: String,
        detail: Option<String>,
    },
    ApprovalResolved {
        approval_id: String,
        approved: bool,
        reason: Option<String>,
    },
    /// Token usage of one finished model request.
    Usage {
        input_tokens: i64,
        output_tokens: i64,
        cache_read_tokens: i64,
        cache_write_tokens: i64,
        reasoning_tokens: i64,
    },
    TurnEnded {
        turn_id: String,
        reason: TurnEndReasonDto,
    },
    TitleChanged {
        title: String,
    },
    /// `fatal == false`: a request attempt failed and is being retried.
    Error {
        message: String,
        fatal: bool,
    },
}
