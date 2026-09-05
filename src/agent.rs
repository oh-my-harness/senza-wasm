//! `EmbeddedAgent` — the `#[wasm_bindgen]` facade over `agent_loop`
//! (senza-wasm spec §4.2).
//!
//! Pump model: `prompt()` spawns a local task driving the agent loop,
//! events queue as JSON lines, the host drains them each frame via
//! `poll()`. Tools arrive as JS closures (`JsTool`) or pump placeholders
//! (`PendingTool`) resolved via `resolve_tool`.

use std::sync::Arc;

use futures::StreamExt;
use parking_lot::Mutex;
use wasm_bindgen::prelude::*;

use llm_harness_loop::{agent_loop, DefaultConvertToLlm, FinalAnswerMode, LoopConfig};
use llm_harness_types::{
    AgentContext, AgentEvent, RunContext, RunRequest, StreamOptions, ToolExecutionMode,
};
use tokio_util::sync::CancellationToken;

use crate::events::event_to_json;
use crate::tools::{JsTool, PendingTool};

pub(crate) struct AgentState {
    pub queue: Vec<String>,
    pub abort: CancellationToken,
    pub tools: Vec<Arc<dyn llm_harness_types::Tool>>,
    pub running: bool,
    /// Pump tools: call_id → result channel. `resolve_tool` pushes here;
    /// the PendingTool's receiver (inside the Tool) awaits it.
    pub pump_tx: std::collections::HashMap<String, futures::channel::mpsc::Sender<String>>,
}

impl AgentState {
    fn new() -> Self {
        Self {
            queue: Vec::new(),
            abort: CancellationToken::new(),
            tools: Vec::new(),
            running: false,
            pump_tx: std::collections::HashMap::new(),
        }
    }
}

/// Immutable per-agent wiring (provider + model parameters).
struct AgentDeps {
    client: Arc<dyn llm_adapter::provider::Provider>,
    model: String,
    max_tokens: u32,
    temperature: Option<f32>,
    system_prompt: Option<String>,
    max_turns: u32,
}

#[wasm_bindgen]
pub struct EmbeddedAgent {
    state: Arc<Mutex<AgentState>>,
    deps: AgentDeps,
}

/// Options accepted by `createAgent` (JSON string).
#[derive(serde::Deserialize)]
struct AgentOpts {
    /// `mock` (test facade) or `openai`.
    #[serde(default = "default_provider")]
    provider: String,
    #[serde(default)]
    api_key: String,
    #[serde(default)]
    base_url: Option<String>,
    model: String,
    #[serde(default)]
    system_prompt: Option<String>,
    #[serde(default = "default_max_tokens")]
    max_tokens: u32,
    #[serde(default = "default_max_turns")]
    max_turns: u32,
    #[serde(default)]
    temperature: Option<f32>,
}

fn default_provider() -> String {
    "openai".into()
}
fn default_max_tokens() -> u32 {
    4096
}
fn default_max_turns() -> u32 {
    16
}

impl EmbeddedAgent {
    fn new_with_client(client: Arc<dyn llm_adapter::provider::Provider>, opts: AgentOpts) -> Self {
        Self {
            state: Arc::new(Mutex::new(AgentState::new())),
            deps: AgentDeps {
                client,
                model: opts.model,
                max_tokens: opts.max_tokens,
                temperature: opts.temperature,
                system_prompt: opts.system_prompt,
                max_turns: opts.max_turns,
            },
        }
    }
}

#[wasm_bindgen]
impl EmbeddedAgent {
    /// Create an agent from a JSON options string:
    /// `{provider, apiKey, baseUrl?, model, systemPrompt?, maxTokens?, maxTurns?, temperature?}`.
    #[wasm_bindgen(constructor)]
    pub fn new(opts_json: String) -> Result<EmbeddedAgent, JsValue> {
        let opts: AgentOpts = serde_json::from_str(&opts_json)
            .map_err(|e| JsValue::from_str(&format!("invalid createAgent options: {e}")))?;
        match opts.provider.as_str() {
            "mock" => {
                let client = Arc::new(llm_harness_loop::test_utils::MockLlmClient::new(vec![
                    llm_harness_loop::test_utils::MockResponse::text("mock response"),
                ]));
                Ok(Self::new_with_client(client, opts))
            }
            "openai" => {
                let mut builder =
                    llm_harness_loop::OpenAIProvider::builder(opts.api_key.clone());
                if let Some(url) = &opts.base_url {
                    builder = builder.base_url(url.clone());
                }
                let client: Arc<dyn llm_adapter::provider::Provider> =
                    Arc::new(builder.build());
                Ok(Self::new_with_client(client, opts))
            }
            other => Err(JsValue::from_str(&format!(
                "unknown provider '{other}' (expected 'openai' or 'mock')"
            ))),
        }
    }

