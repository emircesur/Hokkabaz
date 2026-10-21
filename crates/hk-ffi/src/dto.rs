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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEventKind {
    /// `turn_id`.
    TurnStarted,
    /// `text`.
    UserMessage,
    /// `text`.
    TextDelta,
    /// `text`.
    ReasoningDelta,
    /// Model began a tool call: `index`, `call_id`, `name`. `index` keys the `ToolCallArgs`.
    ToolCallStart,
    /// `index`, `text` = raw JSON fragment of the arguments, in order.
    ToolCallArgs,
    /// Finished assistant message, replaces the accumulated deltas: `text`, `reasoning`.
    AssistantMessage,
    /// Call with parsed arguments, about to run or wait for approval: `call_id`, `name`,
    /// `arguments_json`.
    ToolCall,
    /// `call_id`, `text` = output, `is_error`.
    ToolResult,
    /// `approval_id`, `call_id`, `tool`, `detail`.
    ApprovalRequest,
    /// `approval_id`, `approved`, `reason`.
    ApprovalResolved,
    /// Token usage of one finished model request: `usage`.
    Usage,
    /// `turn_id`, `end_reason`.
    TurnEnded,
    /// `text`.
    TitleChanged,
    /// `text` = message, `fatal`. Non-fatal: a request attempt failed and is being retried.
    Error,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UsageDto {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub reasoning_tokens: i64,
}

/// Live session feed item. A flat struct (not a data-carrying enum) so the Dart side needs no
/// `freezed`: switch on `kind`, read the fields its doc lists; the rest are empty.
///
/// Subscribe (`session_events`) before `submit_turn`: the feed carries only what happens after
/// subscribing; use `get_session` for history.
///
/// Within a step, `TextDelta` / `ReasoningDelta` / `ToolCallStart` / `ToolCallArgs` stream live,
/// then the durable events (`AssistantMessage`, `ToolCall`, `ApprovalRequest`, `ToolResult`,
/// `Usage`, ...) follow a moment later. Durable events are the source of truth; deltas are for
/// live display.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionEventDto {
    pub kind: SessionEventKind,
    pub turn_id: Option<String>,
    pub text: Option<String>,
    pub reasoning: Option<String>,
    pub index: Option<u32>,
    pub call_id: Option<String>,
    pub name: Option<String>,
    pub arguments_json: Option<String>,
    pub approval_id: Option<String>,
    pub tool: Option<String>,
    pub detail: Option<String>,
    pub reason: Option<String>,
    pub is_error: bool,
    pub approved: bool,
    pub fatal: bool,
    pub usage: Option<UsageDto>,
    pub end_reason: Option<TurnEndReasonDto>,
}

impl SessionEventDto {
    pub(crate) fn new(kind: SessionEventKind) -> Self {
        Self {
            kind,
            turn_id: None,
            text: None,
            reasoning: None,
            index: None,
            call_id: None,
            name: None,
            arguments_json: None,
            approval_id: None,
            tool: None,
            detail: None,
            reason: None,
            is_error: false,
            approved: false,
            fatal: false,
            usage: None,
            end_reason: None,
        }
    }
}

/// One node type of the flow engine registry (n8n `INodeTypeDescription`, trimmed). Drives the
/// node picker, port layout and the generated params form.
#[derive(Debug, Clone)]
pub struct NodeTypeDto {
    /// Full workflow `type` string, e.g. `n8n-nodes-base.if`.
    pub type_name: String,
    /// Every supported `typeVersion`, ascending.
    pub versions: Vec<f64>,
    pub default_version: f64,
    pub display_name: String,
    pub description: String,
    /// n8n groups: `trigger`, `transform`, `output`, ...
    pub group: Vec<String>,
    /// Connection type per input (`main`, `ai_tool`, ...). Empty when `inputs_expr` is set.
    pub inputs: Vec<String>,
    /// Connection type per output. Empty when `outputs_expr` is set.
    pub outputs: Vec<String>,
    /// `={{ ... }}` port declaration that depends on node parameters (Switch, Merge).
    pub inputs_expr: Option<String>,
    pub outputs_expr: Option<String>,
    pub output_names: Vec<String>,
    /// JSON array of n8n `NodeProperty` objects.
    pub properties_json: String,
}