    /// Register a tool. With `callback` (an async JS function
    /// `(argsJson) => Promise<resultJson>`) the loop calls it directly;
    /// with `null` the tool becomes a pump placeholder: the loop surfaces
    /// `tool_execution_start` events and the host must call
    /// `resolve_tool(toolUseId, resultJson)`.
    pub fn register_tool(
        &self,
        name: String,
        description: String,
        schema_json: String,
        callback: Option<js_sys::Function>,
    ) -> Result<(), JsValue> {
        let schema: serde_json::Value = serde_json::from_str(&schema_json)
            .map_err(|e| JsValue::from_str(&format!("invalid tool schema: {e}")))?;
        let mut state = self.state.lock();
        match callback {
            Some(cb) => {
                state
                    .tools
                    .push(Arc::new(JsTool::new(name, description, schema, cb)));
            }
            None => {
                // Pump placeholder. The loop keys tool calls by tool_use_id
                // (the LLM-assigned id), so the placeholder is created
                // per-registration with a fresh channel; resolve_tool
                // addresses it by tool_use_id.
                let call_id = uuid::Uuid::new_v4().to_string();
                let (tx, rx) = futures::channel::mpsc::channel::<String>(1);
                state.pump_tx.insert(call_id.clone(), tx);
                state
                    .tools
                    .push(PendingTool::new(name, description, schema, call_id, rx));
            }
        }
        Ok(())
    }

    /// Start one run with `text` as the user message. Returns immediately;
    /// drain events with `poll()`.
    pub fn prompt(&self, text: String) {
        let client = self.deps.client.clone();
        let model = self.deps.model.clone();
        let max_tokens = self.deps.max_tokens;
        let temperature = self.deps.temperature;
        let system_prompt = self.deps.system_prompt.clone();
        let max_turns = self.deps.max_turns;
        let state = self.state.clone();
        let tools = state.lock().tools.clone();
        let abort = state.lock().abort.clone();

        state.lock().running = true;

        wasm_bindgen_futures::spawn_local(async move {
            let request = RunRequest::from_text(text);
            let ctx = AgentContext {
                system_prompt,
                messages: vec![],
            };
            let config = build_config(
                &request,
                &model,
                max_tokens,
                temperature,
                tools,
                abort.clone(),
            );
            let stream = agent_loop(client, ctx, config);
            futures::pin_mut!(stream);
            let mut turns_started = 0u32;
            while let Some(event) = stream.next().await {
                if matches!(event, AgentEvent::TurnStart { .. }) {
                    turns_started += 1;
                    if turns_started > max_turns {
                        state.lock().queue.push(
                            serde_json::json!({
                                "type": "error",
                                "message": format!("max_turns ({max_turns}) exceeded; aborting"),
                                "error_type": "resource_limit",
                            })
                            .to_string(),
                        );
                        abort.cancel();
                    }
                }
                let done = matches!(event, AgentEvent::AgentEnd { .. });
                let err = matches!(event, AgentEvent::Error(_));
                state.lock().queue.push(event_to_json(&event));
                if done || err {
                    break;
                }
            }
            state.lock().running = false;
        });
    }

    /// Drain queued events as JSON lines (one string per event).
    pub fn poll(&self) -> Vec<String> {
        let mut state = self.state.lock();
        std::mem::take(&mut state.queue)
    }

    /// Abort the current run. `error`/`aborted` events follow on `poll()`.
    pub fn cancel(&self) {
        self.state.lock().abort.cancel();
    }

    /// Whether a run is currently in flight.
    pub fn is_running(&self) -> bool {
        self.state.lock().running
    }

    /// Resolve a pump tool call: push `result_json` into the channel the
    /// pending tool's `execute` is awaiting. `tool_use_id` is the id from
    /// the `tool_execution_start` event.
    pub fn resolve_tool(&self, tool_use_id: String, result_json: String) -> Result<(), JsValue> {
        let mut state = self.state.lock();
        match state.pump_tx.remove(&tool_use_id) {
            Some(mut tx) => tx
                .try_send(result_json)
                .map_err(|e| JsValue::from_str(&format!("resolve_tool failed: {e}"))),
            None => Err(JsValue::from_str(&format!(
                "resolve_tool: unknown or already-resolved tool_use_id '{tool_use_id}'"
            ))),
        }
    }
}

/// Single `LoopConfig` construction point — mirrors
/// `llm-harness-runtime/tests/wasm-smoke/src/lib.rs::mock_config`
/// field-for-field, with the idle watchdog armed (exercises the
/// kernel's futures-timer bridge on every turn).
fn build_config(
    request: &RunRequest,
    model: &str,
    max_tokens: u32,
    temperature: Option<f32>,
    tools: Vec<Arc<dyn llm_harness_types::Tool>>,
    abort: CancellationToken,
) -> LoopConfig {
    LoopConfig {
        run: Arc::new(RunContext::new(RunRequest {
            initial_messages: request.initial_messages.clone(),
            extensions: llm_harness_types::RunExtensions::new(),
        })),
        model: model.to_string(),
        max_tokens,
        temperature,
        thinking_level: llm_harness_types::ThinkingLevel::Off,
        tools,
        active_tools: None,
        default_execution_mode: ToolExecutionMode::Parallel,
        final_answer_mode: FinalAnswerMode::default(),
        env: Arc::new(llm_harness_loop::test_utils::NoOpEnv),
        abort,
        stream_options: StreamOptions {
            stream_idle_timeout_ms: Some(5000),
            ..StreamOptions::default()
        },
        convert_to_llm: Arc::new(DefaultConvertToLlm::new()),
        transform_context: None,
        prepare_next_turn: None,
        should_stop: None,
        before_provider_request: None,
        after_provider_response: None,
        final_answer_validator: None,
        provider_error: None,
        steer_rx: None,
        follow_up_rx: None,
        retry: None,
        response_format: None,
    }
}
